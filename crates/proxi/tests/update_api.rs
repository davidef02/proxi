//! HTTP tests for the update routes against the in-process server.
//!
//! Fixtures are real git repos on disk: check and apply run the same git commands the binary
//! would, and the build is a fake `cargo` script (AC2-AC6).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::Value;

use proxi::server::{start, RunningServer, ServerOptions};
use proxi::update::{self, ActiveWork};

const SESSIONS_RUNNING: &str = r#"{"error":"C'è una sessione in corso. Chiudila o conferma per interromperla prima di aggiornare.","code":"sessions_running"}"#;
const DIRTY_TREE: &str = r#"{"error":"Modifiche locali non salvate nel repository. Impossibile aggiornare senza rischiare di sovrascrivere il tuo lavoro.","code":"dirty_tree"}"#;
const DIVERGED: &str = r#"{"error":"I commit locali sono divergenti da quelli remoti. Impossibile eseguire un aggiornamento fast-forward.","code":"diverged"}"#;
const NO_UPSTREAM: &str = r#"{"error":"Nessun branch upstream configurato."}"#;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Remote and clone share this fixture: `main`, a README and a `target/` ignore (the build's exe).
fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).expect("create repo");
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    fs::write(dir.join("README.md"), "# Test\n").expect("write README");
    fs::write(dir.join(".gitignore"), "target/\n").expect("write .gitignore");
    git(dir, &["add", "README.md", ".gitignore"]);
    git(dir, &["commit", "-m", "initial commit"]);
}

