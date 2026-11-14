//! Replays the Node session golden against the Rust server (AC1/AC4).
//!
//! Same flow as `captureSessions` of `app/test/capture-golden.ts` on the same two temp folders:
//! create, poll idle, SSE, message, collect to the idle status, get, list, shutdown, state file.
//! Every output goes through a port of `normaliseSession`, then value and compact serialisation
//! (key order) are compared.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::Value;

use proxi::server::{RunningServer, ServerOptions, start};

fn golden_file() -> PathBuf {
    common::repo_root()
        .join("crates")
        .join("proxi")
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("sessions.json")
}

fn case<'a>(golden: &'a Value, name: &str) -> &'a Value {
    golden["cases"]
        .as_array()
        .expect("cases array")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("no {name} case in the golden"))
}

/// The golden `dataSeed`: raw text, written verbatim, never normalised.
fn write_data_seed(data: &Path, seed: &Value) {
    let Some(files) = seed.as_object() else {
        return;
    };
    for (rel, text) in files {
        let target = data.join(rel);
        fs::create_dir_all(target.parent().expect("data seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(&target, text.as_str().expect("data seed value is a string"))
            .unwrap_or_else(|error| panic!("write data seed {rel}: {error}"));
    }
}

fn write_seed(repo: &Path, seed: &Value) {
    let Some(files) = seed.as_object() else {
        return;
    };
    for (rel, value) in files {
        let target = repo.join(rel);
        fs::create_dir_all(target.parent().expect("seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(
            &target,
            serde_json::to_vec_pretty(value).expect("serialise seed"),
        )
        .unwrap_or_else(|error| panic!("write seed {rel}: {error}"));
    }
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The fake engine is a node script: skip, with a message, when node is missing.
fn require_fake() -> bool {
    if node_available() {
        return true;
    }
    eprintln!("skipping: node is not on PATH");
    false
}

async fn post(client: &Client, server: &RunningServer, path: &str, body: &str) -> Response {
    client
        .post(format!("{}{path}", server.url))
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap_or_else(|error| panic!("POST {path}: {error}"))
}

async fn body_json(response: Response) -> Value {
    let text = response.text().await.expect("read body");
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}

/// Polls `GET /api/sessions/:id` until the status matches; panics after 30 s.
async fn wait_for(client: &Client, server: &RunningServer, id: &str, status: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = client
            .get(format!("{}/api/sessions/{id}", server.url))
            .send()
            .await
            .expect("GET session");
        let state = body_json(response).await;
        if state["status"] == status {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {status}: {state}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Reads chunks into `buffer` until `done` holds; panics after `timeout`.
async fn read_until(
    response: &mut Response,
    buffer: &mut String,
    timeout: Duration,
    done: impl Fn(&str) -> bool,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !done(buffer) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let chunk = tokio::time::timeout(remaining, response.chunk())
            .await
            .unwrap_or_else(|_| panic!("timed out; got {buffer:?}"))
            .expect("read chunk")
            .expect("stream closed early");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
    }
}

fn data_events(raw: &str) -> Vec<Value> {
    raw.split("\n\n")
        .filter_map(|block| {
            let data: String = block
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .collect();
            if data.is_empty() {
                return None;
            }
            Some(
                serde_json::from_str(&data)
                    .unwrap_or_else(|error| panic!("bad SSE event ({error}): {data}")),
            )
        })
        .collect()
}

/// Every path form the server may report for `dir`: the resolved path and, when it exists, the
/// native realpath (Node's `rootVariants`).
fn root_variants(dir: &Path) -> Vec<String> {
    let mut out = vec![proxi::data_root::resolve_path(dir).display().to_string()];
    if let Ok(real) = fs::canonicalize(dir) {
        let real = real.display().to_string();
        if !out.contains(&real) {
            out.push(real);
        }
    }
    out
}

struct Roots {
    repo: Vec<String>,
    data: Vec<String>,
}

/// Compares a Rust output with the golden: values, then the compact serialisation, so key order
/// has to match Node's too.
fn assert_same(label: &str, rust: &Value, node: &Value, roots: &Roots, session_id: &str) {
    let got = normalise_session(rust.clone(), roots, session_id);
    assert_eq!(got, *node, "{label}");
    assert_eq!(
        serde_json::to_string(&got).expect("serialise Rust output"),
        serde_json::to_string(node).expect("serialise golden output"),
        "{label}: key order"
    );
}

/// Port of `normaliseSession` of app/test/capture-golden.ts.
fn normalise_session(value: Value, roots: &Roots, session_id: &str) -> Value {
    match value {
        Value::String(text) => Value::String(normalise_text(&text, roots, session_id)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| normalise_session(item, roots, session_id))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                out.insert(key, normalise_session(item, roots, session_id));
            }
            Value::Object(out)
        }
        other => other,
    }
}

/// In this order: the session id, every other UUID, the fake engine's pids, then the whole-string
/// root and ISO rules of `normalise`.
fn normalise_text(text: &str, roots: &Roots, session_id: &str) -> String {
    let replaced = text.replace(session_id, "<session>");
    let replaced = replace_uuids(&replaced);
    let replaced = replace_pid_counter(&replaced, "msg_");
    let replaced = replace_pid_counter(&replaced, "toolu_fake_");
    if roots.repo.iter().any(|root| root == &replaced) {
        "<repoRoot>".to_string()
    } else if roots.data.iter().any(|root| root == &replaced) {
        "<dataRoot>".to_string()
    } else if is_iso(&replaced) {
        "<iso>".to_string()
    } else {
        replaced
    }
}

/// `[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}`, scanned left to right.
fn replace_uuids(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        if uuid_at(&bytes[at..]) {
            out.push_str("<uuid>");
            at += 36;
        } else {
            let ch = text[at..].chars().next().expect("a char at a boundary");
            out.push(ch);
            at += ch.len_utf8();
        }
    }
    out
}

fn uuid_at(bytes: &[u8]) -> bool {
    if bytes.len() < 36 {
        return false;
    }
    bytes[..36].iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_digit() || (b'a'..=b'f').contains(b),
    })
}

