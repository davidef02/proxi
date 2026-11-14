//! The Google Drive provider and its PKCE OAuth (port of `app/server/sync/gdrive.ts`).
//!
//! The engine decides what moves; this module only talks to Google. Everything here is blocking, so it
//! must run on a plain thread (or inside `spawn_blocking`), never on a tokio worker.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use indexmap::IndexMap;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::sync::github::parse_rfc3339_ms;
use crate::sync::types::{ProviderError, RemoteStat, SyncProvider};

pub const GOOGLE_CLIENT_ID: &str = "596427399450-dbi9kfci4g4hfmdic4l5o8shr6vetbna.apps.googleusercontent.com";
pub const GOOGLE_CLIENT_SECRET: &str = "";
pub const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
pub const DRIVE_FOLDER: &str = crate::data_root::DATA_DIR_NAME;
pub const DRIVE_APP_PROPERTY: &str = "proxi";
pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
pub const FILE_FIELDS: &str = "id,size,md5Checksum,modifiedTime";

pub const NO_CLIENT: &str = "Client Google non configurato: imposta GOOGLE_CLIENT_ID in crates/proxi/src/sync/gdrive.rs oppure googleClientId in .learner/settings.json.";
pub const REVOKED: &str = "Accesso a Google scaduto o revocato: riconnetti.";
pub const NO_REFRESH: &str = "Google non ha restituito un refresh token: riprova.";
pub const NO_ACCESS: &str = "Google non ha restituito un access token: riprova.";

pub const DEFAULT_GOOGLE_AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const DEFAULT_GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";
pub const DEFAULT_GOOGLE_REVOKE: &str = "https://oauth2.googleapis.com/revoke";
pub const DEFAULT_GOOGLE_API: &str = "https://www.googleapis.com/drive/v3";
pub const DEFAULT_GOOGLE_UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3";

/// The five Google bases. `--google-url` points all of them at the same fake in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoogleEndpoints {
    pub auth: String,
    pub token: String,
    pub revoke: String,
    pub api: String,
    pub upload: String,
}

impl GoogleEndpoints {
    pub fn from_override(base: Option<&str>) -> Self {
        match base {
            Some(base) => {
                let base = base.trim_end_matches('/');
                Self {
                    auth: format!("{base}/o/oauth2/v2/auth"),
                    token: format!("{base}/token"),
                    revoke: format!("{base}/revoke"),
                    api: format!("{base}/drive/v3"),
                    upload: format!("{base}/upload/drive/v3"),
                }
            }
            None => Self {
                auth: DEFAULT_GOOGLE_AUTH.to_string(),
                token: DEFAULT_GOOGLE_TOKEN.to_string(),
                revoke: DEFAULT_GOOGLE_REVOKE.to_string(),
                api: DEFAULT_GOOGLE_API.to_string(),
                upload: DEFAULT_GOOGLE_UPLOAD.to_string(),
            },
        }
    }
}

impl Default for GoogleEndpoints {
    fn default() -> Self {
        Self::from_override(None)
    }
}

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// HTTP deps shared by every call. `clone` shares one client.
///
/// Build it only on a blocking thread: reqwest's blocking client starts (and drops) a shell runtime
/// in debug, which panics when the thread already belongs to tokio.
#[derive(Clone)]
pub struct GoogleDeps {
    pub endpoints: GoogleEndpoints,
    pub now: Clock,
    client: reqwest::blocking::Client,
}

impl GoogleDeps {
    pub fn new(endpoints: GoogleEndpoints) -> Self {
        Self::with_clock(endpoints, Arc::new(crate::sync::engine::now_ms))
    }

    pub fn with_clock(endpoints: GoogleEndpoints, now: Clock) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("build Google HTTP client");
        Self {
            endpoints,
            now,
            client,
        }
    }
}

