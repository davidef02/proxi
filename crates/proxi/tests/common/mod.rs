#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

pub struct FakeReply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}

/// A fake AnkiConnect on a real port, so `reqwest` exercises the same wire as Anki.
pub struct FakeAnki {
    pub url: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl FakeAnki {
    pub fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().expect("lock Anki requests").clone()
    }
}

pub fn fake_anki<F>(handler: F) -> FakeAnki
where
    F: Fn(&serde_json::Value) -> FakeReply + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake AnkiConnect");
    let address = listener.local_addr().expect("fake AnkiConnect address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let handler = Arc::new(handler);

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let handler = Arc::clone(&handler);
            let recorded = Arc::clone(&recorded);
            thread::spawn(move || {
                let Some(body) = read_request_body(&mut stream) else {
                    return;
                };
                let request =
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                recorded
                    .lock()
                    .expect("lock Anki requests")
                    .push(request.clone());

                let reply = handler(&request);
                if !reply.delay.is_zero() {
                    thread::sleep(reply.delay);
                }
                let reason = match reply.status {
                    200 => "OK",
                    500 => "Internal Server Error",
                    _ => "Unknown",
                };
                let body = reply.body.as_bytes();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    reason,
                    body.len()
                );
                let _ = stream.write_all(body);
                let _ = stream.shutdown(Shutdown::Write);
            });
        }
    });

    FakeAnki {
        url: format!("http://127.0.0.1:{}", address.port()),
        requests,
    }
}

/// Parses a minimal HTTP request: headers up to `\r\n\r\n`, then `Content-Length` bytes.
fn read_request_body(stream: &mut std::net::TcpStream) -> Option<Vec<u8>> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(position) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break position;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    let mut body = raw.split_off(head_end + 4);
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    Some(body)
}

pub fn fake_anki_scripted(responses: &serde_json::Value) -> FakeAnki {
    let responses = responses.clone();
    fake_anki(move |request| {
        let action = request
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let reply = responses
            .get(action)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({ "result": null, "error": "unsupported action" }));
        FakeReply {
            status: 200,
            body: reply.to_string(),
            delay: Duration::ZERO,
        }
    })
}

pub fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind closed port");
    let port = listener.local_addr().expect("closed port address").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}
