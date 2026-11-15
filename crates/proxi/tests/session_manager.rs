//! The session manager against the fake engine: create/list/get, message, per-session events, persistence.
//!
//! Ported from `app/test/api-sessions.test.ts` (T13 of issue #15), one level below HTTP: the manager
//! API and the SSE channel it feeds.

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::sessions::{
    INTERRUPT_REQUEST, describe, pid_running, raw_inputs, require_fake_engine, seed_demo_course,
    teach_planned, user_texts, wait_for_state_file,
};
use proxi::anki_client::AnkiClient;
use proxi::anki_import::ImportRegistry;
use proxi::claude::ClaudeBin;
use proxi::embeddings::EmbeddingConfig;
use proxi::events::EventBus;
use proxi::limits::LimitsCache;
use proxi::protocol::{ANKI_OPT_IN_QUESTION, SessionState, SessionStatus};
use proxi::semantic::SemanticSearch;
use proxi::sessions::{AttachmentUpload, HttpError, ManagerOptions, SessionManager};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

struct TestEnv {
    repo: common::TempDir,
    data: common::TempDir,
    events: EventBus,
}

impl TestEnv {
    fn new() -> Self {
        let env = Self {
            repo: common::temp_dir("session-manager-repo"),
            data: common::temp_dir("session-manager-data"),
            events: EventBus::new(),
        };
        seed_demo_course(&env.data.0);
        env
    }

    fn manager(&self) -> SessionManager {
        manager_for(&self.repo.0, &self.data.0, &self.events)
    }
}

fn fake_bin() -> ClaudeBin {
    node_bin("fake-claude.mjs")
}

/// A node script as an engine: the same seam as Node's `fakeBins`.
fn node_bin(script: &str) -> ClaudeBin {
    ClaudeBin {
        cmd: "node".to_string(),
        args: common::sessions::fake_script_args(script, &[]),
    }
}

fn manager_for(repo: &Path, data: &Path, events: &EventBus) -> SessionManager {
    manager_with_agy(repo, data, events, node_bin("fake-agy.mjs"))
}

fn manager_with_agy(repo: &Path, data: &Path, events: &EventBus, agy: ClaudeBin) -> SessionManager {
    SessionManager::new(ManagerOptions {
        agy,
        ..options_for(repo, data, events, fake_bin())
    })
}

fn manager_with(env: &TestEnv, extra: &[&str]) -> SessionManager {
    let claude = ClaudeBin {
        cmd: "node".to_string(),
        args: common::sessions::fake_script_args("fake-claude.mjs", extra),
    };
    SessionManager::new(options_for(&env.repo.0, &env.data.0, &env.events, claude))
}

fn options_for(
    repo: &Path,
    data: &Path,
    events: &EventBus,
    claude: ClaudeBin,
) -> ManagerOptions {
    ManagerOptions {
        repo_root: repo.to_path_buf(),
        data_root: data.to_path_buf(),
        code_root: common::repo_root(),
        port: 4599,
        token: "test-token".to_string(),
        claude,
        agy: node_bin("fake-agy.mjs"),
        opencode: node_bin("fake-opencode.mjs"),
        add_dirs: vec![repo.parent().unwrap_or(repo).to_path_buf()],
        events: events.clone(),
        limits: Arc::new(LimitsCache::new()),
        anki: AnkiClient::new("http://127.0.0.1:1").expect("anki client"),
        imports: ImportRegistry::new(),
        jobs: Arc::new(proxi::tools::NoJobs),
        npm_bin: None,
        installs: Default::default(),
        semantic: Arc::new(SemanticSearch::for_repo(
            repo,
            EmbeddingConfig::load(repo).expect("embedding config"),
        )),
    }
}

fn settings(env: &TestEnv, body: &str) {
    let dir = env.repo.0.join(".learner");
    fs::create_dir_all(&dir).expect("create .learner");
    fs::write(dir.join("settings.json"), body).expect("write settings");
}

fn lesson_doc(title: &str, dag: &str) -> Value {
    json!({
        "metadata": {
            "lessonNumber": 1,
            "topic": "Demo",
            "title": title,
            "slug": "prima-lezione",
            "dateCreated": "",
            "dateModified": "",
            "version": "1",
            "initialDag": dag,
        },
        "nodes": [{"nodeId": "alpha", "title": "alpha", "tier": "derivation", "content": "x"}],
    })
}

fn write_lesson(data: &Path, course: &str, filename: &str, doc: Value) {
    let dir = data.join("courses").join(course).join("lessons");
    fs::create_dir_all(&dir).expect("create lessons dir");
    fs::write(
        dir.join(filename),
        format!(r#"<script type="application/json" id="lesson-data">{doc}</script>"#),
    )
    .expect("write lesson");
}

fn state_file(repo: &Path, id: &str) -> Value {
    let file = repo
        .join(".learner")
        .join("sessions")
        .join(id)
        .join("state.json");
    let raw = fs::read_to_string(&file).expect("state.json");
    serde_json::from_str(&raw).expect("state.json is JSON")
}

fn top_level_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect()
}

