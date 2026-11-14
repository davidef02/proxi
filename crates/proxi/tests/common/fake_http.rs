//! A loopback HTTP server for tests: records every request and answers through a closure.
//!
//! Included with `#[path = "common/fake_http.rs"] mod fake_http;` so both plain `#[test]` and
//! `#[tokio::test]` files can use it.
#![allow(dead_code)]

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Router;
use tokio::sync::oneshot;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    /// Path plus query string.
    pub path: String,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    /// Raw body; `body` is the lossy UTF-8 view of the same bytes.
    pub bytes: Vec<u8>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(status: u16, value: impl Into<serde_json::Value>) -> Self {
        Reply {
            status,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: serde_json::to_string(&value.into())
                .expect("serialise fake json reply")
                .into_bytes(),
        }
    }

    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Reply {
            status,
            headers: Vec::new(),
            body: body.into().into_bytes(),
        }
    }

    pub fn bytes(status: u16, body: Vec<u8>) -> Self {
        Reply {
            status,
            headers: Vec::new(),
            body,
        }
    }
}

pub struct FakeHttp {
    pub url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeHttp {
    pub fn start<F>(handler: F) -> FakeHttp
    where
        F: Fn(&Recorded) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake http");
        listener.set_nonblocking(true).expect("fake http nonblocking");
        let addr = listener.local_addr().expect("fake http addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let handler = Arc::new(handler);
        let (shutdown, shutdown_rx) = oneshot::channel::<()>();

        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("fake http runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
                let app = Router::new().fallback(move |request: Request| {
                    let handler = Arc::clone(&handler);
                    let log = Arc::clone(&log);
                    async move {
                        let (parts, body) = request.into_parts();
                        let bytes = axum::body::to_bytes(body, usize::MAX)
                            .await
                            .unwrap_or_default();
                        let path = parts
                            .uri
                            .path_and_query()
                            .map(|value| value.as_str().to_string())
                            .unwrap_or_default();
                        let headers = parts
                            .headers
                            .iter()
                            .map(|(name, value)| {
                                (
                                    name.as_str().to_lowercase(),
                                    value.to_str().unwrap_or_default().to_string(),
                                )
                            })
                            .collect();
                        let recorded = Recorded {
                            method: parts.method.as_str().to_string(),
                            path,
                            headers,
                            body: String::from_utf8_lossy(&bytes).to_string(),
                            bytes: bytes.to_vec(),
                        };
                        log.lock()
                            .expect("fake http log")
                            .push(recorded.clone());
                        let reply = handler(&recorded);

                        let mut response = Response::builder()
                            .status(
                                StatusCode::from_u16(reply.status)
                                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                            )
                            .body(Body::from(reply.body))
                            .expect("build fake http response");
                        for (name, value) in reply.headers {
                            if let (Ok(name), Ok(value)) = (
                                HeaderName::from_bytes(name.as_bytes()),
                                HeaderValue::from_str(&value),
                            ) {
                                response.headers_mut().append(name, value);
                            }
                        }
                        response
                    }
                });
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("fake http serve");
            });
        });

        FakeHttp {
            url: format!("http://{addr}"),
            requests,
            shutdown: Some(shutdown),
            thread: Some(thread),
        }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("fake http log").clone()
    }

    pub fn clear(&self) {
        self.requests.lock().expect("fake http log").clear();
    }
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
