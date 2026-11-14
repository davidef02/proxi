//! Embedding model config, model folder, and the sha1-keyed vector cache.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use sha1::{Digest, Sha1};

/// Where the model file lives, from the repo root.
pub const MODEL_FILE: [&str; 4] = ["app", "server", "graph", "embedding-model.json"];

// The same file, compiled in: a temp repo (tests) has no model file to read.
const BUNDLED_MODEL_FILE: &str = include_str!("../../../app/server/graph/embedding-model.json");

/// The model file, also read by Node (`app/server/graph/embeddings.ts`), which throws at import.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingConfig {
    pub model: String,
    pub dtype: String,
    pub query_prefix: String,
    pub passage_prefix: String,
    pub min_score: f64,
}

impl EmbeddingConfig {
    pub fn model_file(repo_root: &Path) -> PathBuf {
        MODEL_FILE
            .iter()
            .fold(repo_root.to_path_buf(), |path, part| path.join(part))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text)
            .map_err(|e| format!("embedding-model.json: invalid JSON: {e}"))?;
        let Value::Object(map) = value else {
            return Err("embedding-model.json: not an object".to_string());
        };
        Ok(Self {
            model: non_empty_string_field(&map, "model")?,
            dtype: non_empty_string_field(&map, "dtype")?,
            query_prefix: string_field(&map, "queryPrefix")?,
            passage_prefix: string_field(&map, "passagePrefix")?,
            min_score: number_field(&map, "minScore")?,
        })
    }

    pub fn load(repo_root: &Path) -> Result<Self, String> {
        let file = Self::model_file(repo_root);
        match fs::read_to_string(&file) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == ErrorKind::NotFound => Self::parse(BUNDLED_MODEL_FILE),
            Err(e) => Err(format!("{}: {e}", file.display())),
        }
    }
}

fn non_empty_string_field(map: &Map<String, Value>, key: &str) -> Result<String, String> {
    match map.get(key).and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Ok(text.to_string()),
        _ => Err(format!(
            "embedding-model.json: {key} must be a non-empty string"
        )),
    }
}

fn string_field(map: &Map<String, Value>, key: &str) -> Result<String, String> {
    match map.get(key).and_then(Value::as_str) {
        Some(text) => Ok(text.to_string()),
        None => Err(format!("embedding-model.json: {key} must be a string")),
    }
}

fn number_field(map: &Map<String, Value>, key: &str) -> Result<f64, String> {
    match map.get(key).and_then(Value::as_f64) {
        Some(x) if x.is_finite() => Ok(x),
        _ => Err(format!("embedding-model.json: {key} must be a number")),
    }
}

/// `<repo>/.learner/models`, the folder transformers.js uses too.
pub fn models_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".learner").join("models")
}

/// `<models>/<model>`: the model id's slashes are folders.
pub fn model_dir(models_dir: &Path, cfg: &EmbeddingConfig) -> PathBuf {
    cfg.model
        .split('/')
        .fold(models_dir.to_path_buf(), |path, part| path.join(part))
}

/// `config.json` plus at least one onnx weight: the check of `scripts/fetch-embedding-model.mjs`.
pub fn model_present(models_dir: &Path, cfg: &EmbeddingConfig) -> bool {
    let dir = model_dir(models_dir, cfg);
    if !dir.join("config.json").is_file() {
        return false;
    }
    fs::read_dir(dir.join("onnx"))
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".onnx"))
        })
        .unwrap_or(false)
}

pub fn sha1_hex(text: &str) -> String {
    format!("{:x}", Sha1::digest(text.as_bytes()))
}

/// Vectors keyed by the passage text they were embedded from. Model, dtype and both prefixes are in
/// the name, so a config change never reads stale vectors.
pub fn cache_file(repo_root: &Path, cfg: &EmbeddingConfig) -> PathBuf {
    let prefixes = sha1_hex(&format!("{}\0{}", cfg.query_prefix, cfg.passage_prefix));
    repo_root
        .join(".learner")
        .join("embeddings")
        .join(format!(
            "{}__{}__{}.json",
            cfg.model.replace('/', "__"),
            cfg.dtype,
            &prefixes[..8]
        ))
}

/// sha1(passage text) -> vector, insertion-ordered so a rewrite keeps the file stable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorCache {
    keys: Vec<String>,
    vectors: HashMap<String, Vec<f32>>,
}

impl VectorCache {
    pub fn get(&self, key: &str) -> Option<&Vec<f32>> {
        self.vectors.get(key)
    }

