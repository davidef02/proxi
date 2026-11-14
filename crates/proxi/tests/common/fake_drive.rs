//! A stateful in-memory Google Drive + OAuth for the provider tests (port of the in-test `fakeDrive`
//! of `app/test/sync-gdrive.test.ts`).
//!
//! `tests/sync_gdrive.rs` declares the modules as
//! `#[path = "common/fake_http.rs"] mod fake_http;` then `#[path = "common/fake_drive.rs"] mod fake_drive;`.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;
use serde_json::{Value, json};

use proxi::sync::gdrive::{
    DRIVE_APP_PROPERTY, FOLDER_MIME, GoogleEndpoints, md5_hex,
};

use crate::fake_http::{FakeHttp, Recorded, Reply};

/// `new Date(0).toISOString()`: the default mtime of a file created without one.
pub const EPOCH: &str = "1970-01-01T00:00:00.000Z";

#[derive(Clone, Debug)]
pub struct FakeFile {
    pub id: String,
    pub name: String,
    pub parents: Vec<String>,
    pub mime_type: Option<String>,
    pub app_properties: BTreeMap<String, String>,
    pub data: Vec<u8>,
    pub modified_time: String,
    pub trashed: bool,
}

pub struct FakeDriveOpts {
    /// Caps every files.list page so pagination is exercised.
    pub page_size: usize,
    /// The refresh token the token endpoint accepts.
    pub refresh_token: String,
    /// The address `/about` reports.
    pub email: String,
}

impl Default for FakeDriveOpts {
    fn default() -> Self {
        Self {
            page_size: 1000,
            refresh_token: "rt-1".to_string(),
            email: "davide@example.com".to_string(),
        }
    }
}

struct DriveState {
    files: IndexMap<String, FakeFile>,
    next_id: usize,
    token_seq: usize,
    valid_access: HashSet<String>,
    refresh_grants: usize,
    omit_refresh: bool,
    about_fails: bool,
    page_size: usize,
    refresh_token: String,
    email: String,
}

impl DriveState {
    fn insert(
        &mut self,
        name: &str,
        parents: &[&str],
        mime_type: Option<&str>,
        app_properties: BTreeMap<String, String>,
        data: &[u8],
        modified_time: &str,
    ) -> String {
        let id = format!("f{}", self.next_id);
        self.next_id += 1;
        self.files.insert(
            id.clone(),
            FakeFile {
                id: id.clone(),
                name: name.to_string(),
                parents: parents.iter().map(|parent| parent.to_string()).collect(),
                mime_type: mime_type.map(str::to_string),
                app_properties,
                data: data.to_vec(),
                modified_time: modified_time.to_string(),
                trashed: false,
            },
        );
        id
    }
}

pub struct FakeDrive {
    pub http: FakeHttp,
    state: Arc<Mutex<DriveState>>,
}

impl FakeDrive {
    pub fn start(opts: FakeDriveOpts) -> Self {
        let state = Arc::new(Mutex::new(DriveState {
            files: IndexMap::new(),
            next_id: 1,
            token_seq: 0,
            valid_access: HashSet::new(),
            refresh_grants: 0,
            omit_refresh: false,
            about_fails: false,
            page_size: opts.page_size,
            refresh_token: opts.refresh_token,
            email: opts.email,
        }));
        let handler_state = Arc::clone(&state);
        let http = FakeHttp::start(move |request| handle(&handler_state, request));
        FakeDrive { http, state }
    }

    fn lock(&self) -> MutexGuard<'_, DriveState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One base for every Google origin, like `--google-url` does.
    pub fn endpoints(&self) -> GoogleEndpoints {
        GoogleEndpoints::from_override(Some(&self.http.url))
    }

    pub fn add_file(
        &self,
        name: &str,
        parents: &[&str],
        data: &[u8],
        modified_time: &str,
    ) -> String {
        self.lock()
            .insert(name, parents, None, BTreeMap::new(), data, modified_time)
    }

    /// A folder, optionally marked with the `proxi` app property (the app folder is `Some("data")`).
    pub fn add_folder(&self, name: &str, app_property: Option<&str>) -> String {
        self.add_folder_in(name, &[], app_property)
    }

    pub fn add_folder_in(&self, name: &str, parents: &[&str], app_property: Option<&str>) -> String {
        let mut properties = BTreeMap::new();
        if let Some(value) = app_property {
            properties.insert(DRIVE_APP_PROPERTY.to_string(), value.to_string());
        }
        self.lock()
            .insert(name, parents, Some(FOLDER_MIME), properties, &[], EPOCH)
    }

    /// A Google-native document: no `md5Checksum`, like Drive reports it.
    pub fn add_native_doc(&self, name: &str, parents: &[&str]) -> String {
        self.lock().insert(
            name,
            parents,
            Some("application/vnd.google-apps.document"),
            BTreeMap::new(),
            &[],
            EPOCH,
        )
    }

