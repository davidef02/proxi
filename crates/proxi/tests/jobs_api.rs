//! The job routes over real HTTP, including the job MCP endpoint and graph extraction.

mod common;

use std::fs;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

use common::sessions::{
    Fixture, body_json, pid_running, post, require_fake_engine, teach_planned, wait_for,
};
use proxi::server::{RunningServer, start};

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{path}", server.url)
}

fn research_message(question: &str) -> String {
    let input = json!({"question": question});
    format!("FAKE:tool:research:{}", STANDARD.encode(input.to_string()))
}

fn job_script(steps: Value) -> String {
    format!("FAKE:job:{}", STANDARD.encode(steps.to_string()))
}

async fn create_session(client: &Client, server: &RunningServer) -> String {
    let response = post(
        client,
        server,
        "/api/sessions",
        &teach_planned(Some(false)).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["id"]
        .as_str()
        .expect("session id")
        .to_string()
}

async fn only_job(client: &Client, server: &RunningServer) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let response = client
            .get(url(server, "/api/jobs"))
            .send()
            .await
            .expect("GET jobs");
        let jobs = body_json(response).await;
        if jobs.as_array().is_some_and(|rows| rows.len() == 1) {
            return jobs[0].clone();
        }
        assert!(Instant::now() < deadline, "timed out waiting for one job: {jobs}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn job_until(
    client: &Client,
    server: &RunningServer,
    id: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let response = client
            .get(url(server, &format!("/api/jobs/{id}")))
            .send()
            .await
            .expect("GET job");
        let job = body_json(response).await;
        if predicate(&job) {
            return job;
        }
        assert!(Instant::now() < deadline, "timed out waiting for job: {job}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn job_routes_answer_like_node() {
    require_fake_engine();
    let fixture = Fixture::new("jobs-api-routes");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    assert_eq!(
        body_json(
            client
                .get(url(&server, "/api/jobs"))
                .send()
                .await
                .expect("GET jobs"),
        )
        .await,
        json!([])
    );

    for path in ["/api/jobs/nope", "/api/jobs/nope/cancel"] {
        let response = if path.ends_with("cancel") {
            post(&client, &server, path, "").await
        } else {
            client
                .get(url(&server, path))
                .send()
                .await
                .expect("GET missing job")
        };
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await, json!({"error": "Job not found"}));
    }

    let response = client
        .post(url(&server, "/mcp/job/nope"))
        .body("{}")
        .send()
        .await
        .expect("POST missing job MCP without token");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(response).await, json!({"error": "Unauthorized"}));

    let response = client
        .post(url(&server, "/mcp/job/nope"))
        .header("x-learner-token", server.sessions.token())
        .body("{}")
        .send()
        .await
        .expect("POST missing job MCP");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(response).await, json!({"error": "Job not found"}));

    for (method, path) in [
        ("GET", "/mcp/job/nope"),
        ("POST", "/mcp/job/"),
        ("POST", "/mcp/job/a/b"),
    ] {
        let req = if method == "GET" {
            client.get(url(&server, path))
        } else {
            client.post(url(&server, path)).body("{}")
        };
        let response = req
            .header("x-learner-token", server.sessions.token())
            .send()
            .await
            .expect("MCP invalid path request");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await, json!({"error": "Not found"}));
    }

    let response = post(&client, &server, "/api/graph/extract", "null").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Cannot read properties of null (reading 'sources')"})
    );
    let response = post(
        &client,
        &server,
        "/api/graph/extract",
        r#"{"sources":["demo/nope"]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "unknown source: demo/nope"})
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn research_job_runs_through_the_job_endpoint() {
    require_fake_engine();
    let fixture = Fixture::new("jobs-api-research");
    fs::create_dir_all(fixture.repo.0.join(".learner")).expect("create learner dir");
    fs::write(
        fixture.repo.0.join(".learner").join("settings.json"),
        r#"{"autoMap":false}"#,
    )
    .expect("write settings");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let session_id = create_session(&client, &server).await;
    wait_for(&client, &server, &session_id, "idle").await;

    let brief = "Vue 3.5 rende reattivo un ref leggendo .value dentro un effect: la dipendenza viene registrata al momento della lettura. Fonte primaria: la documentazione ufficiale.";
    let input = json!({
        "tool": "submit_research",
        "input": {
            "brief": brief,
            "sources": [{"title": "Vue docs", "url": "https://vuejs.org/guide/essentials/reactivity-fundamentals.html"}]
        }
    });
    let question = format!("Cos'è un ref? {}", job_script(json!([input])));
    post(
        &client,
        &server,
        &format!("/api/sessions/{session_id}/message"),
        &json!({"text": research_message(&question)}).to_string(),
    )
    .await;

    let initial = only_job(&client, &server).await;
    let id = initial["id"].as_str().expect("job id").to_string();
    let done = job_until(&client, &server, &id, |job| {
        job["status"] == "done" && job["delivered"] == true
    })
    .await;
    assert_eq!(done["result"]["kind"], "research");
    assert!(done["costUsd"].as_f64().unwrap_or(0.0) > 0.0);
    assert_eq!(done["activity"], "");
    assert_eq!(done["kind"], "research");

    let listed = body_json(
        client
            .get(url(&server, "/api/jobs"))
            .send()
            .await
            .expect("GET jobs"),
    )
    .await;
    assert!(listed[0].get("result").is_none());
    let saved = fs::read_to_string(
        fixture
            .repo
            .0
            .join(".learner")
            .join("jobs")
            .join(format!("{id}.json")),
    )
    .expect("persisted job");
    assert_eq!(serde_json::from_str::<Value>(&saved).expect("job JSON"), done);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_ends_a_running_job() {
    require_fake_engine();
    let fixture = Fixture::new("jobs-api-cancel");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let session_id = create_session(&client, &server).await;
    wait_for(&client, &server, &session_id, "idle").await;
    post(
        &client,
        &server,
        &format!("/api/sessions/{session_id}/message"),
        &json!({"text": research_message("lento FAKE:slow:300")}).to_string(),
    )
    .await;
    let job = only_job(&client, &server).await;
    let id = job["id"].as_str().expect("job id").to_string();
    job_until(&client, &server, &id, |job| job["status"] == "running").await;

    let response = post(
        &client,
        &server,
        &format!("/api/jobs/{id}/cancel"),
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));
    let cancelled = job_until(&client, &server, &id, |job| job["status"] == "cancelled").await;
    assert!(cancelled.get("error").is_none());
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_ends_the_engine_and_the_browser_page() {
    require_fake_engine();
    let fixture = Fixture::new("jobs-api-browser-cancel");
    let mut options = fixture.options();
    options.browser = Some(proxi::jobs::render::require_test_browser());
    let server = start(options).await.expect("start server");
    let client = Client::new();
    let session_id = create_session(&client, &server).await;
    wait_for(&client, &server, &session_id, "idle").await;
    post(
        &client,
        &server,
        &format!("/api/sessions/{session_id}/message"),
        &json!({"text": research_message("lento FAKE:slow:300")}).to_string(),
    )
    .await;
    let job = only_job(&client, &server).await;
    let id = job["id"].as_str().expect("job id").to_string();
    job_until(&client, &server, &id, |job| job["status"] == "running").await;
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.jobs.live_pids().is_empty() {
        assert!(Instant::now() < deadline, "timed out waiting for job process");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pids = server.jobs.live_pids();
    let renderer = server.jobs.renderer().clone();
    let page_id = id.clone();
    let page = tokio::spawn(async move {
        renderer
            .run_demo_page(&page_id, "<!doctype html><p>x</p>", Duration::from_secs(60))
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.jobs.renderer().open_pages() != 1 {
        assert!(Instant::now() < deadline, "timed out waiting for browser page");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let response = post(
        &client,
        &server,
        &format!("/api/jobs/{id}/cancel"),
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    job_until(&client, &server, &id, |job| job["status"] == "cancelled").await;
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.jobs.renderer().open_pages() != 0
        || !server.jobs.live_pids().is_empty()
        || pids.iter().any(|pid| pid_running(*pid))
    {
        assert!(Instant::now() < deadline, "job process or browser page is still alive");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    page.abort();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_running_jobs() {
    require_fake_engine();
    let fixture = Fixture::new("jobs-api-shutdown");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let session_id = create_session(&client, &server).await;
    wait_for(&client, &server, &session_id, "idle").await;
    post(
        &client,
        &server,
        &format!("/api/sessions/{session_id}/message"),
        &json!({"text": research_message("lento FAKE:slow:300")}).to_string(),
    )
    .await;
    let job = only_job(&client, &server).await;
    let id = job["id"].as_str().expect("job id").to_string();
    job_until(&client, &server, &id, |job| job["status"] == "running").await;
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.jobs.live_pids().is_empty() {
        assert!(Instant::now() < deadline, "timed out waiting for job process");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pids = server.jobs.live_pids();
    server.shutdown().await;

    let file = fixture
        .repo
        .0
        .join(".learner")
        .join("jobs")
        .join(format!("{id}.json"));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let record: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("persisted job")).expect("job JSON");
        if record["status"] == "error" {
            assert_eq!(record["error"], "interrotto alla chiusura dell'app");
            break;
        }
        assert!(Instant::now() < deadline, "job did not settle on shutdown: {record}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while pids.iter().any(|pid| pid_running(*pid)) {
        assert!(Instant::now() < deadline, "job process is still alive after shutdown");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
