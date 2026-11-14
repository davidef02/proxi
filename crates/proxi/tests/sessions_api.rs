//! The session routes over real HTTP: guards, create/get/list, per-session SSE, message, shutdown.
//!
//! Ported from `app/test/api-sessions.test.ts` (T14 of issue #15): the server wiring around the
//! manager, with the fake engine behind `--claude-bin` / `--claude-bin-arg`.

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};

use proxi::server::{start, RunningServer, ServerOptions};

const CURRICULUM: &str = r#"{"chapters":[{"title":"Demo","slug":"demo","lessons":[{"title":"Prima lezione","slug":"prima-lezione","plannedNodes":2}]}]}"#;

/// A temp repo (`.learner`) and a separate temp data folder holding the `demo` course.
struct Fixture {
    repo: common::TempDir,
    data: common::TempDir,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let fixture = Self {
            repo: common::temp_dir(tag),
            data: common::temp_dir(&format!("{tag}-data")),
        };
        let data = &fixture.data.0;
        fs::write(data.join("curriculum.json"), CURRICULUM).expect("write curriculum");
        let course = data.join("courses").join("demo");
        fs::create_dir_all(course.join("lessons")).expect("create lessons dir");
        fs::write(course.join("MISSION.md"), "# Mission\n").expect("write mission");
        fs::write(course.join("NOTES.md"), "# Notes\n").expect("write notes");
        fixture
    }

    /// The fake engine, wired the way `proxi serve --claude-bin node --claude-bin-arg <fake>` is.
    fn options(&self) -> ServerOptions {
        let mut options = ServerOptions::new(self.repo.0.clone());
        options.data_root = Some(self.data.0.clone());
        options.memory_secrets = true;
        options.bins.claude = Some(PathBuf::from("node"));
        options.bins.claude_args = vec![
            common::repo_root()
                .join("app")
                .join("test")
                .join("fake-claude.mjs")
                .display()
                .to_string(),
        ];
        options
    }
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Tests that create sessions skip, with a message, when node is missing.
fn require_fake() -> bool {
    if node_available() {
        return true;
    }
    eprintln!("skipping: node is not on PATH");
    false
}

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{}", server.url, path)
}