async fn wait_for(manager: &SessionManager, id: &str, status: SessionStatus) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let Some(state) = manager.get(id) else {
            panic!("session {id} vanished while waiting for {status:?}");
        };
        if state["status"] == serde_json::to_value(status).unwrap() {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {status:?}: {state}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn next_event(receiver: &mut UnboundedReceiver<String>) -> Value {
    let payload = tokio::time::timeout(Duration::from_secs(20), receiver.recv())
        .await
        .expect("an event within 20 s")
        .expect("the events channel is open");
    serde_json::from_str(&payload).expect("each event is JSON")
}

/// The fake's first delta: the `FAKE:slow` turn is really playing, so an in-band interrupt can reach
/// its script (an interrupt sent before the turn starts is dropped by the fake, unlike the CLI).
async fn wait_for_first_delta(manager: &SessionManager, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = manager.get(id).expect("a session");
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

#[tokio::test(flavor = "multi_thread")]
async fn create_reaches_idle_with_the_node_state_shape() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();

    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;

    assert_eq!(state["title"], "Prima lezione");
    assert_eq!(state["config"]["kind"], "teach");
    assert_eq!(state["config"]["ankiOptIn"], false);
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
    assert!(state.get("lastError").is_none());

    manager.shutdown().await;
    let file = state_file(&env.repo.0, &id);
    assert!(
        file.get("pendingStart").is_none(),
        "a completed opening turn leaves no pendingStart"
    );
    assert!(
        file.get("lastTurn").is_none(),
        "a successful turn forgets it"
    );
    // No rate_limit_event from the fake: the meter file is never created.
    assert!(!env.repo.0.join(".learner").join("limits.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_sends_the_snapshot_then_the_live_turn() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    let mut events = manager.subscribe(&id).expect("a known session");
    let snapshot = next_event(&mut events).await;
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["state"]["id"], id);
    assert_eq!(snapshot["state"]["items"].as_array().unwrap().len(), 2);

    manager.message(&id, json!({"text": "Ciao"})).await.unwrap();
    let mut live = Vec::new();
    loop {
        let event = next_event(&mut events).await;
        let done = event["type"] == "status" && event["status"] == "idle";
        live.push(event);
        if done {
            break;
        }
        assert!(live.len() < 30, "runaway event stream: {live:?}");
    }

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
}

#[tokio::test(flavor = "multi_thread")]
async fn state_file_keeps_node_key_order_and_parses_back() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    let dir = env.repo.0.join(".learner").join("sessions").join(&id);
    assert!(
        dir.join("system.md").is_file(),
        "the prompt reaches the engine"
    );
    assert!(dir.join("mcp.json").is_file(), "the MCP config too");

    manager.shutdown().await;
    let file = state_file(&env.repo.0, &id);
    assert_eq!(
        top_level_keys(&file),
        ["state", "provider", "resume", "canRetry"]
    );
    assert_eq!(file["provider"], "claude");
    assert!(file["resume"].is_string());
    assert_eq!(file["canRetry"], false);

    let state: SessionState = serde_json::from_value(file["state"].clone()).expect("a Node file");
    assert_eq!(state.id, id);
    assert_eq!(state.status, SessionStatus::Idle);
    assert_eq!(state.items.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_configs_are_400() {
    let env = TestEnv::new();
    let manager = env.manager();
    let cases = [
        json!({"course": "demo"}),
        json!({"kind": "teach", "course": "Bad Name", "model": "haiku", "ankiOptIn": false, "target": {"mode": "free"}}),
        json!({"kind": "teach", "course": "demo", "model": "gpt", "ankiOptIn": false, "target": {"mode": "free"}}),
        json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false, "target": {"mode": "planned"}}),
        json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false, "note": "x".repeat(5001), "target": {"mode": "free"}}),
        json!({"kind": null, "course": "demo", "model": "haiku", "ankiOptIn": false, "target": {"mode": "free"}}),
    ];
    for body in cases {
        let error = manager.create(body.clone()).await.unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!error.message.is_empty(), "{body}");
    }
    assert_eq!(manager.list(), json!([]), "a refused create saves nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn create_rejects_unknown_unplanned_and_already_taught_titles() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();

    let unknown = manager
        .create(json!({"kind": "practice", "course": "nope", "model": "haiku"}))
        .await
        .unwrap_err();
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown.message, "Course \"nope\" does not exist");

    let unplanned = manager
        .create(json!({
            "kind": "teach",
            "course": "demo",
            "model": "haiku",
            "ankiOptIn": false,
            "target": {"mode": "planned", "title": "Altra lezione"},
        }))
        .await
        .unwrap_err();
    assert_eq!(unplanned.status, StatusCode::BAD_REQUEST);
    assert!(
        unplanned
            .message
            .contains("is not a planned lesson of demo"),
        "{}",
        unplanned.message
    );

    write_lesson(
        &env.data.0,
        "demo",
        "0001-prima-lezione.html",
        lesson_doc("Prima lezione", "graph TD\nN1[\"Nodo 1: a\"]"),
    );
    let taught = manager
        .create(teach_planned(Some(false)))
        .await
        .unwrap_err();
    assert_eq!(taught.status, StatusCode::BAD_REQUEST);
    assert!(
        taught.message.contains("già stata svolta"),
        "{}",
        taught.message
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_lesson_restores_the_plan() {
    require_fake_engine();
    let env = TestEnv::new();
    let dag = "graph TD\nN1[\"Nodo 1: a\"]\nN2[\"Nodo 2: b\"]";
    write_lesson(
        &env.data.0,
        "demo",
        "0001-prima-lezione.html",
        lesson_doc("Prima lezione", dag),
    );
    let manager = env.manager();
    let id = manager
        .create(json!({
            "kind": "teach",
            "course": "demo",
            "model": "haiku",
            "ankiOptIn": false,
            "target": {"mode": "existing", "lessonPath": "courses/demo/lessons/0001-prima-lezione.html"},
        }))
        .await
        .unwrap();

    let state = manager.get(&id).expect("a session");
    assert_eq!(state["title"], "Prima lezione");
    assert_eq!(
        state["lessonPath"],
        "courses/demo/lessons/0001-prima-lezione.html"
    );
    assert_eq!(state["plan"]["topic"], "Demo");
    assert_eq!(state["plan"]["goal"], "");
    assert_eq!(state["plan"]["dag"], dag);
    assert_eq!(state["plan"]["approved"], true);
    assert_eq!(state["plan"]["nodeStatus"]["N1"], "done");
    assert_eq!(state["plan"]["nodeStatus"]["N2"], "current");
    wait_for(&manager, &id, SessionStatus::Idle).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_target_rejects_a_missing_or_prose_lesson() {
    let env = TestEnv::new();
    let mut prose = lesson_doc("In prosa", "");
    prose["metadata"].as_object_mut().unwrap().remove("initialDag");
    prose["nodes"] = json!([]);
    write_lesson(&env.data.0, "demo", "0001-in-prosa.html", prose);
    let manager = env.manager();

    for path in [
        "courses/demo/lessons/9999-manca.html",
        "courses/demo/lessons/0001-in-prosa.html",
    ] {
        let error = manager
            .create(json!({
                "kind": "teach",
                "course": "demo",
                "model": "haiku",
                "ankiOptIn": false,
                "target": {"mode": "existing", "lessonPath": path},
            }))
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{path}");
        assert!(!error.message.is_empty(), "{path}");
    }
    assert_eq!(manager.list(), json!([]), "a refused create saves nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn free_standalone_lesson_provisions_the_course() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager
        .create(json!({
            "kind": "teach",
            "course": "lezioni-singole",
            "model": "haiku",
            "ankiOptIn": false,
            "target": {"mode": "free"},
            "note": "  Derivata di x^2  ",
        }))
        .await
        .unwrap();
    let state = manager.get(&id).expect("a session");
    assert_eq!(state["title"], "Lezione singola · Derivata di x^2");
    assert!(
        env.data
            .0
            .join("courses")
            .join("lezioni-singole")
            .join("MISSION.md")
            .is_file(),
        "the container is created with its mission"
    );
    wait_for(&manager, &id, SessionStatus::Idle).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_anki_opt_in_waits_without_a_process() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(None)).await.unwrap();

    let state = manager.get(&id).expect("a session");
    assert_eq!(state["status"], "waiting");
    let item = &state["items"][0];
    assert_eq!(item["kind"], "ask");
    assert_eq!(item["input"]["question"], ANKI_OPT_IN_QUESTION);
    assert!(item["requestId"].is_string());
    assert_eq!(state["pendingRequestId"], item["requestId"]);
    assert!(
        !env.repo
            .0
            .join(".learner")
            .join("sessions")
            .join(&id)
            .join("system.md")
            .exists(),
        "no claude process before the answer"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn message_while_waiting_or_running_is_409() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();

    let waiting = manager.create(teach_planned(None)).await.unwrap();
    let error = manager
        .message(&waiting, json!({"text": "Ciao"}))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);

    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;
    manager
        .message(&id, json!({"text": "FAKE:slow:10"}))
        .await
        .unwrap();
    let busy = manager
        .message(&id, json!({"text": "Ciao"}))
        .await
        .unwrap_err();
    assert_eq!(busy.status, StatusCode::CONFLICT);
    assert!(busy.message.contains("busy"), "{}", busy.message);
    wait_for(&manager, &id, SessionStatus::Idle).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_messages_let_exactly_one_turn_through() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    // AC: the closed/busy check and the turn are recorded in one lock, so only the first message
    // finds the session free (Node checks and marks it busy in one synchronous tick).
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let manager = manager.clone();
        let id = id.clone();
        tasks.push(tokio::spawn(async move {
            manager.message(&id, json!({"text": "FAKE:slow:5"})).await
        }));
    }
    let mut accepted = 0;
    let mut busy = 0;
    for task in tasks {
        match task.await.expect("message task") {
            Ok(()) => accepted += 1,
            Err(error) => {
                assert_eq!(error.status, StatusCode::CONFLICT, "{}", error.message);
                assert!(error.message.contains("busy"), "{}", error.message);
                busy += 1;
            }
        }
    }
    assert_eq!((accepted, busy), (1, 3), "one turn accepted, three 409");

    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    assert_eq!(
        state["usage"]["turns"], 2,
        "one engine turn for the message: {state}"
    );
    let learner = state["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "learner")
        .count();
    assert_eq!(learner, 2, "the opening and one message: {state}");
}

#[tokio::test(flavor = "multi_thread")]
async fn message_validation_runs_before_the_id_lookup() {
    let env = TestEnv::new();
    let manager = env.manager();
    for body in [
        json!({}),
        json!({"text": ""}),
        json!({"text": "x".repeat(20001)}),
        json!({"text": "x", "attachments": [{"name": "a"}]}),
    ] {
        let error = manager.message("nope", body.clone()).await.unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!error.message.is_empty(), "{body}");
    }
    let error = manager
        .message("nope", json!({"text": "ciao"}))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "Session not found");
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_marks_the_session_retryable_and_the_next_message_resumes() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    // AC5: the next message must resume the conversation. The fake echoes the `--resume` token in
    // its init event, so a fresh `--session-id` would replace the token saved on disk.
    let state_path = env
        .repo
        .0
        .join(".learner")
        .join("sessions")
        .join(&id)
        .join("state.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    let resume_before = loop {
        let saved = fs::read_to_string(&state_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        if let Some(token) = saved
            .as_ref()
            .and_then(|file| file.get("resume"))
            .and_then(Value::as_str)
        {
            break token.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "the first turn never flushed its resume token"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    manager
        .message(&id, json!({"text": "FAKE:crash"}))
        .await
        .unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Error).await;
    assert_eq!(state["retryable"], true);
    assert!(
        state["lastError"]
            .as_str()
            .unwrap_or("")
            .starts_with("teacher process exited (1)"),
        "{}",
        state["lastError"]
    );

    manager
        .message(&id, json!({"text": "ancora"}))
        .await
        .unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    assert_eq!(state["usage"]["turns"], 2, "the resumed turn counted");

    manager.shutdown().await;
    assert_eq!(
        state_file(&env.repo.0, &id)["resume"],
        json!(resume_before),
        "the relaunch resumed the conversation instead of starting a new one"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_turn_state_file_carries_last_turn_and_can_retry() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();

    let with_attachments = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &with_attachments, SessionStatus::Idle).await;
    manager
        .message(
            &with_attachments,
            json!({"text": "FAKE:crash", "attachments": []}),
        )
        .await
        .unwrap();
    wait_for(&manager, &with_attachments, SessionStatus::Error).await;

    let without = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &without, SessionStatus::Idle).await;
    manager
        .message(&without, json!({"text": "FAKE:crash"}))
        .await
        .unwrap();
    wait_for(&manager, &without, SessionStatus::Error).await;

    manager.shutdown().await;
    let file = state_file(&env.repo.0, &with_attachments);
    assert_eq!(
        top_level_keys(&file),
        ["state", "provider", "resume", "lastTurn", "canRetry"]
    );
    assert_eq!(file["canRetry"], true);
    assert_eq!(
        serde_json::to_string(&file["lastTurn"]).unwrap(),
        r#"{"text":"FAKE:crash","show":["FAKE:crash"],"attachments":[],"turnText":"FAKE:crash","pushedLearner":true}"#
    );

    let plain = state_file(&env.repo.0, &without);
    assert_eq!(plain["canRetry"], true);
    assert_eq!(
        serde_json::to_string(&plain["lastTurn"]).unwrap(),
        r#"{"text":"FAKE:crash","show":["FAKE:crash"],"turnText":"FAKE:crash","pushedLearner":true}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_without_a_key_reports_the_missing_key() {
    require_fake_engine();
    let env = TestEnv::new();
    settings(&env, r#"{"claudeAuth": "apiKey", "anthropicApiKey": ""}"#);
    let manager = env.manager();

    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Error).await;
    assert_eq!(state["retryable"], true);
    assert_eq!(
        state["lastError"],
        "teacher process exited (1): Nessuna API key Anthropic configurata nelle Impostazioni."
    );
}

/// The teacher items' texts, in state order.
fn teacher_texts(state: &Value) -> Vec<String> {
    state["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["kind"] == "teacher")
        .map(|item| item["text"].as_str().unwrap_or("").to_string())
        .collect()
}

/// The `(<prefix>…)` id a fake engine names in its answer (`conv-<pid>` / `ses-<pid>`).
fn engine_id(text: &str, prefix: &str) -> Option<String> {
    let start = text.find(&format!("({prefix}"))? + 1;
    let end = start + text[start..].find(')')?;
    Some(text[start..end].to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn agy_provider_runs_a_session() {
    require_fake_engine();
    let env = TestEnv::new();
    settings(&env, r#"{"provider":"agy"}"#);
    let manager = env.manager();

    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    let conv = teacher_texts(&state)
        .iter()
        .find_map(|text| engine_id(text, "conv-"))
        .expect("the agy teacher names its conversation");

    manager.shutdown().await;
    let file = state_file(&env.repo.0, &id);
    assert_eq!(file["provider"], "agy");
    assert_eq!(file["resume"], conv.as_str());
}

#[tokio::test(flavor = "multi_thread")]
async fn opencode_provider_runs_two_turns_on_one_session() {
    require_fake_engine();
    let env = TestEnv::new();
    settings(&env, r#"{"provider":"opencode"}"#);
    let manager = env.manager();

    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    let session = teacher_texts(&state)
        .iter()
        .find_map(|text| engine_id(text, "ses-"))
        .expect("the opencode teacher names its session");

    manager.message(&id, json!({"text": "Ciao"})).await.unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    let texts = teacher_texts(&state);
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(
        texts
            .iter()
            .all(|text| engine_id(text, "ses-").as_deref() == Some(session.as_str())),
        "{texts:?}"
    );
    assert_eq!(state["status"], "idle");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_agy_executable_fails_like_node() {
    let env = TestEnv::new();
    settings(&env, r#"{"provider":"agy"}"#);
    let name = if cfg!(windows) {
        "missing-agy.exe"
    } else {
        "missing-agy"
    };
    let missing = env.repo.0.join(name);
    let agy = ClaudeBin {
        cmd: missing.display().to_string(),
        args: Vec::new(),
    };
    let manager = manager_with_agy(&env.repo.0, &env.data.0, &env.events, agy);

    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Error).await;
    assert_eq!(
        state["lastError"],
        format!(
            "teacher process exited (null): spawn error: spawn {} ENOENT",
            missing.display()
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn agy_sessions_keep_their_engine_after_a_settings_switch() {
    require_fake_engine();
    let env = TestEnv::new();
    settings(&env, r#"{"provider":"agy"}"#);
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    let conv = teacher_texts(&state)
        .iter()
        .find_map(|text| engine_id(text, "conv-"))
        .expect("the agy teacher names its conversation");
    manager.shutdown().await;

    settings(&env, r#"{"provider":"claude"}"#);
    let manager = env.manager();
    manager
        .message(&id, json!({"text": "Dopo il cambio"}))
        .await
        .unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    let texts = teacher_texts(&state);
    let last = texts.last().expect("an answer after the reload");
    assert!(last.contains(&format!("({conv})")), "{last}");
}

fn hand_state(id: &str, status: &str, items: Value) -> Value {
    json!({
        "id": id,
        "config": {"kind": "teach", "course": "demo", "model": "haiku", "target": {"mode": "planned", "title": "Prima lezione"}, "ankiOptIn": false},
        "title": "Prima lezione",
        "createdAt": "2026-01-01T00:00:00.000Z",
        "updatedAt": "2026-01-01T00:00:00.000Z",
        "status": status,
        "usage": {"inputTokens": 0, "outputTokens": 0, "cacheReadTokens": 0, "cacheCreationTokens": 0, "turns": 0},
        "items": items,
        "pendingRequestId": null,
        "plan": null,
    })
}

fn write_state_file(repo: &Path, id: &str, file: Value) {
    let dir = repo.join(".learner").join("sessions").join(id);
    fs::create_dir_all(&dir).expect("create session dir");
    fs::write(dir.join("state.json"), file.to_string()).expect("write state.json");
}

#[tokio::test(flavor = "multi_thread")]
async fn load_recovers_interrupted_sessions() {
    let env = TestEnv::new();
    write_state_file(
        &env.repo.0,
        "busy-last",
        json!({
            "state": hand_state("busy-last", "running", json!([])),
            "provider": "claude",
            "lastTurn": {"text": "x", "show": ["x"], "turnText": "x", "pushedLearner": true},
        }),
    );
    write_state_file(
        &env.repo.0,
        "busy-plain",
        json!({"state": hand_state("busy-plain", "running", json!([])), "provider": "claude"}),
    );
    write_state_file(
        &env.repo.0,
        "streaming",
        json!({
            "state": hand_state(
                "streaming",
                "idle",
                json!([
                    {"id": "t:m:1", "at": "2026-01-01T00:00:00.000Z", "kind": "teacher", "text": "x", "streaming": true},
                    {"id": "req:q", "at": "2026-01-01T00:00:00.000Z", "kind": "quiz", "requestId": "q1", "input": {"question": "2+2?", "options": []}},
                ]),
            ),
            "provider": "claude",
        }),
    );
    let broken = env.repo.0.join(".learner").join("sessions").join("broken");
    fs::create_dir_all(&broken).expect("create broken dir");
    fs::write(broken.join("state.json"), "{not json").expect("write broken file");

    let manager = env.manager();

    let recovered = manager.get("busy-last").expect("loaded");
    assert_eq!(recovered["status"], "error");
    assert_eq!(
        recovered["lastError"],
        "Sessione interrotta prima del completamento del turno"
    );
    assert_eq!(recovered["retryable"], true);

    let plain = manager.get("busy-plain").expect("loaded");
    assert_eq!(plain["status"], "idle");

    let streaming = manager.get("streaming").expect("loaded");
    assert_eq!(streaming["items"][0]["streaming"], false);
    assert_eq!(streaming["items"][1]["cancelled"], true);
    assert!(streaming["pendingRequestId"].is_null());

    assert_eq!(
        manager.list().as_array().unwrap().len(),
        3,
        "the broken dir is skipped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn load_re_asks_the_anki_opt_in_of_an_unstarted_teach_session() {
    let env = TestEnv::new();
    // The app was closed while the question card was on screen: `pendingStart` is still there and
    // the reload cancels the old card, so the session must ask again instead of waiting forever.
    write_state_file(
        &env.repo.0,
        "asking",
        json!({
            "state": {
                "id": "asking",
                "config": {"kind": "teach", "course": "demo", "model": "haiku", "target": {"mode": "planned", "title": "Prima lezione"}},
                "title": "Prima lezione",
                "createdAt": "2026-01-01T00:00:00.000Z",
                "updatedAt": "2026-01-01T00:00:00.000Z",
                "status": "waiting",
                "usage": {"inputTokens": 0, "outputTokens": 0, "cacheReadTokens": 0, "cacheCreationTokens": 0, "turns": 0},
                "items": [{
                    "id": "req:old",
                    "at": "2026-01-01T00:00:00.000Z",
                    "kind": "ask",
                    "requestId": "old",
                    "input": {"question": ANKI_OPT_IN_QUESTION, "options": []},
                    "cancelled": true,
                }],
                "pendingRequestId": "old",
                "plan": null,
            },
            "provider": "claude",
            "pendingStart": "Prima lezione",
        }),
    );
    let manager = env.manager();

    let state = manager.get("asking").expect("loaded");
    assert_eq!(state["status"], "waiting", "still waiting for the learner");
    let asks: Vec<&Value> = state["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "ask")
        .collect();
    assert_eq!(
        asks.len(),
        1,
        "the cancelled card is replaced, not duplicated: {state}"
    );
    assert_eq!(asks[0]["input"]["question"], ANKI_OPT_IN_QUESTION);
    assert!(
        asks[0].get("cancelled").is_none(),
        "the new card is live: {state}"
    );
    assert_ne!(asks[0]["requestId"], "old", "{state}");
    assert_eq!(state["pendingRequestId"], asks[0]["requestId"]);
    assert!(
        !env.repo
            .0
            .join(".learner")
            .join("sessions")
            .join("asking")
            .join("system.md")
            .exists(),
        "no engine starts before the answer"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_on_a_loaded_pending_start_opens_the_lesson_with_it() {
    require_fake_engine();
    let env = TestEnv::new();
    // A teach session whose opening turn never ran (created and closed before it started, Anki
    // already decided): the next message must carry the lesson opening in the same turn.
    write_state_file(
        &env.repo.0,
        "unstarted",
        json!({
            "state": hand_state("unstarted", "idle", json!([])),
            "provider": "claude",
            "pendingStart": "Prima lezione",
        }),
    );
    let manager = env.manager();

    manager
        .message("unstarted", json!({"text": "Ciao"}))
        .await
        .expect("the opening turn is accepted");

    let deadline = Instant::now() + Duration::from_secs(20);
    let state = loop {
        let state = manager.get("unstarted").expect("the loaded session");
        if state["status"] == "idle" && state["usage"]["turns"] == 1 {
            break state;
        }
        assert!(Instant::now() < deadline, "timed out: {state}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    let learners: Vec<&Value> = state["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "learner")
        .collect();
    let texts: Vec<&str> = learners
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect();
    assert_eq!(
        texts,
        ["Iniziamo la lezione \"Prima lezione\".", "Ciao"],
        "the opening and the message share one turn: {state}"
    );
    assert_eq!(state["config"]["ankiOptIn"], false, "the decision is kept");
    manager.shutdown().await;
    assert!(
        state_file(&env.repo.0, "unstarted")
            .get("pendingStart")
            .is_none(),
        "the opening turn clears pendingStart"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn load_keeps_the_has_run_token_for_the_next_message() {
    require_fake_engine();
    let env = TestEnv::new();
    let token = "11111111-1111-4111-8111-111111111111";
    write_state_file(
        &env.repo.0,
        "old-run",
        json!({
            "state": hand_state("old-run", "idle", json!([])),
            "provider": "claude",
            "hasRun": true,
            "claudeSessionId": token,
        }),
    );
    let manager = env.manager();

    manager
        .message("old-run", json!({"text": "Ciao"}))
        .await
        .unwrap();
    let state = wait_for(&manager, "old-run", SessionStatus::Idle).await;
    assert_eq!(state["usage"]["turns"], 1);

    manager.shutdown().await;
    let file = state_file(&env.repo.0, "old-run");
    assert_eq!(
        file["resume"], token,
        "the legacy token resumes the conversation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_is_newest_first_without_heavy_fields() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let teach = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &teach, SessionStatus::Idle).await;
    let practice = manager
        .create(json!({"kind": "practice", "course": "demo", "model": "haiku"}))
        .await
        .unwrap();
    wait_for(&manager, &practice, SessionStatus::Idle).await;

    let list = manager.list();
    let rows = list.as_array().expect("a list");
    assert_eq!(rows[0]["id"], practice);
    assert_eq!(rows[1]["id"], teach);
    for row in rows {
        for key in ["items", "plan", "exercises", "pendingRequestId"] {
            assert!(row.get(key).is_none(), "{} leaks {key}", row["id"]);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_session_emits_the_sessions_event() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let mut events = env.events.subscribe();

    manager.create(teach_planned(Some(false))).await.unwrap();
    let payload = tokio::time::timeout(Duration::from_millis(1500), events.recv())
        .await
        .expect("the debounced sessions hint")
        .expect("the bus is open");
    assert_eq!(payload, r#"{"type":"sessions"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_sessions_are_not_found() {
    let env = TestEnv::new();
    let manager = env.manager();
    assert!(manager.get("nope").is_none());
    assert!(manager.subscribe("nope").is_none());
    let error: HttpError = manager
        .message("nope", json!({"text": "x"}))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_close_delete_and_retry_on_a_missing_session_are_404() {
    let env = TestEnv::new();
    let manager = env.manager();
    assert_eq!(
        manager.interrupt("nope").unwrap_err().status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        manager.retry("nope").await.unwrap_err().status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        manager.close("nope").await.unwrap_err().status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        manager.delete("nope").await.unwrap_err().status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_are_stored_with_node_names_and_reach_the_engine() {
    require_fake_engine();
    let env = TestEnv::new();
    let input = env.repo.0.join("input.jsonl");
    let input = input.to_str().expect("utf-8 repo path").to_string();
    let manager = manager_with(&env, &["--input-out", &input]);
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    let metas = manager
        .save_attachments(
            &id,
            vec![
                AttachmentUpload {
                    name: "diagram.png".to_string(),
                    mime_type: Some("IMAGE/PNG".to_string()),
                    bytes: vec![0x89, 0x50],
                },
                AttachmentUpload {
                    name: "../x/notes.pdf".to_string(),
                    mime_type: None,
                    bytes: vec![1, 2, 3],
                },
            ],
        )
        .unwrap();
    let metas = metas.as_array().expect("an array of metas").clone();
    assert_eq!(metas.len(), 2);
    assert_eq!(metas[0]["name"], "diagram.png");
    assert_eq!(metas[0]["mimeType"], "image/png", "Blob types are lowercased");
    assert_eq!(metas[0]["size"], 2);
    assert_eq!(metas[1]["name"], "notes.pdf", "the basename, not the path");
    assert_eq!(metas[1]["mimeType"], "application/octet-stream");
    assert_eq!(metas[1]["size"], 3);
    for meta in &metas {
        let file_id = meta["id"].as_str().expect("an id");
        let name = meta["name"].as_str().expect("a name");
        assert_eq!(
            meta["path"],
            format!(".learner/sessions/{id}/attachments/{}_{name}", &file_id[..8])
        );
    }
    assert_eq!(
        fs::read(env.repo.0.join(metas[0]["path"].as_str().unwrap())).unwrap(),
        vec![0x89, 0x50]
    );
    assert_eq!(
        fs::read(env.repo.0.join(metas[1]["path"].as_str().unwrap())).unwrap(),
        vec![1, 2, 3]
    );
    assert_eq!(
        serde_json::to_string(&metas[0]).unwrap(),
        format!(
            r#"{{"id":"{}","name":"diagram.png","path":"{}","mimeType":"image/png","size":2}}"#,
            metas[0]["id"].as_str().unwrap(),
            metas[0]["path"].as_str().unwrap()
        )
    );

    manager
        .message(
            &id,
            json!({"text": "guarda", "attachments": metas.clone()}),
        )
        .await
        .unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    let learner = state["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "learner")
        .next_back()
        .expect("the message bubble");
    assert_eq!(learner["text"], "guarda");
    assert_eq!(learner["attachments"], Value::Array(metas.clone()));
    assert_eq!(
        user_texts(Path::new(&input))
            .last()
            .expect("the turn reached the engine"),
        &format!(
            "[Allegati forniti dallo studente:\n- diagram.png (percorso: {})\n- notes.pdf (percorso: {})]\n\nguarda",
            metas[0]["path"].as_str().unwrap(),
            metas[1]["path"].as_str().unwrap()
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_or_closed_attachments_are_refused() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    let error = manager
        .save_attachments(
            &id,
            vec![AttachmentUpload {
                name: "setup.exe".to_string(),
                mime_type: None,
                bytes: vec![1],
            }],
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "Tipo di file non supportato: .exe");

    let error = manager
        .save_attachments(
            "nope",
            vec![AttachmentUpload {
                name: "a.png".to_string(),
                mime_type: None,
                bytes: vec![1],
            }],
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "Session not found");

    manager.close(&id).await.unwrap();
    let error = manager
        .save_attachments(
            &id,
            vec![AttachmentUpload {
                name: "a.png".to_string(),
                mime_type: None,
                bytes: vec![1],
            }],
        )
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "Session is closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_reruns_the_failed_turn_without_a_second_bubble() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    manager
        .message(&id, json!({"text": "FAKE:error_once x"}))
        .await
        .unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Error).await;
    assert_eq!(state["retryable"], true);

    manager.retry(&id).await.unwrap();
    let state = wait_for(&manager, &id, SessionStatus::Idle).await;
    let items = state["items"].as_array().expect("items");
    assert_eq!(
        items
            .iter()
            .filter(|item| item["kind"] == "learner" && item["text"] == "FAKE:error_once x")
            .count(),
        1,
        "the rerun turn keeps the one bubble: {state}"
    );
    assert!(
        items
            .iter()
            .all(|item| !(item["kind"] == "notice" && item["level"] == "error")),
        "the error notice is gone: {state}"
    );

    // The flush after the idle is asynchronous: wait for the file, then read the flag.
    let file = wait_for_state_file(&env.repo.0, &id, |v| {
        v.get("lastTurn").is_none_or(Value::is_null)
    })
    .await;
    assert_ne!(file["canRetry"], json!(true), "{file}");

    let error = manager.retry(&id).await.unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "Nessun turno fallito da riprovare");
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_is_409_while_busy_or_closed() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    manager
        .message(&id, json!({"text": "FAKE:slow:20"}))
        .await
        .unwrap();
    wait_for(&manager, &id, SessionStatus::Running).await;
    let error = manager.retry(&id).await.unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        error.message,
        "The teacher is busy: wait for the current turn to finish"
    );

    wait_for(&manager, &id, SessionStatus::Idle).await;
    manager.close(&id).await.unwrap();
    let error = manager.retry(&id).await.unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "Session is closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_loaded_interrupted_turn_retries_on_a_fresh_launch() {
    require_fake_engine();
    let env = TestEnv::new();
    write_state_file(
        &env.repo.0,
        "cut",
        json!({
            "state": hand_state(
                "cut",
                "running",
                json!([{"id": "l:1", "at": "2026-01-01T00:00:00.000Z", "kind": "learner", "text": "ciao"}]),
            ),
            "provider": "claude",
            "lastTurn": {"text": "ciao", "show": ["ciao"], "turnText": "ciao", "pushedLearner": true},
        }),
    );
    let manager = env.manager();
    let state = manager.get("cut").expect("loaded");
    assert_eq!(state["status"], "error");
    assert_eq!(state["retryable"], true);

    manager.retry("cut").await.unwrap();
    let state = wait_for(&manager, "cut", SessionStatus::Idle).await;
    let learners = state["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "learner")
        .count();
    assert_eq!(learners, 1, "the retried turn duplicates no bubble: {state}");
    assert_eq!(state["usage"]["turns"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_stops_the_turn_in_band_and_close_leaves_no_process() {
    require_fake_engine();
    let env = TestEnv::new();
    let input = env.repo.0.join("input.jsonl");
    let input = input.to_str().expect("utf-8 repo path").to_string();
    let manager = manager_with(&env, &["--input-out", &input]);
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;

    manager
        .message(&id, json!({"text": "FAKE:slow"}))
        .await
        .unwrap();
    wait_for(&manager, &id, SessionStatus::Running).await;
    wait_for_first_delta(&manager, &id).await;
    manager.interrupt(&id).unwrap();
    let state = tokio::time::timeout(
        Duration::from_secs(8),
        wait_for(&manager, &id, SessionStatus::Idle),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the interrupt idles the turn within 8 s: {:?}",
            manager.get(&id)
        )
    });

    let items = state["items"].as_array().expect("items");
    let notices: Vec<&Value> = items.iter().filter(|item| item["kind"] == "notice").collect();
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
    assert_eq!(state["retryable"], false);
    assert!(
        raw_inputs(Path::new(&input))
            .iter()
            .any(|line| line == INTERRUPT_REQUEST),
        "the engine got the in-band request"
    );

    // In-band for Claude: the process stays up after the interrupt, as in Node.
    assert_eq!(manager.live_pids().len(), 1);
    let pid = manager.live_pids()[0];
    manager.close(&id).await.unwrap();
    assert!(manager.live_pids().is_empty());
    assert!(!pid_running(pid), "the engine process is gone");
    let state = manager.get(&id).expect("a closed session");
    assert_eq!(state["status"], "closed");
    assert_eq!(state_file(&env.repo.0, &id)["state"]["status"], "closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_removes_the_session_and_no_late_flush_recreates_it() {
    require_fake_engine();
    let env = TestEnv::new();
    let manager = env.manager();
    let id = manager.create(teach_planned(Some(false))).await.unwrap();
    wait_for(&manager, &id, SessionStatus::Idle).await;
    manager
        .message(&id, json!({"text": "FAKE:slow"}))
        .await
        .unwrap();
    wait_for(&manager, &id, SessionStatus::Running).await;
    assert_eq!(manager.live_pids().len(), 1);
    let pid = manager.live_pids()[0];
    manager
        .save_attachments(
            &id,
            vec![AttachmentUpload {
                name: "diagram.png".to_string(),
                mime_type: Some("image/png".to_string()),
                bytes: vec![0x89, 0x50],
            }],
        )
        .unwrap();

    manager.delete(&id).await.unwrap();
    assert!(manager.get(&id).is_none());
    let dir = env.repo.0.join(".learner").join("sessions").join(&id);
    assert!(!dir.exists(), "the session dir went away, attachments included");
    assert!(
        manager
            .list()
            .as_array()
            .expect("a list")
            .iter()
            .all(|row| row["id"] != id.as_str())
    );
    assert!(manager.live_pids().is_empty());
    assert!(!pid_running(pid), "the engine process is gone");

    // A save timer scheduled before the delete must not resurrect the dir.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!dir.exists(), "a late flush recreated the deleted dir");

    assert_eq!(
        manager.delete(&id).await.unwrap_err().status,
        StatusCode::NOT_FOUND
    );
}
