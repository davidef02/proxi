//! The session routes over real HTTP: guards, create/get/list, per-session SSE, message, attachments,
//! retry, interrupt, close, delete and shutdown.
//!
//! Ported from `app/test/api-sessions.test.ts` (T14 of issue #15) and `app/test/sessions.test.ts` /
//! `app/test/attachments.test.ts` (#20): the server wiring around the manager, with the fake engine
//! behind `--claude-bin` / `--claude-bin-arg`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

use common::sessions::{
    body_json, data_events, delete, describe, fake_script_args, post, raw_inputs, read_until,
    require_fake_engine, seed_demo_course, teach_planned, upload, user_texts, wait_for,
    wait_for_state_file, UploadPart, INTERRUPT_REQUEST,
};
use proxi::server::{start, RunningServer, ServerOptions};

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
        seed_demo_course(&fixture.data.0);
        fixture
    }

    /// The fake engine, wired the way `proxi serve --claude-bin node --claude-bin-arg <fake>` is.
    fn options(&self) -> ServerOptions {
        self.options_with(&[])
    }

    /// The same with test-hook flags before the fake script (`--input-out`, `--env-out`).
    fn options_with(&self, extra: &[&str]) -> ServerOptions {
        let mut options = ServerOptions::new(self.repo.0.clone());
        options.data_root = Some(self.data.0.clone());
        options.memory_secrets = true;
        options.bins.claude = Some(PathBuf::from("node"));
        options.bins.claude_args = fake_script_args("fake-claude.mjs", extra);
        options
    }
}

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{}", server.url, path)
}

/// The session state over HTTP.
async fn state_of(client: &Client, server: &RunningServer, id: &str) -> Value {
    let response = client
        .get(url(server, &format!("/api/sessions/{id}")))
        .send()
        .await
        .unwrap_or_else(|error| panic!("GET /api/sessions/{id}: {error}"));
    body_json(response).await
}

