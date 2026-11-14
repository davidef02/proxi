//! Downloads the embedding model from Hugging Face when it is missing.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::embedder::{onnx_file, Embedder};
use crate::embeddings::{model_dir, model_present, EmbeddingConfig};

pub const HF_BASE_URL: &str = "https://huggingface.co";
pub const SKIP_MODEL_ENV: &str = "PROXI_SKIP_MODEL";

/// The files transformers.js keeps in a model folder, `config.json` last so `model_present` never
/// sees a half-downloaded model.
pub fn model_files(cfg: &EmbeddingConfig) -> Result<Vec<String>, String> {
    Ok(vec![
        "tokenizer.json".to_string(),
        "tokenizer_config.json".to_string(),
        onnx_file(&cfg.dtype)?,
        "config.json".to_string(),
    ])
}

pub fn file_url(base: &str, model: &str, file: &str) -> String {
    format!(
        "{}/{model}/resolve/main/{file}",
        base.trim_end_matches('/')
    )
}

/// One lock per model folder: two calls for the same model race to be the one that downloads.
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `true` when the model is usable afterwards. Never fails loudly and never panics: the caller can
/// retry on the next start.
pub async fn ensure_model(
    models_dir: &Path,
    cfg: &EmbeddingConfig,
    base_url: Option<&str>,
) -> bool {
    if model_present(models_dir, cfg) {
        return true;
    }
    let lock = {
        let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(locks.entry(model_dir(models_dir, cfg)).or_default())
    };
    let _guard = lock.lock().await;
    if model_present(models_dir, cfg) {
        return true;
    }
    download(models_dir, cfg, base_url.unwrap_or(HF_BASE_URL)).await
}

async fn download(models_dir: &Path, cfg: &EmbeddingConfig, base: &str) -> bool {
    let Ok(files) = model_files(cfg) else {
        return false;
    };
    let staging = staging_dir(models_dir);
    let staged = model_dir(&staging, cfg);

    let mut ok = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => {
            let mut fetched = true;
            for file in &files {
                if !fetch(&client, base, &cfg.model, file, &join_file(&staged, file)).await {
                    fetched = false;
                    break;
                }
            }
            fetched
        }
        Err(_) => false,
    };

    if ok {
        // The copy that does not load here would not load after the move either.
        let dir = staged.clone();
        let dtype = cfg.dtype.clone();
        ok = tokio::task::spawn_blocking(move || {
            let mut embedder = Embedder::load(&dir, &dtype)?;
            embedder.embed(&["ok".to_string()]).map(|_| ())
        })
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false);
    }

    if ok {
        let target = model_dir(models_dir, cfg);
        ok = files.iter().all(|file| {
            move_file(
                &join_file(&staged, file),
                &join_file(&target, file),
            )
        });
    }

    // Also on failure: no half-written staging folder survives a download.
    let _ = std::fs::remove_dir_all(&staging);
    ok && model_present(models_dir, cfg)
}

async fn fetch(
    client: &reqwest::Client,
    base: &str,
    model: &str,
    file: &str,
    dest: &Path,
) -> bool {
    let Ok(mut response) = client.get(file_url(base, model, file)).send().await else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    let Some(parent) = dest.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let Ok(mut out) = std::fs::File::create(dest) else {
        return false;
    };
    loop {
        match response.chunk().await {
            Ok(Some(bytes)) => {
                if out.write_all(&bytes).is_err() {
                    return false;
                }
            }
            Ok(None) => return true,
            Err(_) => return false,
        }
    }
}

fn join_file(base: &Path, file: &str) -> PathBuf {
    file.split('/')
        .fold(base.to_path_buf(), |path, part| path.join(part))
}

fn staging_dir(models_dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    models_dir.join(format!(".download-{}-{nanos}", std::process::id()))
}

fn move_file(from: &Path, to: &Path) -> bool {
    if let Some(parent) = to.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    if std::fs::rename(from, to).is_ok() {
        return true;
    }
    // Windows refuses a rename onto an open or locked file: copy and drop the staged one instead.
    if std::fs::copy(from, to).is_err() {
        return false;
    }
    let _ = std::fs::remove_file(from);
    true
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn cfg() -> EmbeddingConfig {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        EmbeddingConfig::load(&repo).unwrap()
    }

    #[test]
    fn file_url_joins_the_hf_layout() {
        assert_eq!(
            file_url("http://h/", "Xenova/m", "onnx/model_quantized.onnx"),
            "http://h/Xenova/m/resolve/main/onnx/model_quantized.onnx"
        );
        assert_eq!(
            file_url("http://h", "Xenova/m", "config.json"),
            "http://h/Xenova/m/resolve/main/config.json"
        );
    }

    #[test]
    fn model_files_put_config_last() {
        assert_eq!(
            model_files(&cfg()).unwrap(),
            vec![
                "tokenizer.json",
                "tokenizer_config.json",
                "onnx/model_quantized.onnx",
                "config.json"
            ]
        );
    }

    #[test]
    fn model_files_follow_the_dtype() {
        let fp32 = EmbeddingConfig {
            dtype: "fp32".to_string(),
            ..cfg()
        };
        assert_eq!(
            model_files(&fp32).unwrap()[2],
            "onnx/model.onnx".to_string()
        );
    }

    #[test]
    fn model_files_reject_an_unknown_dtype() {
        let odd = EmbeddingConfig {
            dtype: "xx".to_string(),
            ..cfg()
        };
        assert_eq!(
            model_files(&odd).unwrap_err(),
            "unsupported dtype: xx".to_string()
        );
    }

    #[test]
    fn skip_model_env_matches_the_node_constant() {
        assert_eq!(SKIP_MODEL_ENV, "PROXI_SKIP_MODEL");
        assert!(
            include_str!("../../../app/shared/identity.mjs")
                .contains("SKIP_MODEL_ENV = \"PROXI_SKIP_MODEL\"")
        );
    }
}
