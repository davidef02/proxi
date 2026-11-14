mod common;

use std::fs;
use std::path::{Path, PathBuf};

use reqwest::{Client, Method, StatusCode};
use serde_json::Value;

use proxi::server::{start, RunningServer, ServerOptions};

fn golden_file() -> PathBuf {
    common::repo_root()
        .join("crates")
        .join("proxi")
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("graph.json")
}

/// Writes the golden's raw files (lessons, ledgers, legacy positions) verbatim under `dir`.
fn seed(dir: &Path) {
    let file = golden_file();
    let raw = fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    let golden: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("parse {}: {error}", file.display()));
    for (rel, content) in golden["dataSeed"].as_object().expect("dataSeed object") {
        let text = content
            .as_str()
            .unwrap_or_else(|| panic!("dataSeed {rel} is not a string"));
        let target = dir.join(rel);
        fs::create_dir_all(target.parent().expect("seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(&target, text.as_bytes()).unwrap_or_else(|error| panic!("write {rel}: {error}"));
    }
}

struct Pair {
    client: Client,
    node: common::NodeServer,
    rust: RunningServer,
    node_dir: PathBuf,
    rust_dir: PathBuf,
}

impl Pair {
    /// Sends one request to both servers and asserts they answer the same.
    async fn same(&self, method: Method, path: &str, body: Option<&str>) -> Value {
        let node = self.request(&self.node.url(), &method, path, body).await;
        let rust = self.request(&self.rust.url, &method, path, body).await;

        assert_eq!(node.0, rust.0, "{method} {path}");
        if node.0.is_success() {
            assert_eq!(
                serde_json::to_string(&node.1).expect("serialise Node body"),
                serde_json::to_string(&rust.1).expect("serialise Rust body"),
                "{method} {path}"
            );
        }
        node.1
    }

    async fn request(
        &self,
        base: &str,
        method: &Method,
        path: &str,
        body: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = self.client.request(method.clone(), format!("{base}{path}"));
        if let Some(body) = body {
            request = request
                .header("x-learner-client", "1")
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let response = request
            .send()
            .await
            .unwrap_or_else(|error| panic!("{method} {path}: {error}"));
        let status = response.status();
        let text = response.text().await.expect("read body");
        let value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{method} {path}: not JSON ({error}): {text}"));
        (status, value)
    }

    /// The two data folders must hold the same bytes, or both miss the file.
    fn same_file(&self, rel: &str) {
        let node = fs::read(self.node_dir.join(rel));
        let rust = fs::read(self.rust_dir.join(rel));
        match (node, rust) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{rel} bytes"),
            (Err(_), Err(_)) => {}
            (node, rust) => panic!(
                "{rel}: written by one server only (Node ok: {}, Rust ok: {})",
                node.is_ok(),
                rust.is_ok()
            ),
        }
    }
}

#[test]
fn graph_routes_match_node() {
    common::require_node();

    let root = common::temp_dir("graph-parity");
    let node_dir = root.0.join("node");
    let rust_dir = root.0.join("rust");
    fs::create_dir_all(&node_dir).expect("create node dir");
    fs::create_dir_all(&rust_dir).expect("create rust dir");
    seed(&node_dir);
    seed(&rust_dir);

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let node = common::start_node(&node_dir);
        let mut options = ServerOptions::new(rust_dir.clone());
        options.data_root = Some(rust_dir.clone());
        let rust = start(options).await.expect("start Rust server");
        let pair = Pair {
            client: Client::new(),
            node,
            rust,
            node_dir: node_dir.clone(),
            rust_dir: rust_dir.clone(),
        };

        // Same JSON, key order and number shapes included.
        pair.same(Method::GET, "/api/map", None).await;
        pair.same(Method::GET, "/api/graph", None).await;
        pair.same_file("graph/layout.json");

        // Same layout bytes after a positions POST, and the legacy file gone on both (AC2).
        let body = pair
            .same(
                Method::POST,
                "/api/graph/positions",
                Some(r#"{"positions":{"n_a":{"x":5,"y":6.5},"arg_ef":{"x":1,"y":2},"ghost":{"x":1,"y":2}}}"#),
            )
            .await;
        assert_eq!(body, serde_json::json!({ "ok": true }));
        pair.same_file("graph/layout.json");
        pair.same_file("graph/positions.json");

        // Same ledger bytes after deletes (AC4).
        pair.same(Method::POST, "/api/graph/nodes/n_b/delete", Some("{}"))
            .await;
        pair.same(Method::POST, "/api/graph/nodes/arg_gad/delete", Some("{}"))
            .await;
        pair.same_file("graph/nodes.jsonl");
        pair.same_file("graph/chains.jsonl");
        pair.same_file("graph/layout.json");

        pair.same(Method::GET, "/api/graph", None).await;

        // Cross-read: each server on the other's folder answers with the same string.
        let node2 = common::start_node(&rust_dir);
        let mut options = ServerOptions::new(node_dir.clone());
        options.data_root = Some(node_dir.clone());
        let rust2 = start(options).await.expect("start second Rust server");

        let client = Client::new();
        let bases = [
            pair.node.url(),
            pair.rust.url.clone(),
            node2.url(),
            rust2.url.clone(),
        ];
        let mut bodies = Vec::new();
        for base in bases {
            let response = client
                .get(format!("{base}/api/graph"))
                .send()
                .await
                .unwrap_or_else(|error| panic!("GET {base}/api/graph: {error}"));
            assert_eq!(response.status(), StatusCode::OK, "{base}");
            bodies.push(response.text().await.expect("read body"));
        }
        for (index, body) in bodies.iter().enumerate().skip(1) {
            assert_eq!(&bodies[0], body, "cross-read body {index}");
        }
    });
}