/// The fake's first delta: an in-band interrupt sent before the `FAKE:slow` turn starts is dropped
/// (the user line and the request arrive in one stdin read), so wait for the streamed text first.
async fn wait_for_first_delta(client: &Client, server: &RunningServer, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = state_of(client, server, id).await;
        let streaming = state["items"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["kind"] == "teacher"
                    && item["streaming"] == true
                    && item["text"].as_str().is_some_and(|text| !text.is_empty())
            })
        });
        if streaming {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the fake turn never streamed: {state}"
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
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-create");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let id = create(&client, &server, &teach_planned(Some(false))).await;
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
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-events");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
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
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-keep-alive");
    let mut options = fixture.options();
    options.keep_alive = Duration::from_millis(100);
    let server = start(options).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
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
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-busy");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
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
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-crash");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
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
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-list");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let teach = create(&client, &server, &teach_planned(Some(false))).await;
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
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-shutdown");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
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
    require_fake_engine();
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

#[tokio::test(flavor = "multi_thread")]
async fn session_control_routes_need_the_client_header_and_an_existing_id() {
    let fixture = Fixture::new("sessions-api-control-guards");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    // The CSRF guard covers the new verbs too.
    for path in [
        "/api/sessions/nope/retry",
        "/api/sessions/nope/interrupt",
        "/api/sessions/nope/close",
        "/api/sessions/nope/attachments",
    ] {
        let response = client
            .post(url(&server, path))
            .send()
            .await
            .unwrap_or_else(|error| panic!("POST {path}: {error}"));
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let response = client
        .delete(url(&server, "/api/sessions/nope"))
        .send()
        .await
        .expect("DELETE without the header");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // With the header, an unknown id is Node's 404 on every one of them.
    for path in [
        "/api/sessions/nope/retry",
        "/api/sessions/nope/interrupt",
        "/api/sessions/nope/close",
    ] {
        let response = post(&client, &server, path, "{}").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({"error": "Session not found"}),
            "{path}"
        );
    }
    let response = delete(&client, &server, "/api/sessions/nope").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Session not found"})
    );
    let response = upload(
        &client,
        &server,
        "nope",
        &[UploadPart {
            field: "files",
            file_name: "diagram.png",
            mime: "image/png",
            bytes: &[0x89, 0x50],
        }],
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
async fn attachments_upload_in_node_order_and_reach_the_engine() {
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-attachments");
    let input = fixture.repo.0.join("input.jsonl");
    let input = input.to_str().expect("utf-8 repo path");
    let server = start(fixture.options_with(&["--input-out", input]))
        .await
        .expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
    wait_for(&client, &server, &id, "idle").await;

    // The body writes `file` first: the answer still lists every `files` part before it.
    let response = upload(
        &client,
        &server,
        &id,
        &[
            UploadPart {
                field: "file",
                file_name: "b.txt",
                mime: "text/plain",
                bytes: b"b",
            },
            UploadPart {
                field: "files",
                file_name: "a.png",
                mime: "image/png",
                bytes: &[0x89, 0x50],
            },
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let raw = response.text().await.expect("read the metas");
    assert!(raw.starts_with(r#"[{"id":""#), "Node's key order: {raw}");
    let metas: Value = serde_json::from_str(&raw).expect("the metas are JSON");
    let rows = metas.as_array().expect("an array");
    let names: Vec<&str> = rows.iter().filter_map(|row| row["name"].as_str()).collect();
    assert_eq!(names, ["a.png", "b.txt"], "{raw}");
    for row in rows {
        let keys: Vec<&String> = row.as_object().expect("an object").keys().collect();
        assert_eq!(keys, ["id", "name", "path", "mimeType", "size"], "{raw}");
        let stored = fixture.repo.0.join(row["path"].as_str().expect("a path"));
        assert!(stored.is_file(), "{} is on disk", stored.display());
    }
    assert_eq!(rows[0]["mimeType"], "image/png");
    assert_eq!(rows[1]["mimeType"], "text/plain");
    assert_eq!(
        fs::read(fixture.repo.0.join(rows[0]["path"].as_str().unwrap())).unwrap(),
        [0x89, 0x50]
    );

    let text = "Ecco le slide della lezione";
    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/message"),
        &json!({"text": text, "attachments": metas.clone()}).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));
    wait_for(&client, &server, &id, "idle").await;

    let prompt = format!(
        "[Allegati forniti dallo studente:\n- a.png (percorso: {})\n- b.txt (percorso: {})]\n\n{text}",
        rows[0]["path"].as_str().unwrap(),
        rows[1]["path"].as_str().unwrap()
    );
    let sent = user_texts(Path::new(input));
    assert_eq!(
        sent.last().map(String::as_str),
        Some(prompt.as_str()),
        "{sent:?}"
    );

    // An empty form is Node's 400, so is a refused type; a body Node cannot parse is its 500.
    // A browser's empty FormData is only the closing boundary (reqwest's empty Form sends no body
    // at all, which is a truncated multipart body, not an empty form).
    let attachments = url(&server, &format!("/api/sessions/{id}/attachments"));
    let response = client
        .post(&attachments)
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "multipart/form-data; boundary=x")
        .body("--x--\r\n")
        .send()
        .await
        .expect("POST an empty form");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Nessun file caricato"})
    );

    let response = upload(
        &client,
        &server,
        &id,
        &[UploadPart {
            field: "files",
            file_name: "malware.exe",
            mime: "application/x-msdownload",
            bytes: b"MZ",
        }],
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Tipo di file non supportato: .exe"})
    );

    let response = client
        .post(&attachments)
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "multipart/form-data; boundary=x")
        .body("garbage")
        .send()
        .await
        .expect("POST a malformed multipart body");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Failed to parse body as FormData."})
    );

    let response = client
        .post(&attachments)
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body("a=1")
        .send()
        .await
        .expect("POST a form-urlencoded body");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Nessun file caricato"})
    );

    // Node's WHATWG parser gives a part with no `Content-Type` `text/plain`, and an empty one `""`
    // (which the manager maps to `application/octet-stream`).
    let response = client
        .post(&attachments)
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "multipart/form-data; boundary=x")
        .body(concat!(
            "--x\r\n",
            "Content-Disposition: form-data; name=\"files\"; filename=\"a.png\"\r\n",
            "\r\nbytes\r\n--x--\r\n"
        ))
        .send()
        .await
        .expect("POST a part with no content type");
    assert_eq!(response.status(), StatusCode::OK);
    let metas = body_json(response).await;
    assert_eq!(metas[0]["mimeType"], "text/plain", "{metas}");

    let response = client
        .post(&attachments)
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "multipart/form-data; boundary=x")
        .body(concat!(
            "--x\r\n",
            "Content-Disposition: form-data; name=\"files\"; filename=\"b.png\"\r\n",
            "Content-Type: \r\n",
            "\r\nbytes\r\n--x--\r\n"
        ))
        .send()
        .await
        .expect("POST a part with an empty content type");
    assert_eq!(response.status(), StatusCode::OK);
    let metas = body_json(response).await;
    assert_eq!(metas[0]["mimeType"], "application/octet-stream", "{metas}");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_route_sends_a_snapshot_then_the_rerun_turn() {
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-retry");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
    wait_for(&client, &server, &id, "idle").await;

    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/message"),
        r#"{"text":"FAKE:error_once spiegami il principio di induzione"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let failed = wait_for(&client, &server, &id, "error").await;
    assert_eq!(failed["retryable"], true, "{failed}");

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
    let initial = data_events(&raw);
    assert_eq!(initial.len(), 1, "{raw}");
    assert_eq!(initial[0]["type"], "snapshot");

    let response = post(&client, &server, &format!("/api/sessions/{id}/retry"), "{}").await;
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
    let start = described
        .iter()
        .position(|line| line == "snapshot")
        .unwrap_or_else(|| panic!("no retry snapshot: {described:?}"));
    let after = &described[start..];
    let running = after
        .iter()
        .position(|line| line == "status:running")
        .unwrap_or_else(|| panic!("no running status: {described:?}"));
    let idle = after
        .iter()
        .position(|line| line == "status:idle")
        .unwrap_or_else(|| panic!("no idle status: {described:?}"));
    assert!(running < idle, "running comes first: {described:?}");
    assert!(
        !after.iter().any(|line| line.starts_with("item:learner:")),
        "retry adds no learner bubble: {described:?}"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_route_stops_the_turn_with_one_notice() {
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-interrupt");
    let input = fixture.repo.0.join("input.jsonl");
    let input = input.to_str().expect("utf-8 repo path");
    let server = start(fixture.options_with(&["--input-out", input]))
        .await
        .expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
    wait_for(&client, &server, &id, "idle").await;

    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/message"),
        r#"{"text":"FAKE:slow"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    wait_for(&client, &server, &id, "running").await;
    wait_for_first_delta(&client, &server, &id).await;

    let started = Instant::now();
    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/interrupt"),
        "{}",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));
    let state = wait_for(&client, &server, &id, "idle").await;
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the interrupt idles the turn within 8 s, took {:?}: {state}",
        started.elapsed()
    );

    let items = state["items"].as_array().expect("items");
    let notices: Vec<&Value> = items
        .iter()
        .filter(|item| item["kind"] == "notice")
        .collect();
    assert_eq!(
        notices
            .iter()
            .filter(|item| item["level"] == "info" && item["text"] == "Turno interrotto.")
            .count(),
        1,
        "{state}"
    );
    assert!(
        notices.iter().all(|item| item["level"] != "error"),
        "an interrupt is not an error: {state}"
    );
    assert!(
        raw_inputs(Path::new(input))
            .iter()
            .any(|line| line == INTERRUPT_REQUEST),
        "the engine got the in-band request"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_route_closes_and_refuses_new_turns() {
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-close");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
    wait_for(&client, &server, &id, "idle").await;

    let response = post(&client, &server, &format!("/api/sessions/{id}/close"), "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));
    assert_eq!(state_of(&client, &server, &id).await["status"], "closed");

    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/message"),
        r#"{"text":"x"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Session is closed"})
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_route_removes_the_session_data() {
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-delete");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &teach_planned(Some(false))).await;
    wait_for(&client, &server, &id, "idle").await;

    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/message"),
        r#"{"text":"FAKE:slow"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    wait_for(&client, &server, &id, "running").await;
    let response = upload(
        &client,
        &server,
        &id,
        &[UploadPart {
            field: "files",
            file_name: "diagram.png",
            mime: "image/png",
            bytes: &[0x89, 0x50],
        }],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = delete(&client, &server, &format!("/api/sessions/{id}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));
    let response = client
        .get(url(&server, &format!("/api/sessions/{id}")))
        .send()
        .await
        .expect("GET a deleted session");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let listed = body_json(
        client
            .get(url(&server, "/api/sessions"))
            .send()
            .await
            .expect("GET sessions"),
    )
    .await;
    assert!(
        listed
            .as_array()
            .expect("a list")
            .iter()
            .all(|row| row["id"] != id.as_str()),
        "{listed}"
    );
    let dir = fixture.repo.0.join(".learner").join("sessions").join(&id);
    assert!(
        !dir.exists(),
        "the session dir went away, attachments included"
    );

    // A save timer scheduled before the delete must not resurrect the dir.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!dir.exists(), "a late flush recreated the deleted dir");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_lists_the_session_and_makes_the_cut_turn_retryable() {
    require_fake_engine();
    let fixture = Fixture::new("sessions-api-restart");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(
        &client,
        &server,
        &json!({"kind": "practice", "course": "demo", "model": "haiku"}),
    )
    .await;
    wait_for(&client, &server, &id, "idle").await;

    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/message"),
        r#"{"text":"FAKE:slow:15"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    wait_for(&client, &server, &id, "running").await;
    // The cut needs the turn on disk before the process dies: status running and the last turn saved.
    wait_for_state_file(&fixture.repo.0, &id, |value| {
        value["state"]["status"] == "running"
            && value.get("lastTurn").is_some_and(|turn| !turn.is_null())
    })
    .await;

    server.shutdown().await;
    let server = start(fixture.options()).await.expect("restart server");

    let listed = body_json(
        client
            .get(url(&server, "/api/sessions"))
            .send()
            .await
            .expect("GET sessions"),
    )
    .await;
    assert!(
        listed
            .as_array()
            .expect("a list")
            .iter()
            .any(|row| row["id"] == id.as_str()),
        "the restarted server lists the saved session: {listed}"
    );
    let cut = wait_for(&client, &server, &id, "error").await;
    assert_eq!(cut["retryable"], true, "{cut}");
    assert_eq!(
        cut["lastError"],
        "Sessione interrotta prima del completamento del turno"
    );
    let learners = cut["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "learner")
        .count();

    let response = post(&client, &server, &format!("/api/sessions/{id}/retry"), "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let state = wait_for(&client, &server, &id, "idle").await;
    let after = state["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "learner")
        .count();
    assert_eq!(after, learners, "retry adds no learner bubble: {state}");

    server.shutdown().await;
}
