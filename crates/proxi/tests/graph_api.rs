mod common;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};

use proxi::server::{start, RunningServer, ServerOptions};

/// A temp repo plus a data folder seeded with one lesson, one progress line and the two ledgers.
struct Fixture {
    _root: common::TempDir,
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

        let lesson = json!({
            "metadata": {
                "lessonNumber": 1,
                "slug": "t",
                "title": "T",
                "topic": "T",
                "dateCreated": "",
                "dateModified": "",
                "version": "1",
            },
            "nodes": [
                { "nodeId": "alpha", "title": "Alfa", "tier": "axiom", "content": "x" },
                { "nodeId": "beta", "title": "Beta", "tier": "derivation", "content": "x", "dependencies": ["alpha", "ghost"] },
                { "nodeId": "gamma", "title": "Gamma", "tier": "derivation", "content": "x" },
            ],
        });
        let lessons = data.join("courses").join("demo").join("lessons");
        fs::create_dir_all(&lessons).expect("create lessons");
        fs::write(
            lessons.join("0001-t.html"),
            format!(
                "<html><body><script type=\"application/json\" id=\"lesson-data\">{lesson}</script></body></html>"
            ),
        )
        .expect("write lesson");
        fs::write(
            data.join("courses").join("demo").join("progress.jsonl"),
            format!(
                "{}\n",
                json!({
                    "kind": "check",
                    "lessonSlug": "t",
                    "nodeRefs": ["beta"],
                    "correct": false,
                    "at": "2026-01-02T00:00:00Z",
                })
            ),
        )
        .expect("write progress");

        let graph = data.join("graph");
        fs::create_dir_all(&graph).expect("create graph");
        let point = |message: &str| json!({ "chatId": "demo/0001-t", "messageId": message });
        let node = |id: &str, name: &str, message: &str| {
            json!({
                "id": id,
                "name": name,
                "statement": format!("{name} vale."),
                "aliases": [],
                "createdAt": "2026-01-01T00:00:00Z",
                "provenance": [point(message)],
            })
            .to_string()
        };
        fs::write(
            graph.join("nodes.jsonl"),
            format!(
                "{}\n{}\n{}\n{{broken\n",
                node("n_a", "A", "alpha"),
                node("n_b", "B", "beta"),
                node("n_c", "C", "gamma"),
            ),
        )
        .expect("write nodes");
        let chain = |id: &str, premises: Value, conclusion: &str| {
            json!({
                "id": id,
                "premises": premises,
                "conclusion": conclusion,
                "createdAt": "2026-01-01T00:00:00Z",
                "provenance": [{ "chatId": "demo/0001-t", "messageId": "" }],
            })
            .to_string()
        };
        fs::write(
            graph.join("chains.jsonl"),
            format!(
                "{}\n{}\n",
                chain("arg_1", json!(["n_a", "n_b"]), "n_c"),
                chain("arg_2", json!(["n_a"]), "n_b"),
            ),
        )
        .expect("write chains");

