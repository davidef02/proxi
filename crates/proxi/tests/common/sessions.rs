//! Helpers shared by the session test binaries: the fake engine, the HTTP client over its routes,
//! the SSE frames. One copy, so `sessions_api`, `sessions_parity` and `session_manager` check the
//! same shapes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response};
use serde_json::{Value, json};

use proxi::server::{RunningServer, ServerOptions};

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

/// Writes `curriculum.json` and the `demo` course under `data`.
pub fn seed_demo_course(data: &Path) {
    fs::write(data.join("curriculum.json"), CURRICULUM).expect("write curriculum");
    let course = data.join("courses").join("demo");
    fs::create_dir_all(course.join("lessons")).expect("create lessons dir");
    fs::write(course.join("MISSION.md"), "# Mission\n").expect("write mission");
    fs::write(course.join("NOTES.md"), "# Notes\n").expect("write notes");
}

/// A temp repo (`.learner`) and a separate temp data folder holding the `demo` course.
pub struct Fixture {
    pub repo: super::TempDir,
    pub data: super::TempDir,
}

impl Fixture {
    pub fn new(tag: &str) -> Self {
        let fixture = Self {
            repo: super::temp_dir(tag),
            data: super::temp_dir(&format!("{tag}-data")),
        };
        seed_demo_course(&fixture.data.0);
        fixture
    }

    /// The fake engine, wired the way `proxi serve --claude-bin node --claude-bin-arg <fake>` is.
    pub fn options(&self) -> ServerOptions {
        let mut options = ServerOptions::new(self.repo.0.clone());
        options.data_root = Some(self.data.0.clone());
        options.memory_secrets = true;
        options.bins.claude = Some(PathBuf::from("node"));
        options.bins.claude_args = vec![
            super::repo_root()
                .join("app")
                .join("test")
                .join("fake-claude.mjs")
                .display()
                .to_string(),
        ];
        options
    }
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