/// Lowercase hex from the OS random generator.
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("OS random generator");
    let mut out = String::with_capacity(bytes * 2);
    for byte in buffer {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// base64url-no-pad(sha256(bytes)).
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

/// PKCE pair (RFC 7636, S256). The 48 random bytes encode to exactly 64 url-safe chars.
pub fn pkce_pair() -> PkcePair {
    let verifier = URL_SAFE_NO_PAD.encode({
        let mut bytes = [0u8; 48];
        getrandom::fill(&mut bytes).expect("OS random generator");
        bytes
    });
    let challenge = challenge_for(&verifier);
    PkcePair {
        verifier,
        challenge,
    }
}

/// JS `encodeURIComponent`: every UTF-8 byte except `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// WHATWG `URLSearchParams` body: the order of `pairs` is kept, a space becomes `+`.
fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

pub fn auth_url(
    e: &GoogleEndpoints,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Result<String, String> {
    if client_id.is_empty() {
        return Err(NO_CLIENT.to_string());
    }
    let form = form_encode(&[
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", DRIVE_SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ]);
    Ok(format!("{}?{form}", e.auth))
}

fn token_request(d: &GoogleDeps, pairs: &[(&str, &str)]) -> Result<Value, String> {
    let response = d
        .client
        .post(&d.endpoints.token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_encode(pairs))
        .send()
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let text = response.text().map_err(|e| e.to_string())?;
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        if json.get("error").and_then(Value::as_str) == Some("invalid_grant") {
            return Err(REVOKED.to_string());
        }
        let detail = json
            .get("error_description")
            .and_then(Value::as_str)
            .or_else(|| json.get("error").and_then(Value::as_str))
            .unwrap_or("errore sconosciuto");
        return Err(format!("Google {status}: {detail}"));
    }
    Ok(json)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub refresh_token: String,
    pub access_token: String,
    pub expires_at: f64,
}

pub fn exchange_code(
    d: &GoogleDeps,
    client_id: &str,
    client_secret: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Tokens, String> {
    if client_id.is_empty() {
        return Err(NO_CLIENT.to_string());
    }
    let mut pairs = vec![("code", code), ("client_id", client_id)];
    if !client_secret.is_empty() {
        pairs.push(("client_secret", client_secret));
    }
    pairs.push(("redirect_uri", redirect_uri));
    pairs.push(("grant_type", "authorization_code"));
    pairs.push(("code_verifier", verifier));
    let json = token_request(d, &pairs)?;

    let refresh_token = json
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| NO_REFRESH.to_string())?
        .to_string();
    let access_token = json
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| NO_ACCESS.to_string())?
        .to_string();
    let expires_in = json
        .get("expires_in")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Ok(Tokens {
        refresh_token,
        access_token,
        expires_at: (d.now)() as f64 + (expires_in - 60.0) * 1000.0,
    })
}

pub struct GoogleAuth {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    token: Option<(String, f64)>,
}

impl GoogleAuth {
    pub fn new(client_id: &str, client_secret: &str, refresh_token: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            refresh_token: refresh_token.to_string(),
            token: None,
        }
    }

    pub fn access_token(&mut self, d: &GoogleDeps, force: bool) -> Result<String, String> {
        if !force
            && let Some((value, expires_at)) = &self.token
            && ((d.now)() as f64) < *expires_at
        {
            return Ok(value.clone());
        }
        if self.client_id.is_empty() {
            return Err(NO_CLIENT.to_string());
        }
        let mut pairs = vec![("client_id", self.client_id.as_str())];
        if !self.client_secret.is_empty() {
            pairs.push(("client_secret", self.client_secret.as_str()));
        }
        pairs.push(("refresh_token", self.refresh_token.as_str()));
        pairs.push(("grant_type", "refresh_token"));
        let json = token_request(d, &pairs)?;
        let access_token = json
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| NO_ACCESS.to_string())?
            .to_string();
        let expires_in = json
            .get("expires_in")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let expires_at = (d.now)() as f64 + (expires_in - 60.0) * 1000.0;
        self.token = Some((access_token.clone(), expires_at));
        Ok(access_token)
    }
}

/// Best-effort: never fails, like the Node version.
pub fn revoke_token(d: &GoogleDeps, token: &str) {
    let _ = d
        .client
        .post(&d.endpoints.revoke)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_encode(&[("token", token)]))
        .send();
}

/// JS `String(value)` for the error JSON fields (an object becomes `[object Object]`).
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::String(text) => text.clone(),
        Value::Number(number) => match number.as_f64() {
            Some(value) if value.fract() == 0.0 && value.abs() < 1e21 => format!("{value:.0}"),
            _ => number.to_string(),
        },
        Value::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::String(text) => !text.is_empty(),
        Value::Number(number) => number.as_f64().map(|v| v != 0.0).unwrap_or(false),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Port of `errorMessage` (`gdrive.ts:151-160`).
fn drive_error_message(text: &str, status: u16) -> String {
    let fallback = || {
        if text.is_empty() {
            reqwest::StatusCode::from_u16(status)
                .ok()
                .and_then(|code| code.canonical_reason())
                .unwrap_or("")
                .to_string()
        } else {
            text.to_string()
        }
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) else {
        return fallback();
    };
    if let Some(Value::Object(error)) = map.get("error")
        && let Some(message) = error.get("message")
        && js_truthy(message)
    {
        return js_string(message);
    }
    if let Some(value) = map.get("error_description").filter(|value| !value.is_null()) {
        return js_string(value);
    }
    if let Some(value) = map.get("error").filter(|value| !value.is_null()) {
        return js_string(value);
    }
    text.to_string()
}

