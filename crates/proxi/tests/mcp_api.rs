//! The `/mcp/:id` and `/api/sessions/:id/answer` routes over real HTTP: the tool registry, the
//! blocking cards, the answers that resume them and the calls whose connection went away.
//!
//! Ported from the answer flows of `app/test/sessions.test.ts` (T13 of issue #18): the same effects,
//! seen from outside, with the fake engine behind `--claude-bin` / `--claude-bin-arg`.

mod common;

use std::fs;
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use common::cli_golden::normalise_file;
use common::sessions::{
    Fixture, body_json, data_events, post, read_until, require_fake_engine, teach_planned, wait_for,
};
use proxi::lesson::doc::{self, AppendOptions, InitOptions};
use proxi::server::{RunningServer, start};
use proxi::tools::util::CANCELLED;

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{path}", server.url)
}

/// A POST to the MCP route with the engine's own secret: the CSRF header has no business there.
async fn mcp(client: &Client, server: &RunningServer, id: &str, body: &Value) -> Response {
    client
        .post(url(server, &format!("/mcp/{id}")))
        .header("x-learner-token", server.sessions.token())
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap_or_else(|error| panic!("POST /mcp/{id}: {error}"))
}

/// The same POST in its own task, so a blocking card can be answered while it is in flight.
///
/// The client is per call on purpose: dropping the task drops this connection, and nothing else.
fn mcp_in_background(server: &RunningServer, id: &str, body: Value) -> JoinHandle<Response> {
    let url = url(server, &format!("/mcp/{id}"));
    let token = server.sessions.token();
    tokio::spawn(async move {
        Client::new()
            .post(url)
            .header("x-learner-token", token)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .expect("POST /mcp")
    })
}

/// A JSON-RPC `tools/call` body.
fn tools_call(name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    })
}

/// Two options: one with a value, one answered by its label.
fn ask_arguments(question: &str) -> Value {
    json!({
        "question": question,
        "options": [{"label": "Alfa", "value": "a"}, {"label": "Beta"}],
    })
}

/// The text and `isError` of a `tools/call` answer.
fn tool_result(reply: &Value) -> (String, bool) {
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content in {reply}"))
        .to_string();
    (text, reply["result"]["isError"] == true)
}

async fn session(client: &Client, server: &RunningServer, id: &str) -> Value {
    let response = client
        .get(url(server, &format!("/api/sessions/{id}")))
        .send()
        .await
        .expect("GET session");
    body_json(response).await
}

/// The chat item of that kind whose `requestId` is the pending one.
fn pending_card(state: &Value) -> Option<&Value> {
    let pending = state["pendingRequestId"].as_str()?;
    state["items"].as_array()?.iter().find(|item| {
        item["requestId"].as_str() == Some(pending)
    })
}

