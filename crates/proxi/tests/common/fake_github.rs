//! A stateful in-memory GitHub for the provider tests: blobs, trees, commits, one branch ref and the
//! device-flow endpoints. Port of the in-test `fakeGithub` of `app/test/sync-github.test.ts`.
//!
//! `tests/sync_github.rs` declares the modules as
//! `#[path = "common/fake_http.rs"] mod fake_http;` then `#[path = "common/fake_github.rs"] mod fake_github;`.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use proxi::sync::github::{GithubEndpoints, git_blob_sha};

use crate::fake_http::{FakeHttp, Recorded, Reply};

pub const SEED_DATE: &str = "2026-03-01T10:00:00Z";
pub const NEW_DATE: &str = "2026-03-02T10:00:00Z";
pub const LOGIN: &str = "davide";
pub const FULL: &str = "davide/proxi-data";

const README: &[u8] = b"# proxi-data\n\nDati di studio sincronizzati da Proxi.\n";

#[derive(Clone, Debug)]
struct Entry {
    sha: String,
    size: usize,
    mode: String,
}

#[derive(Clone, Debug)]
struct Commit {
    tree: String,
    date: String,
    parents: Vec<String>,
}

/// Everything the fake remembers between requests. Tests flip the rate-limit/ref switches directly.
pub struct GhState {
    pub repo: String,
    pub ref_sha: Option<String>,
    pub ref_moved: bool,
    pub rate_limit_once: bool,
    pub rate_limit_reset_once: Option<u64>,
    pub truncated: bool,
    pub device_replies: Vec<Value>,
    seq: usize,
    blobs: HashMap<String, Vec<u8>>,
    trees: HashMap<String, BTreeMap<String, Entry>>,
    commits: HashMap<String, Commit>,
}

impl Default for GhState {
    fn default() -> Self {
        Self {
            repo: "private".to_string(),
            ref_sha: None,
            ref_moved: false,
            rate_limit_once: false,
            rate_limit_reset_once: None,
            truncated: false,
            device_replies: Vec::new(),
            seq: 0,
            blobs: HashMap::new(),
            trees: HashMap::new(),
            commits: HashMap::new(),
        }
    }
}

impl GhState {
    fn next_id(&mut self, kind: &str) -> String {
        self.seq += 1;
        format!("{kind}{}", self.seq)
    }
}

pub struct FakeGithubOpts {
    /// "private" | "public" | "none".
    pub repo: &'static str,
    /// Initial commit content (rel path -> utf8), omitted means an empty tree.
    pub seed: Vec<(String, String)>,
    /// Extra entries committed with mode 120000.
    pub symlinks: Vec<String>,
    /// Start with no ref at all (an empty repository).
    pub empty_repo: bool,
    /// Answers to `POST /login/oauth/access_token`, shifted one per poll. Empty means pending forever.
    pub device_replies: Vec<Value>,
}

impl Default for FakeGithubOpts {
    fn default() -> Self {
        Self {
            repo: "private",
            seed: Vec::new(),
            symlinks: Vec::new(),
            empty_repo: false,
            device_replies: Vec::new(),
        }
    }
}

pub struct FakeGithub {
    pub http: FakeHttp,
    pub state: Arc<Mutex<GhState>>,
}

impl FakeGithub {
    pub fn start(opts: FakeGithubOpts) -> Self {
        let state = Arc::new(Mutex::new(GhState {
            repo: opts.repo.to_string(),
            device_replies: opts.device_replies,
            ..GhState::default()
        }));
        {
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            if st.repo != "none" {
                let mut tree = BTreeMap::new();
                for (rel, text) in &opts.seed {
                    let data = text.as_bytes();
                    tree.insert(
                        rel.clone(),
                        Entry {
                            sha: put_blob(&mut st, data),
                            size: data.len(),
                            mode: "100644".to_string(),
                        },
                    );
                }
                for rel in &opts.symlinks {
                    let data = b"target";
                    tree.insert(
                        rel.clone(),
                        Entry {
                            sha: put_blob(&mut st, data),
                            size: data.len(),
                            mode: "120000".to_string(),
                        },
                    );
                }
                if !opts.empty_repo {
                    st.ref_sha = Some(put_commit(&mut st, tree, Vec::new(), SEED_DATE));
                }
            }
        }
        let handler_state = Arc::clone(&state);
        let http = FakeHttp::start(move |request| handle(&handler_state, request));
        FakeGithub { http, state }
    }

    /// One base for the API and the OAuth endpoints, like `--github-url` does.
    pub fn endpoints(&self) -> GithubEndpoints {
        GithubEndpoints::from_override(Some(&self.http.url))
    }

    pub fn count(&self, method: &str, path_ends_with: &str) -> usize {
        self.http
            .requests()
            .iter()
            .filter(|request| request.method == method && request.path.ends_with(path_ends_with))
            .count()
    }