fn send(
    d: &GoogleDeps,
    auth: &mut GoogleAuth,
    force: bool,
    method: &reqwest::Method,
    url: &str,
    content_type: Option<&str>,
    body: Option<&[u8]>,
) -> Result<reqwest::blocking::Response, String> {
    let token = auth.access_token(d, force)?;
    let mut request = d
        .client
        .request(method.clone(), url)
        .header("Authorization", format!("Bearer {token}"));
    if let Some(content_type) = content_type {
        request = request.header("Content-Type", content_type);
    }
    if let Some(body) = body {
        request = request.body(body.to_vec());
    }
    request.send().map_err(|e| e.to_string())
}

/// Drive API call. On 401 the access token is refreshed once and the call retried.
fn drive_call(
    d: &GoogleDeps,
    auth: &mut GoogleAuth,
    method: &str,
    url: &str,
    content_type: Option<&str>,
    body: Option<Vec<u8>>,
) -> Result<reqwest::blocking::Response, String> {
    let http_method = reqwest::Method::from_bytes(method.as_bytes()).expect("known HTTP method");
    let mut response = send(
        d,
        auth,
        false,
        &http_method,
        url,
        content_type,
        body.as_deref(),
    )?;
    if response.status().as_u16() == 401 {
        response = send(
            d,
            auth,
            true,
            &http_method,
            url,
            content_type,
            body.as_deref(),
        )?;
    }
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let text = response.text().unwrap_or_default();
        return Err(format!(
            "Google Drive {status}: {}",
            drive_error_message(&text, status)
        ));
    }
    Ok(response)
}

