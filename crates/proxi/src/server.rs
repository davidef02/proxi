//! The local HTTP server: settings, limits, events, Anki status, courses, lesson reading, the map
//! routes, exercises, sync, `/files` and the built UI.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, RawQuery, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tower_http::services::{ServeDir, ServeFile};

use crate::anki_client::{self, AnkiClient};
use crate::anki_import::ImportRegistry;
use crate::anki_status::{start_review, status_all, status_for_course, ReviewStart};
use crate::child;
use crate::course_data::{course_deck_name_for, valid_course};
use crate::courses::{self, CoursesError};
use crate::data_root::{self, DataRootSource, DATA_ROOT_ENV};
use crate::events::{self, EventBus};
use crate::exercises;
use crate::files;
use crate::graph::store::GraphStore;
use crate::js;
use crate::knowledge;
use crate::lesson;
use crate::limits::{self, LimitsCache};
use crate::protocol::{GlobalEvent, LearnerSettings, SettingsResponse, SyncStatus, DEFAULT_PORT};
use crate::settings::{self, SettingsError};
use crate::sync::gdrive::GoogleEndpoints;
use crate::sync::github::{GithubEndpoints, real_sleeper};
use crate::sync::routes::spawn_auto;
use crate::sync::secrets::{ElectronDecryptor, KeychainStore, MemoryStore, SecretStore};
use crate::sync::service::{SyncConfig, SyncService};

/// Shown instead of the SPA when `app/web/dist` has not been built (main.ts:536-537).
const MISSING_UI: &str = "Proxi: la UI non è stata compilata. Esegui `npm run app:build` e riapri questa pagina.";

/// Base URLs for tests; `None` = the real service. Anki, GitHub and Google are honoured now; the rest
/// by the issue that ports each client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Endpoints {
    pub anki: Option<String>,
    pub github: Option<String>,
    pub google: Option<String>,
    pub huggingface: Option<String>,
    pub transcription: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineBins {
    pub claude: Option<PathBuf>,
    pub agy: Option<PathBuf>,
    pub opencode: Option<PathBuf>,
    pub git: Option<PathBuf>,
    pub npm: Option<PathBuf>,
}

pub struct ServerOptions {
    pub repo_root: PathBuf,
    pub data_root: Option<PathBuf>,
    pub port: u16,
    pub dist_dir: Option<PathBuf>,
    pub keep_alive: Duration,
    pub endpoints: Endpoints,
    pub bins: EngineBins,
    pub memory_secrets: bool,
}

impl ServerOptions {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            data_root: None,
            port: 0,
            dist_dir: None,
            keep_alive: events::KEEP_ALIVE,
            endpoints: Endpoints::default(),
            bins: EngineBins::default(),
            memory_secrets: false,
        }
    }
}

pub struct RunningServer {
    pub port: u16,
    pub url: String,
    pub events: EventBus,
    sync: SyncService,
    auto: JoinHandle<()>,
    task: JoinHandle<()>,
}

impl RunningServer {
    pub fn close(self) {
        self.task.abort();
        self.auto.abort();
    }

    /// One last auto pass, bounded so quitting cannot hang, then stop serving.
    pub async fn shutdown(self) {
        self.auto.abort();
        if self.sync.auto_enabled() {
            let sync = self.sync.clone();
            let _ = tokio::time::timeout(
                Duration::from_secs(15),
                tokio::task::spawn_blocking(move || sync.run(false)),
            )
            .await;
        }
        self.close();
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.task.abort();
        self.auto.abort();
    }
}

struct AppState {
    repo_root: PathBuf,
    repo_str: String,
    data_root: PathBuf,
    data_str: String,
    port: u16,
    dist: PathBuf,
    events: EventBus,
    keep_alive: Duration,
    limits: LimitsCache,
    anki: AnkiClient,
    imports: ImportRegistry,
    graph: Arc<GraphStore>,
    npm_bin: Option<PathBuf>,
    installs: exercises::Installs,
    touched: std::sync::Mutex<HashMap<String, std::time::Instant>>,
}

