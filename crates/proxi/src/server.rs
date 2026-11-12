//! The local HTTP server: settings, limits, events and the built UI.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use futures_util::stream::Stream;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tower_http::services::{ServeDir, ServeFile};

use crate::data_root::{self, DATA_ROOT_ENV};
use crate::events::{self, EventBus};
use crate::limits::{self, LimitsCache};
use crate::protocol::{GlobalEvent, LearnerSettings, SettingsResponse, DEFAULT_PORT};
use crate::settings::{self, SettingsError};

/// Shown instead of the SPA when `app/web/dist` has not been built (main.ts:536-537).
const MISSING_UI: &str = "Proxi: la UI non è stata compilata. Esegui `npm run app:build` e riapri questa pagina.";

pub struct ServerOptions {
    pub repo_root: PathBuf,
    pub data_root: Option<PathBuf>,
    pub port: u16,
    pub dist_dir: Option<PathBuf>,
    pub keep_alive: Duration,
}

impl ServerOptions {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            data_root: None,
            port: 0,
            dist_dir: None,
            keep_alive: events::KEEP_ALIVE,
        }
    }
}

pub struct RunningServer {
    pub port: u16,
    pub url: String,
    pub events: EventBus,
    task: JoinHandle<()>,
}

impl RunningServer {
    pub fn close(self) {
        self.task.abort();
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct AppState {
    repo_root: PathBuf,
    repo_str: String,
    data_str: String,
    port: u16,
    dist: PathBuf,
    events: EventBus,
    keep_alive: Duration,
    limits: LimitsCache,
}

pub async fn start(opts: ServerOptions) -> std::io::Result<RunningServer> {
    let repo_root = data_root::resolve_path(&opts.repo_root);
    let data_root = match opts.data_root {
        Some(path) => data_root::resolve_path(&path),
        None => data_root::resolve_data_root(
            &repo_root,
            std::env::var(DATA_ROOT_ENV).ok().as_deref(),
            &data_root::home_dir(),
        )
        .data_root,
    };
    let dist = opts
        .dist_dir
        .unwrap_or_else(|| repo_root.join("app").join("web").join("dist"));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", opts.port)).await?;
    let port = listener.local_addr()?.port();
    let events = EventBus::new();
    let state = Arc::new(AppState {
        repo_str: repo_root.display().to_string(),
        data_str: data_root.display().to_string(),
        repo_root,
        port,
        dist,
        events: events.clone(),
        keep_alive: opts.keep_alive,
        // One cache per server: Node keys it by repo root, and a server serves one repo.
        limits: LimitsCache::new(),
    });

    let app = router(state);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Ok(RunningServer {
        port,
        url: format!("http://127.0.0.1:{port}"),
        events,
        task,
    })
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/api/settings",
            get(get_settings).post(post_settings).fallback(not_found),
        )
        .route("/api/limits", get(get_limits).fallback(not_found))
        .route("/api/events", get(events_handler).fallback(not_found))
        .route("/api/", any(not_found))
        .route("/api/{*rest}", any(not_found))
        .fallback_service(
            ServeDir::new(&state.dist).fallback(ServeFile::new(state.dist.join("index.html"))),
        )
        .layer(axum::middleware::from_fn_with_state(state.clone(), front))
        .with_state(state)
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn not_found_response() -> Response {
    json_error(StatusCode::NOT_FOUND, "Not found")
}

async fn not_found() -> Response {
    not_found_response()
}

/// Same guards as main.ts:189-198 (DNS rebinding + CSRF), plus the SPA fallbacks.
async fn front(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let loopback = format!("127.0.0.1:{}", state.port);
    let localhost = format!("localhost:{}", state.port);
    if host != Some(loopback.as_str()) && host != Some(localhost.as_str()) {
        return json_error(StatusCode::FORBIDDEN, "Forbidden host");
    }

    let path = request.uri().path().to_string();
    let method = request.method();
    let is_read = method == Method::GET || method == Method::HEAD;
    if !is_read && path.starts_with("/api/") {
        let client = request
            .headers()
            .get("x-learner-client")
            .and_then(|value| value.to_str().ok());
        if client != Some("1") {
            return json_error(StatusCode::FORBIDDEN, "Missing x-learner-client header");
        }
    }

    if !path.starts_with("/api/") {
        if !is_read {
            return not_found_response();
        }
        if !state.dist.join("index.html").is_file() {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                MISSING_UI,
            )
                .into_response();
        }
    }

    next.run(request).await
}

fn format_settings(state: &AppState, mut settings: LearnerSettings) -> SettingsResponse {
    settings.anthropic_api_key = settings::mask_api_key(&settings.anthropic_api_key);
    SettingsResponse {
        settings,
        repo_root: state.repo_str.clone(),
        data_root: state.data_str.clone(),
    }
}

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsResponse> {
    Json(format_settings(
        &state,
        settings::read_settings(&state.repo_root),
    ))
}

async fn post_settings(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    // Raw bytes on purpose: the Node server ignores the content-type (c.req.json()).
    let patch: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };
    let saved = match settings::write_settings(&state.repo_root, &patch) {
        Ok(saved) => saved,
        Err(SettingsError::Invalid(message)) => {
            return json_error(StatusCode::BAD_REQUEST, &message);
        }
        Err(SettingsError::Io(error)) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
        }
    };

    if limits::limits_disabled(&saved) {
        state.events.emit(&GlobalEvent::Limits {
            limits: json!({ "at": events::iso_now() }),
        });
    }
    let response = format_settings(&state, saved);
    state.events.emit(&GlobalEvent::Settings {
        settings: response.clone(),
    });
    Json(response).into_response()
}