fn request_id(card: &Value) -> String {
    card["requestId"]
        .as_str()
        .unwrap_or_else(|| panic!("no requestId in {card}"))
        .to_string()
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

async fn answer(client: &Client, server: &RunningServer, id: &str, body: &Value) -> Response {
    post(
        client,
        server,
        &format!("/api/sessions/{id}/answer"),
        &body.to_string(),
    )
    .await
}

/// A free lesson: no plan, so a quiz has no DAG to check its `nodeId` against.
fn free_teach() -> Value {
    json!({
        "kind": "teach",
        "course": "demo",
        "model": "haiku",
        "ankiOptIn": false,
        "target": {"mode": "free"},
    })
}

/// A session continuing a lesson already on disk.
fn existing_teach(lesson_path: &str) -> Value {
    json!({
        "kind": "teach",
        "course": "demo",
        "model": "haiku",
        "ankiOptIn": false,
        "target": {"mode": "existing", "lessonPath": lesson_path},
    })
}

/// Writes `courses/demo/lessons/0001-prima-lezione.html` (N1 → N2); returns its absolute and
/// data-relative paths.
fn seed_lesson(data: &std::path::Path) -> (String, String) {
    let course = data.join("courses").join("demo");
    let init = doc::init_lesson(&InitOptions {
        topic: "Demo".to_string(),
        title: Some("Prima lezione".to_string()),
        slug: None,
        description: None,
        dag: Some(
            "graph TD\n  N1[\"Nodo 1: la prima cosa\"] --> N2[\"Nodo 2: la seconda\"]".to_string(),
        ),
        target_goal: Some("capire".to_string()),
        base_dir: Some(course.display().to_string()),
        lessons_dir: None,
        repo_root: common::repo_root(),
    })
    .expect("init the lesson");
    (
        init.file_path,
        "courses/demo/lessons/0001-prima-lezione.html".to_string(),
    )
}

/// Polls `GET /api/sessions/:id` until `done` holds; panics after `timeout`.
async fn poll_until(
    client: &Client,
    server: &RunningServer,
    id: &str,
    timeout: Duration,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let state = session(client, server, id).await;
        if done(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the session to settle: {state}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// `true` when the item carrying that request id was cancelled.
fn cancelled(state: &Value, request_id: &str) -> bool {
    state["items"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item["requestId"].as_str() == Some(request_id) && item["cancelled"] == true
        })
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_stays_open_until_answered() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-ask");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &free_teach()).await;
    wait_for(&client, &server, &id, "idle").await;

    let call = mcp_in_background(&server, &id, tools_call("ask", ask_arguments("Quale?")));
    let state = wait_for(&client, &server, &id, "waiting").await;
    assert!(!call.is_finished(), "the ask must wait for the learner");
    let card = pending_card(&state).expect("a pending card").clone();
    assert_eq!(card["id"], format!("req:{}", request_id(&card)));
    assert_eq!(card["kind"], "ask");

    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": card["requestId"],
            "kind": "ask",
            "answer": {"selected": ["a"], "other": "x"},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));

    let reply = body_json(call.await.expect("the call task")).await;
    assert_eq!(
        tool_result(&reply),
        (
            r#"{"selected":["Alfa"],"other":"x"}"#.to_string(),
            false
        )
    );

    let state = wait_for(&client, &server, &id, "running").await;
    assert!(state["pendingRequestId"].is_null(), "{state}");
    let card = state["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["kind"] == "ask")
        })
        .expect("the ask card");
    assert_eq!(card["answer"], json!({"selected": ["a"], "other": "x"}));

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn quiz_answer_is_graded_and_logged() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-quiz");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &free_teach()).await;
    wait_for(&client, &server, &id, "idle").await;

    let quiz = json!({
        "kind": "probe",
        "question": "Quanto fa 2+2?",
        "options": [{"label": "3"}, {"label": "4"}, {"label": "5"}],
        "explanation": "Due più due fa quattro.",
        "correctAnswer": "4",
    });
    let call = mcp_in_background(&server, &id, tools_call("quiz", quiz));
    let state = wait_for(&client, &server, &id, "waiting").await;
    let card = pending_card(&state).expect("a pending card").clone();
    assert_eq!(card["kind"], "quiz");

    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": card["requestId"],
            "kind": "quiz",
            "answer": {"selected": ["4"], "dontKnow": false},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let reply = body_json(call.await.expect("the call task")).await;
    assert_eq!(
        tool_result(&reply),
        (
            r#"{"correct":true,"dontKnow":false,"selected":["4"]}"#.to_string(),
            false
        )
    );

    let state = session(&client, &server, &id).await;
    let card = state["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["kind"] == "quiz"))
        .expect("the quiz card");
    assert_eq!(card["result"]["correct"], true);

    let ledger = fs::read_to_string(fixture.data.0.join("courses/demo/progress.jsonl"))
        .expect("the quiz wrote one progress line");
    let lines: Vec<&str> = ledger.lines().collect();
    assert_eq!(lines.len(), 1, "{ledger}");
    let record: Value = serde_json::from_str(lines[0]).expect("a progress record");
    assert_eq!(record["kind"], "probe");
    assert_eq!(record["nodeRefs"], json!([]));
    assert_eq!(record["correct"], true);
    assert_eq!(record["dontKnow"], false);
    assert_eq!(record["sessionId"], id);
    assert_eq!(record["course"], "demo");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn aborting_the_call_cancels_its_card() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-abort");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &free_teach()).await;
    wait_for(&client, &server, &id, "idle").await;

    let call = mcp_in_background(&server, &id, tools_call("ask", ask_arguments("Quale?")));
    let state = wait_for(&client, &server, &id, "waiting").await;
    let card = request_id(pending_card(&state).expect("a pending card"));

    call.abort();
    // The engine hung up: axum drops the handler, which cancels exactly the cards of that call.
    let state = poll_until(&client, &server, &id, Duration::from_secs(5), |state| {
        state["pendingRequestId"].is_null() && cancelled(state, &card)
    })
    .await;
    // The card was dismissed, not answered.
    assert!(state["items"].as_array().is_some_and(|items| {
        items
            .iter()
            .all(|item| item["requestId"].as_str() != Some(card.as_str()) || item.get("answer").is_none())
    }));

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_card_supersedes_the_older() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-supersede");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &free_teach()).await;
    wait_for(&client, &server, &id, "idle").await;

    let first = mcp_in_background(&server, &id, tools_call("ask", ask_arguments("Prima?")));
    let state = wait_for(&client, &server, &id, "waiting").await;
    let first_id = request_id(pending_card(&state).expect("the first card"));

    let second = mcp_in_background(&server, &id, tools_call("ask", ask_arguments("Seconda?")));
    // Node's supersede: the older card is cancelled and its call comes back right away.
    let reply = body_json(first.await.expect("the first call")).await;
    assert_eq!(tool_result(&reply), (CANCELLED.to_string(), true));

    let state = poll_until(&client, &server, &id, Duration::from_secs(10), |state| {
        state["pendingRequestId"]
            .as_str()
            .is_some_and(|pending| pending != first_id)
    })
    .await;
    assert!(cancelled(&state, &first_id), "{state}");
    let second_id = request_id(pending_card(&state).expect("the second card"));

    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": second_id,
            "kind": "ask",
            "answer": {"selected": ["Beta"]},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let reply = body_json(second.await.expect("the second call")).await;
    assert_eq!(
        tool_result(&reply),
        (r#"{"selected":["Beta"]}"#.to_string(), false)
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_cancels_only_its_own_call() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-abort-scope");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &free_teach()).await;
    wait_for(&client, &server, &id, "idle").await;

    let first = mcp_in_background(&server, &id, tools_call("ask", ask_arguments("Prima?")));
    let state = wait_for(&client, &server, &id, "waiting").await;
    let first_id = request_id(pending_card(&state).expect("the first card"));

    let second = mcp_in_background(&server, &id, tools_call("ask", ask_arguments("Seconda?")));
    let state = poll_until(&client, &server, &id, Duration::from_secs(10), |state| {
        state["pendingRequestId"]
            .as_str()
            .is_some_and(|pending| pending != first_id)
    })
    .await;
    let second_id = request_id(pending_card(&state).expect("the second card"));

    // The first call is over; dropping it must leave the card of the other call alone.
    first.abort();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let state = session(&client, &server, &id).await;
    assert_eq!(state["pendingRequestId"], second_id, "{state}");
    assert!(!cancelled(&state, &second_id), "{state}");

    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": second_id,
            "kind": "ask",
            "answer": {"selected": ["a"]},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply = body_json(second.await.expect("the second call")).await;
    assert_eq!(
        tool_result(&reply),
        (r#"{"selected":["Alfa"]}"#.to_string(), false)
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answer_errors() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-answer-errors");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &free_teach()).await;
    wait_for(&client, &server, &id, "idle").await;
    let ask = json!({"requestId": "x", "kind": "ask", "answer": {"selected": []}});

    // Unknown sessions and unknown cards.
    let response = answer(&client, &server, "nope", &ask).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Session not found"})
    );
    let response = answer(&client, &server, &id, &ask).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(response).await,
        json!({"error": "No pending request with that id"})
    );

    // A body zod refuses, before any lookup.
    let response = answer(
        &client,
        &server,
        &id,
        &json!({"kind": "quiz", "answer": {"selected": [], "dontKnow": false}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "requestId: Invalid input: expected string, received undefined"})
    );
    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{id}/answer"),
        "{",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Invalid JSON body"})
    );
    let response = client
        .post(url(&server, &format!("/api/sessions/{id}/answer")))
        .body(ask.to_string())
        .send()
        .await
        .expect("POST without the CSRF header");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // A pending ask: the kind must match, a stale id of the same kind is forwarded.
    let call = mcp_in_background(&server, &id, tools_call("ask", ask_arguments("Quale?")));
    let state = wait_for(&client, &server, &id, "waiting").await;
    let card = pending_card(&state).expect("the pending card").clone();

    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": card["requestId"],
            "kind": "quiz",
            "answer": {"selected": [], "dontKnow": false},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Request is a ask, not a quiz"})
    );

    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": "not-the-active-one",
            "kind": "ask",
            "answer": {"selected": ["a"]},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));
    let reply = body_json(call.await.expect("the call task")).await;
    assert_eq!(
        tool_result(&reply),
        (r#"{"selected":["Alfa"]}"#.to_string(), false)
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn anki_opt_in_answer_starts_the_session() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-anki-opt-in");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    // No `ankiOptIn`: the session opens on the Anki question, like Node's create.
    let id = create(&client, &server, &teach_planned(None)).await;
    let state = wait_for(&client, &server, &id, "waiting").await;
    let card = pending_card(&state).expect("the opt-in card").clone();

    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": card["requestId"],
            "kind": "ask",
            "answer": {"selected": ["no"]},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // The answer stores the opt-in and sends the opening turn the card was holding back.
    let state = wait_for(&client, &server, &id, "idle").await;
    assert_eq!(state["config"]["ankiOptIn"], false);
    assert_eq!(state["usage"]["turns"], 1, "{state}");
    assert!(state["pendingRequestId"].is_null(), "{state}");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn write_node_matches_lesson_append() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-write-node");
    let (absolute, relative) = seed_lesson(&fixture.data.0);
    // The same lesson elsewhere: the tool's write must equal `append_node` run by hand.
    let other = common::temp_dir("mcp-api-write-node-copy");
    let copy = other.0.join(&relative);
    fs::create_dir_all(copy.parent().expect("the copy has a parent")).expect("create the copy dir");
    fs::copy(&absolute, &copy).expect("copy the lesson");

    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &existing_teach(&relative)).await;
    wait_for(&client, &server, &id, "idle").await;

    let node = json!({
        "nodeId": "prima-cosa",
        "title": "La prima cosa",
        "tier": "axiom",
        "summary": "Vera.",
        "content": "<p>vera</p>",
    });
    let reply = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call("write_node", json!({"planNode": "N1", "node": node.clone()})),
        )
        .await,
    )
    .await;
    assert_eq!(
        tool_result(&reply),
        (r#"{"ok":true,"nodeCount":1}"#.to_string(), false)
    );

    doc::append_node(AppendOptions {
        lesson_path: Some(copy.display().to_string()),
        slug: None,
        node: node.clone(),
        base_dir: Some(other.0.display().to_string()),
    })
    .expect("append the same node by hand");

    let written = fs::read_to_string(fixture.data.0.join(&relative)).expect("the written lesson");
    let expected = fs::read_to_string(&copy).expect("the copied lesson");
    assert_eq!(normalise_file(&written), normalise_file(&expected));

    let state = session(&client, &server, &id).await;
    assert_eq!(state["lessonPath"], relative);
    let item = state["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["kind"] == "node"))
        .expect("the node item");
    assert_eq!(item["id"], "node:prima-cosa");
    assert_eq!(item["nodeId"], "prima-cosa");
    assert_eq!(item["planNode"], "N1");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn demo_starts_a_job() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-demo");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &free_teach()).await;
    wait_for(&client, &server, &id, "idle").await;

    let reply = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call("demo", json!({"title": "La caduta", "brief": "Una palla che cade."})),
        )
        .await,
    )
    .await;

    let (text, is_error) = tool_result(&reply);
    assert!(!is_error, "{text}");
    let jobs = body_json(
        client
            .get(format!("{}/api/jobs", server.url))
            .send()
            .await
            .expect("GET jobs"),
    )
    .await;
    let job = jobs
        .as_array()
        .and_then(|jobs| jobs.first())
        .expect("one demo job");
    assert_eq!(job["kind"], "demo");
    assert_eq!(job["title"], "Demo: La caduta");
    assert!(text.contains(job["id"].as_str().expect("job id")));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn write_tools_raise_courses() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-courses-event");
    let (_, relative) = seed_lesson(&fixture.data.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &existing_teach(&relative)).await;
    wait_for(&client, &server, &id, "idle").await;

    // Subscribed first: the hint fires 300 ms after the write.
    let mut events = server.events.subscribe();
    let reply = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call(
                "write_node",
                json!({
                    "planNode": "N1",
                    "node": {
                        "nodeId": "prima-cosa",
                        "title": "La prima cosa",
                        "tier": "axiom",
                        "summary": "Vera.",
                        "content": "<p>vera</p>",
                    },
                }),
            ),
        )
        .await,
    )
    .await;
    assert!(!tool_result(&reply).1, "{reply}");

    let course_event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match events.recv().await {
                Ok(payload) if payload == r#"{"type":"courses"}"# => return payload,
                Ok(_) => continue,
                Err(error) => panic!("the event bus closed: {error}"),
            }
        }
    })
    .await
    .expect("a courses event within 2 s");
    assert_eq!(course_event, r#"{"type":"courses"}"#);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_route_guards() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-guards");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(&client, &server, &free_teach()).await;
    wait_for(&client, &server, &id, "idle").await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});

    // The token is the only door: no header, then a wrong one.
    for token in [None, Some("nope")] {
        let mut request = client.post(url(&server, &format!("/mcp/{id}")));
        if let Some(token) = token {
            request = request.header("x-learner-token", token);
        }
        let response = request
            .body(ping.to_string())
            .send()
            .await
            .expect("POST /mcp without the right token");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{token:?}");
        assert_eq!(
            body_json(response).await,
            json!({"error": "Unauthorized"}),
            "{token:?}"
        );
    }

    // With the token: only `POST /mcp/<one segment>`.
    let response = client
        .get(url(&server, &format!("/mcp/{id}")))
        .header("x-learner-token", server.sessions.token())
        .send()
        .await
        .expect("GET /mcp");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = client
        .post(url(&server, &format!("/mcp/{id}/x")))
        .header("x-learner-token", server.sessions.token())
        .body(ping.to_string())
        .send()
        .await
        .expect("POST /mcp/<id>/x");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // No CSRF header on /mcp: the engine never sends one.
    let response = mcp(&client, &server, &id, &ping).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({"jsonrpc": "2.0", "id": 1, "result": {}})
    );

    // A session that does not exist, with the right token.
    let response = mcp(
        &client,
        &server,
        "00000000-0000-4000-8000-000000000000",
        &ping,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Session not found"})
    );

    server.shutdown().await;
}

