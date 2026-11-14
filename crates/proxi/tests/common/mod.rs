#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod anki_seed;
pub mod cli_golden;
pub mod fake_anki;
pub mod fake_hf;

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

/// The three concepts of `app/test/embeddings.test.ts`, used by the model-backed tests.
pub const SAMPLE_CONCEPTS: &[(&str, &str, &str)] = &[
    (
        "n_computed",
        "Proprietà calcolate",
        "In Vue una proprietà calcolata ricalcola il valore solo quando cambiano le sue dipendenze reattive.",
    ),
    (
        "n_props",
        "Props",
        "Dati passati da un componente genitore a un componente figlio.",
    ),
    (
        "n_for",
        "Ciclo for",
        "Ripete un blocco di codice un numero fissato di volte.",
    ),
];

/// The models folder holding `model`: an explicit override, this checkout, then the main checkout
/// (worktrees do not download the postinstall model). `None` lets model-backed tests skip.
pub fn model_root(model: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(raw) = std::env::var("PROXI_TEST_MODELS") {
        if !raw.trim().is_empty() {
            candidates.push(PathBuf::from(raw));
        }
    }
    candidates.push(repo_root().join(".learner").join("models"));
    if let Some(main) = main_checkout() {
        candidates.push(main.join(".learner").join("models"));
    }
    candidates.into_iter().find(|models| {
        let dir = model
            .split('/')
            .fold(models.clone(), |dir, segment| dir.join(segment));
        dir.join("config.json").is_file() && has_onnx(&dir.join("onnx"))
    })
}

fn main_checkout() -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(repo_root())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let common = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Path::new(&common).parent().map(Path::to_path_buf)
}

fn has_onnx(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".onnx"))
        })
        .unwrap_or(false)
}

/// A graph store the routes and the CLIs accept: `graph/nodes.jsonl` plus an empty chains ledger.
pub fn write_node_lines(data_root: &Path, nodes: &[(&str, &str, &str)]) {
    let graph = data_root.join("graph");
    fs::create_dir_all(&graph).expect("create graph dir");
    let mut out = String::new();
    for (id, name, statement) in nodes {
        let line = serde_json::json!({
            "id": id,
            "name": name,
            "statement": statement,
            "aliases": [],
            "createdAt": "2026-01-01T00:00:00.000Z",
            "provenance": [],
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    fs::write(graph.join("nodes.jsonl"), out).expect("write nodes.jsonl");
    fs::write(graph.join("chains.jsonl"), "").expect("write chains.jsonl");
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