    /// Changes a file as if another PC wrote it.
    pub fn set_remote(&self, id: &str, data: &[u8], modified_time: &str) {
        let mut st = self.lock();
        if let Some(file) = st.files.get_mut(id) {
            file.data = data.to_vec();
            file.modified_time = modified_time.to_string();
        }
    }

    pub fn invalidate_access(&self) {
        self.lock().valid_access.clear();
    }

    pub fn refresh_grants(&self) -> usize {
        self.lock().refresh_grants
    }

    pub fn set_omit_refresh(&self, value: bool) {
        self.lock().omit_refresh = value;
    }

    pub fn set_about_fails(&self, value: bool) {
        self.lock().about_fails = value;
    }

    pub fn file(&self, id: &str) -> Option<FakeFile> {
        self.lock().files.get(id).cloned()
    }

    /// Non-trashed files inside `folder`, newest copy per name.
    pub fn files_in(&self, folder: &str) -> BTreeMap<String, Vec<u8>> {
        let st = self.lock();
        let mut out: BTreeMap<String, (String, Vec<u8>)> = BTreeMap::new();
        for file in st.files.values() {
            if file.trashed || !file.parents.iter().any(|parent| parent == folder) {
                continue;
            }
            let newer = match out.get(&file.name) {
                Some((time, _)) => file.modified_time >= *time,
                None => true,
            };
            if newer {
                out.insert(
                    file.name.clone(),
                    (file.modified_time.clone(), file.data.clone()),
                );
            }
        }
        out.into_iter()
            .map(|(name, (_, data))| (name, data))
            .collect()
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.http.requests()
    }

    pub fn writes_to(&self, prefix: &str) -> usize {
        self.http
            .requests()
            .iter()
            .filter(|request| request.method != "GET" && request.path.contains(prefix))
            .count()
    }
}

fn handle(state: &Arc<Mutex<DriveState>>, request: &Recorded) -> Reply {
    let mut st = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = request.path.split('?').next().unwrap_or("").to_string();
    let query = query_params(&request.path);
    let method = request.method.as_str();

    // OAuth endpoints.
    if method == "POST" && path == "/token" {
        let form = form_params(&request.bytes);
        let grant = form
            .get("grant_type")
            .map(String::as_str)
            .unwrap_or_default();
        if grant == "refresh_token" {
            st.refresh_grants += 1;
            if form.get("refresh_token").map(String::as_str) != Some(st.refresh_token.as_str()) {
                return Reply::json(400, json!({ "error": "invalid_grant" }));
            }
        }
        st.token_seq += 1;
        let access = format!("at-{}", st.token_seq);
        st.valid_access.insert(access.clone());
        let mut out = serde_json::Map::new();
        out.insert("access_token".into(), json!(access));
        out.insert("expires_in".into(), json!(3600));
        if grant == "authorization_code" && !st.omit_refresh {
            out.insert("refresh_token".into(), json!(&st.refresh_token));
        }
        return Reply::json(200, Value::Object(out));
    }
    if method == "POST" && path == "/revoke" {
        return Reply::bytes(200, Vec::new());
    }

    // Everything below is the Drive API: check the bearer token.
    let is_drive = path.starts_with("/drive/v3/") || path.starts_with("/upload/drive/v3/");
    if is_drive {
        let authorized = request
            .header("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(|token| st.valid_access.contains(token))
            .unwrap_or(false);
        if !authorized {
            return Reply::json(401, json!({ "error": { "message": "Invalid Credentials" } }));
        }
    }

    if method == "GET" && path == "/drive/v3/about" {
        if st.about_fails {
            return Reply::json(500, json!({ "error": { "message": "boom" } }));
        }
        return Reply::json(200, json!({ "user": { "emailAddress": &st.email } }));
    }

    if method == "GET" && path == "/drive/v3/files" {
        let q = query.get("q").cloned().unwrap_or_default();
        if q.contains(&format!("mimeType='{FOLDER_MIME}'")) {
            if !q.contains("key='proxi'") {
                // A wrong appProperties key must find nothing.
                return Reply::json(200, json!({ "files": [] }));
            }
            let hits: Vec<Value> = st
                .files
                .values()
                .filter(|file| {
                    file.mime_type.as_deref() == Some(FOLDER_MIME)
                        && !file.trashed
                        && file
                            .app_properties
                            .get(DRIVE_APP_PROPERTY)
                            .map(String::as_str)
                            == Some("data")
                })
                .take(1)
                .map(|file| json!({ "id": file.id }))
                .collect();
            return Reply::json(200, json!({ "files": hits }));
        }
        let parent = q
            .strip_prefix('\'')
            .and_then(|rest| rest.split_once("' in parents"))
            .map(|(parent, _)| parent.to_string())
            .unwrap_or_default();
        let all: Vec<Value> = st
            .files
            .values()
            .filter(|file| !file.trashed && file.parents.contains(&parent))
            .map(stat_json)
            .collect();
        let start = query
            .get("pageToken")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let page: Vec<Value> = all.iter().skip(start).take(st.page_size).cloned().collect();
        let mut out = serde_json::Map::new();
        if start + st.page_size < all.len() {
            out.insert(
                "nextPageToken".into(),
                json!((start + st.page_size).to_string()),
            );
        }
        out.insert("files".into(), Value::Array(page));
        return Reply::json(200, Value::Object(out));
    }

    if method == "POST" && path == "/drive/v3/files" {
        let meta = request.json();
        let id = st.insert(
            meta.get("name").and_then(Value::as_str).unwrap_or_default(),
            &[],
            meta.get("mimeType").and_then(Value::as_str),
            app_properties(&meta),
            &[],
            EPOCH,
        );
        return Reply::json(200, json!({ "id": id }));
    }

    if let Some(id) = path
        .strip_prefix("/drive/v3/files/")
        .filter(|rest| !rest.contains('/'))
    {
        let Some(file) = st.files.get(id).cloned() else {
            return Reply::json(404, json!({ "error": { "message": "File not found" } }));
        };
        if method == "PATCH" {
            let meta = request.json();
            if meta.get("trashed").and_then(Value::as_bool) == Some(true)
                && let Some(file) = st.files.get_mut(id)
            {
                file.trashed = true;
            }
            return Reply::json(
                200,
                stat_json(st.files.get(id).expect("file is present")),
            );
        }
        if query.get("alt").map(String::as_str) == Some("media") {
            return Reply::bytes(200, file.data.clone());
        }
        return Reply::json(200, json!({ "md5Checksum": md5_hex(&file.data) }));
    }

    if let Some(rest) = path.strip_prefix("/upload/drive/v3/files")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        let upload_id = rest.strip_prefix('/').filter(|id| !id.contains('/'));
        let boundary = request
            .header("content-type")
            .and_then(|value| value.split("boundary=").nth(1))
            .unwrap_or("")
            .trim()
            .to_string();
        let parts = multipart_parts(&request.bytes, &boundary);
        let meta = parts
            .first()
            .map(|part| serde_json::from_slice::<Value>(part).unwrap_or(Value::Null))
            .unwrap_or(Value::Null);
        let data = parts.get(1).copied().unwrap_or_default();
        let modified_time = meta
            .get("modifiedTime")
            .and_then(Value::as_str)
            .unwrap_or(EPOCH);
        if let Some(id) = upload_id {
            let Some(file) = st.files.get_mut(id) else {
                return Reply::json(404, json!({ "error": { "message": "File not found" } }));
            };
            file.data = data.to_vec();
            if meta.get("modifiedTime").and_then(Value::as_str).is_some() {
                file.modified_time = modified_time.to_string();
            }
            return Reply::json(
                200,
                stat_json(st.files.get(id).expect("file is present")),
            );
        }
        let parents: Vec<&str> = meta
            .get("parents")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let id = st.insert(
            meta.get("name").and_then(Value::as_str).unwrap_or_default(),
            &parents,
            meta.get("mimeType").and_then(Value::as_str),
            app_properties(&meta),
            data,
            modified_time,
        );
        return Reply::json(200, stat_json(st.files.get(&id).expect("file is present")));
    }

    Reply::json(
        404,
        json!({ "error": { "message": format!("unrouted {method} {}", request.path) } }),
    )
}

