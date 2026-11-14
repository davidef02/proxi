//! The /api/exercises routes over HTTP, against a real server.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use proxi::server::{start, RunningServer, ServerOptions};

/// A temp repo and data folder; the files on disk are the fixture.
struct Fixture {
    root: common::TempDir,
    repo: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = common::temp_dir(tag);
        let repo = root.0.join("repo");
        let data = root.0.join("data");
        fs::create_dir_all(&repo).expect("create repo");
        fs::create_dir_all(&data).expect("create data");
        Self { root, repo, data }
    }

    fn options(&self) -> ServerOptions {
        let mut options = ServerOptions::new(self.repo.clone());
        options.data_root = Some(self.data.clone());
        options.dist_dir = Some(self.root.0.join("missing-dist"));
        // Long enough that only a request's own event can appear in the stream.
        options.keep_alive = Duration::from_secs(120);
        options
    }

    fn data_file(&self, rel: &str, content: &str) {
        write(&self.data, rel, content);
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("a parent")).expect("create parent dir");
    fs::write(path, content).expect("write fixture file");
}

fn sha1_hex(text: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// The demo course shared by the cases (same fixture as api-exercises.test.ts).
fn seed(data: &Path) {
    write(data, "courses/demo/exercises/package.json", "{}");
    write(data, "courses/demo/exercises/01-a/Esercizio.vue", "stub");
    write(data, "courses/demo/exercises/01-a/Esercizio.test.js", "// t");
    write(data, "courses/demo/exercises/01-a/ISTRUZIONI.md", "testo");
    write(data, "courses/demo/exercises/01-a/node_modules/x.js", "x");
    write(data, "courses/demo/exercises/misc/x.js", "x");
}

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{}", server.url, path)
}

async fn body_text(response: Response) -> String {
    response.text().await.expect("read body")
}

async fn get(client: &Client, server: &RunningServer, path: &str) -> Response {
    client
        .get(url(server, path))
        .send()
        .await
        .unwrap_or_else(|error| panic!("GET {path}: {error}"))
}

/// POST as the web UI does: the CSRF header plus JSON.
async fn post(client: &Client, base: &str, path: &str, body: &str) -> Response {
    client
        .post(format!("{base}{path}"))
        .header("x-learner-client", "1")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap_or_else(|error| panic!("POST {path}: {error}"))
}

async fn get_json(client: &Client, server: &RunningServer, path: &str) -> (StatusCode, Value) {
    let response = get(client, server, path).await;
    let status = response.status();
    let text = body_text(response).await;
    let value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("GET {path} is not JSON ({error}): {text}"));
    (status, value)
}

async fn post_json(client: &Client, base: &str, path: &str, body: &Value) -> (StatusCode, Value) {
    let response = post(client, base, path, &body.to_string()).await;
    let status = response.status();
    let text = body_text(response).await;
    let value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("POST {path} is not JSON ({error}): {text}"));
    (status, value)
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

async fn subscribe(client: &Client, base: &str) -> Response {
    let response = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .expect("GET /api/events");
    assert_eq!(response.status(), StatusCode::OK);
    response
}

/// Waits for the first complete `data:` block of an SSE stream.
async fn first_event(stream: &mut Response) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut buffer = String::new();
    loop {
        if let Some(index) = buffer.find("\n\n") {
            let block = buffer[..index].to_string();
            let data: String = block
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .collect();
            assert!(!data.is_empty(), "first SSE block has no data: {block:?}");
            return serde_json::from_str(&data)
                .unwrap_or_else(|error| panic!("SSE event is not JSON ({error}): {data}"));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let chunk = tokio::time::timeout(remaining, stream.chunk())
            .await
            .unwrap_or_else(|_| panic!("no SSE event within 10 s; got {buffer:?}"))
            .expect("read SSE chunk")
            .expect("SSE stream closed before the first event");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
    }
}

/// Fails when anything arrives within `wait`: keep-alive is 120 s in these fixtures.
async fn expect_no_event(stream: &mut Response, wait: Duration) {
    match tokio::time::timeout(wait, stream.chunk()).await {
        Err(_) => {}
        Ok(Ok(Some(chunk))) => {
            panic!("unexpected SSE chunk: {:?}", String::from_utf8_lossy(&chunk))
        }
        Ok(Ok(None)) => panic!("SSE stream closed"),
        Ok(Err(error)) => panic!("SSE read error: {error}"),
    }
}