pub async fn start(opts: ServerOptions) -> std::io::Result<RunningServer> {
    let repo_root = data_root::resolve_path(&opts.repo_root);
    let (data_root, data_root_from_env) = match &opts.data_root {
        Some(path) => (data_root::resolve_path(path), false),
        None => {
            let resolved = data_root::resolve_data_root(
                &repo_root,
                std::env::var(DATA_ROOT_ENV).ok().as_deref(),
                &data_root::home_dir(),
            );
            let from_env = resolved.source == DataRootSource::Env;
            (resolved.data_root, from_env)
        }
    };
    let dist = opts
        .dist_dir
        .unwrap_or_else(|| repo_root.join("app").join("web").join("dist"));
    // The flag wins over the env, read once at start (Node read it per request; flags are the test seam).
    let anki_url = anki_client::resolve_anki_url(
        opts.endpoints.anki.as_deref(),
        std::env::var(anki_client::ANKI_URL_ENV).ok().as_deref(),
    );
    let anki = AnkiClient::new(anki_url).map_err(std::io::Error::other)?;

    let events = EventBus::new();
    let store: Arc<dyn SecretStore> = if opts.memory_secrets {
        Arc::new(MemoryStore::default())
    } else {
        Arc::new(KeychainStore::new())
    };
    let status_events = events.clone();
    let pulled_events = events.clone();
    // Bound before the service: the Google redirect URI points back at this port.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", opts.port)).await?;
    let port = listener.local_addr()?.port();
    let sync_cfg = SyncConfig {
        repo_root: repo_root.clone(),
        data_root: data_root.clone(),
        data_root_from_env,
        github: GithubEndpoints::from_override(opts.endpoints.github.as_deref()),
        google: GoogleEndpoints::from_override(opts.endpoints.google.as_deref()),
        port,
        sleep: real_sleeper(),
        store,
        decryptor: Arc::new(ElectronDecryptor {
            profiles: ElectronDecryptor::default_profiles(),
        }),
        make_provider: None,
        relaunch: None,
        on_status: Arc::new(move |status: &SyncStatus| {
            status_events.emit(&GlobalEvent::Sync {
                status: status.clone(),
            });
        }),
        on_pulled: Arc::new(move || {
            pulled_events.emit(&json!({ "type": "courses" }));
        }),
    };
    // The keychain and the legacy migration are blocking: off the worker threads.
    let sync = tokio::task::spawn_blocking(move || SyncService::new(sync_cfg))
        .await
        .map_err(std::io::Error::other)?;

    let state = Arc::new(AppState {
        repo_str: repo_root.display().to_string(),
        data_str: data_root.display().to_string(),
        repo_root,
        data_root: data_root.clone(),
        port,
        dist,
        events: events.clone(),
        keep_alive: opts.keep_alive,
        // One cache per server: Node keys it by repo root, and a server serves one repo.
        limits: LimitsCache::new(),
        anki,
        imports: ImportRegistry::new(),
        graph: Arc::new(GraphStore::new(data_root)),
        npm_bin: opts.bins.npm,
        installs: exercises::Installs::default(),
        touched: std::sync::Mutex::new(HashMap::new()),
    });

    let app = router(state, sync.clone());
    let auto = spawn_auto(sync.clone(), events.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Ok(RunningServer {
        port,
        url: format!("http://127.0.0.1:{port}"),
        events,
        sync,
        auto,
        task,
    })
}

fn router(state: Arc<AppState>, sync: SyncService) -> Router {
    Router::new()
        .route(
            "/api/settings",
            get(get_settings).post(post_settings).fallback(not_found),
        )
        .route("/api/limits", get(get_limits).fallback(not_found))
        .route("/api/events", get(events_handler).fallback(not_found))
        .route("/api/anki", get(get_anki).fallback(not_found))
        .route(
            "/api/anki/review",
            post(post_anki_review).fallback(not_found),
        )
        .route(
            "/api/anki/import",
            post(post_anki_import).fallback(not_found),
        )
        .route("/api/courses", get(get_courses).fallback(not_found))
        .route(
            "/api/standalone-lessons",
            get(get_standalone_lessons).fallback(not_found),
        )
        .route("/api/lesson", get(get_lesson).fallback(not_found))
        .route("/api/doc", get(get_doc).fallback(not_found))
        .route("/api/reference", get(get_reference).fallback(not_found))
        .route("/api/map", get(get_map).fallback(not_found))
        .route("/api/graph", get(get_graph).fallback(not_found))
        .route(
            "/api/graph/positions",
            post(post_positions).fallback(not_found),
        )
        .route(
            "/api/graph/nodes/{id}/delete",
            post(delete_graph_node).fallback(not_found),
        )
        .route(
            "/api/exercises/{course}",
            get(get_exercises).post(post_exercise).fallback(not_found),
        )
        .route(
            "/api/exercises/{course}/{name}",
            get(get_workspace).fallback(not_found),
        )
        .route("/files/", get(get_file))
        .route("/files/{*rest}", get(get_file))
        .route("/api/", any(not_found))
        .route("/api/{*rest}", any(not_found))
        // Merged before the layer so the host/CSRF guards still cover it.
        .merge(crate::sync::routes::router(sync))
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

/// Read routes ported from main.ts:204-205,483-535. Sync file I/O like the Node handlers; the
/// data folder is small and this is a personal app.
fn courses_response(result: Result<Vec<Value>, CoursesError>) -> Response {
    match result {
        Ok(list) => Json(Value::Array(list)).into_response(),
        // Node: a SyntaxError from JSON.parse becomes 400 in onError (main.ts:184).
        Err(CoursesError::Syntax) => json_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
        Err(CoursesError::Other(message)) => {
            json_error(StatusCode::INTERNAL_SERVER_ERROR, &message)
        }
    }
}

async fn get_courses(State(state): State<Arc<AppState>>) -> Response {
    courses_response(courses::list_courses(&state.data_root))
}

async fn get_standalone_lessons(State(state): State<Arc<AppState>>) -> Response {
    courses_response(courses::list_standalone_lessons(&state.data_root))
}

/// Hono's query decoding, not axum's `Query` (whose rejection is a non-JSON 400).
fn query_path(query: Option<&str>) -> String {
    js::query_param(query, "path").unwrap_or_default()
}

async fn get_lesson(State(state): State<Arc<AppState>>, RawQuery(query): RawQuery) -> Response {
    let raw = query_path(query.as_deref());
    let Some(rel) = lesson::lesson_rel(&raw) else {
        return json_error(StatusCode::BAD_REQUEST, "Percorso della lezione non valido");
    };
    match lesson::read_lesson(&state.data_root, &rel) {
        Ok(Some(lesson)) => Json(lesson.doc).into_response(),
        // A malformed island throws in Node: onError answers 422 with its message.
        Err(message) => json_error(StatusCode::UNPROCESSABLE_ENTITY, &message),
        Ok(None) => match lesson::read_lesson_html(&state.data_root, &rel) {
            // Node reads the file again: a missing file is a 404, EISDIR a 500.
            Ok(None) => json_error(StatusCode::NOT_FOUND, "Lezione non trovata"),
            Err(message) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &message),
            Ok(Some(_)) => json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "La lezione non ha dati strutturati",
            ),
        },
    }
}

