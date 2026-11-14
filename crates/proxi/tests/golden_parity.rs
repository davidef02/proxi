mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{Client, Response};
use serde_json::Value;

use proxi::server::{start, ServerOptions};

fn golden_file(route: &str) -> PathBuf {
    common::repo_root()
        .join("crates")
        .join("proxi")
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join(format!("{route}.json"))
}

/// Replays one captured route against a fresh Rust server and compares status and body. `ordered`
/// also compares the serialised bodies, so key order has to match Node's.
async fn replay(route: &str, ordered: bool) {
    let file = golden_file(route);
    let raw = fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    let golden: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("parse {}: {error}", file.display()));

    let dir = common::temp_dir(route);
    let repo = dir.0.join("repo");
    let data = dir.0.join("data");
    fs::create_dir_all(&repo).expect("create repo");
    fs::create_dir_all(&data).expect("create data");

    for (rel, seed) in golden["seed"].as_object().expect("seed object") {
        let target = repo.join(rel);
        fs::create_dir_all(target.parent().expect("seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(&target, serde_json::to_vec_pretty(seed).expect("serialise seed"))
            .unwrap_or_else(|error| panic!("write seed {rel}: {error}"));
    }
    write_data_seed(&data, &golden["dataSeed"]);

    let mut options = ServerOptions::new(repo.clone());
    options.data_root = Some(data.clone());
    // Long enough that only the request's event can appear in the stream.
    options.keep_alive = Duration::from_secs(120);
    let server = start(options).await.expect("start Rust server");
    let client = Client::new();
    let repo_root = proxi::data_root::resolve_path(&repo).display().to_string();
    let data_root = proxi::data_root::resolve_path(&data).display().to_string();

    for case in golden["cases"].as_array().expect("cases array") {
        let name = case["name"].as_str().expect("case name");
        let label = format!("{route}/{name}");
        let request = &case["request"];
        let method = request["method"].as_str().expect("request method");
        let path = request["path"].as_str().expect("request path");

        // The events case needs its subscriber before the request that triggers the event.
        let mut stream = if case.get("firstEvent").is_some() {
            let response = client
                .get(format!("{}/api/events", server.url))
                .send()
                .await
                .unwrap_or_else(|error| panic!("{label}: GET /api/events: {error}"));
            assert!(
                response.status().is_success(),
                "{label}: GET /api/events: {}",
                response.status()
            );
            Some(response)
        } else {
            None
        };

        let sent = if method == "GET" {
            client.get(format!("{}{path}", server.url)).send().await
        } else {
            let body = match request.get("rawBody") {
                Some(raw) => raw.as_str().expect("rawBody is a string").to_string(),
                None => serde_json::to_string(&request["body"]).expect("serialise request body"),
            };
            client
                .post(format!("{}{path}", server.url))
                .header("x-learner-client", "1")
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
        };
        let response = sent.unwrap_or_else(|error| panic!("{label}: {method} {path}: {error}"));

        let expected_status = case["response"]["status"].as_u64().expect("golden status");
        assert_eq!(
            u64::from(response.status().as_u16()),
            expected_status,
            "{label}"
        );
        let text = response
            .text()
            .await
            .unwrap_or_else(|error| panic!("{label}: read body: {error}"));
        let body: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{label}: not JSON ({error}): {text}"));
        let expected_body = case["response"]["body"].clone();

        if (200..300).contains(&expected_status) {
            let normalised = normalise(body, &repo_root, &data_root);
            assert_eq!(normalised, expected_body, "{label}");
            if ordered {
                assert_eq!(
                    serde_json::to_string(&normalised).expect("serialise Rust body"),
                    serde_json::to_string(&expected_body).expect("serialise golden body"),
                    "{label}: key order"
                );
            }
        } else {
            // Error messages differ word for word; only the shape is shared.
            assert!(
                expected_body["error"].is_string(),
                "{label}: golden body: {expected_body}"
            );
            assert!(body["error"].is_string(), "{label}: Rust body: {body}");
        }

        if let Some(mut stream) = stream.take() {
            let event = first_event(&mut stream).await;
            assert_eq!(
                normalise(event, &repo_root, &data_root),
                case["firstEvent"],
                "{label}: first event"
            );
        }
    }

    server.close();
}

/// Writes a golden's `dataSeed` verbatim under the data folder: raw input, never normalised.
fn write_data_seed(data: &Path, seed: &Value) {
    let Some(files) = seed.as_object() else {
        return;
    };
    for (rel, text) in files {
        let target = data.join(rel);
        fs::create_dir_all(target.parent().expect("data seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(
            &target,
            text.as_str()
                .expect("data seed value is a string")
                .as_bytes(),
        )
        .unwrap_or_else(|error| panic!("write data seed {rel}: {error}"));
    }
}

/// Waits for the first complete `data:` block of an SSE stream.
async fn first_event(stream: &mut Response) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut buffer = String::new();
    loop {
        if let Some(index) = buffer.find("\n\n") {
            let block = buffer[..index].to_string();
            let data: String = block
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .collect();
            assert!(!data.is_empty(), "first SSE block has no data: {block:?}");
            return serde_json::from_str(&data)
                .unwrap_or_else(|error| panic!("SSE event is not JSON ({error}): {data}"));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let chunk = tokio::time::timeout(remaining, stream.chunk())
            .await
            .unwrap_or_else(|_| panic!("no SSE event within 10 s; got {buffer:?}"))
            .expect("read SSE chunk")
            .expect("SSE stream closed before the first event");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
    }
}

/// Mirrors `normalise` of app/test/capture-golden.ts: roots first, then ISO timestamps.
fn normalise(value: Value, repo: &str, data: &str) -> Value {
    match value {
        Value::String(text) => {
            if text == repo {
                Value::String("<repoRoot>".to_string())
            } else if text == data {
                Value::String("<dataRoot>".to_string())
            } else if is_iso(&text) {
                Value::String("<iso>".to_string())
            } else {
                Value::String(text)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| normalise(item, repo, data))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                out.insert(key, normalise(item, repo, data));
            }
            Value::Object(out)
        }
        other => other,
    }
}

/// `YYYY-MM-DDTHH:MM:SS` with an optional fraction, UTC only (no regex crate here).
fn is_iso(s: &str) -> bool {
    let b = s.as_bytes();
    let two_digits = |at: usize| b[at..at + 2].iter().all(u8::is_ascii_digit);
    if b.len() < 20
        || !b[..4].iter().all(u8::is_ascii_digit)
        || b[4] != b'-'
        || !two_digits(5)
        || b[7] != b'-'
        || !two_digits(8)
        || b[10] != b'T'
        || !two_digits(11)
        || b[13] != b':'
        || !two_digits(14)
        || b[16] != b':'
        || !two_digits(17)
    {
        return false;
    }
    match b[19] {
        b'Z' => b.len() == 20,
        b'.' => {
            let rest = &b[20..];
            rest.len() >= 2
                && rest[rest.len() - 1] == b'Z'
                && rest[..rest.len() - 1].iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

#[test]
fn is_iso_accepts_only_utc_timestamps() {
    assert!(is_iso("2026-11-12T10:00:00.000Z"));
    assert!(is_iso("2026-11-12T10:00:00Z"));
    assert!(!is_iso("2026-11-12"));
    assert!(!is_iso("2026-11-12T10:00:00+01:00"));
    assert!(!is_iso("x"));
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_match_the_node_golden() {
    replay("settings", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_match_the_node_golden() {
    replay("limits", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn events_match_the_node_golden() {
    replay("events", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn courses_match_the_node_golden() {
    replay("courses", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lesson_reading_matches_the_node_golden() {
    replay("lesson", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_matches_the_node_golden() {
    replay("graph", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exercises_match_the_node_golden() {
    replay("exercises", true).await;
}
