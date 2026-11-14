//! A fake Hugging Face file host over plain HTTP: either served from memory or from a model folder.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Default)]
struct HfState {
    root: Option<PathBuf>,
    files: HashMap<String, Vec<u8>>,
    requests: Vec<String>,
    hang: bool,
}

pub struct FakeHf {
    pub url: String,
    state: Arc<Mutex<HfState>>,
    task: JoinHandle<()>,
}

impl FakeHf {
    pub async fn start(root: Option<PathBuf>, files: HashMap<String, Vec<u8>>) -> Self {
        Self::start_with(root, files, false).await
    }

    /// A host that accepts the connection and never answers: a download on it never finishes.
    pub async fn start_hanging() -> Self {
        Self::start_with(None, HashMap::new(), true).await
    }

    async fn start_with(
        root: Option<PathBuf>,
        files: HashMap<String, Vec<u8>>,
        hang: bool,
    ) -> Self {
        let state = Arc::new(Mutex::new(HfState {
            root,
            files,
            requests: Vec::new(),
            hang,
        }));
        let app = Router::new().fallback(handle).with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake hf");
        let addr = listener.local_addr().expect("fake hf address");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            url: format!("http://{addr}"),
            state,
            task,
        }
    }

    pub fn requests(&self) -> Vec<String> {
        self.state.lock().expect("fake hf lock").requests.clone()
    }
}

impl Drop for FakeHf {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(state): State<Arc<Mutex<HfState>>>, uri: axum::http::Uri) -> Response {
    let path = uri.path().to_string();
    // Read the file outside the lock: the concurrent-download test needs both requests recorded.
    let (body, root, hang) = {
        let mut state = state.lock().expect("fake hf lock");
        state.requests.push(path.clone());
        (
            state.files.get(&path).cloned(),
            state.root.clone(),
            state.hang,
        )
    };
    if hang {
        // Long enough that only a server which never waits for the download can answer first.
        tokio::time::sleep(Duration::from_secs(60)).await;
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(body) = body {
        return (StatusCode::OK, body).into_response();
    }
    let Some(root) = root else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((model, file)) = path.split_once("/resolve/main/") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let model = model.trim_start_matches('/');
    if model.is_empty() || escapes(model) || escapes(file) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match fs::read(root.join(model).join(file)) {
        Ok(body) => (StatusCode::OK, body).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn escapes(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}
