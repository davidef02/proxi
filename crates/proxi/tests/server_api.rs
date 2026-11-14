mod common;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HOST};
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};

use proxi::server::{start, RunningServer, ServerOptions};

const FIXTURE_INDEX: &str = "<!doctype html><title>fixture</title>";
const MISSING_UI: &str =
    "Proxi: la UI non è stata compilata. Esegui `npm run app:build` e riapri questa pagina.";

/// A temp repo and data folder plus a built-looking dist with a secret just outside it.
struct Fixture {
    root: common::TempDir,
    repo: PathBuf,
    data: PathBuf,
    dist: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = common::temp_dir(tag);
        let repo = root.0.join("repo");
        let data = root.0.join("data");
        let dist = root.0.join("dist");
        fs::create_dir_all(&repo).expect("create repo");
        fs::create_dir_all(&data).expect("create data");
        fs::create_dir_all(dist.join("assets")).expect("create dist");
        fs::write(dist.join("index.html"), FIXTURE_INDEX).expect("write index");
        fs::write(dist.join("assets").join("app.js"), "console.log(1)").expect("write asset");
        fs::write(root.0.join("secret.txt"), "TOP-SECRET").expect("write secret");
        Self {
            root,
            repo,
            data,
            dist,
        }
    }

    fn options(&self) -> ServerOptions {
        let mut options = ServerOptions::new(self.repo.clone());
        options.data_root = Some(self.data.clone());
        options.dist_dir = Some(self.dist.clone());
        options
    }
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

