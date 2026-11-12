mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use reqwest::{Client, Method, StatusCode};
use serde_json::Value;

use proxi::server::{start, RunningServer, ServerOptions};

/// The real Node server, killed on drop so a failing test never leaks a process.
struct NodeServer {
    child: Child,
    port: u16,
}

impl NodeServer {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for NodeServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_node(repo: &Path) -> NodeServer {
    let repo_arg = repo.display().to_string();
    let child = common::node(&["crates/proxi/tests/node/serve.ts", &repo_arg])
        .env_remove("LEARNER_DATA_ROOT")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn Node server");
    let mut server = NodeServer { child, port: 0 };

    let stdout = server.child.stdout.take().expect("stdout pipe");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "Node server did not print PORT in 60 s");
        let line = rx
            .recv_timeout(remaining)
            .expect("Node server exited before printing PORT");
        if let Some(raw) = line.trim().strip_prefix("PORT ") {
            server.port = raw.trim().parse().expect("PORT line has a number");
            return server;
        }
    }
}

struct Side {
    status: StatusCode,
    value: Value,
}

struct Same {
    status: StatusCode,
    node: Value,
    rust: Value,
}

struct Pair {
    client: Client,
    node: NodeServer,
    rust: RunningServer,
    node_repo: PathBuf,
    rust_repo: PathBuf,
}

impl Pair {
    /// Sends one request to both servers and asserts they answer the same.
    async fn same(&self, method: Method, path: &str, body: Option<&str>) -> Same {
        let node = self
            .request(&self.node.url(), &self.node_repo, &method, path, body)
            .await;
        let rust = self
            .request(&self.rust.url, &self.rust_repo, &method, path, body)
            .await;

        assert_eq!(node.status, rust.status, "{method} {path}");
        if node.status.is_success() {
            assert_eq!(
                serde_json::to_string(&node.value).expect("serialise"),
                serde_json::to_string(&rust.value).expect("serialise"),
                "{method} {path}"
            );
        } else {
            assert!(
                node.status.is_client_error(),
                "{method} {path}: unexpected status {}",
                node.status
            );
            assert!(
                node.value["error"].is_string(),
                "{method} {path}: {}",
                node.value
            );
            assert!(
                rust.value["error"].is_string(),
                "{method} {path}: {}",
                rust.value
            );
        }

        if method == Method::POST {
            let a = fs::read(proxi::settings::settings_file(&self.node_repo))
                .unwrap_or_else(|error| panic!("Node settings.json after {path}: {error}"));
            let b = fs::read(proxi::settings::settings_file(&self.rust_repo))
                .unwrap_or_else(|error| panic!("Rust settings.json after {path}: {error}"));
            assert_eq!(a, b, "settings.json bytes after POST {path}");
        }

        Same {
            status: node.status,
            node: node.value,
            rust: rust.value,
        }
    }

    async fn request(
        &self,
        base: &str,
        repo: &Path,
        method: &Method,
        path: &str,
        body: Option<&str>,
    ) -> Side {
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
        let mut value: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{method} {path}: not JSON ({error}): {text}"));

        if status.is_success() {
            let repo_str = proxi::data_root::resolve_path(repo).display().to_string();
            for (key, placeholder) in [("repoRoot", "<repo>"), ("dataRoot", "<data>")] {
                if let Some(field) = value.get_mut(key) {
                    assert_eq!(field.as_str(), Some(repo_str.as_str()), "{key}: {text}");
                    *field = Value::String(placeholder.to_string());
                }
            }
        }

        Side { status, value }
    }
}

fn write_limits(repo: &Path, raw: &str) {
    let file = proxi::limits::limits_file(repo);
    fs::create_dir_all(file.parent().expect("limits file has a parent")).expect("create .learner");
    fs::write(file, raw).expect("write limits.json");
}