fn clone(src: &Path, dest: &Path) {
    fs::create_dir_all(dest.parent().expect("clone parent")).expect("create tmp root");
    let output = Command::new("git")
        .arg("clone")
        .arg(src)
        .arg(dest)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run git clone");
    assert!(
        output.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    git(dest, &["config", "user.name", "Test User"]);
    git(dest, &["config", "user.email", "test@example.com"]);
}

fn commit_file(dir: &Path, name: &str, content: &str, message: &str) {
    fs::write(dir.join(name), content).expect("write committed file");
    git(dir, &["add", name]);
    git(dir, &["commit", "-m", message]);
}

/// `.cmd` on Windows, an executable `sh` script on unix.
fn script(dir: &Path, name: &str, unix_body: &str, windows_body: &str) -> PathBuf {
    let _ = (unix_body, windows_body);
    #[cfg(windows)]
    {
        let path = dir.join(format!("{name}.cmd"));
        fs::write(&path, format!("@echo off\r\n{windows_body}")).expect("write script");
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join(format!("{name}.sh"));
        fs::write(&path, format!("#!/bin/sh\n{unix_body}")).expect("write script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod script");
        path
    }
}

/// The update build logs its arguments and GIT_TERMINAL_PROMPT next to itself, then writes the exe.
const UNIX_BUILD_LOG: &str =
    "echo \"$* GIT_TERMINAL_PROMPT=$GIT_TERMINAL_PROMPT\" >> \"$(dirname \"$0\")/cargo-log.txt\"\n";
const WINDOWS_BUILD_LOG: &str =
    ">>\"%~dp0cargo-log.txt\" echo %* GIT_TERMINAL_PROMPT=%GIT_TERMINAL_PROMPT%\r\n";

fn fake_build(dir: &Path) -> PathBuf {
    script(
        dir,
        "fake-cargo",
        &format!(
            "{UNIX_BUILD_LOG}mkdir -p target/release\necho new build > target/release/proxi\n"
        ),
        &format!(
            "{WINDOWS_BUILD_LOG}if not exist target\\release mkdir target\\release\r\necho new build> target\\release\\proxi.exe\r\n"
        ),
    )
}

fn assert_one_build_line(dir: &Path) {
    let log = fs::read_to_string(dir.join("cargo-log.txt")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "cargo log: {log:?}");
    assert!(lines[0].contains("build --release"), "cargo log: {log:?}");
    assert!(
        lines[0].contains("GIT_TERMINAL_PROMPT=0"),
        "cargo log: {log:?}"
    );
}

/// A server on `repo`. The data folder is a sibling: inside the repo it would make the tree dirty.
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

/// Wire order of the keys, thanks to serde_json's `preserve_order`.
fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("object body")
        .keys()
        .map(String::as_str)
        .collect()
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

fn update_states(raw: &str) -> Vec<String> {
    data_events(raw)
        .iter()
        .filter(|event| event["type"] == "update")
        .filter_map(|event| event["status"]["state"].as_str().map(str::to_string))
        .collect()
}

/// Kills the copied binary at the end of the test even if an assertion fails.
struct KillOnDrop(Option<Child>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Counts active work and records the shutdowns the update asks for (D11's test seam).
struct TestActiveWork {
    count: usize,
    shutdowns: Arc<AtomicUsize>,
}

impl ActiveWork for TestActiveWork {
    fn count(&self) -> usize {
        self.count
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }
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
        r#"{"state":"idle","behind":0,"activeSessions":0}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn check_reports_up_to_date_and_available() {
    let root = common::temp_dir("update-api-check");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);

    let mut options = options(&local);
    options.keep_alive = Duration::from_secs(120);
    let server = start(options).await.expect("start server");
    let client = Client::new();
    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .expect("open events stream");
    assert_eq!(stream.status(), StatusCode::OK);

    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "up_to_date");
    assert_eq!(body["behind"], 0);
    assert_eq!(keys(&body), ["state", "behind", "checkedAt", "activeSessions"]);
    let checked_at = body["checkedAt"].as_str().expect("checkedAt string");
    assert_eq!(checked_at.len(), 24, "{checked_at}");
    assert!(checked_at.ends_with('Z'), "{checked_at}");

    commit_file(&remote, "file1.txt", "hello\n", "feature: add file1");

    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "available");
    assert_eq!(body["behind"], 1);
    assert_eq!(
        keys(&body),
        ["state", "behind", "commits", "checkedAt", "activeSessions"]
    );
    let commit = &body["commits"][0];
    assert_eq!(commit["subject"], "feature: add file1");
    assert_eq!(keys(commit), ["hash", "subject", "relativeTime"]);
    assert!(
        commit["hash"].as_str().is_some_and(|hash| !hash.is_empty()),
        "{commit}"
    );
    assert!(
        commit["relativeTime"]
            .as_str()
            .is_some_and(|time| !time.is_empty()),
        "{commit}"
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
async fn check_without_git_or_upstream_is_idle() {
    let root = common::temp_dir("update-api-no-git");
    let client = Client::new();

    let plain = root.0.join("plain");
    fs::create_dir_all(&plain).expect("create plain dir");
    let server = start(options(&plain)).await.expect("start server");
    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "idle");
    assert_eq!(body["behind"], 0);

    let no_upstream = root.0.join("no-upstream");
    init_repo(&no_upstream);
    let server = start(options(&no_upstream)).await.expect("start server");
    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "idle");
    assert_eq!(body["behind"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn check_tracks_origin_main_from_a_feature_branch() {
    let root = common::temp_dir("update-api-feature-check");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);
    git(&local, &["checkout", "-b", "feature-test-branch"]);
    commit_file(&remote, "feature2.txt", "hello feature 2\n", "feature: add feature2");

    let server = start(options(&local)).await.expect("start server");
    let client = Client::new();
    let response = post(&client, &server, "/api/update/check", "{}").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "available");
    assert!(body["behind"].as_u64().unwrap_or(0) >= 1, "{body}");
    assert!(
        body["commits"]
            .as_array()
            .is_some_and(|commits| commits.iter().any(|c| c["subject"] == "feature: add feature2")),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn check_survives_an_unreachable_remote() {
    let root = common::temp_dir("update-api-offline");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);
    git(
        &local,
        &[
            "remote",
            "set-url",
            "origin",
            "http://127.0.0.1:9/does-not-exist.git",
        ],
    );

    let server = start(options(&local)).await.expect("start server");
    let client = Client::new();
    let response = post(&client, &server, "/api/update/check", "{}").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(
        matches!(
            body["state"].as_str(),
            Some("idle" | "up_to_date" | "available")
        ),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn check_is_single_flight() {
    let root = common::temp_dir("update-api-single-flight");
    let repo = root.0.join("repo");
    // An empty .git is enough: the fake git fails right after the work-tree check.
    fs::create_dir_all(repo.join(".git")).expect("create .git");
    let fake_git = script(
        &root.0,
        "fake-git",
        "echo \"$* GIT_TERMINAL_PROMPT=$GIT_TERMINAL_PROMPT\" >> \"$(dirname \"$0\")/calls.txt\"\nsleep 0.3\nexit 1\n",
        ">>\"%~dp0calls.txt\" echo %* GIT_TERMINAL_PROMPT=%GIT_TERMINAL_PROMPT%\r\nping -n 2 127.0.0.1 >nul\r\nexit /b 1\r\n",
    );
    let mut options = options(&repo);
    options.bins.git = Some(fake_git);
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
    assert_eq!(body["state"], "idle");

    let calls = fs::read_to_string(root.0.join("calls.txt")).unwrap_or_default();
    let lines: Vec<&str> = calls.lines().filter(|line| !line.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "calls: {calls:?}");
    assert!(
        lines[0].contains("rev-parse --is-inside-work-tree"),
        "calls: {calls:?}"
    );
    assert!(lines[0].contains("GIT_TERMINAL_PROMPT=0"), "calls: {calls:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn update_state_is_per_server() {
    let root = common::temp_dir("update-api-per-server");
    let remote = root.0.join("remote");
    init_repo(&remote);
    let behind = root.0.join("behind");
    clone(&remote, &behind);
    commit_file(&remote, "new.txt", "new\n", "feature: new");
    let plain = root.0.join("plain");
    fs::create_dir_all(&plain).expect("create plain dir");

    let first = start(options(&behind)).await.expect("start server A");
    let second = start(options(&plain)).await.expect("start server B");
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
async fn apply_refuses_active_work_without_force() {
    let root = common::temp_dir("update-api-active-work");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);

    let shutdowns = Arc::new(AtomicUsize::new(0));
    let mut options = options(&local);
    options.active_work = Arc::new(TestActiveWork {
        count: 1,
        shutdowns: shutdowns.clone(),
    });
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = get(&client, &server, "/api/update").await;
    assert_eq!(body_json(response).await["activeSessions"], 1);

    for body in ["{}", "not json"] {
        let response = post(&client, &server, "/api/update/apply", body).await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{body}");
        assert_eq!(body_text(response).await, SESSIONS_RUNNING, "{body}");
    }
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);

    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_text(response).await,
        r#"{"ok":true,"relaunching":true}"#
    );
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_refuses_dirty_and_diverged() {
    let root = common::temp_dir("update-api-refusals");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);
    let server = start(options(&local)).await.expect("start server");
    let client = Client::new();

    fs::write(local.join("uncommitted.txt"), "dirty work").expect("write uncommitted file");
    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_text(response).await, DIRTY_TREE);
    fs::remove_file(local.join("uncommitted.txt")).expect("remove uncommitted file");

    commit_file(&remote, "remote-file.txt", "remote\n", "remote commit");
    commit_file(&local, "local-file.txt", "local\n", "local conflicting commit");
    git(&local, &["fetch", "origin"]);
    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_text(response).await, DIVERGED);
    git(&local, &["reset", "--hard", "origin/main"]);

    let no_upstream = root.0.join("no-upstream");
    init_repo(&no_upstream);
    let server = start(options(&no_upstream)).await.expect("start server");
    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_text(response).await, NO_UPSTREAM);
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_pulls_rebuilds_while_running_and_relaunches() {
    let root = common::temp_dir("update-api-apply");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);
    commit_file(&remote, "feature.txt", "feature v2\n", "feature v2");

    // Put the test binary where the update rebuilds, and run it from there: the exe being
    // swapped must be a running process, the A9 case.
    let exe = update::release_exe(&local);
    fs::create_dir_all(exe.parent().expect("release dir")).expect("create release dir");
    fs::copy(common::proxi_bin(), &exe).expect("copy proxi");
    let original_len = fs::read(&exe).expect("read copied exe").len();
    let mut command = Command::new(&exe);
    command
        .args(["serve", "--port", "0", "--no-update-check", "--repo-root"])
        .arg(&local)
        .arg("--data-root")
        .arg(&local)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let running = KillOnDrop(Some(command.spawn().expect("spawn the copied proxi")));

    let cargo = fake_build(&root.0);
    let relaunched = Arc::new(AtomicBool::new(false));
    let flag = relaunched.clone();
    let mut server_options = options(&local);
    server_options.keep_alive = Duration::from_secs(120);
    server_options.bins.cargo = Some(cargo);
    server_options.relaunch = Some(Arc::new(move || flag.store(true, Ordering::SeqCst)));
    let server = start(server_options).await.expect("start server");
    let client = Client::new();
    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .expect("open events stream");
    assert_eq!(stream.status(), StatusCode::OK);

    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_text(response).await,
        r#"{"ok":true,"relaunching":true}"#
    );
    // The relaunch waits 500 ms so the reply can go out first.
    assert!(!relaunched.load(Ordering::SeqCst));

    let local_head = git(&local, &["rev-parse", "HEAD"]);
    let remote_head = git(&remote, &["rev-parse", "HEAD"]);
    assert_eq!(local_head, remote_head);
    let built = fs::read(&exe).expect("read rebuilt exe");
    assert_eq!(
        String::from_utf8_lossy(&built).trim(),
        "new build",
        "{:?}",
        String::from_utf8_lossy(&built)
    );
    let aside = update::aside_exe(&local);
    assert!(aside.is_file(), "the running exe is kept aside");
    assert_eq!(fs::read(&aside).expect("read aside exe").len(), original_len);
    assert_one_build_line(&root.0);

    let raw = read_until(&mut stream, Duration::from_secs(10), |raw| {
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

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !relaunched.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "relaunch callback never ran"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The running copy must be gone before Windows lets the next start delete its bytes.
    drop(running);
    let restarted = start(options(&local)).await.expect("start on the same repo");
    assert!(!aside.exists(), "start removes the leftover proxi.old");
    drop(restarted);
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_switches_a_clean_feature_branch_to_main() {
    let root = common::temp_dir("update-api-branch");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);
    git(&local, &["checkout", "-b", "another-feature-branch"]);
    commit_file(&remote, "feature3.txt", "feature v3\n", "feature v3");

    let mut options = options(&local);
    options.bins.cargo = Some(fake_build(&root.0));
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_text(response).await,
        r#"{"ok":true,"relaunching":true}"#
    );
    assert_eq!(
        git(&local, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert_eq!(
        git(&local, &["rev-parse", "HEAD"]),
        git(&remote, &["rev-parse", "HEAD"])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_build_keeps_the_previous_executable() {
    let root = common::temp_dir("update-api-failed-build");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);
    commit_file(&remote, "feature.txt", "feature\n", "feature: boom");
    let exe = update::release_exe(&local);
    fs::create_dir_all(exe.parent().expect("release dir")).expect("create release dir");
    fs::write(&exe, "old build").expect("write old exe");
    let cargo = script(
        &root.0,
        "fake-cargo",
        "echo boom >&2\nexit 1\n",
        "echo boom 1>&2\r\nexit /b 1\r\n",
    );

    let relaunched = Arc::new(AtomicBool::new(false));
    let flag = relaunched.clone();
    let mut options = options(&local);
    options.bins.cargo = Some(cargo);
    options.relaunch = Some(Arc::new(move || flag.store(true, Ordering::SeqCst)));
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_text(response).await,
        r#"{"error":"cargo build fallito: boom"}"#
    );
    assert_eq!(
        fs::read_to_string(&exe).expect("read restored exe"),
        "old build"
    );
    assert!(!update::aside_exe(&local).exists(), "aside removed on restore");

    let response = get(&client, &server, "/api/update").await;
    let body = body_json(response).await;
    assert_eq!(body["state"], "error");
    assert_eq!(body["error"], "cargo build fallito: boom");

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!relaunched.load(Ordering::SeqCst));
    // The pull already happened: Node behaves the same.
    assert_eq!(
        git(&local, &["rev-parse", "HEAD"]),
        git(&remote, &["rev-parse", "HEAD"])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_after_failed_build_rebuilds_and_relaunches() {
    let root = common::temp_dir("update-api-build-retry");
    let remote = root.0.join("remote");
    let local = root.0.join("local");
    init_repo(&remote);
    clone(&remote, &local);
    commit_file(&remote, "retry.txt", "retry\n", "feature: retry");
    let exe = update::release_exe(&local);
    fs::create_dir_all(exe.parent().expect("release dir")).expect("create release dir");
    fs::write(&exe, "old build").expect("write old exe");

    let cargo = script(
        &root.0,
        "fake-cargo",
        "echo boom >&2\nexit 1\n",
        "echo boom 1>&2\r\nexit /b 1\r\n",
    );
    let relaunched = Arc::new(AtomicBool::new(false));
    let flag = relaunched.clone();
    let mut options = options(&local);
    options.bins.cargo = Some(cargo);
    options.relaunch = Some(Arc::new(move || flag.store(true, Ordering::SeqCst)));
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_text(response).await,
        r#"{"error":"cargo build fallito: boom"}"#
    );
    assert!(
        update::build_pending(&local).is_file(),
        "a failed build leaves the retry marker"
    );
    assert_eq!(
        git(&local, &["rev-parse", "HEAD"]),
        git(&remote, &["rev-parse", "HEAD"])
    );

    // The pull already moved HEAD (behind 0): the marker must make the next check offer the update.
    let response = post(&client, &server, "/api/update/check", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "available");
    assert!(
        body["behind"].as_u64().unwrap_or(0) > 0,
        "the UI shows \"Aggiorna ora\" only with a positive count: {body}"
    );

    // Same path, now a build that succeeds: apply rebuilds even though HEAD did not move again.
    fake_build(&root.0);
    let response = post(&client, &server, "/api/update/apply", r#"{"force":true}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_text(response).await,
        r#"{"ok":true,"relaunching":true}"#
    );
    assert_eq!(
        fs::read_to_string(&exe).expect("read rebuilt exe").trim(),
        "new build"
    );
    assert!(
        !update::build_pending(&local).exists(),
        "a successful retry clears the marker"
    );
    assert_one_build_line(&root.0);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !relaunched.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "relaunch callback never ran"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