async fn get_doc(State(state): State<Arc<AppState>>, RawQuery(query): RawQuery) -> Response {
    let raw = query_path(query.as_deref());
    let Some(rel) = lesson::doc_rel(&raw) else {
        return json_error(StatusCode::BAD_REQUEST, "Percorso del documento non valido");
    };
    match lesson::read_doc_body(&state.data_root, &rel) {
        Some(doc) => Json(doc).into_response(),
        None => json_error(StatusCode::NOT_FOUND, "Documento non trovato"),
    }
}

async fn get_reference(State(state): State<Arc<AppState>>, RawQuery(query): RawQuery) -> Response {
    let raw = query_path(query.as_deref());
    let Some(rel) = lesson::doc_rel(&raw) else {
        return json_error(StatusCode::BAD_REQUEST, "Percorso del documento non valido");
    };
    match lesson::read_reference_document(&state.data_root, &rel) {
        Ok(Some(reference)) => Json(reference).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "Sintesi non trovata"),
        Err(message) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &message),
    }
}

/// The raw URI path is decoded inside `files::serve`: a `Path` extractor would reject bad escapes
/// before the handler sees them.
async fn get_file(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let raw = uri.path().strip_prefix("/files/").unwrap_or("");
    files::serve(&state.data_root, &state.repo_root, raw)
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
        // /files is served from the data folder and the repo, not from the built UI.
        // The Google OAuth callback answers itself even when the UI is not built.
        if !path.starts_with("/files/")
            && path != "/oauth/google/callback"
            && !state.dist.join("index.html").is_file()
        {
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

/// The shape every Anki POST shares: `{"course": <slug>}`, with zod's texts on a bad body.
fn course_from_body(body: &Bytes) -> Result<String, Response> {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return Err(json_error(StatusCode::BAD_REQUEST, "Invalid JSON body")),
    };
    let Some(object) = value.as_object() else {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            &format!(
                ": Invalid input: expected object, received {}",
                js_type(&value)
            ),
        ));
    };
    let course = match object.get("course") {
        Some(Value::String(course)) => course.clone(),
        Some(other) => {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                &format!(
                    "course: Invalid input: expected string, received {}",
                    js_type(other)
                ),
            ));
        }
        None => {
            return Err(json_error(
                StatusCode::BAD_REQUEST,
                "course: Invalid input: expected string, received undefined",
            ));
        }
    };
    if !valid_course(&course) {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "course: Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/",
        ));
    }
    Ok(course)
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

