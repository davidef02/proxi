//! The `/api/sync` HTTP routes and the auto-sync watcher.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::events::EventBus;
use crate::sync::secrets::{GDRIVE_ACCOUNT, GITHUB_ACCOUNT};
use crate::sync::service::SyncService;

/// First auto pass after start (Node's `kick(1000)`).
const START_DELAY: Duration = Duration::from_secs(1);
/// Delay after a `courses`/`graph` event (Node's `kick(5000)`).
const CHANGE_DELAY: Duration = Duration::from_secs(5);

const PROVIDER_OPTIONS: &str = "provider: Invalid option: expected one of \"github\"|\"gdrive\"";
const CHOICE_OPTIONS: &str =
    "choice: Invalid option: expected one of \"local\"|\"remote\"|\"both\"";

/// Same routes as `mountSync` in `service.ts`, plus a 404 fallback on each path.
pub fn router<S: Clone + Send + Sync + 'static>(svc: SyncService) -> Router<S> {
    Router::new()
        .route("/api/sync", get(get_status).fallback(not_found))
        .route("/api/sync/run", post(post_run).fallback(not_found))
        .route(
            "/api/sync/github/start",
            post(post_github_start).fallback(not_found),
        )
        .route(
            "/api/sync/github/cancel",
            post(post_github_cancel).fallback(not_found),
        )
        .route(
            "/api/sync/google/start",
            post(post_google_start).fallback(not_found),
        )
        .route(
            "/oauth/google/callback",
            get(get_google_callback).fallback(not_found),
        )
        .route(
            "/api/sync/disconnect",
            post(post_disconnect).fallback(not_found),
        )
        .route(
            "/api/sync/data-root",
            post(post_data_root).fallback(not_found),
        )
        .route(
            "/api/sync/conflicts",
            get(get_conflicts).fallback(not_found),
        )
        .route(
            "/api/sync/conflicts/resolve",
            post(post_resolve_conflict).fallback(not_found),
        )
        .route("/api/sync/trash", get(get_trash).fallback(not_found))
        .route(
            "/api/sync/trash/restore",
            post(post_restore).fallback(not_found),
        )
        .with_state(svc)
}

async fn not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "Not found")
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn bad_request(message: &str) -> Response {
    json_error(StatusCode::BAD_REQUEST, message)
}

fn status_response(status: crate::protocol::SyncStatus) -> Response {
    Json(status).into_response()
}

/// Runs a blocking service call off the tokio worker pool.
async fn blocking<T, F>(job: F) -> Result<T, Response>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(job).await {
        Ok(value) => Ok(value),
        Err(error) => Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &error.to_string(),
        )),
    }
}

async fn get_status(State(svc): State<SyncService>) -> Response {
    match blocking(move || svc.status()).await {
        Ok(status) => status_response(status),
        Err(response) => response,
    }
}

/// Node's `c.req.json().catch(() => ({}))`: a syntax error is an empty object.
fn request_json(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|_| json!({}))
}

fn js_type(value: &Value) -> &'static str {
    match value {
        Value::Array(_) => "array",
        Value::Null => "null",
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Object(_) => "object",
    }
}

/// zod's root `unrecognized object` error, formatted like `main.ts:182` does (empty path → leading colon).
fn invalid_object(value: &Value) -> Response {
    bad_request(&format!(
        ": Invalid input: expected object, received {}",
        js_type(value)
    ))
}

/// `z.string().min(min).max(max)`: counts UTF-16 code units, like JS `.length`.
fn zod_string(
    object: &Map<String, Value>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<String, String> {
    let text = match object.get(key) {
        Some(Value::String(text)) => text,
        Some(other) => {
            return Err(format!(
                "{key}: Invalid input: expected string, received {}",
                js_type(other)
            ));
        }
        None => {
            return Err(format!(
                "{key}: Invalid input: expected string, received undefined"
            ));
        }
    };
    let length = text.encode_utf16().count();
    if length < min {
        return Err(format!(
            "{key}: Too small: expected string to have >={min} characters"
        ));
    }
    if length > max {
        return Err(format!(
            "{key}: Too big: expected string to have <={max} characters"
        ));
    }
    Ok(text.clone())
}

/// `z.enum(["local", "remote", "both"])` reports a missing, non-string or unknown value the same way.
fn zod_choice(object: &Map<String, Value>) -> Result<String, String> {
    match object.get("choice") {
        Some(Value::String(choice)) if matches!(choice.as_str(), "local" | "remote" | "both") => {
            Ok(choice.clone())
        }
        _ => Err(CHOICE_OPTIONS.to_string()),
    }
}

/// Keeps a field's error in schema order while the other fields are still validated.
fn checked<T>(result: Result<T, String>, issues: &mut Vec<String>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(issue) => {
            issues.push(issue);
            None
        }
    }
}

async fn post_run(State(svc): State<SyncService>, body: Bytes) -> Response {
    let value = request_json(&body);
    let Some(object) = value.as_object() else {
        return invalid_object(&value);
    };
    let confirm = match object.get("confirmDeletions") {
        None => false,
        Some(Value::Bool(confirm)) => *confirm,
        Some(other) => {
            return bad_request(&format!(
                "confirmDeletions: Invalid input: expected boolean, received {}",
                js_type(other)
            ));
        }
    };
    match blocking(move || svc.run(confirm)).await {
        Ok(status) => status_response(status),
        Err(response) => response,
    }
}

