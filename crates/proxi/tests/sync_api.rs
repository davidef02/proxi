//! The `/api/sync` HTTP surface of the in-process server: validation, migration and auto-sync (T8).

mod common;

#[path = "common/fake_drive.rs"]
mod fake_drive;
#[path = "common/fake_github.rs"]
mod fake_github;
#[path = "common/fake_http.rs"]
mod fake_http;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};

use fake_drive::{FakeDrive, FakeDriveOpts};
use fake_github::{FakeGithub, FakeGithubOpts};
use fake_http::{FakeHttp, Reply};
use proxi::data_root::resolve_path;
use proxi::server::{RunningServer, ServerOptions, start};

/// Low-entropy on purpose (L12).
fn token() -> String {
    "t".repeat(12)
}

/// A legacy `secrets.json` in the plaintext shape the Node app wrote.
fn legacy_github_secret() -> String {
    let data = json!({
        "github": {
            "token": token(),
            "login": fake_github::LOGIN,
            "repo": fake_github::FULL,
            "branch": "main",
        }
    })
    .to_string();
    json!({ "v": 1, "encrypted": false, "data": data }).to_string()
}

fn write(root: &Path, rel: &str, body: &str) {
    let target = root.join(rel);
    fs::create_dir_all(target.parent().expect("seed file has a parent")).expect("create parents");
    fs::write(target, body).expect("write seed");
}

struct Fixture {
    repo: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(root: &Path) -> Self {
        let repo = root.join("repo");
        let data = root.join("data");
        fs::create_dir_all(repo.join(".learner")).expect("create .learner");
        fs::create_dir_all(&data).expect("create data folder");
        Self { repo, data }
    }

    fn seed(&self, rel: &str, body: &str) {
        write(&self.repo, rel, body);
    }

    fn seed_data(&self, rel: &str, body: &str) {
        write(&self.data, rel, body);
    }