/// `GET /api/anki`: one course's deck, else every course (Hono's `c.req.query` reads the first one).
async fn get_anki(
    State(state): State<Arc<AppState>>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let pairs = match query {
        Ok(Query(pairs)) => pairs,
        // A query axum cannot decode is no query, as Hono reads it.
        Err(_) => Vec::new(),
    };
    let course = pairs
        .iter()
        .find(|(key, _)| key == "course")
        .map(|(_, value)| value.clone());
    match course {
        Some(course) => {
            if !valid_course(&course) {
                return json_error(StatusCode::BAD_REQUEST, "Corso non valido");
            }
            Json(status_for_course(&state.anki, &state.data_root, &course).await).into_response()
        }
        None => Json(status_all(&state.anki, &state.data_root).await).into_response(),
    }
}

async fn post_anki_review(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let course = match course_from_body(&body) {
        Ok(course) => course,
        Err(response) => return response,
    };
    let deck = course_deck_name_for(&state.data_root, &course);
    match start_review(&state.anki, &deck).await {
        ReviewStart::Started => Json(json!({ "ok": true })).into_response(),
        ReviewStart::NoDeck => {
            json_error(StatusCode::NOT_FOUND, "Nessun mazzo Anki per questo corso")
        }
        ReviewStart::Failed(error) => json_error(StatusCode::BAD_GATEWAY, &error),
    }
}

async fn post_anki_import(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let course = match course_from_body(&body) {
        Ok(course) => course,
        Err(response) => return response,
    };
    Json(
        state
            .imports
            .import(state.anki.clone(), state.data_root.clone(), course)
            .await,
    )
    .into_response()
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

/// `GET /api/map`: scan the lessons and the progress ledger, derive the knowledge map.
async fn get_map(State(state): State<Arc<AppState>>) -> Response {
    let data_root = state.data_root.clone();
    match tokio::task::spawn_blocking(move || knowledge::build_knowledge_map(&data_root)).await {
        Ok(Ok(map)) => Json(map.to_json()).into_response(),
        Ok(Err(message)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &message),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
    }
}

/// `GET /api/graph`: lay out whatever has no position yet, then the snapshot.
async fn get_graph(State(state): State<Arc<AppState>>) -> Response {
    let store = state.graph.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        store.ensure_layout();
        store.read_snapshot().to_json()
    })
    .await;
    match snapshot {
        Ok(value) => Json(value).into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
    }
}

async fn post_positions(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    // Raw bytes on purpose: the Node server ignores the content-type (c.req.json()).
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "positions mancante"),
    };
    let Some(positions) = parsed.get("positions") else {
        return json_error(StatusCode::BAD_REQUEST, "positions mancante");
    };
    if !positions.is_object() && !positions.is_array() {
        return json_error(StatusCode::BAD_REQUEST, "positions mancante");
    }
    let update = positions.clone();
    let store = state.graph.clone();
    match tokio::task::spawn_blocking(move || store.save_positions(&update)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(error)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
    }
}