    pub fn insert(&mut self, key: String, v: Vec<f32>) {
        if self.vectors.insert(key.clone(), v).is_none() {
            self.keys.push(key);
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Vec<f32>)> {
        self.keys
            .iter()
            .filter_map(|key| self.vectors.get(key).map(|v| (key, v)))
    }
}

/// A missing/corrupt/mismatched file is an empty cache, never an error: the vectors come back on the
/// next embedding batch.
pub fn read_cache(file: &Path, cfg: &EmbeddingConfig) -> VectorCache {
    let Ok(text) = fs::read_to_string(file) else {
        return VectorCache::default();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return VectorCache::default();
    };
    let metadata = [
        ("model", cfg.model.as_str()),
        ("dtype", cfg.dtype.as_str()),
        ("queryPrefix", cfg.query_prefix.as_str()),
        ("passagePrefix", cfg.passage_prefix.as_str()),
    ];
    for (key, want) in metadata {
        if map.get(key).and_then(Value::as_str) != Some(want) {
            return VectorCache::default();
        }
    }
    let Some(Value::Object(vectors)) = map.get("vectors") else {
        return VectorCache::default();
    };
    let mut cache = VectorCache::default();
    for (key, value) in vectors {
        let Some(items) = value.as_array() else {
            continue;
        };
        // an entry Node would turn into a NaN cosine is skipped and re-embedded instead
        let numbers: Option<Vec<f32>> = items
            .iter()
            .map(|item| item.as_f64().map(|x| x as f32))
            .collect();
        if let Some(numbers) = numbers {
            cache.insert(key.clone(), numbers);
        }
    }
    cache
}

/// Best effort, never panics: the vectors stay in memory when the disk refuses.
pub fn write_cache(file: &Path, cfg: &EmbeddingConfig, cache: &VectorCache) {
    let Ok(body) = serde_json::to_string(&cache_body(cfg, cache)) else {
        return;
    };
    let mut tmp_name = file.as_os_str().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp_name);
    let written = (|| -> std::io::Result<()> {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&tmp, &body)?;
        fs::rename(&tmp, file)
    })();
    if written.is_err() {
        // Windows: renaming over a file another process holds open fails, so write in place
        let _ = fs::remove_file(&tmp);
        let _ = fs::write(file, &body);
    }
}

fn cache_body(cfg: &EmbeddingConfig, cache: &VectorCache) -> Value {
    let mut vectors = Map::new();
    for (key, vector) in cache.iter() {
        vectors.insert(
            key.clone(),
            Value::Array(vector.iter().map(|&x| Value::from(f64::from(x))).collect()),
        );
    }
    // Key order of `writeCache` in the Node module, so a diff of two cache files stays readable.
    let mut body = Map::new();
    body.insert("model".to_string(), Value::from(cfg.model.clone()));
    body.insert("dtype".to_string(), Value::from(cfg.dtype.clone()));
    body.insert(
        "queryPrefix".to_string(),
        Value::from(cfg.query_prefix.clone()),
    );
    body.insert(
        "passagePrefix".to_string(),
        Value::from(cfg.passage_prefix.clone()),
    );
    body.insert("vectors".to_string(), Value::Object(vectors));
    Value::Object(body)
}

