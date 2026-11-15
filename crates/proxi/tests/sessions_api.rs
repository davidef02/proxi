//! The session routes over real HTTP: guards, create/get/list, per-session SSE, message, shutdown.
//!
//! Ported from `app/test/api-sessions.test.ts` (T14 of issue #15): the server wiring around the
//! manager, with the fake engine behind `--claude-bin` / `--claude-bin-arg`.

mod common;

use std::fs;
use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

use common::sessions::{
    Fixture, body_json, data_events, describe, post, read_until, require_fake_engine,
    teach_planned, wait_for,
};
use proxi::server::{start, RunningServer};

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{}", server.url, path)
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