/// `/msg_\d+_/g` (and its `toolu_fake_` twin): the pid goes, the rest of the id stays.
fn replace_pid_counter(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(prefix) {
        out.push_str(&rest[..start]);
        let after = &rest[start + prefix.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after.as_bytes().get(digits) == Some(&b'_') {
            out.push_str(prefix);
            out.push_str("<pid>_");
            rest = &after[digits + 1..];
        } else {
            out.push_str(prefix);
            rest = after;
        }
    }
    out.push_str(rest);
    out
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
fn uuid_scanner_matches_the_js_regex() {
    assert_eq!(
        replace_uuids("l:3f4c7a91-2b0e-4c51-9a8d-1f2e3d4c5b6a"),
        "l:<uuid>"
    );
    assert_eq!(
        replace_uuids("3F4C7A91-2B0E-4C51-9A8D-1F2E3D4C5B6A"),
        "3F4C7A91-2B0E-4C51-9A8D-1F2E3D4C5B6A"
    );
    assert_eq!(
        replace_uuids("3f4c7a91-2b0e-4c51-9a8d-1f2e3d4c5b6g"),
        "3f4c7a91-2b0e-4c51-9a8d-1f2e3d4c5b6g"
    );
    assert_eq!(
        replace_uuids("t:msg_1_1 3f4c7a91-2b0e-4c51-9a8d-1f2e3d4c5b6a!"),
        "t:msg_1_1 <uuid>!"
    );
}

#[test]
fn pid_normaliser_matches_the_js_regex() {
    assert_eq!(replace_pid_counter("msg_123_1", "msg_"), "msg_<pid>_1");
    assert_eq!(replace_pid_counter("msg_x_1", "msg_"), "msg_x_1");
    assert_eq!(
        replace_pid_counter("toolu_fake_42_2", "toolu_fake_"),
        "toolu_fake_<pid>_2"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_match_the_node_golden() {
    if !require_fake() {
        return;
    }
    let raw = fs::read_to_string(golden_file())
        .unwrap_or_else(|error| panic!("read {}: {error}", golden_file().display()));
    let golden: Value = serde_json::from_str(&raw).expect("parse sessions golden");

    let dir = common::temp_dir("sessions-parity");
    let repo = dir.0.join("repo");
    let data = dir.0.join("data");
    fs::create_dir_all(&repo).expect("create repo");
    fs::create_dir_all(&data).expect("create data");
    write_seed(&repo, &golden["seed"]);
    write_data_seed(&data, &golden["dataSeed"]);

    let roots = Roots {
        repo: root_variants(&repo),
        data: root_variants(&data),
    };

    let mut options = ServerOptions::new(repo.clone());
    options.data_root = Some(data.clone());
    options.memory_secrets = true;
    // Long enough that only the turn's events can appear in the stream, like the Node capture.
    options.keep_alive = Duration::from_secs(120);
    options.bins.claude = Some(PathBuf::from("node"));
    options.bins.claude_args = vec![
        common::repo_root()
            .join("app")
            .join("test")
            .join("fake-claude.mjs")
            .display()
            .to_string(),
    ];
    let server = start(options).await.expect("start Rust server");
    let client = Client::new();

    let create = case(&golden, "create");
    let response = post(
        &client,
        &server,
        create["request"]["path"].as_str().expect("create path"),
        &create["request"]["body"].to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "POST /api/sessions");
    let created = body_json(response).await;
    let session_id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {created}"))
        .to_string();
    assert_same(
        "create",
        &created,
        &create["response"]["body"],
        &roots,
        &session_id,
    );

    wait_for(&client, &server, &session_id, "idle").await;

    let mut stream = client
        .get(format!("{}/api/sessions/{session_id}/events", server.url))
        .send()
        .await
        .expect("GET events");
    assert_eq!(stream.status(), StatusCode::OK);
    let mut raw = String::new();
    read_until(&mut stream, &mut raw, Duration::from_secs(10), |buffer| {
        buffer.contains("\n\n")
    })
    .await;
    let snapshot_events = data_events(&raw).len();
    assert_eq!(
        snapshot_events, 1,
        "one snapshot before the message: {raw:?}"
    );

    let message = case(&golden, "message");
    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{session_id}/message"),
        &message["request"]["body"].to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "POST message");
    let message_body = body_json(response).await;
    assert_same(
        "message response",
        &message_body,
        &message["response"]["body"],
        &roots,
        &session_id,
    );

    // Poll the stream until the turn's idle status has arrived after the snapshot.
    read_until(&mut stream, &mut raw, Duration::from_secs(30), |buffer| {
        data_events(buffer)
            .into_iter()
            .skip(snapshot_events)
            .any(|event| event["type"] == "status" && event["status"] == "idle")
    })
    .await;
    drop(stream);

    let events = data_events(&raw);
    let expected = message["events"].as_array().expect("golden events");
    assert_eq!(
        events.len(),
        expected.len(),
        "event count: {}",
        serde_json::to_string(&events).expect("serialise events")
    );
    for (index, (event, expected)) in events.into_iter().zip(expected).enumerate() {
        assert_same(
            &format!("message event {index}"),
            &event,
            expected,
            &roots,
            &session_id,
        );
    }

    let got = case(&golden, "get");
    let response = client
        .get(format!(
            "{}{}",
            server.url,
            got["request"]["path"]
                .as_str()
                .expect("get path")
                .replace("<session>", &session_id)
        ))
        .send()
        .await
        .expect("GET session");
    assert_eq!(response.status(), StatusCode::OK, "GET session");
    assert_same(
        "get",
        &body_json(response).await,
        &got["response"]["body"],
        &roots,
        &session_id,
    );

    let list = case(&golden, "list");
    let response = client
        .get(format!(
            "{}{}",
            server.url,
            list["request"]["path"].as_str().expect("list path")
        ))
        .send()
        .await
        .expect("GET sessions");
    assert_eq!(response.status(), StatusCode::OK, "GET sessions");
    assert_same(
        "list",
        &body_json(response).await,
        &list["response"]["body"],
        &roots,
        &session_id,
    );

    server.shutdown().await;

    let file = repo
        .join(".learner")
        .join("sessions")
        .join(&session_id)
        .join("state.json");
    let text = fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    let state_file: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("parse {} ({error}): {text}", file.display()));
    assert_same(
        "stateFile",
        &state_file,
        &golden["stateFile"],
        &roots,
        &session_id,
    );

    // The state file's top-level keys are the contract of AC4: no `pendingStart`/`lastTurn` after
    // a successful turn, `canRetry` last.
    let keys: Vec<&str> = state_file
        .as_object()
        .expect("state file object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["state", "provider", "resume", "canRetry"], "{text}");
}