/// `^(n|arg)_[\w-]+$`, hand-rolled: the crate has no regex.
fn valid_graph_id(id: &str) -> bool {
    let rest = id
        .strip_prefix("arg_")
        .or_else(|| id.strip_prefix("n_"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

async fn delete_graph_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    if !valid_graph_id(&id) {
        return json_error(StatusCode::BAD_REQUEST, "id non valido");
    }
    let store = state.graph.clone();
    match tokio::task::spawn_blocking(move || store.delete_node(&id)).await {
        Ok(Ok(_)) => {
            state.events.emit(&GlobalEvent::Graph {
                revision: state.graph.next_revision(),
            });
            Json(json!({ "ok": true })).into_response()
        }
        Ok(Err(error)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
    }
}

/// `^[a-z0-9]+(?:-[a-z0-9]+)*$` as zod words it; a bad path segment has an empty issue path.
const COURSE_PATTERN: &str = "Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/";
const NAME_PATTERN: &str = "Invalid string: must match pattern /^[A-Za-z0-9._-]+$/";

/// Exercise autosave fires per keystroke: `courses` at most once per 10 s per exercise (main.ts:417-424).
fn touch_courses(state: &AppState, key: String) {
    let now = std::time::Instant::now();
    let mut touched = state.touched.lock().expect("touched map lock");
    if let Some(last) = touched.get(&key) {
        if now.duration_since(*last) < Duration::from_secs(10) {
            return;
        }
    }
    touched.insert(key, now);
    drop(touched);
    state.events.emit(&json!({ "type": "courses" }));
}

/// `GET /api/exercises/:course`: the listed exercises, `[]` when the course has none.
async fn get_exercises(
    State(state): State<Arc<AppState>>,
    course: Result<Path<String>, PathRejection>,
) -> Response {
    let Ok(Path(course)) = course else {
        return not_found_response();
    };
    if !valid_course(&course) {
        return json_error(StatusCode::BAD_REQUEST, &format!(": {COURSE_PATTERN}"));
    }
    match exercises::list_exercises(&state.data_root, &course) {
        Ok(list) => Json(Value::Array(list)).into_response(),
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

/// `GET /api/exercises/:course/:name`: the workspace (info plus editable files).
async fn get_workspace(
    State(state): State<Arc<AppState>>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let Ok(Path((course, name))) = path else {
        return not_found_response();
    };
    if !valid_course(&course) {
        return json_error(StatusCode::BAD_REQUEST, &format!(": {COURSE_PATTERN}"));
    }
    match exercises::get_workspace(&state.data_root, &course, &name) {
        Ok(Some(workspace)) => Json(workspace).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "Esercizio non trovato"),
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

/// One of `exerciseSave` / `exerciseRef` (main.ts:115-116); `sessionId` is accepted and ignored.
#[derive(Debug)]
struct ExerciseBody {
    course: String,
    name: String,
    path: String,
    content: String,
    base_version: Option<String>,
}

/// Zod's `.min`/`.max` count JS string length, which is UTF-16 code units.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn push_issue(issues: &mut Vec<String>, key: &str, message: &str) {
    issues.push(format!("{key}: {message}"));
}

/// A required string key, with zod's text for a missing or non-string value.
fn required_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    issues: &mut Vec<String>,
) -> Option<String> {
    match object.get(key) {
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => {
            push_issue(
                issues,
                key,
                &format!(
                    "Invalid input: expected string, received {}",
                    js_type(other)
                ),
            );
            None
        }
        None => {
            push_issue(
                issues,
                key,
                "Invalid input: expected string, received undefined",
            );
            None
        }
    }
}

/// Hand-rolled zod: schema key order, issues joined by `"; "`. `Err` is the 400 to send.
fn exercise_body(bytes: &Bytes, save: bool) -> Result<ExerciseBody, Response> {
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(_) => return Err(json_error(StatusCode::BAD_REQUEST, "Invalid JSON body")),
    };
    let Some(object) = value.as_object() else {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            &format!(
                ": Invalid input: expected object, received {}",
                js_type(&value)
            ),
        ));
    };

    let mut issues = Vec::new();
    let course = required_string(object, "course", &mut issues);
    if let Some(course) = &course {
        if !valid_course(course) {
            push_issue(&mut issues, "course", COURSE_PATTERN);
        }
    }
    let name = required_string(object, "name", &mut issues);
    if let Some(name) = &name {
        if !exercises::safe_name(name) {
            push_issue(&mut issues, "name", NAME_PATTERN);
        }
    }

    let mut path = String::new();
    let mut content = String::new();
    let mut base_version = None;
    if save {
        if let Some(value) = required_string(object, "path", &mut issues) {
            let len = utf16_len(&value);
            if len < 1 {
                push_issue(
                    &mut issues,
                    "path",
                    "Too small: expected string to have >=1 characters",
                );
            } else if len > 200 {
                push_issue(
                    &mut issues,
                    "path",
                    "Too big: expected string to have <=200 characters",
                );
            }
            path = value;
        }
        if let Some(value) = required_string(object, "content", &mut issues) {
            if utf16_len(&value) > 200_000 {
                push_issue(
                    &mut issues,
                    "content",
                    "Too big: expected string to have <=200000 characters",
                );
            }
            content = value;
        }
        match object.get("baseVersion") {
            None => {}
            Some(Value::String(value)) => {
                if utf16_len(value) > 64 {
                    push_issue(
                        &mut issues,
                        "baseVersion",
                        "Too big: expected string to have <=64 characters",
                    );
                }
                base_version = Some(value.clone());
            }
            Some(other) => push_issue(
                &mut issues,
                "baseVersion",
                &format!(
                    "Invalid input: expected string, received {}",
                    js_type(other)
                ),
            ),
        }
    } else if let Some(other) = object.get("sessionId") {
        if !other.is_string() {
            push_issue(
                &mut issues,
                "sessionId",
                &format!(
                    "Invalid input: expected string, received {}",
                    js_type(other)
                ),
            );
        }
    }

    if !issues.is_empty() {
        return Err(json_error(StatusCode::BAD_REQUEST, &issues.join("; ")));
    }
    Ok(ExerciseBody {
        course: course.expect("a required string"),
        name: name.expect("a required string"),
        path,
        content,
        base_version,
    })
}

/// Reads and throws away the request body. A response sent while the body is unread makes hyper
/// drop the connection, which the client sees as a reset (ECONNRESET in the TS test).
async fn drain_body(body: Body) {
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        if chunk.is_err() {
            break;
        }
    }
}