async fn post_github_start(State(svc): State<SyncService>) -> Response {
    match blocking(move || svc.github_start()).await {
        Ok(status) => status_response(status),
        Err(response) => response,
    }
}

async fn post_github_cancel(State(svc): State<SyncService>) -> Response {
    match blocking(move || svc.github_cancel()).await {
        Ok(status) => status_response(status),
        Err(response) => response,
    }
}

async fn post_google_start(State(svc): State<SyncService>) -> Response {
    match blocking(move || svc.google_start()).await {
        Ok(Ok(url)) => Json(json!({ "url": url })).into_response(),
        Ok(Err(message)) => bad_request(&message),
        Err(response) => response,
    }
}

/// The loopback redirect: Hono's `c.req.query` reads the first occurrence of each name.
async fn get_google_callback(State(svc): State<SyncService>, RawQuery(raw): RawQuery) -> Response {
    let query = raw.unwrap_or_default();
    let pairs: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let first = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let (code, state, error) = (first("code"), first("state"), first("error"));
    match blocking(move || svc.google_callback(code.as_deref(), state.as_deref(), error.as_deref()))
        .await
    {
        Ok((status, html)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [(header::CONTENT_TYPE, "text/html; charset=UTF-8")],
            html,
        )
            .into_response(),
        Err(response) => response,
    }
}

async fn post_disconnect(State(svc): State<SyncService>, body: Bytes) -> Response {
    let value = request_json(&body);
    let Some(object) = value.as_object() else {
        return invalid_object(&value);
    };
    let provider = match object.get("provider") {
        Some(Value::String(name)) if name == GITHUB_ACCOUNT || name == GDRIVE_ACCOUNT => {
            name.clone()
        }
        _ => return bad_request(PROVIDER_OPTIONS),
    };
    match blocking(move || svc.disconnect(&provider)).await {
        Ok(Ok(status)) => status_response(status),
        Ok(Err(message)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &message),
        Err(response) => response,
    }
}

async fn post_data_root(State(svc): State<SyncService>, body: Bytes) -> Response {
    let value = request_json(&body);
    let Some(object) = value.as_object() else {
        return invalid_object(&value);
    };
    let dir = match zod_string(object, "dataRoot", 1, 500) {
        Ok(dir) => dir,
        Err(issue) => return bad_request(&issue),
    };
    match blocking(move || svc.set_data_root(&dir)).await {
        Ok(Ok(status)) => status_response(status),
        Ok(Err((code, message))) => json_error(
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            &message,
        ),
        Err(response) => response,
    }
}

async fn get_conflicts(State(svc): State<SyncService>) -> Response {
    match blocking(move || svc.conflicts()).await {
        Ok(entries) => Json(entries).into_response(),
        Err(response) => response,
    }
}

async fn post_resolve_conflict(State(svc): State<SyncService>, body: Bytes) -> Response {
    let value = request_json(&body);
    let Some(object) = value.as_object() else {
        return invalid_object(&value);
    };
    let mut issues = Vec::new();
    let id = checked(zod_string(object, "id", 1, 1000), &mut issues);
    let choice = checked(zod_choice(object), &mut issues);
    let (Some(id), Some(choice)) = (id, choice) else {
        return bad_request(&issues.join("; "));
    };
    match blocking(move || svc.resolve_conflict(&id, &choice)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(message)) => bad_request(&message),
        Err(response) => response,
    }
}

async fn get_trash(State(svc): State<SyncService>) -> Response {
    match blocking(move || svc.trash()).await {
        Ok(entries) => Json(entries).into_response(),
        Err(response) => response,
    }
}

async fn post_restore(State(svc): State<SyncService>, body: Bytes) -> Response {
    let value = request_json(&body);
    let Some(object) = value.as_object() else {
        return invalid_object(&value);
    };
    let id = match zod_string(object, "id", 1, 1000) {
        Ok(id) => id,
        Err(issue) => return bad_request(&issue),
    };
    match blocking(move || svc.restore(&id)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(message)) => bad_request(&message),
        Err(response) => response,
    }
}

/// Auto sync: one pass 1 s after start, and 5 s after a `courses`/`graph` event (Node's `startAuto`).
///
/// Each pass goes on the blocking pool and is not awaited: the watcher must stay responsive.
pub fn spawn_auto(svc: SyncService, events: EventBus) -> tokio::task::JoinHandle<()> {
    let mut rx = events.subscribe();
    tokio::spawn(async move {
        let mut deadline = Some(tokio::time::Instant::now() + START_DELAY);
        loop {
            let at = deadline;
            let timer = async move {
                match at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    // Fired and no new change: wait for the next `courses`/`graph` event.
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = timer => {
                    deadline = None;
                    let service = svc.clone();
                    tokio::task::spawn_blocking(move || {
                        if service.auto_enabled() {
                            service.run(false);
                        }
                    });
                }
                message = rx.recv() => match message {
                    Ok(payload) => {
                        let changed = serde_json::from_str::<Value>(&payload)
                            .ok()
                            .and_then(|value| {
                                value
                                    .get("type")
                                    .and_then(Value::as_str)
                                    .map(|kind| kind == "courses" || kind == "graph")
                            })
                            .unwrap_or(false);
                        if changed {
                            deadline = Some(tokio::time::Instant::now() + CHANGE_DELAY);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    })
}