/// A vitest stand-in: writes the report vitest would.
const VITEST_FAKE: &str = r#"import fs from "node:fs";
const out = process.argv.find((a) => a.startsWith("--outputFile=")).slice("--outputFile=".length);
fs.writeFileSync(out, JSON.stringify({ numTotalTests: 1, numPassedTests: 1, numFailedTests: 0, testResults: [] }));
"#;

#[tokio::test(flavor = "multi_thread")]
async fn list_and_workspace_over_http() {
    let fixture = Fixture::new("exercises-api-list");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let (status, list) = get_json(&client, &server, "/api/exercises/demo").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["name"], "01-a");
    assert_eq!(
        keys(&list[0]),
        vec![
            "course",
            "name",
            "dir",
            "files",
            "instructions"
        ]
    );
    assert_eq!(list[0]["dir"], "courses/demo/exercises/01-a");
    assert_eq!(
        list[0]["files"],
        json!(["Esercizio.test.js", "Esercizio.vue", "ISTRUZIONI.md"])
    );

    fixture.data_file(
        "courses/demo/exercises/01-a/SUGGERIMENTI.json",
        &json!({"hints": ["a"], "revealed": 0}).to_string(),
    );
    fixture.data_file(
        "courses/demo/exercises/01-a/.app-lastrun.json",
        &json!({"ok": true}).to_string(),
    );
    let (_, with_meta) = get_json(&client, &server, "/api/exercises/demo").await;
    assert_eq!(
        keys(&with_meta[0]),
        vec![
            "course",
            "name",
            "dir",
            "files",
            "lastRun",
            "hints",
            "hintsRevealed",
            "final",
            "instructions"
        ]
    );
    assert_eq!(with_meta[0]["lastRun"], json!({"ok": true}));

    let (status, none) = get_json(&client, &server, "/api/exercises/nessuno").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(none, json!([]));

    let (status, bad) = get_json(&client, &server, "/api/exercises/Bad").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        bad,
        json!({"error": ": Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/"})
    );

    let (status, detail) = get_json(&client, &server, "/api/exercises/demo/01-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keys(&detail), vec!["info", "files"]);
    assert_eq!(
        keys(&detail["files"][0]),
        vec!["path", "content", "version", "readOnly"]
    );
    let pairs: Vec<(&str, bool)> = detail["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|file| {
            (
                file["path"].as_str().expect("a path"),
                file["readOnly"].as_bool().expect("a boolean"),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![("Esercizio.test.js", true), ("Esercizio.vue", false)]
    );
    assert_eq!(detail["files"][1]["version"], sha1_hex("stub"));

    for name in ["09-none", "misc"] {
        let (status, body) = get_json(
            &client,
            &server,
            &format!("/api/exercises/demo/{name}"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{name}");
        assert_eq!(body, json!({"error": "Esercizio non trovato"}), "{name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn save_conflict_is_409_stale() {
    let fixture = Fixture::new("exercises-api-stale");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let file = fixture
        .data
        .join("courses/demo/exercises/01-a/Esercizio.vue");

    let version = sha1_hex("stub");
    fs::write(&file, "da VS Code").expect("external write");
    let (status, body) = post_json(
        &client,
        &server.url,
        "/api/exercises/save",
        &json!({
            "course": "demo",
            "name": "01-a",
            "path": "Esercizio.vue",
            "content": "mio",
            "baseVersion": version,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({"error": "Il file è cambiato fuori dall'editor", "code": "stale"})
    );
    assert_eq!(fs::read_to_string(&file).expect("read file"), "da VS Code");

    // The happy path returns the hash of what was written.
    let (status, body) = post_json(
        &client,
        &server.url,
        "/api/exercises/save",
        &json!({
            "course": "demo",
            "name": "01-a",
            "path": "Esercizio.vue",
            "content": "nuovo",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"ok": true, "version": sha1_hex("nuovo")}));
    assert_eq!(fs::read_to_string(&file).expect("read file"), "nuovo");
}

#[tokio::test(flavor = "multi_thread")]
async fn hint_writes_the_same_file_and_emits_courses() {
    let fixture = Fixture::new("exercises-api-hint");
    seed(&fixture.data);
    fixture.data_file(
        "courses/demo/exercises/01-a/SUGGERIMENTI.json",
        &json!({"hints": ["a", "b"], "final": false, "revealed": 0}).to_string(),
    );
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let mut stream = subscribe(&client, &server.url).await;
    let (status, info) = post_json(
        &client,
        &server.url,
        "/api/exercises/hint",
        &json!({"course": "demo", "name": "01-a"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["hintsRevealed"], 1);
    assert_eq!(
        first_event(&mut stream).await,
        json!({"type": "courses"})
    );

    let text = fs::read_to_string(
        fixture
            .data
            .join("courses/demo/exercises/01-a/SUGGERIMENTI.json"),
    )
    .expect("read hints");
    assert_eq!(
        text,
        "{\n  \"hints\": [\n    \"a\",\n    \"b\"\n  ],\n  \"final\": false,\n  \"revealed\": 1\n}\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn save_events_are_throttled_per_exercise() {
    let fixture = Fixture::new("exercises-api-events");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();
    let mut stream = subscribe(&client, &server.url).await;

    let first = post(
        &client,
        &server.url,
        "/api/exercises/save",
        &json!({
            "course": "demo",
            "name": "01-a",
            "path": "Esercizio.vue",
            "content": "uno",
        })
        .to_string(),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first_event(&mut stream).await,
        json!({"type": "courses"})
    );

    let second = post(
        &client,
        &server.url,
        "/api/exercises/save",
        &json!({
            "course": "demo",
            "name": "01-a",
            "path": "Esercizio.vue",
            "content": "due",
        })
        .to_string(),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    expect_no_event(&mut stream, Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_returns_the_result_and_writes_last_run() {
    let fixture = Fixture::new("exercises-api-run");
    seed(&fixture.data);
    fixture.data_file(
        "courses/demo/exercises/node_modules/vitest/vitest.mjs",
        VITEST_FAKE,
    );
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = post(
        &client,
        &server.url,
        "/api/exercises/run",
        &json!({"course": "demo", "name": "01-a"}).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let text = body_text(response).await;
    let body: Value = serde_json::from_str(&text).expect("run JSON");
    assert_eq!(
        keys(&body),
        vec![
            "ok",
            "passed",
            "failed",
            "total",
            "failures",
            "tests",
            "durationMs",
            "at"
        ]
    );
    assert_eq!(body["ok"], true);
    assert_eq!(body["tests"], json!([]));

    let last_run = fixture
        .data
        .join("courses/demo/exercises/01-a/.app-lastrun.json");
    assert_eq!(fs::read_to_string(last_run).expect("last run file"), text);

    let (_, list) = get_json(&client, &server, "/api/exercises/demo").await;
    assert_eq!(list[0]["lastRun"], body);
}

#[tokio::test(flavor = "multi_thread")]
async fn run_unknown_and_missing() {
    let fixture = Fixture::new("exercises-api-run-errors");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let (status, body) = post_json(
        &client,
        &server.url,
        "/api/exercises/run",
        &json!({"course": "demo", "name": "misc"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        keys(&body),
        vec![
            "ok",
            "passed",
            "failed",
            "total",
            "failures",
            "durationMs",
            "at",
            "error"
        ]
    );
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "Esercizio sconosciuto: misc");

    let (status, body) = post_json(
        &client,
        &server.url,
        "/api/exercises/run",
        &json!({"course": "demo", "name": "09-none"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Esercizio non trovato"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn open_missing_is_404() {
    let fixture = Fixture::new("exercises-api-open");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let (status, body) = post_json(
        &client,
        &server.url,
        "/api/exercises/open",
        &json!({"course": "demo", "name": "09-none"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "Esercizio non trovato"}));

    let (status, body) = post_json(
        &client,
        &server.url,
        "/api/exercises/open",
        &json!({"course": "Bad", "name": "01-a"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": "course: Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/"})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_save_is_413() {
    let fixture = Fixture::new("exercises-api-huge");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let (status, body) = post_json(
        &client,
        &server.url,
        "/api/exercises/save",
        &json!({
            "course": "demo",
            "name": "01-a",
            "path": "Esercizio.vue",
            "content": "x".repeat(600_001),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body, json!({"error": "File troppo grande"}));
}

/// Answering a 413 with the request body still unread makes the client's write fail with a reset
/// (undici turns it into ECONNRESET in app/test/api-exercises.test.ts). Node reads the body first.
#[tokio::test(flavor = "multi_thread")]
async fn oversized_save_reads_the_whole_body_before_413() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let fixture = Fixture::new("exercises-api-drain");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");

    let payload = json!({
        "course": "demo",
        "name": "01-a",
        "path": "Esercizio.vue",
        "content": "x".repeat(600_001),
    })
    .to_string();

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
        .await
        .expect("connect");
    let head = format!(
        "POST /api/exercises/save HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nx-learner-client: 1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        server.port,
        payload.len()
    );
    stream.write_all(head.as_bytes()).await.expect("write head");

    // The body check happens on the headers, so the 413 is ready; it must wait for the body.
    let mut early = [0u8; 1];
    match tokio::time::timeout(Duration::from_secs(1), stream.read(&mut early)).await {
        Err(_) => {}
        Ok(Ok(0)) => panic!("connection closed before the body was sent"),
        Ok(Ok(_)) => panic!("the server answered before reading the body"),
        Ok(Err(error)) => panic!("read before sending the body: {error}"),
    }

    stream
        .write_all(payload.as_bytes())
        .await
        .expect("write body");
    let mut response = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !String::from_utf8_lossy(&response).contains("\"File troppo grande\"") {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let mut chunk = [0u8; 1024];
        let read = tokio::time::timeout(remaining, stream.read(&mut chunk))
            .await
            .expect("a response within 10 s")
            .expect("read response");
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
    }
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 413 "), "{response}");
    assert!(
        response.contains("\"error\":\"File troppo grande\""),
        "{response}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_bodies_use_the_zod_texts() {
    let fixture = Fixture::new("exercises-api-bodies");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let cases: [(&str, &str, &str); 8] = [
        ("/api/exercises/save", "{", "Invalid JSON body"),
        (
            "/api/exercises/save",
            "[]",
            ": Invalid input: expected object, received array",
        ),
        (
            "/api/exercises/save",
            "{}",
            "course: Invalid input: expected string, received undefined; name: Invalid input: expected string, received undefined; path: Invalid input: expected string, received undefined; content: Invalid input: expected string, received undefined",
        ),
        (
            "/api/exercises/save",
            r#"{"course":"Bad","name":"01-a","path":"a.js","content":"x"}"#,
            "course: Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/",
        ),
        (
            "/api/exercises/save",
            r#"{"course":"demo","name":"a b","path":"a.js","content":"x"}"#,
            "name: Invalid string: must match pattern /^[A-Za-z0-9._-]+$/",
        ),
        (
            "/api/exercises/save",
            r#"{"course":"demo","name":"01-a","path":"","content":"x"}"#,
            "path: Too small: expected string to have >=1 characters",
        ),
        (
            "/api/exercises/save",
            r#"{"course":"demo","name":"01-a","path":"a.js","content":"x","baseVersion":null}"#,
            "baseVersion: Invalid input: expected string, received null",
        ),
        (
            "/api/exercises/hint",
            r#"{"course":"demo","name":"01-a","sessionId":5}"#,
            "sessionId: Invalid input: expected string, received number",
        ),
    ];
    for (path, body, want) in cases {
        let response = post(&client, &server.url, path, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        let text = body_text(response).await;
        let parsed: Value =
            serde_json::from_str(&text).unwrap_or_else(|error| panic!("{body}: {error}: {text}"));
        assert_eq!(parsed, json!({"error": want}), "{body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn get_save_lists_course_save() {
    let fixture = Fixture::new("exercises-api-save-route");
    seed(&fixture.data);
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    // `/api/exercises/save` is also a GET for the course "save", as in Hono.
    let response = get(&client, &server, "/api/exercises/save").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_text(response).await, "[]");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_without_node_on_path_names_node() {
    let root = common::temp_dir("exercises-api-no-node");
    let repo = root.0.join("repo");
    let data = root.0.join("data");
    let empty = root.0.join("empty-path");
    fs::create_dir_all(&repo).expect("create repo");
    fs::create_dir_all(&data).expect("create data");
    fs::create_dir_all(&empty).expect("create empty path");
    seed(&data);
    write(
        &data,
        "courses/demo/exercises/node_modules/vitest/vitest.mjs",
        VITEST_FAKE,
    );

    let repo_arg = repo.display().to_string();
    let data_arg = data.display().to_string();
    let (_server, base) = common::spawn_serve_with_path(
        &[
            "serve",
            "--port",
            "0",
            "--repo-root",
            &repo_arg,
            "--data-root",
            &data_arg,
            "--memory-secrets",
        ],
        empty.as_os_str(),
    );

    let client = Client::new();
    let response = post(
        &client,
        &base,
        "/api/exercises/run",
        &json!({"course": "demo", "name": "01-a"}).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_str(&body_text(response).await).expect("run JSON");
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], proxi::exercises::NODE_MISSING);
    assert!(
        proxi::exercises::NODE_MISSING.contains("Node"),
        "the message names Node"
    );
}