/// `POST /api/exercises/:action` with `run|open|save|hint` (main.ts:411-460).
async fn post_exercise(
    State(state): State<Arc<AppState>>,
    action: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Ok(Path(action)) = action else {
        return not_found_response();
    };
    if !matches!(action.as_str(), "run" | "open" | "save" | "hint") {
        return not_found_response();
    }
    let save = action == "save";
    if save {
        // Node checks the advertised length before reading: a 600 kB autosave is refused early.
        let too_big = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().parse::<f64>().unwrap_or(0.0))
            .unwrap_or(0.0)
            > 600_000.0;
        if too_big {
            // Answering before reading makes hyper close the socket while the client still writes,
            // which undici reports as ECONNRESET; Node reads the body first, so drain it here.
            drain_body(body).await;
            return json_error(StatusCode::PAYLOAD_TOO_LARGE, "File troppo grande");
        }
    }
    let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return json_error(StatusCode::PAYLOAD_TOO_LARGE, "File troppo grande"),
    };
    let parsed = match exercise_body(&bytes, save) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let course = parsed.course;
    let name = parsed.name;

    match action.as_str() {
        "run" => {
            if let Err(response) = exercise_dir(&state, &course, &name) {
                return response;
            }
            let cfg = exercises::RunConfig::new(
                child::node_on_path(),
                exercises::npm_command(
                    state.npm_bin.as_deref(),
                    std::env::var_os("LEARNER_NPM_BIN").as_deref(),
                ),
            );
            // Awaited here: a dropped request drops this future and kills the vitest tree.
            Json(
                exercises::run_exercise(&state.data_root, &course, &name, &cfg, &state.installs)
                    .await,
            )
            .into_response()
        }
        "open" => {
            let dir = match exercise_dir(&state, &course, &name) {
                Ok(dir) => dir,
                Err(response) => return response,
            };
            exercises::open_in_editor(&dir);
            Json(json!({ "ok": true })).into_response()
        }
        "save" => {
            let dir = match exercise_dir(&state, &course, &name) {
                Ok(dir) => dir,
                Err(response) => return response,
            };
            match exercises::save_exercise_file(
                &state.data_root,
                &course,
                &name,
                &parsed.path,
                &parsed.content,
                parsed.base_version.as_deref(),
            ) {
                Ok(()) => {
                    touch_courses(&state, format!("{course}/{name}"));
                    Json(json!({
                        "ok": true,
                        "version": exercises::file_version(&dir.join(&parsed.path)),
                    }))
                    .into_response()
                }
                Err(exercises::SaveError::Conflict) => (
                    StatusCode::CONFLICT,
                    Json(json!({ "error": exercises::CONFLICT, "code": "stale" })),
                )
                    .into_response(),
                Err(exercises::SaveError::Invalid(message)) => {
                    json_error(StatusCode::BAD_REQUEST, &message)
                }
                Err(exercises::SaveError::Io(error)) => {
                    json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
                }
            }
        }
        "hint" => {
            if let Err(response) = exercise_dir(&state, &course, &name) {
                return response;
            }
            match exercises::reveal_hint(&state.data_root, &course, &name) {
                Ok(Some(info)) => {
                    touch_courses(&state, format!("{course}/{name}"));
                    Json(info).into_response()
                }
                Ok(None) => json_error(
                    StatusCode::NOT_FOUND,
                    "Nessun suggerimento per questo esercizio",
                ),
                Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
            }
        }
        _ => not_found_response(),
    }
}

/// The exercise directory, or Node's 404 (`exerciseDir`, main.ts:412-416).
fn exercise_dir(state: &AppState, course: &str, name: &str) -> Result<PathBuf, Response> {
    let dir = state
        .data_root
        .join("courses")
        .join(course)
        .join("exercises")
        .join(name);
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(json_error(StatusCode::NOT_FOUND, "Esercizio non trovato"))
    }
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