/// A fake AnkiConnect for the sync tools: no card to import, a deck that accepts every note.
fn anki_connect() -> common::FakeAnki {
    common::fake_anki(|request| {
        let action = request["action"].as_str().unwrap_or_default();
        let result = match action {
            "findCards" => json!([]),
            "version" => json!(6),
            "createDeck" => json!(1),
            "modelNames" => json!(["Basic", "Cloze"]),
            "modelFieldNames" => match request["params"]["modelName"].as_str() {
                Some("Basic") => json!(["Front", "Back"]),
                _ => json!(["Text", "Extra"]),
            },
            "addNotes" => {
                let count = request["params"]["notes"].as_array().map_or(0, Vec::len);
                Value::Array((1..=count).map(|id| json!(id)).collect())
            }
            _ => Value::Null,
        };
        common::FakeReply {
            status: 200,
            body: json!({"result": result, "error": null}).to_string(),
            delay: Duration::ZERO,
        }
    })
}

/// The deck and note writes the fake saw: the review import never does either.
fn writes(fake: &common::FakeAnki) -> usize {
    fake.requests()
        .iter()
        .filter(|request| matches!(request["action"].as_str(), Some("createDeck" | "addNotes")))
        .count()
}

/// The request count once no new request arrived for 500 ms (the import runs behind the create).
async fn settled_len(fake: &common::FakeAnki) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut len = fake.requests().len();
    let mut unchanged = Instant::now();
    loop {
        assert!(
            Instant::now() < deadline,
            "the fake Anki never settled: {len} requests"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = fake.requests().len();
        if now == len {
            if unchanged.elapsed() >= Duration::from_millis(500) {
                return len;
            }
        } else {
            len = now;
            unchanged = Instant::now();
        }
    }
}

