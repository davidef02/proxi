mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode};
use serde_json::Value;

use proxi::data_root::resolve_path;

/// Every option AC2 requires `proxi serve --help` to list.
const FLAGS: &[&str] = &[
    "--repo-root",
    "--data-root",
    "--port",
    "--claude-bin",
    "--agy-bin",
    "--opencode-bin",
    "--git-bin",
    "--npm-bin",
    "--anki-url",
    "--github-url",
    "--google-url",
    "--huggingface-url",
    "--transcription-url",
    "--memory-secrets",
];

/// A spawned server, killed on drop so a failing test never leaks a process.
struct ServeProcess {
    child: Child,
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawns `proxi serve` and streams its stdout lines to a channel.
fn spawn_serve(args: &[&str]) -> (ServeProcess, mpsc::Receiver<String>) {
    let mut child = Command::new(common::proxi_bin())
        .args(args)
        .env_remove("LEARNER_PORT")
        .env_remove("LEARNER_DATA_ROOT")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn proxi serve");

    let stdout = child.stdout.take().expect("stdout pipe");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    (ServeProcess { child }, rx)
}

/// Waits for the `Proxi: http://127.0.0.1:<port>` line, at most 30 s.
fn wait_for_url(rx: &mpsc::Receiver<String>) -> (String, u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "proxi serve did not print `Proxi: <url>` in 30 s"
        );
        let line = rx
            .recv_timeout(remaining)
            .expect("proxi serve exited before printing the URL");
        let Some(url) = line.trim().strip_prefix("Proxi: ") else {
            continue;
        };
        let port = url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|raw| raw.parse().ok())
            .expect("Proxi: line carries http://127.0.0.1:<port>");
        return (url.to_string(), port);
    }
}

fn root_string(path: &Path) -> String {
    resolve_path(path).display().to_string()
}

#[test]
fn serve_help_lists_the_test_options() {
    let output = Command::new(common::proxi_bin())
        .args(["serve", "--help"])
        .output()
        .expect("run proxi serve --help");
    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    for flag in FLAGS {
        assert!(stdout.contains(flag), "{flag} missing from help:\n{stdout}");
    }
}

#[test]
fn serve_port_zero_binds_a_free_port_and_uses_the_root_flags() {
    let root = common::temp_dir("serve-cli-free-port");
    let repo = root.0.join("repo");
    let data = root.0.join("data");
    fs::create_dir_all(&repo).expect("create repo");
    fs::create_dir_all(&data).expect("create data");

    let repo_arg = repo.display().to_string();
    let data_arg = data.display().to_string();
    let (_server, rx) = spawn_serve(&[
        "serve",
        "--port",
        "0",
        "--repo-root",
        &repo_arg,
        "--data-root",
        &data_arg,
    ]);
    let (url, port) = wait_for_url(&rx);
    assert_ne!(port, 0);
    assert_ne!(port, 4317);

    let body = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            let response = Client::new()
                .get(format!("{url}/api/settings"))
                .send()
                .await
                .expect("GET /api/settings");
            assert_eq!(response.status(), StatusCode::OK);
            response.text().await.expect("read settings")
        });
    let settings: Value = serde_json::from_str(&body).expect("settings JSON");
    assert_eq!(settings["repoRoot"], root_string(&repo));
    assert_eq!(settings["dataRoot"], root_string(&data));
}

#[test]
fn serve_rejects_an_out_of_range_port_flag() {
    let output = Command::new(common::proxi_bin())
        .args(["serve", "--port", "70000"])
        .output()
        .expect("run proxi serve --port 70000");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--port"), "{stderr}");
}