/// `proxi serve` flags. Help text is English: the CLI is a dev tool, not learner copy.
/// The doc comments are the help text.
#[derive(clap::Args, Debug, Default)]
pub struct ServeArgs {
    /// Code repo root (default: LEARNER_REPO_ROOT, then the binary's repo)
    #[arg(long, value_name = "PATH")]
    pub repo_root: Option<PathBuf>,
    /// Data folder (default: LEARNER_DATA_ROOT, .learner/config.json, ~/Documents/proxi-data)
    #[arg(long, value_name = "PATH")]
    pub data_root: Option<PathBuf>,
    /// Port; 0 = a free port (default: LEARNER_PORT, then 4317)
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// Claude executable (tests: a fake)
    #[arg(long, value_name = "PATH")]
    pub claude_bin: Option<PathBuf>,
    /// Antigravity executable (tests: a fake)
    #[arg(long, value_name = "PATH")]
    pub agy_bin: Option<PathBuf>,
    /// OpenCode executable (tests: a fake)
    #[arg(long, value_name = "PATH")]
    pub opencode_bin: Option<PathBuf>,
    /// Git executable (tests: a fake)
    #[arg(long, value_name = "PATH")]
    pub git_bin: Option<PathBuf>,
    /// npm executable (tests: a fake)
    #[arg(long, value_name = "PATH")]
    pub npm_bin: Option<PathBuf>,
    /// AnkiConnect base URL (tests: a local fake)
    #[arg(long, value_name = "URL")]
    pub anki_url: Option<String>,
    /// GitHub base URL for the API and OAuth (tests: a local fake)
    #[arg(long, value_name = "URL")]
    pub github_url: Option<String>,
    /// Google base URL for OAuth and Drive (tests: a local fake)
    #[arg(long, value_name = "URL")]
    pub google_url: Option<String>,
    /// Hugging Face base URL (tests: a local fake)
    #[arg(long, value_name = "URL")]
    pub huggingface_url: Option<String>,
    /// Transcription service base URL (tests: a local fake)
    #[arg(long, value_name = "URL")]
    pub transcription_url: Option<String>,
    /// Keep secrets in memory instead of the OS store (tests)
    #[arg(long)]
    pub memory_secrets: bool,
}

impl ServeArgs {
    /// The flag wins over `LEARNER_PORT`; 0 stays 0 (ephemeral port).
    pub fn into_options(self, env_port: Option<&str>) -> Result<ServerOptions, String> {
        let port = match self.port {
            Some(port) => port,
            None => port_from_env(env_port)?,
        };
        let mut options = ServerOptions::new(self.repo_root.unwrap_or_else(data_root::repo_root));
        // `None` here keeps the env/config resolution in `start`.
        options.data_root = self.data_root;
        options.port = port;
        options.endpoints = Endpoints {
            anki: self.anki_url,
            github: self.github_url,
            google: self.google_url,
            huggingface: self.huggingface_url,
            transcription: self.transcription_url,
        };
        options.bins = EngineBins {
            claude: self.claude_bin,
            agy: self.agy_bin,
            opencode: self.opencode_bin,
            git: self.git_bin,
            npm: self.npm_bin,
        };
        options.memory_secrets = self.memory_secrets;
        Ok(options)
    }
}

