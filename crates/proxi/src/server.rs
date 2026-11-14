//! The local HTTP server: settings, limits, events, courses, lesson reading, `/files` and the
//! built UI.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{RawQuery, Request, State};
use axum::http::{header, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use futures_util::stream::Stream;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tower_http::services::{ServeDir, ServeFile};

use crate::courses::{self, CoursesError};
use crate::data_root::{self, DATA_ROOT_ENV};
use crate::events::{self, EventBus};
use crate::files;
use crate::js;
use crate::lesson;
use crate::limits::{self, LimitsCache};
use crate::protocol::{GlobalEvent, LearnerSettings, SettingsResponse, DEFAULT_PORT};
use crate::settings::{self, SettingsError};

/// Shown instead of the SPA when `app/web/dist` has not been built (main.ts:536-537).
const MISSING_UI: &str = "Proxi: la UI non è stata compilata. Esegui `npm run app:build` e riapri questa pagina.";

/// Base URLs for tests; `None` = the real service. Stored now, honoured by the issues that port each client.
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
    data_root: PathBuf,
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
        data_root,
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
        .route("/api/courses", get(get_courses).fallback(not_found))
        .route(
            "/api/standalone-lessons",
            get(get_standalone_lessons).fallback(not_found),
        )
        .route("/api/lesson", get(get_lesson).fallback(not_found))
        .route("/api/doc", get(get_doc).fallback(not_found))
        .route("/api/reference", get(get_reference).fallback(not_found))
        .route("/files/", get(get_file))
        .route("/files/{*rest}", get(get_file))
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
        if !path.starts_with("/files/") && !state.dist.join("index.html").is_file() {
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

    runtime.block_on(async move {
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
}