    /// Blob paths of the head commit's tree, sorted. Empty without a ref.
    pub fn head_paths(&self) -> Vec<String> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(head) = &st.ref_sha else {
            return Vec::new();
        };
        let Some(commit) = st.commits.get(head) else {
            return Vec::new();
        };
        let Some(tree) = st.trees.get(&commit.tree) else {
            return Vec::new();
        };
        tree.keys().cloned().collect()
    }
}

fn handle(state: &Arc<Mutex<GhState>>, request: &Recorded) -> Reply {
    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
    let path = request.path.split('?').next().unwrap_or("").to_string();
    let method = request.method.as_str();
    let body = request.json();

    // The next request fails once, so a caller can be checked for waiting and retrying.
    if st.rate_limit_once {
        st.rate_limit_once = false;
        return Reply {
            status: 403,
            headers: vec![("retry-after".to_string(), "1".to_string())],
            body: b"{}".to_vec(),
        };
    }
    if let Some(reset) = st.rate_limit_reset_once.take() {
        return Reply {
            status: 403,
            headers: vec![
                ("x-ratelimit-remaining".to_string(), "0".to_string()),
                ("x-ratelimit-reset".to_string(), reset.to_string()),
            ],
            body: b"{}".to_vec(),
        };
    }

    if method == "POST" && path == "/login/device/code" {
        return Reply::json(
            200,
            json!({
                "device_code": "dev-1",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                "interval": 5,
            }),
        );
    }
    if method == "POST" && path == "/login/oauth/access_token" {
        let reply = if st.device_replies.is_empty() {
            json!({ "error": "authorization_pending" })
        } else {
            st.device_replies.remove(0)
        };
        return Reply::json(200, reply);
    }
    if method == "GET" && path == "/user" {
        return Reply::json(200, json!({ "login": LOGIN }));
    }
    if method == "GET" && path == format!("/repos/{FULL}") {
        if st.repo == "none" {
            return Reply::json(404, json!({ "message": "Not Found" }));
        }
        return Reply::json(
            200,
            json!({
                "full_name": FULL,
                "private": st.repo == "private",
                "default_branch": "main",
            }),
        );
    }
    if method == "POST" && path == "/user/repos" {
        let private = body.get("private").and_then(Value::as_bool).unwrap_or(false);
        st.repo = if private { "private" } else { "public" }.to_string();
        if body.get("auto_init").and_then(Value::as_bool) == Some(true) {
            let mut tree = BTreeMap::new();
            tree.insert(
                "README.md".to_string(),
                Entry {
                    sha: put_blob(&mut st, README),
                    size: README.len(),
                    mode: "100644".to_string(),
                },
            );
            st.ref_sha = Some(put_commit(&mut st, tree, Vec::new(), SEED_DATE));
        }
        return Reply::json(
            201,
            json!({ "full_name": FULL, "private": private, "default_branch": "main" }),
        );
    }

    const PREFIX: &str = "/repos/davide/proxi-data/";
    if let Some(rest) = path.strip_prefix(PREFIX) {
        if st.repo == "none" {
            return Reply::json(404, json!({ "message": "Not Found" }));
        }
        return repo_reply(&mut st, method, rest, &body);
    }
    Reply::json(404, json!({ "message": "unhandled" }))
}