#[tokio::test(flavor = "multi_thread")]
async fn guards_match_the_node_server() {
    let fixture = Fixture::new("api-guards");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client
        .get(url(&server, "/api/settings"))
        .header(HOST, format!("evil.example:{}", server.port))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Forbidden host"})
    );

    let response = client
        .get(url(&server, "/api/settings"))
        .header(HOST, format!("localhost:{}", server.port))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = client
        .post(url(&server, "/api/settings"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Missing x-learner-client header"})
    );

    let response = client
        .post(url(&server, "/api/nope"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Missing x-learner-client header"})
    );

    let response = post(&client, &server, "/api/nope", "{}").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(response).await, json!({"error": "Not found"}));

    for path in ["/api/nope", "/api/"] {
        let response = client.get(url(&server, path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({"error": "Not found"}),
            "{path}"
        );
    }

    let response = client
        .put(url(&server, "/api/settings"))
        .header("x-learner-client", "1")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(response).await, json!({"error": "Not found"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_round_trip_masks_the_api_key() {
    let fixture = Fixture::new("api-settings");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client
        .get(url(&server, "/api/settings"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let defaults = body_json(response).await;
    assert_eq!(defaults["provider"], "claude");
    assert_eq!(defaults["anthropicApiKey"], "");
    assert_eq!(
        defaults["repoRoot"],
        proxi::data_root::resolve_path(&fixture.repo)
            .display()
            .to_string()
    );
    assert_eq!(
        defaults["dataRoot"],
        proxi::data_root::resolve_path(&fixture.data)
            .display()
            .to_string()
    );

    let key = common::fake_key("-ant-1234567890abcd");
    let response = post(
        &client,
        &server,
        "/api/settings",
        &format!(r#"{{"anthropicApiKey":"{key}"}}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["anthropicApiKey"],
        common::fake_key("-ant-…abcd")
    );

    let raw = fs::read_to_string(fixture.repo.join(".learner").join("settings.json"))
        .expect("read settings file");
    assert!(
        raw.contains(&format!("\"anthropicApiKey\": \"{key}\"")),
        "{raw}"
    );

    let response = post(&client, &server, "/api/settings", "{").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({"error": "Invalid JSON body"})
    );

    let response = post(&client, &server, "/api/settings", r#"{"nope":1}"#).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(body["error"].is_string(), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_use_the_node_cache() {
    let fixture = Fixture::new("api-limits");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client
        .get(url(&server, "/api/limits"))
        .send()
        .await
        .unwrap();
    assert_eq!(body_json(response).await, Value::Null);

    let first = r#"{"fiveHour":{"utilization":0.42,"resetsAt":1700000000},"at":"2026-11-12T10:00:00.000Z"}"#;
    let limits = fixture.repo.join(".learner").join("limits.json");
    fs::create_dir_all(limits.parent().unwrap()).expect("create .learner");
    fs::write(&limits, first).expect("write limits");

    let response = client
        .get(url(&server, "/api/limits"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        body_json(response).await,
        serde_json::from_str::<Value>(first).unwrap()
    );

    // Mirror of Node's in-memory cache: the second edit is invisible until restart.
    fs::write(&limits, r#"{"at":"2026-11-12T11:00:00.000Z"}"#).expect("overwrite limits");
    let response = client
        .get(url(&server, "/api/limits"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        body_json(response).await,
        serde_json::from_str::<Value>(first).unwrap()
    );

    let response = post(&client, &server, "/api/settings", r#"{"provider":"agy"}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = client
        .get(url(&server, "/api/limits"))
        .send()
        .await
        .unwrap();
    assert_eq!(body_json(response).await, Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_event_arrives_with_a_masked_key() {
    let fixture = Fixture::new("api-events-settings");
    let mut options = fixture.options();
    options.keep_alive = Duration::from_secs(120); // keep the stream quiet for the test
    let server = start(options).await.expect("start server");
    let client = Client::new();

    // Persist a real key first: the event must carry the mask, never the secret.
    let response = post(
        &client,
        &server,
        "/api/settings",
        &format!(
            r#"{{"anthropicApiKey":"{}"}}"#,
            common::fake_key("-ant-1234567890abcd")
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let content_type = stream
        .headers()
        .get(CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .expect("ascii content-type")
        .to_string();
    assert!(
        content_type.starts_with("text/event-stream"),
        "{content_type}"
    );

    let response = post(&client, &server, "/api/settings", r#"{"autoMap":false}"#).await;
    assert_eq!(response.status(), StatusCode::OK);

    let raw = read_until(&mut stream, Duration::from_secs(5), |raw| {
        data_events(raw).iter().any(|event| event["type"] == "settings")
    })
    .await;
    let events = data_events(&raw);
    let settings = events
        .iter()
        .find(|event| event["type"] == "settings")
        .expect("settings event");
    assert_eq!(settings["settings"]["autoMap"], false);
    assert_eq!(
        settings["settings"]["anthropicApiKey"],
        common::fake_key("-ant-…abcd")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_event_precedes_settings_when_limits_turn_off() {
    let fixture = Fixture::new("api-events-order");
    let mut options = fixture.options();
    options.keep_alive = Duration::from_secs(120);
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);

    let response = post(&client, &server, "/api/settings", r#"{"provider":"opencode"}"#).await;
    assert_eq!(response.status(), StatusCode::OK);

    let raw = read_until(&mut stream, Duration::from_secs(5), |raw| {
        data_events(raw).len() >= 2
    })
    .await;
    let events = data_events(&raw);
    assert_eq!(events[0]["type"], "limits");
    let at = events[0]["limits"]["at"].as_str().expect("at string");
    assert_eq!(at.len(), 24, "{at}");
    assert_eq!(events[1]["type"], "settings");
    assert_eq!(events[1]["settings"]["provider"], "opencode");
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_alive_can_be_shortened_for_tests() {
    let fixture = Fixture::new("api-events-fast");
    let mut options = fixture.options();
    options.keep_alive = Duration::from_millis(200);
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .unwrap();
    let raw = read_until(&mut stream, Duration::from_secs(2), |raw| {
        raw.contains(": keep-alive\n\n")
    })
    .await;
    assert!(raw.contains(": keep-alive\n\n"), "{raw:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn default_keep_alive_comes_after_fifteen_seconds() {
    let fixture = Fixture::new("api-events-default");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let started = tokio::time::Instant::now();

    let raw = read_until(&mut stream, Duration::from_secs(17), |raw| {
        raw.contains(": keep-alive\n\n")
    })
    .await;
    let elapsed = started.elapsed();
    assert_eq!(raw, ": keep-alive\n\n");
    assert!(elapsed >= Duration::from_secs(14), "{elapsed:?}");
    assert!(elapsed <= Duration::from_secs(17), "{elapsed:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn static_ui_is_served_with_spa_fallback() {
    let fixture = Fixture::new("api-static");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    for path in ["/", "/corsi/demo/lezione"] {
        let response = client.get(url(&server, path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.text().await.unwrap(), FIXTURE_INDEX, "{path}");
    }

    let response = client
        .get(url(&server, "/assets/app.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .expect("ascii content-type")
        .to_string();
    assert!(content_type.contains("javascript"), "{content_type}");
    assert_eq!(response.text().await.unwrap(), "console.log(1)");

    let response = client.head(url(&server, "/")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = post(&client, &server, "/corsi", "{}").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(response).await, json!({"error": "Not found"}));

    for path in ["/..%2fsecret.txt", "/assets/..%2f..%2fsecret.txt"] {
        let response = client.get(url(&server, path)).send().await.unwrap();
        let body = response.bytes().await.unwrap();
        assert!(
            !String::from_utf8_lossy(&body).contains("TOP-SECRET"),
            "{path}: {}",
            String::from_utf8_lossy(&body)
        );
    }

    let empty = fixture.root.0.join("empty-dist");
    fs::create_dir_all(&empty).expect("create empty dist");
    let mut options = fixture.options();
    options.dist_dir = Some(empty);
    let server = start(options).await.expect("start missing-build server");
    let response = client.get(url(&server, "/x")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), MISSING_UI);
}

#[tokio::test(flavor = "multi_thread")]
async fn real_build_serves_the_built_spa() {
    let index = common::repo_root()
        .join("app")
        .join("web")
        .join("dist")
        .join("index.html");
    assert!(index.is_file(), "run npm run app:build first");
    let expected = fs::read(&index).expect("read built index");

    let data = common::temp_dir("api-real-data");
    let mut options = ServerOptions::new(common::repo_root());
    options.data_root = Some(data.0.clone());
    options.fetch_model_on_start = Some(false);
    let server = start(options).await.expect("start server");
    let client = Client::new();

    for path in ["/", "/qualsiasi/percorso"] {
        let response = client.get(url(&server, path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body = response.bytes().await.unwrap();
        assert_eq!(body.as_ref(), expected.as_slice(), "{path}");
    }
}

/// The `serve` CLI must exit 1 on a port Node's `listen` also rejects (main.ts:628,549-560).
#[test]
fn serve_cli_exits_1_on_an_invalid_port() {
    let output = std::process::Command::new(common::proxi_bin())
        .arg("serve")
        .env("LEARNER_PORT", "70000")
        .output()
        .expect("run proxi serve");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LEARNER_PORT non valido: 70000"),
        "{stderr}"
    );
}