fn app_properties(meta: &Value) -> BTreeMap<String, String> {
    meta.get("appProperties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Like Drive: folders and Google-native docs carry no md5Checksum.
fn stat_json(file: &FakeFile) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("id".into(), json!(&file.id));
    out.insert("name".into(), json!(&file.name));
    out.insert("size".into(), json!(file.data.len().to_string()));
    let native = file
        .mime_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with("application/vnd.google-apps."));
    if !native {
        out.insert("md5Checksum".into(), json!(md5_hex(&file.data)));
    }
    out.insert("modifiedTime".into(), json!(&file.modified_time));
    Value::Object(out)
}

fn query_params(path: &str) -> BTreeMap<String, String> {
    let query = path.split_once('?').map(|(_, query)| query).unwrap_or("");
    form_params(query.as_bytes())
}

fn form_params(bytes: &[u8]) -> BTreeMap<String, String> {
    form_urlencoded::parse(bytes)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

/// The two payload parts of a `multipart/related` body, headers dropped.
pub fn multipart_parts<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let delimiter = format!("--{boundary}");
    let mut positions = Vec::new();
    let mut cursor = 0;
    while let Some(index) = find_bytes(&body[cursor..], delimiter.as_bytes()) {
        positions.push(cursor + index);
        cursor += index + delimiter.len();
    }
    let mut parts = Vec::new();
    for pair in positions.windows(2) {
        let mut part = &body[pair[0] + delimiter.len()..pair[1]];
        if let Some(rest) = part.strip_prefix(b"\r\n") {
            part = rest;
        }
        if let Some(rest) = part.strip_suffix(b"\r\n") {
            part = rest;
        }
        if let Some(index) = find_bytes(part, b"\r\n\r\n") {
            part = &part[index + 4..];
        }
        parts.push(part);
    }
    parts
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
