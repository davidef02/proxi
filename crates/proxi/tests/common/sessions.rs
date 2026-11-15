//! Helpers shared by the session test binaries: the fake engine and what it received, the HTTP
//! client over its routes, uploads, the SSE frames, the saved state. One copy, so `sessions_api`,
//! `sessions_parity` and `session_manager` check the same shapes.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::multipart::{Form, Part};
use reqwest::{Client, Response};
use serde_json::{Value, json};

use proxi::server::RunningServer;

/// The data seed of the session tests: one planned chapter, one course, no lesson taught yet.
pub const CURRICULUM: &str = r#"{"chapters":[{"title":"Demo","slug":"demo","lessons":[{"title":"Prima lezione","slug":"prima-lezione","plannedNodes":2}]}]}"#;

/// The fake engine is a node script (`app/test/fake-claude.mjs`): a session test without node
/// would check nothing, so it fails like `common::require_node` instead of skipping.
pub fn require_fake_engine() {
    let available = Command::new("node")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    assert!(
        available,
        "session tests need node on PATH (the fake engine is a node script)"
    );
}

/// The fake engine command line: the node script `script` under `app/test`, then `extra` (test
/// hooks such as `--input-out`).
pub fn fake_script_args(script: &str, extra: &[&str]) -> Vec<String> {
    let fake = super::repo_root().join("app").join("test").join(script);
    let mut args = vec![fake.display().to_string()];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    args
}

/// The raw stdin lines the fake engine kept in its `--input-out` file; empty before they exist.
pub fn raw_inputs(file: &Path) -> Vec<String> {
    match fs::read_to_string(file) {
        Ok(raw) => raw.lines().map(str::to_string).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("read {}: {error}", file.display()),
    }
}

/// Every stdin line of the fake engine (`--input-out`), parsed.
pub fn fake_inputs(file: &Path) -> Vec<Value> {
    raw_inputs(file)
        .into_iter()
        .map(|line| {
            serde_json::from_str(&line)
                .unwrap_or_else(|error| panic!("bad input line ({error}): {line}"))
        })
        .collect()
}

