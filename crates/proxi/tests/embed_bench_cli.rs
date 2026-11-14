mod common;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use proxi::embeddings::{model_dir, models_dir, EmbeddingConfig};

use common::fake_hf::FakeHf;

fn bench(repo: &Path, data: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(common::proxi_bin())
        .arg("embed-bench")
        .args(args)
        .env("LEARNER_REPO_ROOT", repo)
        .env("LEARNER_DATA_ROOT", data)
        .env_remove("PROXI_SKIP_MODEL")
        .output()
        .expect("run proxi embed-bench");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn write_queries(data: &Path, body: &str) {
    let dir = data.join("bench");
    fs::create_dir_all(&dir).expect("create bench dir");
    fs::write(dir.join("embed-bench-queries.json"), body).expect("write queries");
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_queries_fail_before_any_download() {
    let repo = common::temp_dir("embed-bench-repo");
    let data = common::temp_dir("embed-bench-data");
    common::write_node_lines(&data.0, common::SAMPLE_CONCEPTS);
    let fake = FakeHf::start(None, HashMap::new()).await;
    let url = fake.url.as_str();

    let cases = [
        ("[]", "must be a non-empty JSON array"),
        (
            r#"[{"q":"x","lang":"fr","expect":"Props"}]"#,
            r#"lang must be "it" or "en""#,
        ),
        (
            r#"[{"q":"x","lang":"it","expect":"Nope"}]"#,
            "does not match any concept name: Nope",
        ),
    ];
    for (body, message) in cases {
        write_queries(&data.0, body);
        let (code, _, err) = bench(&repo.0, &data.0, &["--huggingface-url", url]);
        assert_eq!(code, Some(1), "{body}: {err}");
        assert!(err.contains(message), "{body}: {err}");
        assert!(
            fake.requests().is_empty(),
            "{body}: the model was fetched first"
        );
    }

    fs::remove_file(data.0.join("bench").join("embed-bench-queries.json"))
        .expect("remove queries");
    let (code, _, err) = bench(&repo.0, &data.0, &["--huggingface-url", url]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("queries not found:"), "{err}");
    assert!(fake.requests().is_empty());

    let empty = common::temp_dir("embed-bench-empty");
    let (code, _, err) = bench(&repo.0, &empty.0, &["--huggingface-url", url]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("no concepts in"), "{err}");
    assert!(fake.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_download_exits_1() {
    let repo = common::temp_dir("embed-bench-failed-download-repo");
    let data = common::temp_dir("embed-bench-failed-download-data");
    common::write_node_lines(&data.0, common::SAMPLE_CONCEPTS);
    write_queries(
        &data.0,
        r#"[{"q":"Props","lang":"it","expect":"Props"}]"#,
    );
    // no model stored and a host that serves nothing: the download cannot succeed
    let fake = FakeHf::start(None, HashMap::new()).await;

    let (code, out, err) = bench(
        &repo.0,
        &data.0,
        &["--huggingface-url", &fake.url, "--no-write"],
    );
    assert_eq!(code, Some(1), "{out}\n{err}");
    assert!(
        err.contains("embedding model not available: download of"),
        "{err}"
    );
    assert!(!fake.requests().is_empty(), "the download was never tried");
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_measures_and_records() {
    let cfg = EmbeddingConfig::load(&common::repo_root()).expect("embedding-model.json");
    let Some(model_root) = common::model_root(&cfg.model) else {
        eprintln!("skipped: embedding model not downloaded");
        return;
    };

    let repo = common::temp_dir("embed-bench-record-repo");
    let data = common::temp_dir("embed-bench-record-data");
    common::write_node_lines(&data.0, common::SAMPLE_CONCEPTS);
    let model_file = EmbeddingConfig::model_file(&repo.0);
    fs::create_dir_all(model_file.parent().expect("model file dir")).expect("create model dir");
    fs::copy(
        common::repo_root().join("app/server/graph/embedding-model.json"),
        &model_file,
    )
    .expect("copy the real model file");
    write_queries(
        &data.0,
        r#"[
            {"q":"Props","lang":"it","expect":"Props"},
            {"q":"Ciclo for","lang":"it","expect":"Ciclo for"},
            {"q":"computed","lang":"en","expect":"Proprietà calcolate"}
        ]"#,
    );
    let fake = FakeHf::start(Some(model_root), HashMap::new()).await;

    let (code, out, err) = bench(&repo.0, &data.0, &["--huggingface-url", &fake.url]);
    assert!(
        code == Some(0) || (code == Some(1) && out.contains("FAIL")),
        "exit {code:?}\nstdout: {out}\nstderr: {err}"
    );
    assert!(out.contains("recall@3: 100.0% (3/3)"), "{out}\n{err}");
    let downloaded = model_dir(&models_dir(&repo.0), &cfg).join("config.json");
    assert!(
        downloaded.is_file(),
        "model not downloaded to {}",
        downloaded.display()
    );

    let text = fs::read_to_string(&model_file).expect("read model file");
    let value: serde_json::Value = serde_json::from_str(&text).expect("parse model file");
    let keys: Vec<&str> = value
        .as_object()
        .expect("model file object")
        .keys()
        .map(String::as_str)
        .collect();
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
    let record = &value["bench"];
    assert_eq!(record["model"], serde_json::json!(cfg.model));
    assert_eq!(record["dtype"], serde_json::json!(cfg.dtype));
    assert_eq!(record["recall"], serde_json::json!(1.0));
    assert_eq!(record["queries"], serde_json::json!(3));
    assert_eq!(record["concepts"], serde_json::json!(3));
    assert!(record["medianMs"].is_number(), "{}", record["medianMs"]);
    assert!(record["passes"].is_boolean(), "{}", record["passes"]);
    assert_eq!(
        record["measuredAt"]
            .as_str()
            .expect("measuredAt is a string")
            .chars()
            .count(),
        10
    );

    let before = fs::read(&model_file).expect("read model file bytes");
    let requests = fake.requests().len();
    let (_, out, err) = bench(
        &repo.0,
        &data.0,
        &["--huggingface-url", &fake.url, "--no-write"],
    );
    assert!(
        out.contains("--no-write: model file not updated"),
        "{out}\n{err}"
    );
    assert_eq!(
        fs::read(&model_file).expect("read model file bytes"),
        before,
        "--no-write changed the model file"
    );
    assert_eq!(fake.requests().len(), requests, "--no-write fetched again");
}
