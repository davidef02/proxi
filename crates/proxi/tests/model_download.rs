mod common;

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use proxi::embedder::{onnx_file, Embedder};
use proxi::embeddings::{model_dir, model_present, models_dir, EmbeddingConfig};
use proxi::model_download::{ensure_model, model_files};

use common::fake_hf::FakeHf;

fn config() -> EmbeddingConfig {
    EmbeddingConfig::load(&common::repo_root()).expect("embedding config")
}

fn model_paths(cfg: &EmbeddingConfig) -> Vec<String> {
    model_files(cfg)
        .expect("model files")
        .iter()
        .map(|file| format!("/{}/resolve/main/{file}", cfg.model))
        .collect()
}

/// Names of the staging folders a failed download would leave behind.
fn leftover_downloads(models: &Path) -> Vec<String> {
    fs::read_dir(models)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.starts_with(".download-"))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_from_the_configured_url_then_search_works() {
    let cfg = config();
    let Some(root) = common::model_root(&cfg.model) else {
        eprintln!("skipped: embedding model not downloaded");
        return;
    };
    let temp = common::temp_dir("model-download-ok");
    let models = models_dir(&temp.0);
    let fake = FakeHf::start(Some(root), HashMap::new()).await;

    assert!(ensure_model(&models, &cfg, Some(&fake.url)).await);

    assert_eq!(fake.requests(), model_paths(&cfg));
    assert!(model_present(&models, &cfg));
    assert!(leftover_downloads(&models).is_empty());

    let mut embedder =
        Embedder::load(&model_dir(&models, &cfg), &cfg.dtype).expect("downloaded model loads");
    let vectors = embedder
        .embed(&["computed".to_string()])
        .expect("downloaded model embeds");
    assert_eq!(vectors.len(), 1);
    assert!(!vectors[0].is_empty());
    assert!(vectors[0].iter().all(|v| v.is_finite()));
    let norm = vectors[0].iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "not normalised: {norm}");
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_calls_download_once() {
    let cfg = config();
    let Some(root) = common::model_root(&cfg.model) else {
        eprintln!("skipped: embedding model not downloaded");
        return;
    };
    let temp = common::temp_dir("model-download-once");
    let models = models_dir(&temp.0);
    let fake = FakeHf::start(Some(root), HashMap::new()).await;

    let (first, second) = tokio::join!(
        ensure_model(&models, &cfg, Some(&fake.url)),
        ensure_model(&models, &cfg, Some(&fake.url))
    );
    assert!(first);
    assert!(second);

    let config_path = format!("/{}/resolve/main/config.json", cfg.model);
    let configs = fake
        .requests()
        .iter()
        .filter(|path| **path == config_path)
        .count();
    assert_eq!(configs, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn present_model_makes_no_request() {
    let cfg = config();
    let temp = common::temp_dir("model-download-present");
    let models = models_dir(&temp.0);
    let dir = model_dir(&models, &cfg);
    fs::create_dir_all(dir.join("onnx")).expect("create model dir");
    fs::write(dir.join("config.json"), b"{}").expect("write config.json");
    fs::write(dir.join(onnx_file(&cfg.dtype).unwrap()), b"x").expect("write onnx");
    let fake = FakeHf::start(None, HashMap::new()).await;

    assert!(ensure_model(&models, &cfg, Some(&fake.url)).await);
    assert!(fake.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_files_fail_cleanly() {
    let cfg = config();
    let temp = common::temp_dir("model-download-missing");
    let models = models_dir(&temp.0);
    let fake = FakeHf::start(None, HashMap::new()).await;

    assert!(!ensure_model(&models, &cfg, Some(&fake.url)).await);
    assert!(!model_present(&models, &cfg));
    assert!(!model_dir(&models, &cfg).join("config.json").exists());
    assert!(leftover_downloads(&models).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_download_fails_the_smoke_load() {
    let cfg = config();
    let temp = common::temp_dir("model-download-corrupt");
    let models = models_dir(&temp.0);
    let mut files = HashMap::new();
    for path in model_paths(&cfg) {
        files.insert(path, b"not a model".to_vec());
    }
    let fake = FakeHf::start(None, files).await;

    assert!(!ensure_model(&models, &cfg, Some(&fake.url)).await);
    assert!(!model_present(&models, &cfg));
    assert!(!model_dir(&models, &cfg).join("config.json").exists());
    assert!(leftover_downloads(&models).is_empty());
    assert_eq!(fake.requests(), model_paths(&cfg));
}
