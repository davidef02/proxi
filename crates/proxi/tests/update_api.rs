//! HTTP tests for the update routes against the in-process server.
//!
//! Fixtures are signed-release manifests served by a local axum server (AC1-AC5).

mod common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode as AxumStatusCode;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::Router;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use proxi::server::{start, RunningServer, ServerOptions};
use proxi::update;

const TEST_PUBKEY: &str =
    "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n";

const VALID_PREHASHED_SIG: &str = "untrusted comment: signature from minisign secret key\n\
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\n\
trusted comment: timestamp:1556193335\tfile:test\n\
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==\n";

fn valid_sig_b64() -> String {
    STANDARD.encode(VALID_PREHASHED_SIG.as_bytes())
}

fn flipped_sig_b64() -> String {
    let target = "RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=";
    let mut flipped = target.to_string();
    flipped.replace_range(10..11, "A");
    let flipped_sig_str = VALID_PREHASHED_SIG.replace(target, &flipped);
    STANDARD.encode(flipped_sig_str.as_bytes())
}

fn release_json(version: &str, notes: &str, installer_url: &str, sig_b64: &str) -> Value {
    json!({
        "version": version,
        "notes": notes,
        "pub_date": "2026-11-22T00:00:00Z",
        "platforms": {
            "windows-x86_64": {
                "signature": sig_b64,
                "url": installer_url
            }
        }
    })
}

struct FakeReleasesState {
    latest_json: Option<Value>,
    installer_bytes: Vec<u8>,
    requests: Vec<String>,
    delay: Option<Duration>,
}

struct FakeReleases {
    pub url: String,
    state: Arc<Mutex<FakeReleasesState>>,
    task: JoinHandle<()>,
}

impl FakeReleases {
    pub async fn start(latest_json: Option<Value>, installer_bytes: Vec<u8>) -> Self {
        Self::start_with_delay(latest_json, installer_bytes, None).await
    }

    pub async fn start_with_delay(
        latest_json: Option<Value>,
        installer_bytes: Vec<u8>,
        delay: Option<Duration>,
    ) -> Self {
        let state = Arc::new(Mutex::new(FakeReleasesState {
            latest_json,
            installer_bytes,
            requests: Vec::new(),
            delay,
        }));
        let app = Router::new()
            .fallback(handle_fake_releases)
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake releases");
        let addr = listener.local_addr().expect("fake releases addr");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            url: format!("http://{addr}"),
            state,
            task,
        }
    }

    pub fn set_latest_json(&self, val: Option<Value>) {
        self.state.lock().expect("fake lock").latest_json = val;
    }

    pub fn set_installer_bytes(&self, bytes: Vec<u8>) {
        self.state.lock().expect("fake lock").installer_bytes = bytes;
    }

    pub fn requests(&self) -> Vec<String> {
        self.state.lock().expect("fake lock").requests.clone()
    }
}

impl Drop for FakeReleases {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_fake_releases(
    State(state): State<Arc<Mutex<FakeReleasesState>>>,
    uri: axum::http::Uri,
) -> AxumResponse {
    let path = uri.path().to_string();
    let (latest_json, installer_bytes, delay) = {
        let mut guard = state.lock().expect("fake lock");
        guard.requests.push(path.clone());
        (
            guard.latest_json.clone(),
            guard.installer_bytes.clone(),
            guard.delay,
        )
    };

    if let Some(d) = delay {
        tokio::time::sleep(d).await;
    }

    if path == "/latest.json" {
        match latest_json {
            Some(val) => (AxumStatusCode::OK, axum::Json(val)).into_response(),
            None => AxumStatusCode::NOT_FOUND.into_response(),
        }
    } else if path == "/proxi-setup.exe" || path.ends_with(".exe") {
        (AxumStatusCode::OK, installer_bytes).into_response()
    } else {
        AxumStatusCode::NOT_FOUND.into_response()
    }
}

fn options(repo: &Path) -> ServerOptions {
    let mut options = ServerOptions::new(repo.to_path_buf());
    options.data_root = Some(repo.parent().expect("repo parent").join("data"));
    options
}

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{}", server.url, path)
}

