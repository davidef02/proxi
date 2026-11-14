//! Local sentence embeddings: ONNX Runtime + tokenizers, mean pooling, L2 norm.

use std::borrow::Cow;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use ort::environment::Environment;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

/// ONNX Runtime allows one environment per process; every session reuses this one.
static ENV: OnceLock<Environment> = OnceLock::new();

/// The ONNX file a transformers.js dtype maps to inside a model folder.
pub fn onnx_file(dtype: &str) -> Result<String, String> {
    let suffix = match dtype {
        "fp32" => "",
        "fp16" => "_fp16",
        "int8" => "_int8",
        "uint8" => "_uint8",
        "q8" => "_quantized",
        "q4" => "_q4",
        "q4f16" => "_q4f16",
        "bnb4" => "_bnb4",
        other => return Err(format!("unsupported dtype: {other}")),
    };
    Ok(format!("onnx/model{suffix}.onnx"))
}

fn environment() -> Result<Environment, String> {
    if let Some(env) = ENV.get() {
        return Ok(env.clone());
    }
    // Two first loads at once would both call init, and the second would be told only one
    // environment is allowed per process.
    static BUILD: Mutex<()> = Mutex::new(());
    let _guard = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(env) = ENV.get() {
        return Ok(env.clone());
    }
    let env = ort::init()
        .with_name("proxi")
        .build()
        .map_err(|e| e.to_string())?;
    let _ = ENV.set(env.clone());
    Ok(env)
}

pub struct Embedder {
    session: Session,
    tokenizer: tokenizers::Tokenizer,
    inputs: Vec<String>,
    output: String,
}

impl Embedder {
    pub fn load(model_dir: &Path, dtype: &str) -> Result<Self, String> {
        let mut tokenizer = tokenizers::Tokenizer::from_file(model_dir.join("tokenizer.json"))
            .map_err(|e| format!("tokenizer: {e}"))?;
        if tokenizer.get_padding().is_none() {
            tokenizer.with_padding(Some(tokenizers::PaddingParams::default()));
        }
        if tokenizer.get_truncation().is_none() {
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: 512,
                    ..Default::default()
                }))
                .map_err(|e| format!("tokenizer: {e}"))?;
        }

        let env = environment()?;
        let model = model_dir.join(onnx_file(dtype)?);
        let session = Session::builder(&env)
            .map_err(|e| format!("model: {e}"))?
            .commit_from_file(&model)
            .map_err(|e| format!("model: {e}"))?;

        let inputs: Vec<String> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .collect();
        for name in &inputs {
            if !matches!(
                name.as_str(),
                "input_ids" | "attention_mask" | "token_type_ids"
            ) {
                return Err(format!("unexpected model input: {name}"));
            }
        }

        let output = if session
            .outputs()
            .iter()
            .any(|out| out.name() == "last_hidden_state")
        {
            "last_hidden_state".to_string()
        } else {
            session
                .outputs()
                .first()
                .map(|out| out.name().to_string())
                .ok_or_else(|| "model: no outputs".to_string())?
        };

        Ok(Self {
            session,
            tokenizer,
            inputs,
            output,
        })
    }

    pub fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| format!("tokenizer: {e}"))?;
        let batch = encodings.len();
        let seq = encodings[0].get_ids().len();
        if encodings.iter().any(|e| e.get_ids().len() != seq) {
            return Err("tokenizer: padded batch has ragged lengths".to_string());
        }

        let mut ids = Vec::with_capacity(batch * seq);
        let mut mask = Vec::with_capacity(batch * seq);
        let mut types = Vec::with_capacity(batch * seq);
        for encoding in &encodings {
            ids.extend(encoding.get_ids().iter().map(|&v| i64::from(v)));
            mask.extend(encoding.get_attention_mask().iter().map(|&v| i64::from(v)));
            types.extend(encoding.get_type_ids().iter().map(|&v| i64::from(v)));
        }

        let shape = vec![batch as i64, seq as i64];
        let mut values: Vec<(Cow<'_, str>, SessionInputValue<'_>)> =
            Vec::with_capacity(self.inputs.len());
        for name in &self.inputs {
            let data = match name.as_str() {
                "input_ids" => ids.clone(),
                "attention_mask" => mask.clone(),
                _ => types.clone(),
            };
            let tensor =
                Tensor::<i64>::from_array((shape.clone(), data)).map_err(|e| e.to_string())?;
            values.push((Cow::Owned(name.clone()), tensor.into()));
        }

        let outputs = self.session.run(values).map_err(|e| e.to_string())?;
        let (out_shape, hidden) = outputs[self.output.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        let dims: &[i64] = out_shape;
        if dims.len() != 3 || dims[0] != batch as i64 || dims[1] != seq as i64 {
            return Err(format!("model: unexpected output shape {dims:?}"));
        }

        Ok(mean_pool_normalize(
            hidden,
            batch,
            seq,
            dims[2] as usize,
            &mask,
        ))
    }
}