/// Add or replace the `bench` record, keeping every other key and its order.
pub fn record_bench(repo_root: &Path, record: Value) -> Result<(), String> {
    let file = EmbeddingConfig::model_file(repo_root);
    let text = fs::read_to_string(&file).map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            format!("model file not found: {}", file.display())
        } else {
            format!("{}: {e}", file.display())
        }
    })?;
    let mut value: Value =
        serde_json::from_str(&text).map_err(|e| format!("embedding-model.json: invalid JSON: {e}"))?;
    let Value::Object(ref mut map) = value else {
        return Err("embedding-model.json: not an object".to_string());
    };
    map.insert("bench".to_string(), record);
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    crate::js::write_atomic(&file, body.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-embeddings-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn real_repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn cfg() -> EmbeddingConfig {
        EmbeddingConfig::parse(BUNDLED_MODEL_FILE).unwrap()
    }

    #[test]
    fn load_reads_the_real_model_file() {
        let cfg = EmbeddingConfig::load(&real_repo()).unwrap();
        assert_eq!(cfg.model, "Xenova/paraphrase-multilingual-MiniLM-L12-v2");
        assert_eq!(cfg.dtype, "q8");
        assert_eq!(cfg.query_prefix, "");
        assert_eq!(cfg.passage_prefix, "");
        assert_eq!(cfg.min_score, 0.3);
    }

    #[test]
    fn load_falls_back_to_the_bundled_model_file() {
        let empty = temp_dir("bundled");
        assert_eq!(
            EmbeddingConfig::load(&empty).unwrap(),
            EmbeddingConfig::load(&real_repo()).unwrap()
        );
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn parse_rejects_malformed_files() {
        fn error(text: &str) -> String {
            EmbeddingConfig::parse(text).unwrap_err()
        }

        assert!(error("{").starts_with("embedding-model.json: invalid JSON:"));
        assert_eq!(error("[]"), "embedding-model.json: not an object");
        assert_eq!(
            error("{\"model\":1,\"dtype\":\"q8\",\"queryPrefix\":\"\",\"passagePrefix\":\"\",\"minScore\":0.3}"),
            "embedding-model.json: model must be a non-empty string"
        );
        assert_eq!(
            error("{\"model\":\"\",\"dtype\":\"q8\",\"queryPrefix\":\"\",\"passagePrefix\":\"\",\"minScore\":0.3}"),
            "embedding-model.json: model must be a non-empty string"
        );
        assert_eq!(
            error("{\"model\":\"m\",\"dtype\":\"q8\",\"passagePrefix\":\"\",\"minScore\":0.3}"),
            "embedding-model.json: queryPrefix must be a string"
        );
        assert_eq!(
            error("{\"model\":\"m\",\"dtype\":\"q8\",\"queryPrefix\":\"\",\"passagePrefix\":\"\"}"),
            "embedding-model.json: minScore must be a number"
        );
        assert_eq!(
            error("{\"model\":\"m\",\"dtype\":\"q8\",\"queryPrefix\":\"\",\"passagePrefix\":\"\",\"minScore\":\"x\"}"),
            "embedding-model.json: minScore must be a number"
        );
        // extra keys are ignored: the bench writes its record into this same file
        assert!(
            EmbeddingConfig::parse(
                "{\"model\":\"m\",\"dtype\":\"q8\",\"queryPrefix\":\"\",\"passagePrefix\":\"\",\"minScore\":0.3,\"bench\":{}}"
            )
            .is_ok()
        );
    }

    #[test]
    fn models_dir_is_the_learner_models_folder() {
        assert_eq!(
            models_dir(Path::new("repo")),
            Path::new("repo").join(".learner").join("models")
        );
    }

    #[test]
    fn cache_file_is_named_after_the_config() {
        let cfg = cfg();
        let file = cache_file(Path::new("repo"), &cfg);
        assert_eq!(
            file,
            Path::new("repo")
                .join(".learner")
                .join("embeddings")
                .join("Xenova__paraphrase-multilingual-MiniLM-L12-v2__q8__5ba93c9d.json")
        );
        assert_ne!(
            cache_file(
                Path::new("repo"),
                &EmbeddingConfig {
                    dtype: "fp32".to_string(),
                    ..cfg.clone()
                }
            ),
            file
        );
        assert_ne!(
            cache_file(
                Path::new("repo"),
                &EmbeddingConfig {
                    passage_prefix: "passage: ".to_string(),
                    ..cfg.clone()
                }
            ),
            file
        );
    }

    #[test]
    fn sha1_hex_is_lowercase_sha1() {
        assert_eq!(
            sha1_hex("Props. Dati passati dal genitore"),
            "12541d624ffd5c5de144ceacf469cb0ba17b4d65"
        );
        assert_eq!(sha1_hex(""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn model_dir_splits_the_model_id_on_slashes() {
        let cfg = EmbeddingConfig {
            model: "a/b".to_string(),
            ..cfg()
        };
        assert_eq!(
            model_dir(Path::new("models"), &cfg),
            Path::new("models").join("a").join("b")
        );
    }

    #[test]
    fn model_present_needs_config_json_and_an_onnx_file() {
        let dir = temp_dir("present");
        let cfg = cfg();
        let model = model_dir(&dir, &cfg);
        assert!(!model_present(&dir, &cfg));
        fs::create_dir_all(&model).unwrap();
        assert!(!model_present(&dir, &cfg));
        fs::write(model.join("config.json"), b"{}").unwrap();
        assert!(!model_present(&dir, &cfg));
        fs::create_dir_all(model.join("onnx")).unwrap();
        fs::write(model.join("onnx").join("x.txt"), b"x").unwrap();
        assert!(!model_present(&dir, &cfg));
        fs::write(model.join("onnx").join("m.onnx"), b"x").unwrap();
        assert!(model_present(&dir, &cfg));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_round_trips_in_order() {
        let dir = temp_dir("roundtrip");
        let cfg = cfg();
        let file = dir.join("vectors.json");
        let mut cache = VectorCache::default();
        cache.insert("k1".to_string(), vec![0.5, -0.25, 1.0]);
        cache.insert("k2".to_string(), vec![0.0, 0.125]);
        cache.insert("k2".to_string(), vec![0.0, 0.125]);
        assert_eq!(cache.len(), 2);
        write_cache(&file, &cfg, &cache);

        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with(
            "{\"model\":\"Xenova/paraphrase-multilingual-MiniLM-L12-v2\",\"dtype\":\"q8\",\"queryPrefix\":\"\",\"passagePrefix\":\"\",\"vectors\":{"
        ));

        let read = read_cache(&file, &cfg);
        assert_eq!(read.len(), 2);
        assert_eq!(
            read.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            vec!["k1", "k2"]
        );
        assert_eq!(read.get("k1").unwrap(), &vec![0.5f32, -0.25, 1.0]);
        assert_eq!(read.get("k2").unwrap(), &vec![0.0f32, 0.125]);
        assert!(read.get("nope").is_none());
        assert!(!read.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_with_other_metadata_reads_empty() {
        let dir = temp_dir("mismatch");
        let cfg = cfg();
        let file = dir.join("vectors.json");
        let mut cache = VectorCache::default();
        cache.insert("k1".to_string(), vec![1.0, 0.0]);
        write_cache(&file, &cfg, &cache);
        let valid = fs::read_to_string(&file).unwrap();

        assert_eq!(read_cache(&file, &cfg).len(), 1);

        let variants = [
            valid.replace(
                "\"Xenova/paraphrase-multilingual-MiniLM-L12-v2\"",
                "\"other\"",
            ),
            valid.replace("\"dtype\":\"q8\"", "\"dtype\":\"other\""),
            valid.replace("\"queryPrefix\":\"\"", "\"queryPrefix\":\"q\""),
            valid.replace("\"passagePrefix\":\"\"", "\"passagePrefix\":\"p\""),
        ];
        for text in variants {
            assert_ne!(text, valid);
            fs::write(&file, text).unwrap();
            assert!(read_cache(&file, &cfg).is_empty());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_cache_reads_empty_or_skips_the_entry() {
        let dir = temp_dir("corrupt");
        let cfg = cfg();
        let file = dir.join("vectors.json");
        fs::write(&file, "not json").unwrap();
        assert!(read_cache(&file, &cfg).is_empty());
        fs::write(&file, "null").unwrap();
        assert!(read_cache(&file, &cfg).is_empty());
        fs::write(
            &file,
            "{\"model\":\"Xenova/paraphrase-multilingual-MiniLM-L12-v2\",\"dtype\":\"q8\",\"queryPrefix\":\"\",\"passagePrefix\":\"\",\"vectors\":[]}",
        )
        .unwrap();
        assert!(read_cache(&file, &cfg).is_empty());
        fs::write(
            &file,
            "{\"model\":\"Xenova/paraphrase-multilingual-MiniLM-L12-v2\",\"dtype\":\"q8\",\"queryPrefix\":\"\",\"passagePrefix\":\"\",\"vectors\":{\"a\":[1,0],\"b\":[\"a\",1]}}",
        )
        .unwrap();
        let cache = read_cache(&file, &cfg);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get("a").unwrap(), &vec![1.0f32, 0.0]);
        assert!(read_cache(&dir.join("nope.json"), &cfg).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_cache_into_a_broken_path_does_not_panic() {
        let dir = temp_dir("broken-path");
        let cfg = cfg();
        let blocked = dir.join("blocked");
        fs::write(&blocked, b"x").unwrap();
        let mut cache = VectorCache::default();
        cache.insert("k".to_string(), vec![1.0]);
        write_cache(&blocked.join("vectors.json"), &cfg, &cache);
        assert!(!blocked.join("vectors.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_bench_appends_and_replaces() {
        let repo = temp_dir("record-bench");
        let file = EmbeddingConfig::model_file(&repo);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::copy(
            real_repo().join("app/server/graph/embedding-model.json"),
            &file,
        )
        .unwrap();

        record_bench(&repo, json!({"recall": 1.0, "passes": true})).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.ends_with('\n'));
        let value: Value = serde_json::from_str(&text).unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "model",
                "dtype",
                "queryPrefix",
                "passagePrefix",
                "minScore",
                "bench"
            ]
        );
        assert_eq!(value["bench"]["recall"], json!(1.0));

        record_bench(&repo, json!({"recall": 0.5, "passes": false})).unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 6);
        assert_eq!(value["bench"]["recall"], json!(0.5));
        assert_eq!(
            value["model"],
            json!("Xenova/paraphrase-multilingual-MiniLM-L12-v2")
        );

        let missing = temp_dir("record-bench-missing");
        assert!(
            record_bench(&missing, json!({}))
                .unwrap_err()
                .starts_with("model file not found")
        );
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&missing);
    }
}