async fn get_limits(State(state): State<Arc<AppState>>) -> Json<Value> {
    if limits::limits_disabled(&settings::read_settings(&state.repo_root)) {
        return Json(Value::Null);
    }
    // Mirror Node's cache (limits.ts:24-33): never read the file per request.
    Json(state.limits.get(&state.repo_root).unwrap_or(Value::Null))
}

async fn events_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send> {
    // Subscribe before the response is built: a POST right after the headers must be seen.
    let rx = state.events.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(payload) => {
                    return Some((Ok::<_, Infallible>(Event::default().data(payload)), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(state.keep_alive)
            .text("keep-alive"),
    )
}

/// Mirrors `Number(env.LEARNER_PORT) || DEFAULT_PORT` followed by `listen` (main.ts:628,549-560).
///
/// Two documented divergences from JS `Number`: `0x`/`0o`/`0b` literals and `Infinity` are
/// non-numeric here, so they fall back to the default instead of picking a port or exiting.
pub fn port_from_env(v: Option<&str>) -> Result<u16, String> {
    let Some(raw) = v else {
        return Ok(DEFAULT_PORT);
    };
    let t = raw.trim();
    let numeric_chars = t
        .chars()
        .all(|c| matches!(c, '0'..='9' | '+' | '-' | '.' | 'e' | 'E'));
    if t.is_empty() || !numeric_chars {
        return Ok(DEFAULT_PORT);
    }
    let Ok(n) = t.parse::<f64>() else {
        return Ok(DEFAULT_PORT);
    };
    if n.is_nan() || n == 0.0 {
        return Ok(DEFAULT_PORT);
    }
    if n.is_finite() && n.fract() == 0.0 && (1.0..=65535.0).contains(&n) {
        return Ok(n as u16);
    }
    Err(format!("LEARNER_PORT non valido: {raw}"))
}

pub fn serve_cli() -> i32 {
    let port = match port_from_env(std::env::var("LEARNER_PORT").ok().as_deref()) {
        Ok(port) => port,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };

    runtime.block_on(async move {
        let opts = ServerOptions {
            port,
            ..ServerOptions::new(data_root::repo_root())
        };
        match start(opts).await {
            Ok(server) => {
                println!("Proxi: {}", server.url);
                let _ = tokio::signal::ctrl_c().await;
                server.close();
                0
            }
            Err(error) => {
                eprintln!("Error: {error}");
                1
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_defaults_when_unset_or_non_numeric() {
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("0"),
            Some("-0"),
            Some("abc"),
            Some("nan"),
            Some("inf"),
        ] {
            assert_eq!(port_from_env(raw), Ok(DEFAULT_PORT), "{raw:?}");
        }
    }

    #[test]
    fn port_parses_decimal_numbers_like_listen() {
        for (raw, want) in [
            (" 5000 ", 5000),
            ("+5000", 5000),
            ("5000.0", 5000),
            ("5e3", 5000),
            ("1", 1),
            ("65535", 65535),
        ] {
            assert_eq!(port_from_env(Some(raw)), Ok(want), "{raw}");
        }
    }

    #[test]
    fn port_rejects_numbers_listen_cannot_bind() {
        for raw in ["70000", "65536", "-1", "5000.5", "1e400"] {
            let message = port_from_env(Some(raw)).unwrap_err();
            assert!(message.contains(raw), "{raw}: {message}");
            assert!(message.starts_with("LEARNER_PORT non valido:"), "{message}");
        }
    }

    #[test]
    fn port_divergence_from_js_number_is_documented() {
        // JS Number("0x1388") is 5000 and Number("Infinity") is Infinity; we fall back to the
        // default for both (see the doc comment on port_from_env).
        assert_eq!(port_from_env(Some("0x1388")), Ok(DEFAULT_PORT));
        assert_eq!(port_from_env(Some("Infinity")), Ok(DEFAULT_PORT));
    }
}