async fn get(client: &Client, server: &RunningServer, path: &str) -> Response {
    client
        .get(url(server, path))
        .send()
        .await
        .expect("GET request")
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

async fn body_text(response: Response) -> String {
    response.text().await.expect("read body")
}

async fn body_json(response: Response) -> Value {
    let text = body_text(response).await;
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("object body")
        .keys()
        .map(String::as_str)
        .collect()
}

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

fn update_states(raw: &str) -> Vec<String> {
    data_events(raw)
        .iter()
        .filter(|event| event["type"] == "update")
        .filter_map(|event| event["status"]["state"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn get_update_starts_idle() {
    let root = common::temp_dir("update-api-idle");
    let repo = root.0.join("repo");
    fs::create_dir_all(&repo).expect("create repo");
    let server = start(options(&repo)).await.expect("start server");
    let client = Client::new();

    let response = get(&client, &server, "/api/update").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_text(response).await,
        r#"{"state":"idle","activeSessions":0}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn check_reports_available_with_version_and_notes() {
    let sig_b64 = valid_sig_b64();
    let fake = FakeReleases::start(
        Some(release_json("0.1.0", "Release 0.1.0", "proxi-setup.exe", &sig_b64)),
        b"test".to_vec(),
    )
    .await;

    let root = common::temp_dir("update-api-check");
    let repo = root.0.join("repo");
    fs::create_dir_all(&repo).expect("create repo");

    let mut options = options(&repo);
    options.keep_alive = Duration::from_secs(120);
    options.endpoints.releases = Some(format!("{}/latest.json", fake.url));
    options.update_pubkey = Some(TEST_PUBKEY.to_string());
    let server = start(options).await.expect("start server");
    let client = Client::new();
    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .expect("open events stream");
    assert_eq!(stream.status(), StatusCode::OK);

    // Initial check: 0.1.0 is current version, so up_to_date
    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "up_to_date");
    assert_eq!(keys(&body), ["state", "checkedAt", "activeSessions"]);
    let checked_at = body["checkedAt"].as_str().expect("checkedAt string");
    assert_eq!(checked_at.len(), 24, "{checked_at}");
    assert!(checked_at.ends_with('Z'), "{checked_at}");

    // Release 9.9.9 is newer, so available with version and notes
    fake.set_latest_json(Some(release_json(
        "9.9.9",
        "Nuova versione disponibile con correzioni.",
        "proxi-setup.exe",
        &sig_b64,
    )));

    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "available");
    assert_eq!(body["version"], "9.9.9");
    assert_eq!(
        body["notes"],
        "Nuova versione disponibile con correzioni."
    );
    assert_eq!(
        keys(&body),
        ["state", "version", "notes", "checkedAt", "activeSessions"]
    );

    let raw = read_until(&mut stream, Duration::from_secs(5), |raw| {
        update_states(raw).len() >= 2
    })
    .await;
    let states: Vec<String> = update_states(&raw);
    assert_eq!(
        states.iter().map(String::as_str).collect::<Vec<_>>(),
        ["up_to_date", "available"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn check_survives_an_unreachable_releases_url() {
    let root = common::temp_dir("update-api-unreachable");
    let repo = root.0.join("repo");
    fs::create_dir_all(&repo).expect("create repo");

    let mut options = options(&repo);
    options.endpoints.releases = Some("http://127.0.0.1:1/latest.json".to_string());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "idle");
}

#[tokio::test(flavor = "multi_thread")]
async fn check_is_single_flight() {
    let sig_b64 = valid_sig_b64();
    let fake = FakeReleases::start_with_delay(
        Some(release_json("9.9.9", "notes", "proxi-setup.exe", &sig_b64)),
        b"test".to_vec(),
        Some(Duration::from_millis(300)),
    )
    .await;

    let root = common::temp_dir("update-api-single-flight");
    let repo = root.0.join("repo");
    fs::create_dir_all(&repo).expect("create repo");

    let mut options = options(&repo);
    options.endpoints.releases = Some(format!("{}/latest.json", fake.url));
    options.update_pubkey = Some(TEST_PUBKEY.to_string());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let (first, second) = tokio::join!(
        post(&client, &server, "/api/update/check", "{}"),
        post(&client, &server, "/api/update/check", "{}")
    );

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    let first = body_text(first).await;
    let second = body_text(second).await;
    assert_eq!(first, second);
    let body: Value = serde_json::from_str(&first).expect("JSON body");
    assert_eq!(body["state"], "available");

    let requests = fake.requests();
    let latest_requests: Vec<&String> = requests
        .iter()
        .filter(|r| r.as_str() == "/latest.json")
        .collect();
    assert_eq!(latest_requests.len(), 1, "requests: {requests:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn check_reports_available_when_pending_marker_present() {
    let root = common::temp_dir("update-api-pending-marker");
    let repo = root.0.join("repo");
    fs::create_dir_all(&repo).expect("create repo");

    let pending = update::PendingUpdate {
        installer: "C:/path/to/fake-setup.exe".to_string(),
        version: "9.9.9".to_string(),
    };
    update::write_pending(&repo, &pending).expect("write pending");

    let mut options = options(&repo);
    // Even with an unreachable releases URL, the pending marker makes check return available!
    options.endpoints.releases = Some("http://127.0.0.1:1/latest.json".to_string());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "available");
    assert_eq!(body["version"], "9.9.9");
}

#[tokio::test(flavor = "multi_thread")]
async fn update_state_is_per_server() {
    let sig_b64 = valid_sig_b64();
    let fake = FakeReleases::start(
        Some(release_json("9.9.9", "notes", "proxi-setup.exe", &sig_b64)),
        b"test".to_vec(),
    )
    .await;

    let root = common::temp_dir("update-api-per-server");
    let repo_a = root.0.join("repo-a");
    let repo_b = root.0.join("repo-b");
    fs::create_dir_all(&repo_a).expect("create repo a");
    fs::create_dir_all(&repo_b).expect("create repo b");

    let mut options_a = options(&repo_a);
    options_a.endpoints.releases = Some(format!("{}/latest.json", fake.url));
    options_a.update_pubkey = Some(TEST_PUBKEY.to_string());

    let mut options_b = options(&repo_b);
    options_b.endpoints.releases = Some(format!("{}/latest.json", fake.url));
    options_b.update_pubkey = Some(TEST_PUBKEY.to_string());

    let first = start(options_a).await.expect("start server A");
    let second = start(options_b).await.expect("start server B");
    let client = Client::new();

    let response = post(&client, &first, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["state"], "available");

    let response = get(&client, &second, "/api/update").await;
    assert_eq!(body_json(response).await["state"], "idle");

    let response = get(&client, &first, "/api/update").await;
    assert_eq!(body_json(response).await["state"], "available");
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_downloads_verifies_writes_the_marker_and_signals_relaunch() {
    let sig_b64 = valid_sig_b64();
    let fake = FakeReleases::start(
        Some(release_json(
            "9.9.9",
            "Release notes",
            "proxi-setup.exe",
            &sig_b64,
        )),
        b"test".to_vec(),
    )
    .await;

    let root = common::temp_dir("update-api-apply");
    let repo = root.0.join("repo");
    fs::create_dir_all(&repo).expect("create repo");

    let relaunched = Arc::new(AtomicBool::new(false));
    let flag = relaunched.clone();
    let mut options = options(&repo);
    options.keep_alive = Duration::from_secs(120);
    options.endpoints.releases = Some(format!("{}/latest.json", fake.url));
    options.update_pubkey = Some(TEST_PUBKEY.to_string());
    options.relaunch = Some(Arc::new(move || flag.store(true, Ordering::SeqCst)));
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .expect("open events stream");
    assert_eq!(stream.status(), StatusCode::OK);

    let response = post(&client, &server, "/api/update/apply", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_text(response).await,
        r#"{"ok":true,"relaunching":true}"#
    );

    // The relaunch waits 500 ms so the reply can go out first.
    assert!(!relaunched.load(Ordering::SeqCst));

    // Installer file bytes == b"test"
    let installer = update::installer_path(&repo, "9.9.9");
    assert!(installer.is_file(), "installer file exists");
    let installer_bytes = fs::read(&installer).expect("read installer bytes");
    assert_eq!(installer_bytes, b"test");

    // Marker JSON has installer + version
    let pending = update::read_pending(&repo).expect("pending marker exists");
    assert_eq!(pending.version, "9.9.9");
    assert_eq!(pending.installer, installer.to_string_lossy());

    // SSE states updating -> relaunching in order
    let raw = read_until(&mut stream, Duration::from_secs(5), |raw| {
        let states = update_states(raw);
        states.iter().any(|state| state == "updating")
            && states.iter().any(|state| state == "relaunching")
    })
    .await;
    let states = update_states(&raw);
    let updating = states
        .iter()
        .position(|state| state == "updating")
        .expect("updating event");
    let relaunching = states
        .iter()
        .position(|state| state == "relaunching")
        .expect("relaunching event");
    assert!(updating < relaunching, "{states:?}");

    // Relaunch fires within 2 s
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !relaunched.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "relaunch callback never ran"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_refuses_a_tampered_signature() {
    let sig_b64 = valid_sig_b64();
    // Tampered payload: b"Test" instead of b"test"
    let fake = FakeReleases::start(
        Some(release_json(
            "9.9.9",
            "Release notes",
            "proxi-setup.exe",
            &sig_b64,
        )),
        b"Test".to_vec(),
    )
    .await;

    let root = common::temp_dir("update-api-tampered");
    let repo = root.0.join("repo");
    fs::create_dir_all(&repo).expect("create repo");

    let relaunched = Arc::new(AtomicBool::new(false));
    let flag = relaunched.clone();
    let mut options = options(&repo);
    options.endpoints.releases = Some(format!("{}/latest.json", fake.url));
    options.update_pubkey = Some(TEST_PUBKEY.to_string());
    options.relaunch = Some(Arc::new(move || flag.store(true, Ordering::SeqCst)));
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/update/apply", "{}").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let err_body = body_text(response).await;
    assert!(
        err_body.to_lowercase().contains("firma"),
        "error must mention the signature: {err_body}"
    );

    let get_resp = get(&client, &server, "/api/update").await;
    let get_body = body_json(get_resp).await;
    assert_eq!(get_body["state"], "error");

    let installer = update::installer_path(&repo, "9.9.9");
    assert!(!installer.exists(), "installer file must not exist");
    assert!(
        update::read_pending(&repo).is_none(),
        "pending marker must not exist"
    );

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!relaunched.load(Ordering::SeqCst), "relaunch must not run");

    // Also test flipped-byte signature
    let bad_sig_b64 = flipped_sig_b64();
    fake.set_latest_json(Some(release_json(
        "9.9.9",
        "Release notes",
        "proxi-setup.exe",
        &bad_sig_b64,
    )));
    fake.set_installer_bytes(b"test".to_vec());

    let response2 = post(&client, &server, "/api/update/apply", "{}").await;
    assert_eq!(response2.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let err_body2 = body_text(response2).await;
    assert!(
        err_body2.to_lowercase().contains("firma"),
        "error must mention the signature: {err_body2}"
    );
    assert!(!installer.exists());
    assert!(update::read_pending(&repo).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_refuses_a_live_session_without_force() {
    common::sessions::require_fake_engine();
    let fixture = common::sessions::Fixture::new("update-api-live-session");
    let sig_b64 = valid_sig_b64();
    let fake = FakeReleases::start(
        Some(release_json(
            "9.9.9",
            "Release notes",
            "proxi-setup.exe",
            &sig_b64,
        )),
        b"test".to_vec(),
    )
    .await;

    let mut options = common::sessions::fake_engine_options(&fixture.repo.0, &fixture.data.0);
    options.endpoints.releases = Some(format!("{}/latest.json", fake.url));
    options.update_pubkey = Some(TEST_PUBKEY.to_string());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let initial = get(&client, &server, "/api/update").await;
    assert_eq!(body_json(initial).await["activeSessions"], 0);

    let mut session_body = common::sessions::teach_planned(Some(false));
    session_body["note"] = json!("FAKE:slow");
    let create_resp = post(
        &client,
        &server,
        "/api/sessions",
        &session_body.to_string(),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);

    let active_resp = get(&client, &server, "/api/update").await;
    let active_body = body_json(active_resp).await;
    assert!(
        active_body["activeSessions"].as_u64().unwrap_or(0) >= 1,
        "activeSessions should be >= 1: {active_body}"
    );

    let response = post(&client, &server, "/api/update/apply", "{}").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert_eq!(body["code"], "sessions_running");
    assert!(
        body["error"].as_str().unwrap().contains("sessione in corso"),
        "{body}"
    );

    let pids = server.sessions.live_pids();
    assert!(!pids.is_empty(), "session process should be running");

    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_text(response).await,
        r#"{"ok":true,"relaunching":true}"#
    );

    // Verify session is shut down: no live pids and the OS processes are terminated
    assert!(
        server.sessions.live_pids().is_empty(),
        "live pids should be empty after shutdown"
    );
    for pid in pids {
        assert!(
            !common::sessions::pid_running(pid),
            "process {pid} should no longer be running"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_works_in_a_plain_non_git_dir() {
    let sig_b64 = valid_sig_b64();
    let fake = FakeReleases::start(
        Some(release_json(
            "9.9.9",
            "Release notes",
            "proxi-setup.exe",
            &sig_b64,
        )),
        b"test".to_vec(),
    )
    .await;

    let root = common::temp_dir("update-api-plain-dir");
    let plain = root.0.join("plain");
    fs::create_dir_all(&plain).expect("create plain dir");
    assert!(!plain.join(".git").exists());

    let mut options = options(&plain);
    options.endpoints.releases = Some(format!("{}/latest.json", fake.url));
    options.update_pubkey = Some(TEST_PUBKEY.to_string());
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/update/apply", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_text(response).await,
        r#"{"ok":true,"relaunching":true}"#
    );

    let installer = update::installer_path(&plain, "9.9.9");
    assert!(installer.is_file());
    assert_eq!(fs::read(&installer).expect("read installer"), b"test");

    let pending = update::read_pending(&plain).expect("pending marker exists");
    assert_eq!(pending.version, "9.9.9");
}