/// The user prompts the fake engine received, in order: what the server actually sent.
pub fn user_texts(file: &Path) -> Vec<String> {
    fake_inputs(file)
        .into_iter()
        .filter(|line| line["type"] == "user")
        .map(|line| {
            line["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The exact in-band interrupt Claude receives; the counter starts at 1 per process.
pub const INTERRUPT_REQUEST: &str =
    r#"{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}"#;

/// Writes `curriculum.json` and the `demo` course under `data`.
pub fn seed_demo_course(data: &Path) {
    fs::write(data.join("curriculum.json"), CURRICULUM).expect("write curriculum");
    let course = data.join("courses").join("demo");
    fs::create_dir_all(course.join("lessons")).expect("create lessons dir");
    fs::write(course.join("MISSION.md"), "# Mission\n").expect("write mission");
    fs::write(course.join("NOTES.md"), "# Notes\n").expect("write notes");
}

/// A POST with the CSRF header the routes require.
pub async fn post(client: &Client, server: &RunningServer, path: &str, body: &str) -> Response {
    client
        .post(format!("{}{path}", server.url))
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap_or_else(|error| panic!("POST {path}: {error}"))
}

/// One file of a multipart upload: a `files` (or `file`) part carrying a filename.
pub struct UploadPart<'a> {
    pub field: &'a str,
    pub file_name: &'a str,
    pub mime: &'a str,
    pub bytes: &'a [u8],
}

/// A multipart POST, parts in the order given (the route reads all `files`, then all `file`).
pub async fn upload(
    client: &Client,
    server: &RunningServer,
    id: &str,
    parts: &[UploadPart<'_>],
) -> Response {
    let mut form = Form::new();
    for part in parts {
        let field = Part::bytes(part.bytes.to_vec())
            .file_name(part.file_name.to_string())
            .mime_str(part.mime)
            .expect("mime");
        form = form.part(part.field.to_string(), field);
    }
    let path = format!("/api/sessions/{id}/attachments");
    client
        .post(format!("{}{path}", server.url))
        .header("x-learner-client", "1")
        .multipart(form)
        .send()
        .await
        .unwrap_or_else(|error| panic!("POST {path}: {error}"))
}

/// A DELETE with the CSRF header the routes require.
pub async fn delete(client: &Client, server: &RunningServer, path: &str) -> Response {
    client
        .delete(format!("{}{path}", server.url))
        .header("x-learner-client", "1")
        .send()
        .await
        .unwrap_or_else(|error| panic!("DELETE {path}: {error}"))
}

pub async fn body_json(response: Response) -> Value {
    let text = response.text().await.expect("read body");
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}

/// Polls `GET /api/sessions/:id` until the status matches; panics after 30 s.
pub async fn wait_for(client: &Client, server: &RunningServer, id: &str, status: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
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
            std::time::Instant::now() < deadline,
            "timed out waiting for {status}: {state}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Polls the saved state of `id` every 25 ms until `done` accepts it; panics after 20 s with the
/// last content. A flush after the last event is asynchronous, so a test that reads `canRetry`
/// off the file waits here instead of racing it.
pub async fn wait_for_state_file(repo: &Path, id: &str, done: impl Fn(&Value) -> bool) -> Value {
    let file = repo
        .join(".learner")
        .join("sessions")
        .join(id)
        .join("state.json");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut last = String::from("<no state.json>");
    loop {
        if let Ok(raw) = fs::read_to_string(&file) {
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                if done(&value) {
                    return value;
                }
            }
            last = raw;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "state.json never matched: {last}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Whether `pid` is still a live process: `tasklist` on Windows, signal 0 elsewhere.
#[cfg(windows)]
pub fn pid_running(pid: u32) -> bool {
    let pid = pid.to_string();
    let output = Command::new("tasklist")
        .arg("/FI")
        .arg(format!("PID eq {pid}"))
        .arg("/NH")
        .output()
        .expect("run tasklist");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .any(|token| token == pid.as_str())
}

#[cfg(not(windows))]
pub fn pid_running(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Reads SSE chunks until `done` sees the accumulated text; returns it; panics after `timeout`.
pub async fn read_until(
    response: &mut Response,
    timeout: Duration,
    done: impl Fn(&str) -> bool,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buffer = String::new();
    while !done(&buffer) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let chunk = tokio::time::timeout(remaining, response.chunk())
            .await
            .unwrap_or_else(|_| panic!("timed out; got {buffer:?}"))
            .expect("read chunk")
            .expect("stream closed early");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
    }
    buffer
}

/// The `data:` payloads of a raw SSE buffer, parsed.
pub fn data_events(raw: &str) -> Vec<Value> {
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

/// One line per live event, so a whole sequence is one assertion.
pub fn describe(event: &Value) -> String {
    match event["type"].as_str().unwrap_or("") {
        "item" => format!(
            "item:{}:{}",
            event["item"]["kind"].as_str().unwrap_or(""),
            event["item"]["text"].as_str().unwrap_or("")
        ),
        "delta" => format!("delta:{}", event["text"].as_str().unwrap_or("")),
        "status" => format!("status:{}", event["status"].as_str().unwrap_or("")),
        "usage" => format!("usage:{}", event["usage"]["turns"]),
        other => other.to_string(),
    }
}

/// The planned teach body of the session tests; `None` leaves `ankiOptIn` out (the Anki question).
pub fn teach_planned(anki_opt_in: Option<bool>) -> Value {
    let mut body = json!({
        "kind": "teach",
        "course": "demo",
        "model": "haiku",
        "target": {"mode": "planned", "title": "Prima lezione"},
    });
    if let Some(opt_in) = anki_opt_in {
        body["ankiOptIn"] = json!(opt_in);
    }
    body
}