async fn post(client: &Client, server: &RunningServer, path: &str, body: &str) -> Response {
    client
        .post(url(server, path))
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .expect("POST request")
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
            .get(url(server, &format!("/api/sessions/{id}")))
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

async fn create(client: &Client, server: &RunningServer, body: &Value) -> String {
    let response = post(client, server, "/api/sessions", &body.to_string()).await;
    assert_eq!(response.status(), StatusCode::OK, "POST /api/sessions");
    let created = body_json(response).await;
    created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {created}"))
        .to_string()
}

fn teach_planned(anki_opt_in: bool) -> Value {
    json!({
        "kind": "teach",
        "course": "demo",
        "model": "haiku",
        "ankiOptIn": anki_opt_in,
        "target": {"mode": "planned", "title": "Prima lezione"},
    })
}

/// Reads chunks until `done` sees the accumulated text; panics after `timeout`.
async fn read_until(
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

fn data_events(raw: &str) -> Vec<Value> {
    raw.split("\n\n")
        .filter_map(|event| {
            event
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .and_then(|json| serde_json::from_str(json).ok())
        })
        .collect()
}

/// One line per live event, so the whole AC1 sequence is one assertion.
fn describe(event: &Value) -> String {
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

#[tokio::test(flavor = "multi_thread")]
async fn guards_and_validation_match_the_node_routes() {
    let fixture = Fixture::new("sessions-api-guards");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    // CSRF: a POST without the custom header.
    let response = client
        .post(url(&server, "/api/sessions"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Missing x-learner-client header"})
    );

    // Invalid JSON is Node's SyntaxError.
    let response = post(&client, &server, "/api/sessions", "{").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Invalid JSON body"})
    );

    // Schema errors, before any filesystem work; the wording is close to zod, status and a string.
    for body in [
        json!({"kind": "teach", "course": "Bad Name", "target": {"mode": "free"}}),
        json!({"kind": "practice", "course": "nope", "model": "haiku"}),
        json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false, "target": {"mode": "planned", "title": "Prima Lezione"}}),
    ] {
        let response = post(&client, &server, "/api/sessions", &body.to_string()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        let error = body_json(response).await;
        assert!(
            error["error"].as_str().is_some_and(|text| !text.is_empty()),
            "{body}: {error}"
        );
    }

    // Unknown ids are 404 on every route that looks one up...
    for path in ["/api/sessions/nope", "/api/sessions/nope/events"] {
        let response = client.get(url(&server, path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({"error": "Session not found"}),
            "{path}"
        );
    }

    // ...but message bodies are validated first (Node's `textBody` runs before `manager.message`).
    for body in [
        json!({}),
        json!({"text": ""}),
        json!({"text": "x".repeat(20001)}),
        json!({"text": "x", "attachments": [{"name": "a"}]}),
    ] {
        let response = post(
            &client,
            &server,
            "/api/sessions/nope/message",
            &body.to_string(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        let error = body_json(response).await;
        assert!(
            error["error"].as_str().is_some_and(|text| !text.is_empty()),
            "{body}: {error}"
        );
    }
    let response = post(&client, &server, "/api/sessions/nope/message", "not json").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Invalid JSON body"})
    );

    let response = post(
        &client,
        &server,
        "/api/sessions/nope/message",
        r#"{"text":"ciao"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Session not found"})
    );

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn create_returns_the_id_and_get_reaches_idle() {
    if !require_fake() {
        return;
    }
    let fixture = Fixture::new("sessions-api-create");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let id = create(&client, &server, &teach_planned(false)).await;
    let state = wait_for(&client, &server, &id, "idle").await;

    assert_eq!(state["title"], "Prima lezione");
    assert_eq!(state["items"].as_array().unwrap().len(), 2);
    assert_eq!(state["items"][0]["kind"], "learner");
    assert_eq!(
        state["items"][0]["text"],
        "Iniziamo la lezione \"Prima lezione\"."
    );
    assert_eq!(state["items"][1]["kind"], "teacher");
    assert_eq!(state["items"][1]["text"], "Ciao, sono il docente.");
    assert_eq!(state["items"][1]["streaming"], false);
    assert_eq!(state["usage"]["turns"], 1);
    assert_eq!(state["retryable"], false);
    assert!(state["pendingRequestId"].is_null());

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn events_send_a_snapshot_then_the_live_turn() {
    if !require_fake() {
        return;
    }
    let fixture = Fixture::new("sessions-api-events");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(false)).await;
    wait_for(&client, &server, &id, "idle").await;

    let mut stream = client
        .get(url(&server, &format!("/api/sessions/{id}/events")))
        .send()
        .await
        .expect("GET events");
    assert_eq!(stream.status(), StatusCode::OK);
    let raw = read_until(&mut stream, Duration::from_secs(10), |buffer| {
        buffer.contains("\n\n")
    })
    .await;
    let mut first = data_events(&raw);
    assert_eq!(first.len(), 1, "{raw}");
    let snapshot = first.remove(0);
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["state"]["id"], id);
    assert_eq!(snapshot["state"]["items"].as_array().unwrap().len(), 2);

    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/message"),
        r#"{"text":"Ciao"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));

    let raw = read_until(&mut stream, Duration::from_secs(30), |buffer| {
        data_events(buffer)
            .iter()
            .any(|event| event["type"] == "status" && event["status"] == "idle")
    })
    .await;
    let live = data_events(&raw);
    let described: Vec<String> = live.iter().map(describe).collect();
    assert_eq!(
        described,
        [
            "item:learner:Ciao",
            "status:running",
            "item:teacher:",
            "delta:Ciao",
            "delta:, sono il",
            "delta: docente.",
            "item:teacher:Ciao, sono il docente.",
            "usage:2",
            "status:idle",
        ]
    );
    assert!(
        live.iter().all(|event| event["type"] != "snapshot"),
        "one snapshot per connection"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn events_keep_the_connection_alive() {
    if !require_fake() {
        return;
    }
    let fixture = Fixture::new("sessions-api-keep-alive");
    let mut options = fixture.options();
    options.keep_alive = Duration::from_millis(100);
    let server = start(options).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(false)).await;
    wait_for(&client, &server, &id, "idle").await;

    let mut stream = client
        .get(url(&server, &format!("/api/sessions/{id}/events")))
        .send()
        .await
        .expect("GET events");
    let raw = read_until(&mut stream, Duration::from_secs(10), |buffer| {
        buffer.contains(": keep-alive")
    })
    .await;
    assert!(raw.contains(": keep-alive"), "{raw:?}");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_session_refuses_a_second_message() {
    if !require_fake() {
        return;
    }
    let fixture = Fixture::new("sessions-api-busy");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(false)).await;
    wait_for(&client, &server, &id, "idle").await;

    let path = format!("/api/sessions/{id}/message");
    let response = post(&client, &server, &path, r#"{"text":"FAKE:slow:10"}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));

    let response = post(&client, &server, &path, r#"{"text":"ancora"}"#).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = body_json(response).await;
    assert!(
        error["error"].as_str().unwrap_or_default().contains("busy"),
        "{error}"
    );

    wait_for(&client, &server, &id, "idle").await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crashed_engine_is_retryable_and_the_next_message_resumes() {
    if !require_fake() {
        return;
    }
    let fixture = Fixture::new("sessions-api-crash");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(false)).await;
    wait_for(&client, &server, &id, "idle").await;

    let path = format!("/api/sessions/{id}/message");
    let response = post(&client, &server, &path, r#"{"text":"FAKE:crash"}"#).await;
    assert_eq!(response.status(), StatusCode::OK);

    let state = wait_for(&client, &server, &id, "error").await;
    assert_eq!(state["retryable"], true);
    assert!(
        state["lastError"]
            .as_str()
            .unwrap_or_default()
            .starts_with("teacher process exited (1)"),
        "{}",
        state["lastError"]
    );

    let response = post(&client, &server, &path, r#"{"text":"ancora"}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = wait_for(&client, &server, &id, "idle").await;
    assert_eq!(state["usage"]["turns"], 2, "the resumed turn counted");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn list_is_newest_first_and_leaves_the_heavy_fields_out() {
    if !require_fake() {
        return;
    }
    let fixture = Fixture::new("sessions-api-list");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let teach = create(&client, &server, &teach_planned(false)).await;
    wait_for(&client, &server, &teach, "idle").await;
    let practice = create(
        &client,
        &server,
        &json!({"kind": "practice", "course": "demo", "model": "haiku"}),
    )
    .await;
    wait_for(&client, &server, &practice, "idle").await;

    let response = client
        .get(url(&server, "/api/sessions"))
        .send()
        .await
        .expect("GET sessions");
    assert_eq!(response.status(), StatusCode::OK);
    let listed = body_json(response).await;
    let rows = listed.as_array().expect("a list");
    assert_eq!(rows.len(), 2, "{listed}");
    assert_eq!(rows[0]["id"], practice, "newest first");
    assert_eq!(rows[1]["id"], teach);
    for row in rows {
        for key in ["items", "plan", "exercises", "pendingRequestId"] {
            assert!(row.get(key).is_none(), "{key} in {row}");
        }
        assert!(row.get("usage").is_some(), "{row}");
        assert!(row.get("createdAt").is_some(), "{row}");
    }

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_a_running_turn_and_writes_the_state_file() {
    if !require_fake() {
        return;
    }
    let fixture = Fixture::new("sessions-api-shutdown");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(false)).await;
    wait_for(&client, &server, &id, "idle").await;

    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/message"),
        r#"{"text":"FAKE:slow:50"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    wait_for(&client, &server, &id, "running").await;

    let started = Instant::now();
    server.shutdown().await;
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "shutdown waited {:?} on a killed turn",
        started.elapsed()
    );

    let file = fixture
        .repo
        .0
        .join(".learner")
        .join("sessions")
        .join(&id)
        .join("state.json");
    let raw = fs::read_to_string(&file).expect("state.json after shutdown");
    let value: Value = serde_json::from_str(&raw).expect("state.json is JSON");
    assert_eq!(value["state"]["id"], id);
    assert!(value["state"]["items"].is_array(), "{value}");
}

#[tokio::test(flavor = "multi_thread")]
async fn global_events_announce_a_created_session() {
    if !require_fake() {
        return;
    }
    let fixture = Fixture::new("sessions-api-bus");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    // Subscribe first: the debounced hint fires 300 ms after the create.
    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .expect("GET events");
    assert_eq!(stream.status(), StatusCode::OK);

    let id = create(
        &client,
        &server,
        &json!({"kind": "practice", "course": "demo", "model": "haiku"}),
    )
    .await;
    let raw = read_until(&mut stream, Duration::from_secs(10), |buffer| {
        buffer.contains(r#"{"type":"sessions"}"#)
    })
    .await;
    assert!(raw.contains(r#"data: {"type":"sessions"}"#), "{raw:?}");

    wait_for(&client, &server, &id, "idle").await;
    server.shutdown().await;
}