#[test]
fn settings_and_limits_match_node() {
    common::require_node();

    let node_repo = common::temp_dir("parity-node");
    let rust_repo = common::temp_dir("parity-rust");
    assert!(fs::read_dir(&node_repo.0).unwrap().next().is_none());
    assert!(fs::read_dir(&rust_repo.0).unwrap().next().is_none());

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let node = start_node(&node_repo.0);
        let mut options = ServerOptions::new(rust_repo.0.clone());
        options.data_root = Some(rust_repo.0.clone());
        let rust = start(options).await.expect("start Rust server");
        let pair = Pair {
            client: Client::new(),
            node,
            rust,
            node_repo: node_repo.0.clone(),
            rust_repo: rust_repo.0.clone(),
        };

        // Defaults: same ordered JSON from both.
        let body = pair.same(Method::GET, "/api/settings", None).await;
        assert_eq!(body.rust["provider"], "claude");
        assert_eq!(body.rust["anthropicApiKey"], "");
        assert_eq!(body.rust["repoRoot"], "<repo>");
        assert_eq!(body.rust["dataRoot"], "<data>");

        let body = pair.same(Method::GET, "/api/limits", None).await;
        assert_eq!(body.rust, Value::Null);

        // Limits: the first successful read sticks in memory on both (limits.ts:24-33).
        let first = r#"{"fiveHour":{"utilization":0.42,"resetsAt":1700000000},"at":"2026-11-12T10:00:00.000Z"}"#;
        write_limits(&node_repo.0, first);
        write_limits(&rust_repo.0, first);
        let body = pair.same(Method::GET, "/api/limits", None).await;
        assert_eq!(body.rust["fiveHour"]["utilization"], 0.42);
        assert_eq!(body.rust["fiveHour"]["resetsAt"], 1700000000);

        write_limits(&node_repo.0, r#"{"at":"2026-11-12T11:00:00.000Z"}"#);
        write_limits(&rust_repo.0, r#"{"at":"2026-11-12T11:00:00.000Z"}"#);
        let body = pair.same(Method::GET, "/api/limits", None).await;
        assert_eq!(
            body.rust["at"], "2026-11-12T10:00:00.000Z",
            "the first read stays cached until restart"
        );

        let body = pair
            .same(
                Method::POST,
                "/api/settings",
                Some(r#"{"anthropicApiKey":"sk-ant-1234567890abcd","effort":{"teacher":"high"},"agyModels":{"teacher":"t","subagent":"s","map":"m"}}"#),
            )
            .await;
        assert_eq!(body.rust["anthropicApiKey"], "sk-ant-…abcd");
        assert_eq!(body.rust["effort"]["teacher"], "high");
        assert_eq!(body.rust["agyModels"]["teacher"], "t");

        let body = pair.same(Method::GET, "/api/settings", None).await;
        assert_eq!(body.rust["autoMap"], true);
        assert_eq!(body.rust["effort"]["subagent"], "default");
        assert_eq!(body.rust["agyModels"]["map"], "m");

        // The masked key the UI sends back is ignored: the stored one survives.
        let body = pair
            .same(
                Method::POST,
                "/api/settings",
                Some(r#"{"anthropicApiKey":"sk-ant-…abcd","autoMap":false}"#),
            )
            .await;
        assert_eq!(body.rust["anthropicApiKey"], "sk-ant-…abcd");
        assert_eq!(body.rust["autoMap"], false);

        let body = pair
            .same(
                Method::POST,
                "/api/settings",
                Some(r#"{"anthropicApiKey":"short"}"#),
            )
            .await;
        assert_eq!(body.rust["anthropicApiKey"], "••••••••");

        let body = pair.same(Method::GET, "/api/settings", None).await;
        assert_eq!(body.rust["anthropicApiKey"], "••••••••");
        assert_eq!(body.rust["autoMap"], false);

        // apiKey auth turns the subscription meter off.
        let body = pair
            .same(Method::POST, "/api/settings", Some(r#"{"claudeAuth":"apiKey"}"#))
            .await;
        assert_eq!(body.rust["claudeAuth"], "apiKey");
        let body = pair.same(Method::GET, "/api/limits", None).await;
        assert_eq!(body.rust, Value::Null);

        // Unknown key: 400 with the same shape on both.
        let body = pair
            .same(Method::POST, "/api/settings", Some(r#"{"nope":1}"#))
            .await;
        assert_eq!(body.status, StatusCode::BAD_REQUEST);

        // Invalid JSON: 400 and the exact same body.
        let body = pair.same(Method::POST, "/api/settings", Some("{")).await;
        assert_eq!(body.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::to_string(&body.node).unwrap(),
            r#"{"error":"Invalid JSON body"}"#
        );
        assert_eq!(
            serde_json::to_string(&body.rust).unwrap(),
            r#"{"error":"Invalid JSON body"}"#
        );

        // googleClientId is a valid 300-char string setting (settings.ts:26) and round-trips.
        let google_client_id = "g".repeat(300);
        let body = pair
            .same(
                Method::POST,
                "/api/settings",
                Some(&format!(
                    r#"{{"googleClientId":"{google_client_id}"}}"#
                )),
            )
            .await;
        assert_eq!(body.status, StatusCode::OK);
        assert_eq!(body.rust["googleClientId"], google_client_id);

        // A bad file is read one key at a time: only the invalid values fall back.
        let lenient = r#"{"provider":"bogus","teacherModel":"opus","effort":{"map":"low"},"googleClientId":"client-id-123","extra":1}"#;
        for repo in [&node_repo.0, &rust_repo.0] {
            fs::write(proxi::settings::settings_file(repo), lenient).expect("overwrite settings");
        }
        let body = pair.same(Method::GET, "/api/settings", None).await;
        assert_eq!(body.rust["provider"], "claude");
        assert_eq!(body.rust["teacherModel"], "opus");
        assert_eq!(body.rust["effort"]["map"], "low");
        assert_eq!(body.rust["effort"]["teacher"], "default");
        assert_eq!(body.rust["claudeAuth"], "subscription");
    });
}
