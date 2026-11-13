//! The GitHub provider and its OAuth device flow (port of `app/server/sync/github.ts`).
//!
//! The engine decides what moves; this module only talks to the GitHub REST API. Everything here is
//! blocking, so it must run on a plain thread (or inside `spawn_blocking`), never on a tokio worker.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use indexmap::IndexMap;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::sync::types::{ProviderError, RemoteStat, SyncProvider};

pub const GITHUB_CLIENT_ID: &str = "Ov23li8h0SouuYu9iGlp";
pub const DATA_REPO: &str = crate::data_root::DATA_DIR_NAME;
pub const DEFAULT_API: &str = "https://api.github.com";
pub const DEFAULT_WEB: &str = "https://github.com";

const README_TEXT: &str = "# proxi-data\n\nDati di studio sincronizzati da Proxi.\n";

/// The two GitHub bases. `--github-url` points both at the same fake in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubEndpoints {
    pub api: String,
    pub web: String,
}

impl GithubEndpoints {
    pub fn from_override(base: Option<&str>) -> Self {
        match base {
            Some(base) => {
                let base = base.trim_end_matches('/');
                Self {
                    api: base.to_string(),
                    web: base.to_string(),
                }
            }
            None => Self {
                api: DEFAULT_API.to_string(),
                web: DEFAULT_WEB.to_string(),
            },
        }
    }
}

impl Default for GithubEndpoints {
    fn default() -> Self {
        Self::from_override(None)
    }
}

/// Cancels a device-flow poll: set the flag, wake every waiter.
pub struct CancelToken {
    cancelled: Mutex<bool>,
    signal: Condvar,
}

impl CancelToken {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: Mutex::new(false),
            signal: Condvar::new(),
        })
    }

    pub fn cancel(&self) {
        *self.cancelled.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.signal.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sleeps at most `d`; true when the token was cancelled first.
    pub fn wait(&self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        let mut cancelled = self.cancelled.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if *cancelled {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, _) = self
                .signal
                .wait_timeout(cancelled, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            cancelled = guard;
        }
    }
}

pub type Sleeper = Arc<dyn Fn(Duration, &CancelToken) + Send + Sync>;

pub fn real_sleeper() -> Sleeper {
    Arc::new(|d: Duration, token: &CancelToken| {
        token.wait(d);
    })
}

/// HTTP deps shared by every call. `clone` shares one client.
///
/// Build it only on a blocking thread: reqwest's blocking client starts (and drops) a shell runtime
/// in debug, which panics when the thread already belongs to tokio.
#[derive(Clone)]
pub struct GithubDeps {
    pub endpoints: GithubEndpoints,
    pub sleep: Sleeper,
    client: reqwest::blocking::Client,
}