/// Mean of the unmasked token vectors, then L2-normalised; padding never contributes.
pub fn mean_pool_normalize(
    hidden: &[f32],
    batch: usize,
    seq: usize,
    dim: usize,
    mask: &[i64],
) -> Vec<Vec<f32>> {
    let mut pooled = Vec::with_capacity(batch);
    for b in 0..batch {
        let mut sum = vec![0.0_f32; dim];
        let mut count = 0_usize;
        for t in 0..seq {
            if mask.get(b * seq + t).copied().unwrap_or(0) == 0 {
                continue;
            }
            count += 1;
            let row = (b * seq + t) * dim;
            for (d, slot) in sum.iter_mut().enumerate() {
                if let Some(value) = hidden.get(row + d) {
                    *slot += value;
                }
            }
        }
        let divisor = count.max(1) as f32;
        for slot in &mut sum {
            *slot /= divisor;
        }
        let norm = sum.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 && norm.is_finite() {
            for slot in &mut sum {
                *slot /= norm;
            }
        }
        pooled.push(sum);
    }
    pooled
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("proxi-embedder-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn onnx_file_follows_the_dtype_suffix_map() {
        assert_eq!(onnx_file("q8").unwrap(), "onnx/model_quantized.onnx");
        assert_eq!(onnx_file("fp32").unwrap(), "onnx/model.onnx");
        assert_eq!(onnx_file("q4f16").unwrap(), "onnx/model_q4f16.onnx");
        assert_eq!(onnx_file("xx").unwrap_err(), "unsupported dtype: xx");
    }

    #[test]
    fn mean_pool_normalizes_and_ignores_padding() {
        // 2 rows x 3 tokens x 2 dims; padding values are 100 to prove the mask is honoured.
        let hidden = [
            1.0, 2.0, 5.0, 6.0, 100.0, 100.0, // row 0: mean [3, 4], norm 5
            0.0, 2.0, 100.0, 100.0, 100.0, 100.0, // row 1: mean [0, 2], norm 2
        ];
        let out = mean_pool_normalize(&hidden, 2, 3, 2, &[1, 1, 0, 1, 0, 0]);
        assert_eq!(out.len(), 2);
        assert!((out[0][0] - 0.6).abs() < 1e-6);
        assert!((out[0][1] - 0.8).abs() < 1e-6);
        assert!(out[1][0].abs() < 1e-6);
        assert!((out[1][1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_of_an_empty_row_is_zero_without_nan() {
        let hidden = [1.0, 2.0, 3.0, 4.0];
        let out = mean_pool_normalize(&hidden, 1, 2, 2, &[0, 0]);
        assert!(out[0].iter().all(|v| *v == 0.0));
        assert!(out[0].iter().all(|v| v.is_finite()));
    }

    #[test]
    fn load_without_a_tokenizer_names_the_tokenizer() {
        let dir = temp_dir("empty");
        let err = Embedder::load(&dir, "q8")
            .err()
            .expect("load without a tokenizer should fail");
        assert!(err.starts_with("tokenizer:"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_with_a_garbage_tokenizer_errors() {
        let dir = temp_dir("garbage");
        fs::write(dir.join("tokenizer.json"), b"not json").unwrap();
        assert!(Embedder::load(&dir, "q8").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