fn response_json(response: reqwest::blocking::Response) -> Result<Value, String> {
    let text = response.text().map_err(|e| e.to_string())?;
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

pub fn ensure_folder(d: &GoogleDeps, auth: &mut GoogleAuth) -> Result<String, String> {
    let q = format!(
        "mimeType='{FOLDER_MIME}' and trashed=false and appProperties has {{ key='{DRIVE_APP_PROPERTY}' and value='data' }}"
    );
    let url = format!(
        "{}/files?q={}&fields=files(id)&spaces=drive",
        d.endpoints.api,
        encode_uri_component(&q)
    );
    let found = response_json(drive_call(d, auth, "GET", &url, None, None)?)?;
    if let Some(id) = found.pointer("/files/0/id").and_then(Value::as_str) {
        return Ok(id.to_string());
    }

    let mut properties = serde_json::Map::new();
    properties.insert(
        DRIVE_APP_PROPERTY.to_string(),
        Value::String("data".to_string()),
    );
    let meta = json!({
        "name": DRIVE_FOLDER,
        "mimeType": FOLDER_MIME,
        "appProperties": properties,
    });
    let created = response_json(drive_call(
        d,
        auth,
        "POST",
        &format!("{}/files", d.endpoints.api),
        Some("application/json"),
        Some(meta.to_string().into_bytes()),
    )?)?;
    Ok(created
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

pub fn drive_email(d: &GoogleDeps, auth: &mut GoogleAuth) -> Option<String> {
    let url = format!("{}/about?fields=user(emailAddress)", d.endpoints.api);
    let response = drive_call(d, auth, "GET", &url, None, None).ok()?;
    let json = response_json(response).ok()?;
    Some(json.pointer("/user/emailAddress")?.as_str()?.to_string())
}

/// multipart/related upload: part 1 is the JSON metadata, part 2 the file bytes.
fn upload(
    d: &GoogleDeps,
    auth: &mut GoogleAuth,
    url: &str,
    method: &str,
    meta: &Value,
    data: &[u8],
) -> Result<Value, String> {
    let boundary = format!("proxi-{}", random_hex(12));
    let mut body = Vec::with_capacity(data.len() + 256);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n",
            meta
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
    let response = drive_call(
        d,
        auth,
        method,
        url,
        Some(&format!("multipart/related; boundary={boundary}")),
        Some(body),
    )?;
    response_json(response)
}

/// RFC 1321 MD5, lowercase hex. A change token compared with Drive's `md5Checksum`, not a security primitive.
pub fn md5_hex(data: &[u8]) -> String {
    const SHIFTS: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    for chunk in message.chunks(64) {
        let mut words = [0u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            let start = index * 4;
            *word = u32::from_le_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a
                .wrapping_add(f)
                .wrapping_add(K[i])
                .wrapping_add(words[g])
                .rotate_left(SHIFTS[i]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

pub struct GdriveProvider {
    deps: GoogleDeps,
    auth: GoogleAuth,
    folder_id: String,
    /// rel -> every Drive file id with that name, newest first.
    ids: IndexMap<String, Vec<String>>,
    /// rel -> md5 of the newest copy at list time (the write-time guard).
    tokens: HashMap<String, String>,
}

impl GdriveProvider {
    pub fn new(deps: GoogleDeps, auth: GoogleAuth, folder_id: &str) -> Self {
        Self {
            deps,
            auth,
            folder_id: folder_id.to_string(),
            ids: IndexMap::new(),
            tokens: HashMap::new(),
        }
    }
}

impl SyncProvider for GdriveProvider {
    fn key(&self) -> String {
        format!("gdrive:{}", self.folder_id)
    }

    fn list(&mut self) -> Result<Vec<(String, RemoteStat)>, ProviderError> {
        self.ids.clear();
        self.tokens.clear();
        let q = format!("'{}' in parents and trashed=false", self.folder_id);
        let fields = "nextPageToken,files(id,name,size,md5Checksum,modifiedTime)";
        let mut found: IndexMap<String, Vec<(String, String, u64, f64)>> = IndexMap::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/files?q={}&fields={}&pageSize=1000&spaces=drive",
                self.deps.endpoints.api,
                encode_uri_component(&q),
                encode_uri_component(fields),
            );
            if let Some(token) = &page_token {
                url.push_str(&format!("&pageToken={}", encode_uri_component(token)));
            }
            let page = response_json(drive_call(
                &self.deps,
                &mut self.auth,
                "GET",
                &url,
                None,
                None,
            )?)?;
            let files = page
                .get("files")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for file in files {
                let Some(md5) = file.get("md5Checksum").and_then(Value::as_str) else {
                    continue; // folders and Google-native docs
                };
                if md5.is_empty() {
                    continue;
                }
                let name = file
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let id = file
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let size = file
                    .get("size")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                let mtime_ms = file
                    .get("modifiedTime")
                    .and_then(Value::as_str)
                    .map(parse_rfc3339_ms)
                    .unwrap_or(f64::NAN);
                found
                    .entry(name)
                    .or_default()
                    .push((id, md5.to_string(), size, mtime_ms));
            }
            page_token = page
                .get("nextPageToken")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }

        let mut out = Vec::with_capacity(found.len());
        for (name, mut copies) in found {
            // Duplicate names: the newest is the file, the rest are kept for remove().
            copies.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
            let newest = copies[0].clone();
            self.ids
                .insert(name.clone(), copies.iter().map(|copy| copy.0.clone()).collect());
            self.tokens.insert(name.clone(), newest.1.clone());
            out.push((
                name,
                RemoteStat {
                    size: newest.2,
                    mtime_ms: newest.3,
                    token: newest.1,
                },
            ));
        }
        Ok(out)
    }

    fn read(&mut self, rel: &str) -> Result<Vec<u8>, ProviderError> {
        let Some(id) = self.ids.get(rel).and_then(|ids| ids.first()).cloned() else {
            return Err(ProviderError::Other(format!(
                "File remoto sconosciuto: {rel}"
            )));
        };
        let url = format!("{}/files/{id}?alt=media", self.deps.endpoints.api);
        let response = drive_call(&self.deps, &mut self.auth, "GET", &url, None, None)?;
        Ok(response
            .bytes()
            .map_err(|e| ProviderError::Other(e.to_string()))?
            .to_vec())
    }

    fn write(&mut self, rel: &str, data: &[u8], mtime_ms: f64) -> Result<RemoteStat, ProviderError> {
        let modified_time = crate::events::iso_from_millis(mtime_ms.trunc() as i64);
        let id = self.ids.get(rel).and_then(|ids| ids.first()).cloned();
        let file = match id {
            None => {
                let mut properties = serde_json::Map::new();
                properties.insert(
                    DRIVE_APP_PROPERTY.to_string(),
                    Value::String("file".to_string()),
                );
                let meta = json!({
                    "name": rel,
                    "parents": [&self.folder_id],
                    "appProperties": properties,
                    "modifiedTime": modified_time,
                });
                let url = format!(
                    "{}/files?uploadType=multipart&fields={FILE_FIELDS}",
                    self.deps.endpoints.upload
                );
                upload(&self.deps, &mut self.auth, &url, "POST", &meta, data)?
            }
            Some(id) => {
                // Guard: refuse to overwrite a file another PC changed since list(). The check and
                // the PATCH are two requests, so a change in between is not caught (Drive has no
                // compare-and-swap here).
                let url = format!("{}/files/{id}?fields=md5Checksum", self.deps.endpoints.api);
                let current = response_json(drive_call(
                    &self.deps,
                    &mut self.auth,
                    "GET",
                    &url,
                    None,
                    None,
                )?)?;
                if current.get("md5Checksum").and_then(Value::as_str)
                    != self.tokens.get(rel).map(String::as_str)
                {
                    return Err(ProviderError::RemoteMoved);
                }
                let meta = json!({ "modifiedTime": modified_time });
                let url = format!(
                    "{}/files/{id}?uploadType=multipart&fields={FILE_FIELDS}",
                    self.deps.endpoints.upload
                );
                upload(&self.deps, &mut self.auth, &url, "PATCH", &meta, data)?
            }
        };
        let file_id = file
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let md5 = file
            .get("md5Checksum")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let size = file
            .get("size")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(data.len() as u64);
        let mtime = file
            .get("modifiedTime")
            .and_then(Value::as_str)
            .map(parse_rfc3339_ms)
            .unwrap_or(f64::NAN);
        let others: Vec<String> = self
            .ids
            .get(rel)
            .map(|ids| {
                ids.iter()
                    .filter(|other| **other != file_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let all = std::iter::once(file_id)
            .chain(others)
            .collect::<Vec<String>>();
        self.ids.insert(rel.to_string(), all);
        self.tokens.insert(rel.to_string(), md5.clone());
        Ok(RemoteStat {
            size,
            mtime_ms: mtime,
            token: md5,
        })
    }

    fn remove(&mut self, rel: &str) -> Result<(), ProviderError> {
        let Some(all) = self.ids.get(rel).cloned() else {
            return Ok(());
        };
        if all.is_empty() {
            return Ok(());
        }
        // Every copy goes, not only the newest: a surviving duplicate would reappear on the next
        // list() with old bytes.
        let body = serde_json::to_vec(&json!({ "trashed": true }))
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        for id in &all {
            let url = format!("{}/files/{id}", self.deps.endpoints.api);
            drive_call(
                &self.deps,
                &mut self.auth,
                "PATCH",
                &url,
                Some("application/json"),
                Some(body.clone()),
            )?;
        }
        self.ids.shift_remove(rel);
        self.tokens.remove(rel);
        Ok(())
    }

    fn commit(&mut self, _message: &str) -> Result<(), ProviderError> {
        // Drive has no batch commit: every write and removal is already applied.
        Ok(())
    }

    fn content_equals(&self, data: &[u8], r: &RemoteStat) -> Option<bool> {
        Some(md5_hex(data) == r.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_hex_matches_the_rfc_1321_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
        assert_eq!(
            md5_hex(&vec![b'a'; 1_000_000]),
            "7707d6ae4e027c70eea2a935c2296f21"
        );
        assert_eq!(md5_hex(&[b'a'; 55]), "ef1772b6dff9a122358552954ad0df65");
        assert_eq!(md5_hex(&[b'a'; 56]), "3b0c8ac703f828b04c6c197006d17218");
        assert_eq!(md5_hex(&[b'a'; 64]), "014842d480b571495a4a0363793f7367");
    }

    #[test]
    fn encode_uri_component_keeps_the_javascript_unreserved_set() {
        assert_eq!(
            encode_uri_component("a b'(*)!~/?&=é"),
            "a%20b'(*)!~%2F%3F%26%3D%C3%A9"
        );
    }

    #[test]
    fn endpoints_from_override_maps_the_five_bases() {
        let endpoints = GoogleEndpoints::from_override(Some("http://x/"));
        assert_eq!(endpoints.auth, "http://x/o/oauth2/v2/auth");
        assert_eq!(endpoints.token, "http://x/token");
        assert_eq!(endpoints.revoke, "http://x/revoke");
        assert_eq!(endpoints.api, "http://x/drive/v3");
        assert_eq!(endpoints.upload, "http://x/upload/drive/v3");

        assert_eq!(
            GoogleEndpoints::default(),
            GoogleEndpoints {
                auth: DEFAULT_GOOGLE_AUTH.to_string(),
                token: DEFAULT_GOOGLE_TOKEN.to_string(),
                revoke: DEFAULT_GOOGLE_REVOKE.to_string(),
                api: DEFAULT_GOOGLE_API.to_string(),
                upload: DEFAULT_GOOGLE_UPLOAD.to_string(),
            }
        );
    }
}
