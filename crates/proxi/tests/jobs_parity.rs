//! Replays the Node jobs golden against the Rust server (AC1 of #49).

mod common;

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};

use common::session_golden::{
    Roots, assert_same, golden_file, root_variants, write_data_seed, write_seed,
};
use common::sessions::{
    body_json, data_events, fake_engine_options, post, require_fake_engine, wait_for,
};
use proxi::server::{RunningServer, start};

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{path}", server.url)
}

async fn job_until(
    client: &Client,
    server: &RunningServer,
    id: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
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
        assert!(
            Instant::now() < deadline,
            "timed out waiting for job: {job}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn response_body(response: Response) -> Value {
    let text = response.text().await.expect("read response body");
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}

async fn job_token(repo: &std::path::Path, id: &str) -> String {
    let file = repo.join(".learner").join("jobs").join(id).join("mcp.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(raw) = fs::read_to_string(&file) {
            let value: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|error| panic!("parse {} ({error}): {raw}", file.display()));
            if let Some(token) = value["mcpServers"]["job"]["headers"]["x-learner-token"].as_str() {
                return token.to_string();
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            file.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_match_the_node_golden() {
    require_fake_engine();
    let file = golden_file("jobs.json");
    let raw = fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    let golden: Value = serde_json::from_str(&raw).expect("parse jobs golden");

    let dir = common::temp_dir("jobs-parity");
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
    let server = start(fake_engine_options(&repo, &data))
        .await
        .expect("start Rust server");
    let client = Client::new();

    let events = Arc::new(Mutex::new(String::new()));
    let events_buffer = Arc::clone(&events);
    let event_client = client.clone();
    let event_url = format!("{}/api/events", server.url);
    let event_task = tokio::spawn(async move {
        let mut response = event_client
            .get(event_url)
            .send()
            .await
            .expect("GET events");
        assert_eq!(response.status(), StatusCode::OK, "GET events");
        while let Some(chunk) = response.chunk().await.expect("read events") {
            events_buffer
                .lock()
                .expect("lock events")
                .push_str(&String::from_utf8_lossy(&chunk));
        }
    });

    let create_body = &golden["createBody"].to_string();
    let response = post(&client, &server, "/api/sessions", create_body).await;
    assert_eq!(response.status(), StatusCode::OK, "POST /api/sessions");
    let created = body_json(response).await;
    let session_id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no session id in {created}"))
        .to_string();
    wait_for(&client, &server, &session_id, "idle").await;

    let message = &golden["message"];
    let response = post(
        &client,
        &server,
        &format!("/api/sessions/{session_id}/message"),
        &json!({"text": message["text"]}).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "POST message");
    let _ = body_json(response).await;

    let deadline = Instant::now() + Duration::from_secs(30);
    let job_id = loop {
        let response = client
            .get(url(&server, "/api/jobs"))
            .send()
            .await
            .expect("GET jobs");
        let jobs = body_json(response).await;
        if let Some(id) = jobs
            .as_array()
            .and_then(|jobs| (jobs.len() == 1).then(|| jobs[0]["id"].as_str()))
            .flatten()
        {
            break id.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for one job: {jobs}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let job = job_until(&client, &server, &job_id, |job| {
        job["status"] != "running" && job["delivered"] == true
    })
    .await;
    wait_for(&client, &server, &session_id, "idle").await;

    let list_response = client
        .get(url(&server, "/api/jobs"))
        .send()
        .await
        .expect("GET jobs");
    let list = body_json(list_response).await;
    let persisted_file = repo
        .join(".learner")
        .join("jobs")
        .join(format!("{job_id}.json"));
    let persisted_raw = fs::read_to_string(&persisted_file)
        .unwrap_or_else(|error| panic!("read {}: {error}", persisted_file.display()));
    let persisted: Value = serde_json::from_str(&persisted_raw)
        .unwrap_or_else(|error| panic!("parse {} ({error}): {error}", persisted_file.display()));

    assert_same("job", &job, &golden["job"], &roots, &session_id);
    assert_same("list", &list, &golden["list"], &roots, &session_id);
    assert_same(
        "persisted",
        &persisted,
        &golden["persisted"],
        &roots,
        &session_id,
    );

    let event_values = data_events(&events.lock().expect("lock events"));
    let job_events: Vec<&Value> = event_values
        .iter()
        .filter(|event| event["type"] == "job" && event["job"]["id"] == job_id)
        .collect();
    let mut statuses = Vec::new();
    let mut activities = Vec::new();
    let mut events_carry_result = false;
    for event in job_events {
        let status = event["job"]["status"]
            .as_str()
            .expect("job event status")
            .to_string();
        if statuses.last() != Some(&status) {
            statuses.push(status);
        }
        if let Some(activity) = event["job"]["activity"].as_str() {
            if !activity.is_empty() && !activities.contains(&activity.to_string()) {
                activities.push(activity.to_string());
            }
        }
        events_carry_result |= event["job"].get("result").is_some();
    }
    activities.sort();
    assert_eq!(json!(statuses), golden["statuses"], "job event statuses");
    assert_eq!(
        json!(activities),
        golden["activities"],
        "job event activities"
    );
    assert_eq!(
        events_carry_result, golden["eventsCarryResult"],
        "job event result payload"
    );

    let mcp_file = repo
        .join(".learner")
        .join("jobs")
        .join(&job_id)
        .join("mcp.json");
    let mcp_raw = fs::read_to_string(&mcp_file)
        .unwrap_or_else(|error| panic!("read {}: {error}", mcp_file.display()));
    let mcp: Value = serde_json::from_str(&mcp_raw)
        .unwrap_or_else(|error| panic!("parse {} ({error}): {error}", mcp_file.display()));
    let mcp_url = mcp["mcpServers"]["job"]["url"]
        .as_str()
        .expect("job MCP URL")
        .replace(&format!(":{}", server.port), ":<port>");
    assert_same(
        "mcpUrl",
        &Value::String(mcp_url),
        &golden["mcpUrl"],
        &roots,
        &session_id,
    );
    let token = job_token(&repo, &job_id).await;

    for expected in golden["cases"].as_array().expect("golden cases") {
        let name = expected["name"].as_str().expect("case name");
        let request = &expected["request"];
        let path = request["path"]
            .as_str()
            .expect("case path")
            .replace("<job>", &job_id);
        let method = request["method"].as_str().expect("case method");
        let mut builder = match method {
            "GET" => client.get(url(&server, &path)),
            "POST" => client
                .post(url(&server, &path))
                .header(CONTENT_TYPE, "application/json")
                .body(
                    request
                        .get("body")
                        .map(Value::to_string)
                        .unwrap_or_default(),
                ),
            other => panic!("{name}: unsupported method {other}"),
        };
        if request["headers"].get("x-learner-client").is_some() {
            builder = builder.header("x-learner-client", "1");
        }
        if request["headers"].get("x-learner-token").is_some() {
            builder = builder.header("x-learner-token", token.as_str());
        }
        let response = builder
            .send()
            .await
            .unwrap_or_else(|error| panic!("{name}: {method} {path}: {error}"));
        assert_eq!(
            response.status().as_u16(),
            expected["response"]["status"]
                .as_u64()
                .expect("case status") as u16,
            "{name}: status"
        );
        let body = response_body(response).await;
        assert_same(
            name,
            &body,
            &expected["response"]["body"],
            &roots,
            &session_id,
        );
    }

    event_task.abort();
    server.shutdown().await;
}