        Self {
            _root: root,
            repo,
            data,
        }
    }

    fn options(&self) -> ServerOptions {
        let mut options = ServerOptions::new(self.repo.clone());
        options.data_root = Some(self.data.clone());
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
async fn map_serves_the_knowledge_map() {
    let fixture = Fixture::new("graph-api-map");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client.get(url(&server, "/api/map")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let keys: Vec<&str> = body["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|node| node["key"].as_str().expect("node key"))
        .collect();
    assert_eq!(keys, ["demo/t#alpha", "demo/t#beta", "demo/t#gamma"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_lays_out_in_js_number_shapes() {
    let fixture = Fixture::new("graph-api-graph");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client.get(url(&server, "/api/graph")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let ids: Vec<&str> = body["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|node| node["id"].as_str().expect("node id"))
        .collect();
    assert!(ids.contains(&"arg_1"), "{ids:?}");
    assert_eq!(
        body["positions"].as_object().expect("positions").len(),
        ids.len()
    );

    let layout =
        fs::read_to_string(fixture.data.join("graph").join("layout.json")).expect("layout file");
    assert!(layout.contains("\"x\":0"), "{layout}");
    assert!(!layout.contains(".0"), "{layout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn positions_need_the_header_and_known_ids() {
    let fixture = Fixture::new("graph-api-positions");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    // No x-learner-client: the CSRF guard answers before the route runs (main.ts:189-198).
    let response = client
        .post(url(&server, "/api/graph/positions"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    for body in ["{}", "{"] {
        let response = post(&client, &server, "/api/graph/positions", body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            body_json(response).await,
            json!({"error": "positions mancante"}),
            "{body}"
        );
    }

    let response = post(
        &client,
        &server,
        "/api/graph/positions",
        r#"{"positions":{"n_a":{"x":5,"y":6},"ghost":{"x":1,"y":2}}}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));

    let layout =
        fs::read_to_string(fixture.data.join("graph").join("layout.json")).expect("layout file");
    assert_eq!(layout, "{\"n_a\":{\"x\":5,\"y\":6}}");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_removes_lines_and_emits_growing_revisions() {
    let fixture = Fixture::new("graph-api-delete");
    let mut options = fixture.options();
    options.keep_alive = Duration::from_secs(120); // keep the stream quiet for the test
    let server = start(options).await.expect("start server");
    let client = Client::new();

    let mut stream = client
        .get(url(&server, "/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);

    let response = post(&client, &server, "/api/graph/nodes/n_b/delete", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"ok": true}));

    let raw = read_until(&mut stream, Duration::from_secs(5), |raw| {
        !data_events(raw).is_empty()
    })
    .await;
    assert_eq!(
        data_events(&raw)[0],
        json!({"type": "graph", "revision": 1})
    );

    let nodes = fs::read_to_string(fixture.data.join("graph").join("nodes.jsonl"))
        .expect("read nodes");
    assert!(!nodes.contains("\"n_b\""), "{nodes}");
    assert!(nodes.contains("{broken"), "{nodes}");
    let chains = fs::read_to_string(fixture.data.join("graph").join("chains.jsonl"))
        .expect("read chains");
    assert!(!chains.contains("arg_1"), "{chains}");
    assert!(!chains.contains("arg_2"), "{chains}");

    let response = post(&client, &server, "/api/graph/nodes/n_c/delete", "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    let raw = read_until(&mut stream, Duration::from_secs(5), |raw| {
        data_events(raw).iter().any(|event| event["revision"] == 2)
    })
    .await;
    assert_eq!(
        data_events(&raw)[0],
        json!({"type": "graph", "revision": 2})
    );

    let response = client.get(url(&server, "/api/graph")).send().await.unwrap();
    let body = body_json(response).await;
    let ids: Vec<&str> = body["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|node| node["id"].as_str().expect("node id"))
        .collect();
    assert!(!ids.contains(&"n_b"), "{ids:?}");
    assert!(!ids.contains(&"n_c"), "{ids:?}");

    for id in ["x_1", "n_"] {
        let response = post(
            &client,
            &server,
            &format!("/api/graph/nodes/{id}/delete"),
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{id}");
        assert_eq!(
            body_json(response).await,
            json!({"error": "id non valido"}),
            "{id}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_routes_reject_the_wrong_method() {
    let fixture = Fixture::new("graph-api-methods");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client
        .get(url(&server, "/api/graph/positions"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(response).await, json!({"error": "Not found"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn map_fails_but_graph_survives_a_broken_lesson() {
    let fixture = Fixture::new("graph-api-broken-lesson");
    // An island whose `nodes` is missing makes the lesson scan throw (main.ts:180-185).
    fs::write(
        fixture
            .data
            .join("courses")
            .join("demo")
            .join("lessons")
            .join("0002-u.html"),
        "<html><body><script type=\"application/json\" id=\"lesson-data\">{\"metadata\":{\"lessonNumber\":2,\"slug\":\"u\"}}</script></body></html>",
    )
    .expect("write broken lesson");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client.get(url(&server, "/api/map")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_json(response).await;
    assert!(body["error"].is_string(), "{body}");

    let response = client.get(url(&server, "/api/graph")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