    fn options(&self) -> ServerOptions {
        let mut options = ServerOptions::new(self.repo.clone());
        options.data_root = Some(self.data.clone());
        options.memory_secrets = true;
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

async fn get_status(client: &Client, server: &RunningServer) -> Value {
    let response = client
        .get(url(server, "/api/sync"))
        .send()
        .await
        .expect("GET /api/sync");
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

/// Node's loopback page, escaped like `service.ts` does.
fn page(text: &str) -> String {
    let escaped = text
        .replace('&', "&#38;")
        .replace('<', "&#60;")
        .replace('>', "&#62;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;");
    format!(
        "<!doctype html><html lang=\"it\"><head><meta charset=\"utf-8\"><title>Proxi</title></head><body><h1>Proxi</h1><p>{escaped}</p></body></html>"
    )
}

/// A POST with the guard header and a JSON body, asserted to be a 400 with `want`.
async fn assert_bad_request(
    client: &Client,
    server: &RunningServer,
    path: &str,
    body: &str,
    want: &str,
) {
    let response = post(client, server, path, body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path} {body}");
    assert_eq!(
        body_json(response).await,
        json!({ "error": want }),
        "{path} {body}"
    );
}

/// Polls until the pass started by the callback or the watcher has completed.
async fn wait_for_pass(client: &Client, server: &RunningServer) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status = get_status(client, server).await;
        if (status["state"] != "syncing" && status["at"].is_string())
            || tokio::time::Instant::now() >= deadline
        {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_patches(fake: &FakeGithub, want: usize) -> usize {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let count = fake.count("PATCH", "/git/refs/heads/main");
        if count >= want || tokio::time::Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Polls until the pass that wrote `at`/`pushed` finished (the watcher runs it on another thread).
async fn wait_for_idle(client: &Client, server: &RunningServer) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status = get_status(client, server).await;
        let done = status["state"] == "idle"
            && status["at"].is_string()
            && status["pushed"].as_u64().unwrap_or(0) >= 1;
        if done || tokio::time::Instant::now() >= deadline {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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
async fn get_sync_shape_on_a_fresh_folder() {
    let tmp = common::temp_dir("sync-api-shape");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let status = get_status(&client, &server).await;
    assert_eq!(status["state"], "idle");
    assert_eq!(status["provider"], "local");
    assert_eq!(status["needsData"], true);
    assert_eq!(status["encrypted"], false);
    assert_eq!(status["github"]["connected"], false);
    assert_eq!(status["gdrive"]["connected"], false);
    assert_eq!(status["gdrive"]["pending"], false);
    assert_eq!(status["conflictCount"], 0);
    assert_eq!(status["trashCount"], 0);
    assert!(status.get("restartRequired").is_none(), "{status}");

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn run_needs_the_header_and_bad_bodies_get_the_zod_strings() {
    let tmp = common::temp_dir("sync-api-run");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client
        .post(url(&server, "/api/sync/run"))
        .header(CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .expect("POST without the guard header");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        json!({ "error": "Missing x-learner-client header" })
    );

    // Node's `c.req.json().catch(() => ({}))`: an unparsable body runs with the defaults.
    let response = post(&client, &server, "/api/sync/run", "not json").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["state"], "idle");

    for (body, message) in [
        (
            r#"{"confirmDeletions":"yes"}"#,
            "confirmDeletions: Invalid input: expected boolean, received string",
        ),
        (
            r#"{"confirmDeletions":null}"#,
            "confirmDeletions: Invalid input: expected boolean, received null",
        ),
        ("[]", ": Invalid input: expected object, received array"),
        (
            r#""x""#,
            ": Invalid input: expected object, received string",
        ),
    ] {
        let response = post(&client, &server, "/api/sync/run", body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": message }),
            "{body}"
        );
    }

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_validates_provider() {
    let tmp = common::temp_dir("sync-api-disconnect-args");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    for body in [r#"{}"#, r#"{"provider":"x"}"#, r#"{"provider":5}"#] {
        let response = post(&client, &server, "/api/sync/disconnect", body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": r#"provider: Invalid option: expected one of "github"|"gdrive""# }),
            "{body}"
        );
    }

    let response = post(&client, &server, "/api/sync/disconnect", "null").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({ "error": ": Invalid input: expected object, received null" })
    );

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn get_run_is_404_and_unknown_api_paths_are_404() {
    let tmp = common::temp_dir("sync-api-404");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    for path in ["/api/sync/run", "/api/nope"] {
        let response = client
            .get(url(&server, path))
            .send()
            .await
            .expect("GET request");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": "Not found" }),
            "{path}"
        );
    }

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn plaintext_legacy_file_is_migrated_at_start_and_the_file_is_gone() {
    let tmp = common::temp_dir("sync-api-migrate");
    let fixture = Fixture::new(&tmp.0);
    fixture.seed(".learner/settings.json", r#"{"syncProvider":"local"}"#);
    fixture.seed(".learner/secrets.json", &legacy_github_secret());
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let status = get_status(&client, &server).await;
    assert_eq!(status["github"]["connected"], true);
    assert_eq!(status["github"]["login"], fake_github::LOGIN);
    assert_eq!(status["github"]["repo"], fake_github::FULL);
    assert_eq!(status["provider"], "local");
    assert!(!fixture.repo.join(".learner").join("secrets.json").exists());

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_removes_the_secret() {
    let tmp = common::temp_dir("sync-api-disconnect");
    let fixture = Fixture::new(&tmp.0);
    fixture.seed(".learner/settings.json", r#"{"syncProvider":"local"}"#);
    fixture.seed(".learner/secrets.json", &legacy_github_secret());
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let status = get_status(&client, &server).await;
    assert_eq!(status["github"]["connected"], true);
    assert_eq!(status["provider"], "local");

    let response = post(
        &client,
        &server,
        "/api/sync/disconnect",
        r#"{"provider":"github"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let status = body_json(response).await;
    assert_eq!(status["github"]["connected"], false);
    assert_eq!(status["provider"], "local");

    // Back to github without a secret: run says how to reconnect.
    let response = post(
        &client,
        &server,
        "/api/settings",
        r#"{"syncProvider":"github"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let status = body_json(post(&client, &server, "/api/sync/run", "{}").await).await;
    assert_eq!(status["state"], "error");
    assert_eq!(status["error"], "Non connesso: premi Accedi.");

    server.close();
}

fn fake_github_with_readme() -> FakeGithub {
    FakeGithub::start(FakeGithubOpts {
        seed: vec![("README.md".to_string(), "# proxi-data\n".to_string())],
        ..Default::default()
    })
}

fn seed_connected_github(fixture: &Fixture) {
    fixture.seed(".learner/settings.json", r#"{"syncProvider":"github"}"#);
    fixture.seed(".learner/secrets.json", &legacy_github_secret());
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_sync_runs_a_pass_one_second_after_start_when_github_is_connected() {
    let tmp = common::temp_dir("sync-api-auto");
    let fixture = Fixture::new(&tmp.0);
    seed_connected_github(&fixture);
    fixture.seed_data("courses/algebra/notes.md", "# algebra\n");
    let fake = fake_github_with_readme();
    let mut options = fixture.options();
    options.endpoints.github = Some(fake.http.url.clone());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    // No sync request here: the watcher fires 1 s after start on its own.
    assert_eq!(wait_for_patches(&fake, 1).await, 1);
    let status = wait_for_idle(&client, &server).await;
    assert_eq!(status["state"], "idle", "{status}");
    assert_eq!(status["at"].is_string(), true, "{status}");
    assert!(status["pushed"].as_u64().unwrap_or(0) >= 1, "{status}");
    assert!(
        fake.head_paths()
            .iter()
            .any(|path| path == "courses/algebra/notes.md"),
        "{:?}",
        fake.head_paths()
    );

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_runs_a_last_pass() {
    let tmp = common::temp_dir("sync-api-shutdown");
    let fixture = Fixture::new(&tmp.0);
    seed_connected_github(&fixture);
    fixture.seed_data("courses/algebra/notes.md", "# algebra\n");
    let fake = fake_github_with_readme();
    let mut options = fixture.options();
    options.endpoints.github = Some(fake.http.url.clone());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    assert_eq!(wait_for_patches(&fake, 1).await, 1);
    assert_eq!(wait_for_idle(&client, &server).await["state"], "idle");

    // Written straight to the disk: only shutdown's last pass can see it.
    fixture.seed_data("courses/algebra/extra.md", "# extra\n");
    server.shutdown().await;

    assert_eq!(fake.count("PATCH", "/git/refs/heads/main"), 2);
    assert!(
        fake.head_paths()
            .iter()
            .any(|path| path == "courses/algebra/extra.md"),
        "{:?}",
        fake.head_paths()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_with_a_local_provider_touches_no_remote() {
    let tmp = common::temp_dir("sync-api-shutdown-local");
    let fixture = Fixture::new(&tmp.0);
    fixture.seed(".learner/settings.json", r#"{"syncProvider":"local"}"#);
    fixture.seed(".learner/secrets.json", &legacy_github_secret());
    fixture.seed_data("courses/algebra/notes.md", "# algebra\n");
    let fake = fake_github_with_readme();
    let mut options = fixture.options();
    options.endpoints.github = Some(fake.http.url.clone());
    let server = start(options).await.expect("start server");

    server.shutdown().await;

    assert!(
        fake.http.requests().is_empty(),
        "{:?}",
        fake.http.requests()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn github_start_and_cancel_against_the_fake() {
    let tmp = common::temp_dir("sync-api-device");
    let fixture = Fixture::new(&tmp.0);
    fixture.seed(
        ".learner/settings.json",
        r#"{"githubClientId":"gh-client"}"#,
    );
    let http = FakeHttp::start(|request| {
        if request.method == "POST" && request.path == "/login/device/code" {
            Reply::json(
                200,
                json!({
                    "device_code": "dev-1",
                    "user_code": "WXYZ-1234",
                    "verification_uri": "https://github.com/login/device",
                    "expires_in": 900,
                    "interval": 1,
                }),
            )
        } else {
            Reply::json(200, json!({ "error": "authorization_pending" }))
        }
    });
    let mut options = fixture.options();
    options.endpoints.github = Some(http.url.clone());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/sync/github/start", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let status = body_json(response).await;
    let pending = &status["github"]["pending"];
    assert_eq!(pending["userCode"], "WXYZ-1234");
    assert_eq!(
        pending["verificationUri"],
        "https://github.com/login/device"
    );
    let expires_at = pending["expiresAt"].as_str().expect("expiresAt string");
    assert_eq!(expires_at.len(), 24, "{expires_at}");
    assert!(expires_at.ends_with('Z'), "{expires_at}");

    let requests = http.requests();
    let device = requests
        .iter()
        .find(|request| request.method == "POST" && request.path == "/login/device/code")
        .expect("device-code request");
    assert_eq!(
        device.json(),
        json!({ "client_id": "gh-client", "scope": "repo" })
    );

    let response = post(&client, &server, "/api/sync/github/cancel", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let status = body_json(response).await;
    assert!(status["github"]["pending"].is_null(), "{status}");
    assert!(status["github"]["error"].is_null(), "{status}");

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn github_cancel_emits_a_sync_event_on_the_events_stream() {
    let tmp = common::temp_dir("sync-api-cancel-event");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .expect("GET /api/events");
    assert_eq!(stream.status(), StatusCode::OK);

    let response = post(&client, &server, "/api/sync/github/cancel", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);

    let raw = read_until(&mut stream, Duration::from_secs(5), |raw| {
        data_events(raw).iter().any(|event| event["type"] == "sync")
    })
    .await;
    let event = data_events(&raw)
        .into_iter()
        .find(|event| event["type"] == "sync")
        .expect("sync event");
    assert_eq!(event["status"]["provider"], "local");
    assert_eq!(event["status"]["state"], "idle");

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn start_without_memory_secrets_and_no_legacy_file_never_touches_the_keychain() {
    let tmp = common::temp_dir("sync-api-keychain");
    let fixture = Fixture::new(&tmp.0);
    let mut options = ServerOptions::new(fixture.repo.clone());
    options.data_root = Some(fixture.data.clone());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = client
        .get(url(&server, "/api/settings"))
        .send()
        .await
        .expect("GET /api/settings");
    assert_eq!(response.status(), StatusCode::OK);
    let settings = body_json(response).await;
    assert_eq!(
        settings["dataRoot"],
        proxi::data_root::resolve_path(&fixture.data)
            .display()
            .to_string()
    );

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_google_callback_answers_html_even_without_a_built_ui() {
    let tmp = common::temp_dir("sync-api-google-page");
    let fixture = Fixture::new(&tmp.0);
    let dist = tmp.0.join("empty-dist");
    fs::create_dir_all(&dist).expect("create empty dist");
    let mut options = fixture.options();
    options.dist_dir = Some(dist);
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = client
        .get(url(&server, "/oauth/google/callback?state=x"))
        .send()
        .await
        .expect("GET callback");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=UTF-8")
    );
    assert_eq!(
        response.text().await.expect("callback body"),
        page("Richiesta di accesso non valida o scaduta: riprova da Proxi.")
    );

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn google_start_then_callback_connects_against_the_fake_drive() {
    let tmp = common::temp_dir("sync-api-google");
    let fixture = Fixture::new(&tmp.0);
    let drive = FakeDrive::start(FakeDriveOpts::default());
    let mut options = fixture.options();
    options.endpoints.google = Some(drive.http.url.clone());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/sync/google/start", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let auth_url = body["url"].as_str().expect("url string");
    assert!(
        auth_url.starts_with(&format!("{}/o/oauth2/v2/auth?client_id=", drive.http.url)),
        "{auth_url}"
    );
    let query = auth_url.split_once('?').expect("query string").1;
    let params: std::collections::BTreeMap<String, String> =
        form_urlencoded::parse(query.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
    let redirect = format!("http://127.0.0.1:{}/oauth/google/callback", server.port);
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some(redirect.as_str())
    );
    let state = params.get("state").expect("state").clone();

    let callback = client
        .get(url(
            &server,
            &format!("/oauth/google/callback?code=abc&state={state}"),
        ))
        .send()
        .await
        .expect("GET callback");
    assert_eq!(callback.status(), StatusCode::OK);
    assert_eq!(
        callback.text().await.expect("callback body"),
        page("Accesso a Google completato. Puoi chiudere questa scheda e tornare a Proxi.")
    );

    let status = get_status(&client, &server).await;
    assert_eq!(status["gdrive"]["connected"], true, "{status}");
    assert_eq!(status["gdrive"]["email"], "davide@example.com", "{status}");
    assert_eq!(status["provider"], "gdrive", "{status}");
    assert_eq!(wait_for_pass(&client, &server).await["state"], "idle");

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_google_callback_answers_the_cancelled_page() {
    let tmp = common::temp_dir("sync-api-google-denied");
    let fixture = Fixture::new(&tmp.0);
    let drive = FakeDrive::start(FakeDriveOpts::default());
    let mut options = fixture.options();
    options.endpoints.google = Some(drive.http.url.clone());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let body = body_json(post(&client, &server, "/api/sync/google/start", "{}").await).await;
    let auth_url = body["url"].as_str().expect("url string");
    let state = auth_url
        .split_once('?')
        .expect("query string")
        .1
        .split('&')
        .find_map(|pair| pair.strip_prefix("state="))
        .expect("state");

    let response = client
        .get(url(
            &server,
            &format!("/oauth/google/callback?error=access_denied&code=abc&state={state}"),
        ))
        .send()
        .await
        .expect("GET callback");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.text().await.expect("callback body"),
        page("Accesso a Google annullato.")
    );
    assert!(
        drive
            .requests()
            .iter()
            .all(|request| request.path != "/token"),
        "{:?}",
        drive.requests()
    );
    assert_eq!(
        get_status(&client, &server).await["gdrive"]["connected"],
        false
    );

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_and_restore_bodies_get_the_zod_strings() {
    let tmp = common::temp_dir("sync-api-zod");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let long = "x".repeat(1001);
    let emoji = "😀".repeat(501);
    let resolve_cases = [
        (
            "{}".to_string(),
            "id: Invalid input: expected string, received undefined; choice: Invalid option: expected one of \"local\"|\"remote\"|\"both\"",
        ),
        (
            r#"{"id":"","choice":"mine"}"#.to_string(),
            "id: Too small: expected string to have >=1 characters; choice: Invalid option: expected one of \"local\"|\"remote\"|\"both\"",
        ),
        (
            r#"{"id":5,"choice":"local"}"#.to_string(),
            "id: Invalid input: expected string, received number",
        ),
        (
            r#"{"id":"x"}"#.to_string(),
            "choice: Invalid option: expected one of \"local\"|\"remote\"|\"both\"",
        ),
        (
            r#"{"id":true,"choice":true}"#.to_string(),
            "id: Invalid input: expected string, received boolean; choice: Invalid option: expected one of \"local\"|\"remote\"|\"both\"",
        ),
        (
            format!(r#"{{"id":"{long}","choice":"local"}}"#),
            "id: Too big: expected string to have <=1000 characters",
        ),
        (
            format!(r#"{{"id":"{emoji}","choice":"local"}}"#),
            "id: Too big: expected string to have <=1000 characters",
        ),
        (
            "[]".to_string(),
            ": Invalid input: expected object, received array",
        ),
    ];
    for (body, want) in resolve_cases {
        assert_bad_request(&client, &server, "/api/sync/conflicts/resolve", &body, want).await;
    }

    // A single emoji is 2 UTF-16 units, so validation lets it through to the store's own refusal.
    assert_bad_request(
        &client,
        &server,
        "/api/sync/conflicts/resolve",
        r#"{"id":"😀","choice":"both"}"#,
        "Conflitto non valido.",
    )
    .await;

    let restore_cases = [
        (
            "{}".to_string(),
            "id: Invalid input: expected string, received undefined",
        ),
        (
            r#"{"id":""}"#.to_string(),
            "id: Too small: expected string to have >=1 characters",
        ),
        (
            r#"{"id":5}"#.to_string(),
            "id: Invalid input: expected string, received number",
        ),
        (
            format!(r#"{{"id":"{long}"}}"#),
            "id: Too big: expected string to have <=1000 characters",
        ),
        (
            format!(r#"{{"id":"{emoji}"}}"#),
            "id: Too big: expected string to have <=1000 characters",
        ),
        (
            "null".to_string(),
            ": Invalid input: expected object, received null",
        ),
    ];
    for (body, want) in restore_cases {
        assert_bad_request(&client, &server, "/api/sync/trash/restore", &body, want).await;
    }

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn data_root_bodies_get_the_zod_and_italian_strings() {
    let tmp = common::temp_dir("sync-api-data-root-errors");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let config = fixture.repo.join(".learner").join("config.json");

    let long = "x".repeat(501);
    let emoji = "😀".repeat(251);
    let cases = [
        (
            "{}".to_string(),
            "dataRoot: Invalid input: expected string, received undefined",
        ),
        (
            r#"{"dataRoot":5}"#.to_string(),
            "dataRoot: Invalid input: expected string, received number",
        ),
        (
            r#"{"dataRoot":null}"#.to_string(),
            "dataRoot: Invalid input: expected string, received null",
        ),
        (
            r#"{"dataRoot":""}"#.to_string(),
            "dataRoot: Too small: expected string to have >=1 characters",
        ),
        (
            format!(r#"{{"dataRoot":"{long}"}}"#),
            "dataRoot: Too big: expected string to have <=500 characters",
        ),
        (
            format!(r#"{{"dataRoot":"{emoji}"}}"#),
            "dataRoot: Too big: expected string to have <=500 characters",
        ),
    ];
    for (body, want) in cases {
        assert_bad_request(&client, &server, "/api/sync/data-root", &body, want).await;
    }

    assert_bad_request(
        &client,
        &server,
        "/api/sync/data-root",
        r#"{"dataRoot":"relative/dir"}"#,
        "La cartella dei dati deve essere un percorso assoluto.",
    )
    .await;

    let inside = fixture.repo.join("inside");
    assert_bad_request(
        &client,
        &server,
        "/api/sync/data-root",
        &json!({ "dataRoot": inside.display().to_string() }).to_string(),
        "La cartella dei dati non può stare dentro il codice di Proxi (né contenerlo).",
    )
    .await;

    // 500 UTF-16 units pass validation: 250 emoji are 500 units, 251 are 502.
    assert_bad_request(
        &client,
        &server,
        "/api/sync/data-root",
        &json!({ "dataRoot": "😀".repeat(250) }).to_string(),
        "La cartella dei dati deve essere un percorso assoluto.",
    )
    .await;

    assert_eq!(config.exists(), false);

    let current = resolve_path(&fixture.data).display().to_string();
    let response = post(
        &client,
        &server,
        "/api/sync/data-root",
        &json!({ "dataRoot": current }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await.get("restartRequired"), None);
    assert_eq!(config.exists(), false);

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicts_and_trash_lists_serialise_in_node_key_order() {
    let tmp = common::temp_dir("sync-api-lists");
    let fixture = Fixture::new(&tmp.0);
    let conflict = "courses/demo/NOTES.md.remoto-1700000000000";
    let trashed = "1700000000000/courses/demo/gone.md";
    write(
        &fixture.repo,
        &format!(".learner/drive-conflicts/{conflict}"),
        "remote notes",
    );
    write(&fixture.repo, &format!(".learner/trash/{trashed}"), "gone");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client
        .get(url(&server, "/api/sync/conflicts"))
        .send()
        .await
        .expect("GET conflicts");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.text().await.expect("conflicts body"),
        format!(
            r#"[{{"id":"{conflict}","rel":"courses/demo/NOTES.md","side":"remoto","at":"2023-11-14T22:13:20.000Z"}}]"#
        )
    );

    let response = client
        .get(url(&server, "/api/sync/trash"))
        .send()
        .await
        .expect("GET trash");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.text().await.expect("trash body"),
        format!(
            r#"[{{"id":"{trashed}","rel":"courses/demo/gone.md","deletedAt":"2023-11-14T22:13:20.000Z","size":4}}]"#
        )
    );

    let status = get_status(&client, &server).await;
    assert_eq!(status["conflictCount"], 1);
    assert_eq!(status["trashCount"], 1);

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_and_restore_answer_ok_true() {
    let tmp = common::temp_dir("sync-api-resolve");
    let fixture = Fixture::new(&tmp.0);
    let conflict = "courses/demo/NOTES.md.remoto-1700000000000";
    let trashed = "1700000000000/courses/demo/gone.md";
    write(
        &fixture.repo,
        &format!(".learner/drive-conflicts/{conflict}"),
        "remote notes",
    );
    write(&fixture.repo, &format!(".learner/trash/{trashed}"), "gone");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = post(
        &client,
        &server,
        "/api/sync/conflicts/resolve",
        &json!({ "id": conflict, "choice": "both" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({ "ok": true }));
    assert_eq!(
        fs::read_to_string(fixture.data.join("conflitti").join(conflict)).expect("both copy"),
        "remote notes"
    );

    let response = post(
        &client,
        &server,
        "/api/sync/trash/restore",
        &json!({ "id": trashed }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({ "ok": true }));
    assert_eq!(
        fs::read_to_string(fixture.data.join("courses/demo/gone.md")).expect("restored file"),
        "gone"
    );

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_new_post_routes_need_the_x_learner_client_header() {
    let tmp = common::temp_dir("sync-api-new-guard");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    for path in [
        "/api/sync/google/start",
        "/api/sync/data-root",
        "/api/sync/conflicts/resolve",
        "/api/sync/trash/restore",
    ] {
        let response = client
            .post(url(&server, path))
            .header(CONTENT_TYPE, "application/json")
            .body("{}")
            .send()
            .await
            .expect("POST without the guard header");
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": "Missing x-learner-client header" }),
            "{path}"
        );
    }

    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn get_on_a_post_route_is_404() {
    let tmp = common::temp_dir("sync-api-new-404");
    let fixture = Fixture::new(&tmp.0);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    for path in [
        "/api/sync/google/start",
        "/api/sync/data-root",
        "/api/sync/conflicts/resolve",
        "/api/sync/trash/restore",
    ] {
        let response = client
            .get(url(&server, path))
            .send()
            .await
            .expect("GET request");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": "Not found" }),
            "{path}"
        );
    }

    // The same fallback on the GET routes: a POST is a 404 too.
    for path in ["/api/sync/conflicts", "/api/sync/trash"] {
        let response = post(&client, &server, path, "{}").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": "Not found" }),
            "{path}"
        );
    }

    server.close();
}
