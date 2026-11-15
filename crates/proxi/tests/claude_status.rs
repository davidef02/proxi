//! The Claude CLI probe: `<bin> --version` over the node mocks of `claude-version.test.ts:68-120`.

mod common;

use std::fs;
use std::time::Duration;

use proxi::claude_status::{CLAUDE_MIN_VERSION, probe_claude_cli};
use proxi::protocol::ClaudeStatusResponse;

fn mock(dir: &common::TempDir, body: &str) -> String {
    let script = dir.0.join("mock-claude.mjs");
    fs::write(&script, body).expect("write the mock claude");
    script.display().to_string()
}

/// The program the probe runs for a `.mjs` mock: Node's own executable, as `process.execPath` in
/// `claude-version.test.ts`.
fn node_program() -> String {
    proxi::child::node_on_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "node".to_string())
}

async fn probe(script: &str) -> ClaudeStatusResponse {
    probe_claude_cli(script, Duration::from_secs(10)).await
}

#[tokio::test]
async fn meets_minimum() {
    common::require_node();
    let dir = common::temp_dir("claude-status-good");
    let script = mock(&dir, "console.log(\"2.1.314 (Claude Code)\");\n");

    let status = probe(&script).await;
    assert_eq!(status.status, "meets-minimum");
    assert_eq!(status.version.as_deref(), Some("2.1.314"));
    assert_eq!(status.min_version, CLAUDE_MIN_VERSION);
    assert_eq!(status.bin_path.as_deref(), Some(script.as_str()));
    assert_eq!(status.error, None);
}

#[tokio::test]
async fn update_required() {
    common::require_node();
    let dir = common::temp_dir("claude-status-old");
    let script = mock(&dir, "console.log(\"2.0.100 (Claude Code)\");\n");

    let status = probe(&script).await;
    assert_eq!(status.status, "update-required");
    assert_eq!(status.version.as_deref(), Some("2.0.100"));
    assert_eq!(status.min_version, CLAUDE_MIN_VERSION);
    assert_eq!(status.bin_path.as_deref(), Some(script.as_str()));
    assert_eq!(status.error, None);
}

#[tokio::test]
async fn failing_process_is_version_unchecked() {
    common::require_node();
    let dir = common::temp_dir("claude-status-fail");
    let script = mock(&dir, "console.error(\"fatal crash\"); process.exit(1);\n");

    let status = probe(&script).await;
    assert_eq!(status.status, "version-unchecked");
    assert_eq!(status.version, None);
    assert_eq!(status.min_version, CLAUDE_MIN_VERSION);
    assert_eq!(status.bin_path.as_deref(), Some(script.as_str()));
    let error = status.error.expect("the failing process has a message");
    assert!(
        error.starts_with("Command failed: "),
        "Node's execFile text: {error}"
    );
}

#[tokio::test]
async fn a_silent_failure_still_ends_with_node_newline() {
    common::require_node();
    let dir = common::temp_dir("claude-status-silent");
    let script = mock(&dir, "process.exit(1);\n");

    let status = probe(&script).await;
    assert_eq!(status.status, "version-unchecked");
    // Node's execFile always appends the newline before the stderr, empty included.
    let expected = format!("Command failed: {} {script} --version\n", node_program());
    assert_eq!(status.error.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn a_timeout_is_a_command_failed_with_the_newline() {
    common::require_node();
    let dir = common::temp_dir("claude-status-timeout");
    let script = mock(&dir, "setTimeout(() => {}, 5000);\n");

    let status = probe_claude_cli(&script, Duration::from_millis(300)).await;
    assert_eq!(status.status, "version-unchecked");
    let expected = format!("Command failed: {} {script} --version\n", node_program());
    assert_eq!(status.error.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn unrecognised_output() {
    common::require_node();
    let dir = common::temp_dir("claude-status-hello");
    let script = mock(&dir, "console.log(\"hello\");\n");

    let status = probe(&script).await;
    assert_eq!(status.status, "version-unchecked");
    assert_eq!(status.version, None);
    assert_eq!(status.min_version, CLAUDE_MIN_VERSION);
    assert_eq!(status.bin_path.as_deref(), Some(script.as_str()));
    assert_eq!(
        status.error.as_deref(),
        Some("Output non riconosciuto: hello")
    );
}

#[tokio::test]
async fn missing_binary_is_not_found() {
    common::require_node();
    let dir = common::temp_dir("claude-status-missing");
    let missing = dir.0.join("missing-claude.exe").display().to_string();

    let status = probe(&missing).await;
    // Key order is Node's: status, minVersion, binPath, error.
    let expected = serde_json::json!({
        "status": "not-found",
        "minVersion": CLAUDE_MIN_VERSION,
        "binPath": &missing,
        "error": format!("spawn {missing} ENOENT"),
    });
    assert_eq!(
        serde_json::to_string(&status).expect("the status is JSON"),
        expected.to_string()
    );
}
