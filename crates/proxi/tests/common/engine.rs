//! Engine-adapter helpers: the fakes as bins, a launch with sane defaults, message collection and a
//! fake MCP endpoint. Shared by the agy/opencode integration tests (T4/T5).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

use proxi::engine::{EngineBin, EngineEvent, EngineLaunch, EngineMsg, Grant, McpServer};
use proxi::protocol::LearnerSettings;

/// A `node <repo>/app/test/<script> [extra…]` bin: the fake engine.
pub fn fake_bin(script: &str, extra: &[&str]) -> EngineBin {
    let fake = super::repo_root()
        .join("app")
        .join("test")
        .join(script);
    let mut args = vec![fake.display().to_string()];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    EngineBin {
        cmd: "node".to_string(),
        args,
    }
}

/// A `node -e <js> --` bin: a throwaway engine with an exact behaviour.
pub fn inline_bin(js: &str) -> EngineBin {
    EngineBin {
        cmd: "node".to_string(),
        args: vec!["-e".to_string(), js.to_string(), "--".to_string()],
    }
}

/// The usual launch of the adapter tests: one read grant, the fake app MCP, no extra dirs.
pub fn launch(bin: EngineBin, dir: &Path, mcp_url: &str, resume: Option<&str>) -> EngineLaunch {
    EngineLaunch {
        bin,
        repo_root: dir.to_path_buf(),
        data_root: None,
        scratch_dir: dir.join("scratch"),
        model: "m".to_string(),
        effort: None,
        system_prompt: "Sei il docente.".to_string(),
        grant: vec![Grant::Read],
        mcp: McpServer {
            name: "app".to_string(),
            url: mcp_url.to_string(),
            token: "tok".to_string(),
        },
        add_dirs: Vec::new(),
        max_turns: None,
        resume: resume.map(str::to_string),
        settings: LearnerSettings::default(),
        on_limits: Arc::new(|_| {}),
    }
}

/// Drains the channel until `done` accepts a message; panics with the messages so far on timeout.
pub async fn collect_until(
    rx: &mut mpsc::UnboundedReceiver<EngineMsg>,
    mut done: impl FnMut(&EngineMsg) -> bool,
    timeout: Duration,
) -> Vec<EngineMsg> {
    let mut messages = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => panic!("the engine channel closed; got {messages:?}"),
            Err(_) => panic!("no engine message within {timeout:?}; got {messages:?}"),
        };
        let finished = done(&msg);
        messages.push(msg);
        if finished {
            return messages;
        }
    }
}

/// The event kinds in Node's spelling, stderr and exit skipped.
pub fn kinds(msgs: &[EngineMsg]) -> Vec<&'static str> {
    msgs.iter().filter_map(event_kind).collect()
}

fn event_kind(msg: &EngineMsg) -> Option<&'static str> {
    match msg {
        EngineMsg::Event(EngineEvent::Init { .. }) => Some("init"),
        EngineMsg::Event(EngineEvent::TextStart { .. }) => Some("text-start"),
        EngineMsg::Event(EngineEvent::TextDelta { .. }) => Some("text-delta"),
        EngineMsg::Event(EngineEvent::Assistant { .. }) => Some("assistant"),
        EngineMsg::Event(EngineEvent::ToolResult { .. }) => Some("tool-result"),
        EngineMsg::Event(EngineEvent::Result { .. }) => Some("result"),
        EngineMsg::Event(EngineEvent::ControlResponse { .. }) => Some("control-response"),
        EngineMsg::Event(EngineEvent::ResumeFailed) => Some("resume-failed"),
        EngineMsg::Event(EngineEvent::ActionDenied { .. }) => Some("action-denied"),
        EngineMsg::Event(EngineEvent::AuthFailed { .. }) => Some("auth-failed"),
        EngineMsg::Event(EngineEvent::Limits { .. }) => Some("limits"),
        EngineMsg::Event(EngineEvent::Ignored) => Some("ignored"),
        _ => None,
    }
}

/// A fake MCP endpoint plus the token every request presented (`None` when the header was absent).
pub struct FakeMcp {
    pub url: String,
    pub tokens: Arc<Mutex<Vec<Option<String>>>>,
}

/// One tool "echo", the mirror of `app/test/engine.test.ts:29-45`.
pub async fn start_fake_mcp() -> FakeMcp {
    let tokens: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = tokens.clone();
    let app = axum::Router::new().route(
        "/mcp/x",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, body: String| async move {
                let message: serde_json::Value =
                    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                let token = headers
                    .get("x-learner-token")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                recorded
                    .lock()
                    .expect("lock MCP tokens")
                    .push(token);
                // A notification (no id) gets the 202 Node's fake answers with.
                let Some(id) = message.get("id") else {
                    return (axum::http::StatusCode::ACCEPTED, String::new());
                };
                let params = message.get("params").cloned().unwrap_or(json!({}));
                let result = match message.get("method").and_then(serde_json::Value::as_str) {
                    Some("initialize") => json!({
                        "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(serde_json::Value::Null),
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "app", "version": "1" },
                    }),
                    Some("tools/list") => json!({
                        "tools": [{
                            "name": "echo",
                            "inputSchema": { "type": "object" },
                            "_meta": { "antigravity/alwaysLoad": true },
                        }],
                    }),
                    _ => json!({
                        "content": [{
                            "type": "text",
                            "text": format!(
                                "eco {}",
                                params.get("arguments").cloned().unwrap_or(json!({}))
                            ),
                        }],
                    }),
                };
                let body = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                (axum::http::StatusCode::OK, body)
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake MCP");
    let address = listener.local_addr().expect("fake MCP address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    FakeMcp {
        url: format!("http://127.0.0.1:{}/mcp/x", address.port()),
        tokens,
    }
}