impl GithubDeps {
    pub fn new(endpoints: GithubEndpoints, sleep: Sleeper) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("build GitHub HTTP client");
        Self {
            endpoints,
            sleep,
            client,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: f64,
    pub interval: f64,
}

/// Git blob id of `data`: sha1("blob <len>\0" + data). Equals the `sha` GitHub reports for the blob.
pub fn git_blob_sha(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", data.len()).as_bytes());
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn assert_client_id(client_id: &str) -> Result<(), String> {
    if client_id.is_empty() {
        return Err(
            "Client GitHub non configurato: imposta GITHUB_CLIENT_ID in crates/proxi/src/sync/github.rs \
             oppure githubClientId in .learner/settings.json."
                .to_string(),
        );
    }
    Ok(())
}

fn text_field(json: &Value, key: &str) -> Option<String> {
    json.get(key).and_then(Value::as_str).map(str::to_string)
}

fn device_error(json: &Value, status: u16) -> String {
    text_field(json, "error_description")
        .or_else(|| text_field(json, "error"))
        .unwrap_or_else(|| format!("GitHub {status}"))
}

fn post_json(d: &GithubDeps, url: &str, body: &Value) -> Result<(u16, Value), String> {
    let response = d
        .client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let text = response.text().map_err(|e| e.to_string())?;
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((status, json))
}

pub fn start_device_flow(d: &GithubDeps, client_id: &str) -> Result<DeviceCode, String> {
    assert_client_id(client_id)?;
    let (status, json) = post_json(
        d,
        &format!("{}/login/device/code", d.endpoints.web),
        &json!({ "client_id": client_id, "scope": "repo" }),
    )?;
    let failed = status >= 400
        || json
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|e| !e.is_empty());
    if failed {
        return Err(device_error(&json, status));
    }
    Ok(DeviceCode {
        device_code: text_field(&json, "device_code").unwrap_or_default(),
        user_code: text_field(&json, "user_code").unwrap_or_default(),
        verification_uri: text_field(&json, "verification_uri").unwrap_or_default(),
        expires_in: json.get("expires_in").and_then(Value::as_f64).unwrap_or(0.0),
        interval: json.get("interval").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

pub fn poll_device_flow(
    d: &GithubDeps,
    client_id: &str,
    code: &DeviceCode,
    cancel: &CancelToken,
) -> Result<String, String> {
    assert_client_id(client_id)?;
    let mut interval = code.interval;
    let mut waited = 0.0;
    loop {
        if cancel.is_cancelled() {
            return Err("Annullato.".to_string());
        }
        if waited >= code.expires_in * 1000.0 {
            return Err("Codice scaduto: riprova.".to_string());
        }
        (d.sleep.as_ref())(Duration::from_millis((interval * 1000.0) as u64), cancel);
        waited += interval * 1000.0;
        if cancel.is_cancelled() {
            return Err("Annullato.".to_string());
        }
        let (status, json) = post_json(
            d,
            &format!("{}/login/oauth/access_token", d.endpoints.web),
            &json!({
                "client_id": client_id,
                "device_code": code.device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }),
        )?;
        if let Some(token) = json.get("access_token").and_then(Value::as_str) {
            if !token.is_empty() {
                return Ok(token.to_string());
            }
        }
        match json.get("error").and_then(Value::as_str).unwrap_or("") {
            "authorization_pending" => {}
            "slow_down" => {
                interval = json
                    .get("interval")
                    .and_then(Value::as_f64)
                    .unwrap_or(interval + 5.0);
            }
            "expired_token" => return Err("Codice scaduto: riprova.".to_string()),
            "access_denied" => return Err("Accesso a GitHub negato.".to_string()),
            _ => return Err(device_error(&json, status)),
        }
    }
}

/// One GitHub API failure: `status` so callers can special-case 404 and 422.
#[derive(Clone, Debug, PartialEq)]
pub struct GhError {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for GhError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GitHub {}: {}", self.status, self.message)
    }
}

impl std::error::Error for GhError {}

fn message_or_text(text: &str) -> String {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => map
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| text.to_string()),
        _ => text.to_string(),
    }
}

fn transport_error(error: reqwest::Error) -> GhError {
    GhError {
        status: 0,
        message: error.to_string(),
    }
}

/// One GitHub API call. Waits out rate limits (403/429) at most twice, capped at 60 s per wait.
fn gh(
    d: &GithubDeps,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, GhError> {
    let url = format!("{}{}", d.endpoints.api, path);
    let http_method = reqwest::Method::from_bytes(method.as_bytes()).expect("known HTTP method");
    let mut attempt = 0;
    loop {
        let mut request = d
            .client
            .request(http_method.clone(), &url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "proxi");
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().map_err(transport_error)?;
        let status = response.status().as_u16();
        if status == 403 || status == 429 {
            let headers = response.headers();
            let retry_after = headers
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty());
            let remaining = headers
                .get("x-ratelimit-remaining")
                .and_then(|value| value.to_str().ok());
            let reset = headers
                .get("x-ratelimit-reset")
                .and_then(|value| value.to_str().ok());
            if attempt < 2 && (retry_after.is_some() || remaining == Some("0")) {
                let secs = match retry_after {
                    Some(raw) => raw.trim().parse::<f64>().unwrap_or(f64::NAN),
                    None => {
                        let reset = reset
                            .and_then(|value| value.trim().parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs() as f64)
                            .unwrap_or(0.0);
                        (reset - now.floor()).max(0.0)
                    }
                };
                // A NaN from an unparsable retry-after behaves like Node's setTimeout(NaN): no wait.
                let millis = if secs.is_nan() {
                    0.0
                } else {
                    secs.min(60.0) * 1000.0
                };
                (d.sleep.as_ref())(Duration::from_millis(millis as u64), &CancelToken::new());
                attempt += 1;
                continue;
            }
        }
        if !(200..300).contains(&status) {
            let text = response.text().map_err(transport_error)?;
            return Err(GhError {
                status,
                message: message_or_text(&text),
            });
        }
        if status == 204 {
            return Ok(Value::Null);
        }
        let text = response.text().map_err(transport_error)?;
        return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RepoInfo {
    pub login: String,
    pub repo: String,
    pub branch: String,
}

pub fn setup_repo(d: &GithubDeps, token: &str) -> Result<RepoInfo, String> {
    let user = gh(d, token, "GET", "/user", None).map_err(|e| e.to_string())?;
    let login = text_field(&user, "login").unwrap_or_default();
    let repo = match gh(d, token, "GET", &format!("/repos/{login}/{DATA_REPO}"), None) {
        Ok(repo) => repo,
        Err(e) if e.status == 404 => gh(
            d,
            token,
            "POST",
            "/user/repos",
            Some(&json!({
                "name": DATA_REPO,
                "private": true,
                "auto_init": true,
                "description": "Proxi learner data (synced by the app)",
            })),
        )
        .map_err(|e| e.to_string())?,
        Err(e) => return Err(e.to_string()),
    };
    if repo.get("private") == Some(&Value::Bool(false)) {
        return Err(format!(
            "Il repository {DATA_REPO} è pubblico: rendilo privato o rinominalo, poi riprova."
        ));
    }
    Ok(RepoInfo {
        login,
        repo: text_field(&repo, "full_name").unwrap_or_default(),
        branch: text_field(&repo, "default_branch").unwrap_or_default(),
    })
}

/// `Date.parse` for the two shapes GitHub sends: `2026-03-01T10:00:00Z` and a fractional second.
/// Milliseconds since the Unix epoch, `NaN` when the text is not such a timestamp.
fn parse_rfc3339_ms(text: &str) -> f64 {
    let Some((date, time)) = text.split_once('T') else {
        return f64::NAN;
    };
    let mut date_parts = date.split('-');
    let (Some(year), Some(month), Some(day)) = (
        date_parts.next().and_then(|v| v.parse::<i64>().ok()),
        date_parts.next().and_then(|v| v.parse::<i64>().ok()),
        date_parts.next().and_then(|v| v.parse::<i64>().ok()),
    ) else {
        return f64::NAN;
    };
    let time = time.strip_suffix('Z').unwrap_or(time);
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, fraction),
        None => (time, ""),
    };
    let mut clock_parts = clock.split(':');
    let (Some(hour), Some(minute), Some(second)) = (
        clock_parts.next().and_then(|v| v.parse::<i64>().ok()),
        clock_parts.next().and_then(|v| v.parse::<i64>().ok()),
        clock_parts.next().and_then(|v| v.parse::<i64>().ok()),
    ) else {
        return f64::NAN;
    };
    let mut digits: String = fraction.chars().take(3).collect();
    while digits.len() < 3 {
        digits.push('0');
    }
    let millis = digits.parse::<f64>().unwrap_or(0.0);
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    seconds as f64 * 1000.0 + millis
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub struct GithubProvider {
    deps: GithubDeps,
    token: String,
    repo: String,
    branch: String,
    /// Head commit and tree from the last `list`.
    head: Option<(String, String)>,
    /// rel -> blob sha at head.
    entries: IndexMap<String, String>,
    /// rel -> bytes (write) or None (remove), flushed by `commit`.
    staged: IndexMap<String, Option<Vec<u8>>>,
}

impl GithubProvider {
    pub fn new(deps: GithubDeps, token: &str, repo: &str, branch: &str) -> Self {
        Self {
            deps,
            token: token.to_string(),
            repo: repo.to_string(),
            branch: branch.to_string(),
            head: None,
            entries: IndexMap::new(),
            staged: IndexMap::new(),
        }
    }

    fn base(&self) -> String {
        format!("/repos/{}", self.repo)
    }

    fn head_sha(&self) -> Result<String, ProviderError> {
        let ref_path = format!("{}/git/ref/heads/{}", self.base(), self.branch);
        match gh(&self.deps, &self.token, "GET", &ref_path, None) {
            Ok(value) => Ok(sha_of(&value, "/object/sha")),
            Err(e) if e.status == 404 || e.status == 409 => {
                // Empty repo: seed a README so the branch exists, then read the ref again.
                gh(
                    &self.deps,
                    &self.token,
                    "PUT",
                    &format!("{}/contents/README.md", self.base()),
                    Some(&json!({
                        "message": "init",
                        "content": STANDARD.encode(README_TEXT.as_bytes()),
                        "branch": self.branch,
                    })),
                )
                .map_err(|e| e.to_string())?;
                let value = gh(&self.deps, &self.token, "GET", &ref_path, None)
                    .map_err(|e| e.to_string())?;
                Ok(sha_of(&value, "/object/sha"))
            }
            Err(e) => Err(ProviderError::from(e.to_string())),
        }
    }
}

fn sha_of(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl SyncProvider for GithubProvider {
    fn key(&self) -> String {
        format!("github:{}", self.repo)
    }

    fn list(&mut self) -> Result<Vec<(String, RemoteStat)>, ProviderError> {
        self.staged.clear();
        let commit_sha = self.head_sha()?;
        let commit = gh(
            &self.deps,
            &self.token,
            "GET",
            &format!("{}/git/commits/{commit_sha}", self.base()),
            None,
        )
        .map_err(|e| e.to_string())?;
        let tree_sha = sha_of(&commit, "/tree/sha");
        let mtime_ms = commit
            .pointer("/committer/date")
            .and_then(Value::as_str)
            .map(parse_rfc3339_ms)
            .unwrap_or(f64::NAN);
        let tree = gh(
            &self.deps,
            &self.token,
            "GET",
            &format!("{}/git/trees/{tree_sha}?recursive=1", self.base()),
            None,
        )
        .map_err(|e| e.to_string())?;
        if tree.get("truncated").and_then(Value::as_bool) == Some(true) {
            return Err(ProviderError::Other(
                "Repository troppo grande per la sincronizzazione.".to_string(),
            ));
        }

        self.head = Some((commit_sha, tree_sha));
        self.entries.clear();
        let mut out = Vec::new();
        let entries = tree
            .get("tree")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in entries {
            let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
            let mode = entry.get("mode").and_then(Value::as_str).unwrap_or("");
            if kind != "blob" || mode == "120000" {
                continue;
            }
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let token = entry
                .get("sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let size = entry.get("size").and_then(Value::as_u64).unwrap_or(0);
            self.entries.insert(path.clone(), token.clone());
            out.push((
                path,
                RemoteStat {
                    size,
                    mtime_ms,
                    token,
                },
            ));
        }
        Ok(out)
    }

    fn read(&mut self, rel: &str) -> Result<Vec<u8>, ProviderError> {
        let Some(sha) = self.entries.get(rel).cloned() else {
            return Err(ProviderError::Other(format!(
                "File remoto sconosciuto: {rel}"
            )));
        };
        let blob = gh(
            &self.deps,
            &self.token,
            "GET",
            &format!("{}/git/blobs/{sha}", self.base()),
            None,
        )
        .map_err(|e| e.to_string())?;
        // GitHub wraps the base64 every 60 chars; the STANDARD engine rejects the newlines.
        let content: String = blob
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        STANDARD
            .decode(content.as_bytes())
            .map_err(|e| ProviderError::Other(e.to_string()))
    }

    fn write(&mut self, rel: &str, data: &[u8], mtime_ms: f64) -> Result<RemoteStat, ProviderError> {
        self.staged.insert(rel.to_string(), Some(data.to_vec()));
        Ok(RemoteStat {
            size: data.len() as u64,
            mtime_ms,
            token: git_blob_sha(data),
        })
    }

    fn remove(&mut self, rel: &str) -> Result<(), ProviderError> {
        self.staged.insert(rel.to_string(), None);
        Ok(())
    }

    fn content_equals(&self, data: &[u8], r: &RemoteStat) -> Option<bool> {
        Some(git_blob_sha(data) == r.token)
    }

    fn commit(&mut self, message: &str) -> Result<(), ProviderError> {
        if self.staged.is_empty() {
            return Ok(());
        }
        let Some((head_commit, head_tree)) = self.head.clone() else {
            return Err(ProviderError::Other("commit prima di list()".to_string()));
        };
        let mut tree = Vec::new();
        for (rel, data) in &self.staged {
            let Some(data) = data else {
                tree.push(json!({ "path": rel, "mode": "100644", "type": "blob", "sha": null }));
                continue;
            };
            let blob = gh(
                &self.deps,
                &self.token,
                "POST",
                &format!("{}/git/blobs", self.base()),
                Some(&json!({
                    "content": STANDARD.encode(data),
                    "encoding": "base64",
                })),
            )
            .map_err(|e| e.to_string())?;
            tree.push(json!({
                "path": rel,
                "mode": "100644",
                "type": "blob",
                "sha": sha_of(&blob, "/sha"),
            }));
        }
        let new_tree = gh(
            &self.deps,
            &self.token,
            "POST",
            &format!("{}/git/trees", self.base()),
            Some(&json!({ "base_tree": head_tree, "tree": tree })),
        )
        .map_err(|e| e.to_string())?;
        let tree_sha = sha_of(&new_tree, "/sha");
        let new_commit = gh(
            &self.deps,
            &self.token,
            "POST",
            &format!("{}/git/commits", self.base()),
            Some(&json!({
                "message": message,
                "tree": tree_sha,
                "parents": [head_commit],
            })),
        )
        .map_err(|e| e.to_string())?;
        let commit_sha = sha_of(&new_commit, "/sha");
        match gh(
            &self.deps,
            &self.token,
            "PATCH",
            &format!("{}/git/refs/heads/{}", self.base(), self.branch),
            Some(&json!({ "sha": commit_sha, "force": false })),
        ) {
            Ok(_) => {}
            Err(e) if e.status == 422 => return Err(ProviderError::RemoteMoved),
            Err(e) => return Err(ProviderError::from(e.to_string())),
        }
        for (rel, data) in &self.staged {
            match data {
                None => {
                    self.entries.shift_remove(rel);
                }
                Some(data) => {
                    self.entries.insert(rel.clone(), git_blob_sha(data));
                }
            }
        }
        self.head = Some((commit_sha, tree_sha));
        self.staged.clear();
        Ok(())
    }
}