fn repo_reply(st: &mut GhState, method: &str, rest: &str, body: &Value) -> Reply {
    match (method, rest) {
        ("GET", "git/ref/heads/main") => match &st.ref_sha {
            Some(sha) => Reply::json(200, json!({ "object": { "sha": sha } })),
            None => Reply::json(409, json!({ "message": "Git Repository is empty." })),
        },
        ("PUT", "contents/README.md") => {
            let data = decode_b64(body.get("content").and_then(Value::as_str).unwrap_or(""));
            let mut tree = st
                .ref_sha
                .as_ref()
                .and_then(|sha| st.commits.get(sha))
                .and_then(|commit| st.trees.get(&commit.tree))
                .cloned()
                .unwrap_or_default();
            tree.insert(
                "README.md".to_string(),
                Entry {
                    sha: put_blob(st, &data),
                    size: data.len(),
                    mode: "100644".to_string(),
                },
            );
            let parents: Vec<String> = st.ref_sha.iter().cloned().collect();
            st.ref_sha = Some(put_commit(st, tree, parents, NEW_DATE));
            Reply::json(201, json!({}))
        }
        ("GET", _) if rest.starts_with("git/commits/") => {
            let sha = &rest["git/commits/".len()..];
            match st.commits.get(sha) {
                Some(commit) => Reply::json(
                    200,
                    json!({
                        "sha": sha,
                        "tree": { "sha": commit.tree },
                        "committer": { "date": commit.date },
                        "parents": commit.parents.iter().map(|p| json!({ "sha": p })).collect::<Vec<_>>(),
                    }),
                ),
                None => not_found(),
            }
        }
        ("GET", _) if rest.starts_with("git/trees/") => {
            let sha = rest["git/trees/".len()..].split('?').next().unwrap_or("");
            match st.trees.get(sha) {
                Some(tree) => Reply::json(
                    200,
                    json!({
                        "sha": sha,
                        "truncated": st.truncated,
                        "tree": tree_entries(tree),
                    }),
                ),
                None => not_found(),
            }
        }
        ("GET", _) if rest.starts_with("git/blobs/") => {
            let sha = &rest["git/blobs/".len()..];
            match st.blobs.get(sha) {
                Some(data) => Reply::json(
                    200,
                    json!({
                        "sha": sha,
                        "size": data.len(),
                        "encoding": "base64",
                        "content": wrap_b64(data),
                    }),
                ),
                None => not_found(),
            }
        }
        ("POST", "git/blobs") => {
            let data = decode_b64(body.get("content").and_then(Value::as_str).unwrap_or(""));
            Reply::json(201, json!({ "sha": put_blob(st, &data) }))
        }
        ("POST", "git/trees") => {
            let base = body.get("base_tree").and_then(Value::as_str).unwrap_or("");
            let mut tree = st.trees.get(base).cloned().unwrap_or_default();
            let changes = body
                .get("tree")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for change in changes {
                let path = change
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match change.get("sha") {
                    Some(Value::Null) | None => {
                        tree.remove(&path);
                    }
                    Some(sha) => {
                        let sha = sha.as_str().unwrap_or("").to_string();
                        let size = st.blobs.get(&sha).map(Vec::len).unwrap_or(0);
                        let mode = change
                            .get("mode")
                            .and_then(Value::as_str)
                            .unwrap_or("100644")
                            .to_string();
                        tree.insert(path, Entry { sha, size, mode });
                    }
                }
            }
            let sha = put_tree(st, tree);
            Reply::json(201, json!({ "sha": sha }))
        }
        ("POST", "git/commits") => {
            let tree = body
                .get("tree")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let parents = body
                .get("parents")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            let sha = put_commit_with_tree(st, tree, parents, NEW_DATE);
            Reply::json(201, json!({ "sha": sha }))
        }
        ("PATCH", "git/refs/heads/main") => {
            if st.ref_moved || body.get("force") != Some(&Value::Bool(false)) {
                return Reply::json(422, json!({ "message": "Update is not a fast forward" }));
            }
            let sha = body.get("sha").and_then(Value::as_str).unwrap_or("");
            let fast_forward = st
                .commits
                .get(sha)
                .map(|commit| commit.parents.first().cloned() == st.ref_sha)
                .unwrap_or(false);
            if !fast_forward {
                return Reply::json(422, json!({ "message": "Update is not a fast forward" }));
            }
            st.ref_sha = Some(sha.to_string());
            Reply::json(200, json!({ "object": { "sha": sha } }))
        }
        _ => not_found(),
    }
}

fn not_found() -> Reply {
    Reply::json(404, json!({ "message": "Not Found" }))
}

fn put_blob(st: &mut GhState, data: &[u8]) -> String {
    let sha = git_blob_sha(data);
    st.blobs.insert(sha.clone(), data.to_vec());
    sha
}

fn put_tree(st: &mut GhState, tree: BTreeMap<String, Entry>) -> String {
    let sha = st.next_id("tree");
    st.trees.insert(sha.clone(), tree);
    sha
}

fn put_commit_with_tree(
    st: &mut GhState,
    tree: String,
    parents: Vec<String>,
    date: &str,
) -> String {
    let sha = st.next_id("commit");
    st.commits.insert(
        sha.clone(),
        Commit {
            tree,
            date: date.to_string(),
            parents,
        },
    );
    sha
}

fn put_commit(st: &mut GhState, tree: BTreeMap<String, Entry>, parents: Vec<String>, date: &str) -> String {
    let tree = put_tree(st, tree);
    put_commit_with_tree(st, tree, parents, date)
}

/// `{path, mode, type, sha, size}` for every blob plus one tree entry per directory, sorted by path.
fn tree_entries(tree: &BTreeMap<String, Entry>) -> Vec<Value> {
    let mut entries = Vec::new();
    let mut dirs = BTreeSet::new();
    for (path, entry) in tree {
        entries.push(json!({
            "path": path,
            "mode": entry.mode,
            "type": "blob",
            "sha": entry.sha,
            "size": entry.size,
        }));
        for (index, _) in path.match_indices('/') {
            dirs.insert(path[..index].to_string());
        }
    }
    for dir in dirs {
        entries.push(json!({
            "path": dir,
            "mode": "040000",
            "type": "tree",
            "sha": format!("dir-{dir}"),
        }));
    }
    entries.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    entries
}

/// GitHub wraps base64 content every 60 chars, with a newline at the end.
fn wrap_b64(data: &[u8]) -> String {
    let encoded = STANDARD.encode(data);
    let mut out = String::new();
    for chunk in encoded.as_bytes().chunks(60) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        out.push('\n');
    }
    out
}

fn decode_b64(text: &str) -> Vec<u8> {
    let cleaned: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    STANDARD.decode(cleaned.as_bytes()).unwrap_or_default()
}