/// The names of a `tools/list`, in registry order.
async fn tool_names(client: &Client, server: &RunningServer, id: &str) -> Vec<String> {
    let reply = body_json(
        mcp(
            client,
            server,
            id,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await,
    )
    .await;
    reply["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("no tools in {reply}"))
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Writes the seeded lesson, plus the node whose Anki params give the sync one card to add.
fn seed_lesson_with_cards(data: &std::path::Path) -> String {
    let (absolute, relative) = seed_lesson(data);
    doc::append_node(AppendOptions {
        lesson_path: Some(absolute),
        slug: None,
        node: json!({
            "nodeId": "prima-cosa",
            "title": "La prima cosa",
            "tier": "unconditional-truth",
            "content": "<p>vera</p>",
            "ankiParams": {"targetQuestion": "Q?", "targetFact": "F"},
        }),
        base_dir: Some(data.display().to_string()),
    })
    .expect("append the Anki node");
    relative
}

/// AC4: no request reaches AnkiConnect before the opt-in answer said `si`, and the tool exists only
/// after it. The create already imports reviews, so only the writes are compared, never total traffic.
#[tokio::test(flavor = "multi_thread")]
async fn sync_anki_waits_for_the_explicit_yes() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-anki-sync");
    let relative = seed_lesson_with_cards(&fixture.data.0);
    let fake = anki_connect();
    let mut options = fixture.options();
    options.endpoints.anki = Some(fake.url.clone());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    // No `ankiOptIn`: the session opens on the card, and nothing has written to Anki yet.
    let body = json!({
        "kind": "teach",
        "course": "demo",
        "model": "haiku",
        "target": {"mode": "existing", "lessonPath": relative},
    });
    let id = create(&client, &server, &body).await;
    let state = wait_for(&client, &server, &id, "waiting").await;
    let card = pending_card(&state).expect("the opt-in card").clone();
    let base = settled_len(&fake).await;
    let base_writes = writes(&fake);
    assert_eq!(base_writes, 0, "a create writes no cards: {:?}", fake.requests());

    let names = tool_names(&client, &server, &id).await;
    assert!(!names.contains(&"sync_anki".to_string()), "{names:?}");
    let refused = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call("sync_anki", json!({})),
        )
        .await,
    )
    .await;
    let (text, is_error) = tool_result(&refused);
    assert!(is_error, "{text}");
    assert!(
        text.starts_with("Unknown tool \"sync_anki\". Available:"),
        "{text}"
    );
    assert_eq!(settled_len(&fake).await, base);
    assert_eq!(writes(&fake), base_writes);

    // The yes alone opens the lesson: it writes no card either.
    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": card["requestId"],
            "kind": "ask",
            "answer": {"selected": ["si"]},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    wait_for(&client, &server, &id, "idle").await;
    assert_eq!(writes(&fake), base_writes);

    let names = tool_names(&client, &server, &id).await;
    assert!(names.contains(&"sync_anki".to_string()), "{names:?}");
    let synced = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call("sync_anki", json!({})),
        )
        .await,
    )
    .await;
    let (text, is_error) = tool_result(&synced);
    assert!(!is_error, "{text}");
    let result: Value = serde_json::from_str(&text).expect("a JSON result");
    assert!(result["summary"].is_string(), "{text}");
    assert!(fake.requests().len() > base, "{:?}", fake.requests());
    assert!(writes(&fake) > base_writes);

    // A second session answers no: its own baseline (its review import adds a request), no writes.
    let id = create(&client, &server, &body).await;
    let state = wait_for(&client, &server, &id, "waiting").await;
    let card = pending_card(&state).expect("the opt-in card").clone();
    let base2 = settled_len(&fake).await;
    let writes2 = writes(&fake);
    let response = answer(
        &client,
        &server,
        &id,
        &json!({
            "requestId": card["requestId"],
            "kind": "ask",
            "answer": {"selected": ["no"]},
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    wait_for(&client, &server, &id, "idle").await;

    let names = tool_names(&client, &server, &id).await;
    assert!(!names.contains(&"sync_anki".to_string()), "{names:?}");
    let refused = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call("sync_anki", json!({})),
        )
        .await,
    )
    .await;
    assert!(tool_result(&refused).1, "{refused}");
    assert_eq!(settled_len(&fake).await, base2);
    assert_eq!(writes(&fake), writes2);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn write_exercise_updates_the_session_and_its_stream() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-write-exercise");
    common::write(
        &fixture.data.0,
        "courses/demo/exercises/package.json",
        "{\n  \"name\": \"demo-exercises\",\n  \"private\": true\n}\n",
    );
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(
        &client,
        &server,
        &json!({"kind": "practice", "course": "demo", "model": "haiku"}),
    )
    .await;
    wait_for(&client, &server, &id, "idle").await;

    // Subscribed first: the list is pushed to the open stream, not only saved.
    let mut stream = client
        .get(url(&server, &format!("/api/sessions/{id}/events")))
        .send()
        .await
        .expect("GET events");
    assert_eq!(stream.status(), StatusCode::OK);
    read_until(&mut stream, Duration::from_secs(10), |buffer| {
        buffer.contains("\n\n")
    })
    .await;

    let reply = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call(
                "write_exercise",
                json!({
                    "name": "01-demo",
                    "instructions": "Fai **questo**.",
                    "files": [{"path": "Esercizio.vue", "content": "<template/>\n"}],
                }),
            ),
        )
        .await,
    )
    .await;
    assert!(!tool_result(&reply).1, "{reply}");

    let state = session(&client, &server, &id).await;
    assert_eq!(state["exercises"][0]["name"], "01-demo", "{state}");
    assert!(
        fixture
            .data
            .0
            .join("courses/demo/exercises/01-demo/Esercizio.vue")
            .is_file(),
        "the exercise file is on disk"
    );

    let raw = read_until(&mut stream, Duration::from_secs(10), |buffer| {
        data_events(buffer)
            .iter()
            .any(|event| event["type"] == "exercises")
    })
    .await;
    let events = data_events(&raw);
    let update = events
        .iter()
        .find(|event| event["type"] == "exercises")
        .expect("the exercises event");
    assert_eq!(update["exercises"][0]["name"], "01-demo", "{update}");

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_open_problem_per_session_across_calls() {
    require_fake_engine();
    let fixture = Fixture::new("mcp-api-problem");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let id = create(
        &client,
        &server,
        &json!({"kind": "practice", "course": "demo", "model": "haiku"}),
    )
    .await;
    wait_for(&client, &server, &id, "idle").await;

    let problem = json!({
        "statement": "Risolvi.",
        "mode": "recall",
        "nodeKeys": ["demo/x#n1"],
        "modelSolution": "Così.",
    });
    let reply = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call("problem", problem.clone()),
        )
        .await,
    )
    .await;
    assert_eq!(
        tool_result(&reply),
        (
            "Problem shown. Wait for the learner's attempt in chat; coach with the hint ladder; call conclude when done."
                .to_string(),
            false
        )
    );

    // A second POST builds a new host, but the problem belongs to the session: it is still open.
    let reply = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call("problem", problem.clone()),
        )
        .await,
    )
    .await;
    assert_eq!(
        tool_result(&reply),
        (
            "A problem is already open: coach it to the end and call conclude before opening another."
                .to_string(),
            true
        )
    );

    let reply = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call(
                "conclude",
                json!({
                    "outcome": "solvedWithHints",
                    "verdict": "ok",
                    "rung": "R2",
                    "missClass": null,
                    "retest": false,
                }),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(tool_result(&reply), ("Concluded.".to_string(), false));

    let reply = body_json(
        mcp(
            &client,
            &server,
            &id,
            &tools_call("problem", problem),
        )
        .await,
    )
    .await;
    assert!(!tool_result(&reply).1, "{reply}");

    server.shutdown().await;
}