pub fn serve_cli(args: ServeArgs) -> i32 {
    let opts = match args.into_options(std::env::var("LEARNER_PORT").ok().as_deref()) {
        Ok(opts) => opts,
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

    let code = runtime.block_on(async move {
        match start(opts).await {
            Ok(server) => {
                println!("Proxi: {}", server.url);
                let _ = tokio::signal::ctrl_c().await;
                server.shutdown().await;
                0
            }
            Err(error) => {
                eprintln!("Error: {error}");
                1
            }
        }
    });
    // Do not wait for the blocking pool: a pass stuck on GitHub would keep quitting for minutes.
    runtime.shutdown_timeout(Duration::from_secs(1));
    code
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// Parses `serve` flags the way clap does inside `proxi serve`.
    #[derive(Parser)]
    struct ServeArgsParser {
        #[command(flatten)]
        args: ServeArgs,
    }

    fn parse_serve_args(args: &[&str]) -> Result<ServeArgs, clap::Error> {
        ServeArgsParser::try_parse_from(std::iter::once("proxi-serve").chain(args.iter().copied()))
            .map(|parsed| parsed.args)
    }

    #[test]
    fn serve_args_port_flag_wins_over_env() {
        let args = parse_serve_args(&["--port", "0"]).expect("parse --port 0");
        assert_eq!(args.into_options(Some("5000")).unwrap().port, 0);

        let args = parse_serve_args(&[]).expect("parse without a port");
        assert_eq!(args.into_options(Some("5000")).unwrap().port, 5000);

        let args = parse_serve_args(&[]).expect("parse without a port");
        assert_eq!(args.into_options(None).unwrap().port, DEFAULT_PORT);
    }

    #[test]
    fn serve_args_invalid_env_port_is_an_error() {
        let args = parse_serve_args(&[]).expect("parse without a port");
        let Err(message) = args.into_options(Some("70000")) else {
            panic!("expected an invalid LEARNER_PORT error");
        };
        assert!(message.starts_with("LEARNER_PORT non valido:"), "{message}");
    }

    #[test]
    fn serve_args_fill_endpoints_bins_and_secrets() {
        let args = parse_serve_args(&[
            "--repo-root",
            "r",
            "--data-root",
            "x",
            "--port",
            "4321",
            "--claude-bin",
            "c",
            "--agy-bin",
            "a",
            "--opencode-bin",
            "o",
            "--git-bin",
            "g",
            "--npm-bin",
            "n",
            "--anki-url",
            "http://anki",
            "--github-url",
            "http://github",
            "--google-url",
            "http://google",
            "--huggingface-url",
            "http://hf",
            "--transcription-url",
            "http://stt",
            "--memory-secrets",
        ])
        .expect("parse full flags");
        let opts = args.into_options(None).expect("into options");

        assert_eq!(opts.port, 4321);
        assert_eq!(opts.repo_root, PathBuf::from("r"));
        assert_eq!(opts.data_root, Some(PathBuf::from("x")));
        assert_eq!(
            opts.endpoints,
            Endpoints {
                anki: Some("http://anki".to_string()),
                github: Some("http://github".to_string()),
                google: Some("http://google".to_string()),
                huggingface: Some("http://hf".to_string()),
                transcription: Some("http://stt".to_string()),
            }
        );
        assert_eq!(
            opts.bins,
            EngineBins {
                claude: Some(PathBuf::from("c")),
                agy: Some(PathBuf::from("a")),
                opencode: Some(PathBuf::from("o")),
                git: Some(PathBuf::from("g")),
                npm: Some(PathBuf::from("n")),
            }
        );
        assert!(opts.memory_secrets);
    }

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

    /// Status and `error` text of a rejected body.
    async fn rejected<T: std::fmt::Debug>(result: Result<T, Response>) -> (StatusCode, String) {
        let response = result.expect_err("the body must be rejected");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the error body");
        let body: Value = serde_json::from_slice(&bytes).expect("the error body is JSON");
        let error = body["error"].as_str().expect("error is a string");
        (status, error.to_string())
    }

    #[tokio::test]
    async fn course_from_body_matches_the_node_schema_errors() {
        let valid = course_from_body(&Bytes::from_static(br#"{"course":"demo"}"#));
        assert_eq!(valid.expect("a valid body"), "demo");

        let cases: [(&[u8], &str); 5] = [
            (b"{", "Invalid JSON body"),
            (b"[]", ": Invalid input: expected object, received array"),
            (
                b"{}",
                "course: Invalid input: expected string, received undefined",
            ),
            (
                br#"{"course":5}"#,
                "course: Invalid input: expected string, received number",
            ),
            (
                br#"{"course":"Bad"}"#,
                "course: Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/",
            ),
        ];
        for (body, want) in cases {
            let (status, error) = rejected(course_from_body(&Bytes::from_static(body))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{want}");
            assert_eq!(error, want);
        }
    }

    #[tokio::test]
    async fn exercise_body_matches_the_node_schema_errors() {
        let valid = exercise_body(
            &Bytes::from_static(br#"{"course":"demo","name":"01-a","path":"a.js","content":"x"}"#),
            true,
        )
        .expect("a valid save body");
        assert_eq!(valid.course, "demo");
        assert_eq!(valid.name, "01-a");
        assert_eq!(valid.path, "a.js");
        assert_eq!(valid.content, "x");
        assert_eq!(valid.base_version, None);

        assert!(
            exercise_body(
                &Bytes::from_static(br#"{"course":"demo","name":"01-a","sessionId":"s"}"#),
                false,
            )
            .is_ok()
        );

        let cases: [(&[u8], bool, &str); 8] = [
            (b"{", true, "Invalid JSON body"),
            (
                b"[]",
                true,
                ": Invalid input: expected object, received array",
            ),
            (
                b"{}",
                true,
                "course: Invalid input: expected string, received undefined; name: Invalid input: expected string, received undefined; path: Invalid input: expected string, received undefined; content: Invalid input: expected string, received undefined",
            ),
            (
                br#"{"course":"Bad","name":"01-a","path":"a.js","content":"x"}"#,
                true,
                "course: Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/",
            ),
            (
                br#"{"course":"demo","name":"a b","path":"a.js","content":"x"}"#,
                true,
                "name: Invalid string: must match pattern /^[A-Za-z0-9._-]+$/",
            ),
            (
                br#"{"course":"demo","name":"01-a","path":"","content":"x"}"#,
                true,
                "path: Too small: expected string to have >=1 characters",
            ),
            (
                br#"{"course":"demo","name":"01-a","path":"a.js","content":"x","baseVersion":null}"#,
                true,
                "baseVersion: Invalid input: expected string, received null",
            ),
            (
                br#"{"course":"demo","name":"01-a","sessionId":5}"#,
                false,
                "sessionId: Invalid input: expected string, received number",
            ),
        ];
        for (body, save, want) in cases {
            let (status, error) = rejected(exercise_body(&Bytes::from_static(body), save)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{want}");
            assert_eq!(error, want);
        }
    }
}
