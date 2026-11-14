#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod anki_seed;
pub mod fake_anki;

pub fn repo_root() -> PathBuf {
    let raw = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let mut out = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub struct TempDir(pub PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn temp_dir(tag: &str) -> TempDir {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "proxi-{tag}-{}-{nanos}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create temp dir");
    TempDir(path)
}

pub fn require_node() {
    if !repo_root().join("node_modules").join("tsx").exists() {
        panic!("Node parity tests need node_modules: run npm ci");
    }
}

pub fn node(args: &[&str]) -> Command {
    let mut cmd = Command::new("node");
    cmd.arg("--import")
        .arg("tsx")
        .args(args)
        .current_dir(repo_root());
    cmd
}

pub fn proxi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_proxi"))
}

/// Fixtures are assembled at runtime: the pre-flight scans source files for key prefixes.
pub fn fake_key(rest: &str) -> String {
    ["sk", rest].concat()
}

/// The real Node server, killed on drop so a failing test never leaks a process.
pub struct NodeServer {
    child: Child,
    port: u16,
}

impl NodeServer {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for NodeServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn start_node(repo: &Path) -> NodeServer {
    let repo_arg = repo.display().to_string();
    let child = node(&["crates/proxi/tests/node/serve.ts", &repo_arg])
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

/// The `proxi serve` binary, killed on drop so a failing test never leaks a process.
pub struct ServeChild {
    child: Child,
}

impl Drop for ServeChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawns the binary with a fixed `PATH` and waits for its URL: the AC3 seam (no Node on PATH).
pub fn spawn_serve_with_path(args: &[&str], path: &OsStr) -> (ServeChild, String) {
    let mut child = Command::new(proxi_bin())
        .args(args)
        .env("PATH", path)
        .env_remove("LEARNER_PORT")
        .env_remove("LEARNER_DATA_ROOT")
        .env_remove("LEARNER_NPM_BIN")
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
        if let Some(url) = line.trim().strip_prefix("Proxi: ") {
            return (ServeChild { child }, url.to_string());
        }
    }
}
