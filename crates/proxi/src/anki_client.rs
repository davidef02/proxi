//! The one adapter onto AnkiConnect (port of extensions/anki-client.ts).

use std::time::Duration;

use serde_json::{json, Map, Value};

pub const DEFAULT_ANKI_URL: &str = "http://127.0.0.1:8765";
pub const ANKI_VERSION: u64 = 6;
pub const ANKI_URL_ENV: &str = "LEARNER_ANKI_URL";
pub const SHORT_TIMEOUT: Duration = Duration::from_secs(2);
pub const LONG_TIMEOUT: Duration = Duration::from_secs(5);
pub const TIMED_OUT: &str = "AnkiConnect request timed out";
pub const FETCH_FAILED: &str = "fetch failed";
pub const INVALID_JSON: &str = "AnkiConnect returned invalid JSON";

/// AnkiConnect always answers with exactly one of the two fields set.
#[derive(Debug, Clone, PartialEq)]
pub struct AnkiResponse {
    pub result: Value,
    pub error: Option<String>,
}

impl AnkiResponse {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            result: Value::Null,
            error: Some(message.into()),
        }
    }
}

/// First non-empty of the flag and the env var, else the default (Node: `flag || env || DEFAULT`).
pub fn resolve_anki_url(flag: Option<&str>, env: Option<&str>) -> String {
    [flag, env]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
        .unwrap_or(DEFAULT_ANKI_URL)
        .to_string()
}

pub fn deck_query(deck: &str) -> String {
    format!("deck:\"{}\"", deck.replace('"', "\\\""))
}

#[derive(Clone, Debug)]
pub struct AnkiClient {
    url: String,
    http: reqwest::Client,
}

/// AnkiConnect is plain local HTTP: the client ignores proxy variables, like Node's fetch.
impl AnkiClient {
    pub fn new(url: impl Into<String>) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            url: url.into(),
            http,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Sends one JSON-RPC action. Never panics: a dead Anki is an `error`, not an exception.
    pub async fn invoke(
        &self,
        action: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> AnkiResponse {
        // `params` is omitted when absent: Node's JSON.stringify drops `undefined`.
        let mut body = Map::new();
        body.insert("action".to_string(), Value::String(action.to_string()));
        body.insert("version".to_string(), Value::from(ANKI_VERSION));
        if let Some(params) = params {
            body.insert("params".to_string(), params);
        }

        let response = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json; charset=utf-8")
            .body(Value::Object(body).to_string())
            .timeout(timeout)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => return AnkiResponse::failed(transport_error(&error)),
        };

        let status = response.status();
        if !status.is_success() {
            return AnkiResponse::failed(format!(
                "HTTP {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ));
        }

        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => return AnkiResponse::failed(transport_error(&error)),
        };
        let parsed: Value = match serde_json::from_slice(&bytes) {
            Ok(parsed) => parsed,
            Err(_) => return AnkiResponse::failed(INVALID_JSON),
        };

        let Value::Object(mut parsed) = parsed else {
            return AnkiResponse {
                result: Value::Null,
                error: None,
            };
        };
        let error = match parsed.remove("error") {
            Some(Value::String(message)) => Some(message),
            _ => None,
        };
        AnkiResponse {
            result: parsed.remove("result").unwrap_or(Value::Null),
            error,
        }
    }

    pub async fn deck_names_and_ids(&self) -> AnkiResponse {
        self.invoke("deckNamesAndIds", None, SHORT_TIMEOUT).await
    }

    pub async fn get_deck_stats(&self, decks: &[String]) -> AnkiResponse {
        self.invoke("getDeckStats", Some(json!({ "decks": decks })), SHORT_TIMEOUT)
            .await
    }

    pub async fn gui_deck_review(&self, name: &str) -> AnkiResponse {
        self.invoke("guiDeckReview", Some(json!({ "name": name })), SHORT_TIMEOUT)
            .await
    }

    pub async fn find_cards(&self, query: &str) -> AnkiResponse {
        self.invoke("findCards", Some(json!({ "query": query })), SHORT_TIMEOUT)
            .await
    }

    pub async fn cards_info(&self, cards: &[Value]) -> AnkiResponse {
        self.invoke("cardsInfo", Some(json!({ "cards": cards })), LONG_TIMEOUT)
            .await
    }

    pub async fn notes_info(&self, notes: &[Value]) -> AnkiResponse {
        self.invoke("notesInfo", Some(json!({ "notes": notes })), LONG_TIMEOUT)
            .await
    }

    pub async fn get_reviews_of_cards(&self, cards: &[Value]) -> AnkiResponse {
        self.invoke(
            "getReviewsOfCards",
            Some(json!({ "cards": cards })),
            LONG_TIMEOUT,
        )
        .await
    }
}

/// undici says `fetch failed` for anything that never reached the server; a timeout has its own text.
fn transport_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        TIMED_OUT.to_string()
    } else {
        FETCH_FAILED.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::extract::{Request, State};
    use axum::http::{header, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::Router;
    use tokio::task::JoinHandle;

    use super::*;

    #[derive(Clone)]
    struct FakeState {
        seen: Arc<Mutex<Vec<(Option<String>, String)>>>,
        status: StatusCode,
        body: &'static str,
        delay: Duration,
    }

    /// One throwaway AnkiConnect: answers every request the same way, records what it saw.
    struct FakeAnki {
        url: String,
        seen: Arc<Mutex<Vec<(Option<String>, String)>>>,
        task: JoinHandle<()>,
    }

    impl FakeAnki {
        async fn start(status: StatusCode, body: &'static str, delay: Duration) -> Self {
            let state = FakeState {
                seen: Arc::new(Mutex::new(Vec::new())),
                status,
                body,
                delay,
            };
            let seen = state.seen.clone();
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .expect("bind fake anki");
            let url = format!("http://{}", listener.local_addr().expect("fake anki addr"));
            let app = Router::new().fallback(answer).with_state(state);
            let task = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            Self { url, seen, task }
        }

        fn requests(&self) -> Vec<(Option<String>, String)> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for FakeAnki {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn answer(State(state): State<FakeState>, request: Request) -> Response {
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .unwrap_or_default();
        state.seen.lock().unwrap().push((
            content_type,
            String::from_utf8_lossy(&body).into_owned(),
        ));
        if !state.delay.is_zero() {
            tokio::time::sleep(state.delay).await;
        }
        (state.status, state.body).into_response()
    }

    fn client(url: &str) -> AnkiClient {
        AnkiClient::new(url).expect("build client")
    }

    #[tokio::test]
    async fn invoke_sends_the_node_json_body() {
        let fake = FakeAnki::start(
            StatusCode::OK,
            r#"{"result":[],"error":null}"#,
            Duration::ZERO,
        )
        .await;
        let anki = client(&fake.url);

        anki.invoke("deckNamesAndIds", None, SHORT_TIMEOUT).await;
        anki.invoke(
            "findCards",
            Some(json!({ "query": deck_query("Proxi::Demo") })),
            SHORT_TIMEOUT,
        )
        .await;

        let requests = fake.requests();
        assert_eq!(
            requests,
            vec![
                (
                    Some("application/json; charset=utf-8".to_string()),
                    r#"{"action":"deckNamesAndIds","version":6}"#.to_string(),
                ),
                (
                    Some("application/json; charset=utf-8".to_string()),
                    r#"{"action":"findCards","version":6,"params":{"query":"deck:\"Proxi::Demo\""}}"#
                        .to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn reads_a_result_and_a_null_error() {
        let fake = FakeAnki::start(
            StatusCode::OK,
            r#"{"result":[1,2],"error":null}"#,
            Duration::ZERO,
        )
        .await;

        let response = client(&fake.url).deck_names_and_ids().await;

        assert_eq!(
            response,
            AnkiResponse {
                result: json!([1, 2]),
                error: None,
            }
        );
    }

    #[tokio::test]
    async fn reads_an_error_string() {
        let fake = FakeAnki::start(
            StatusCode::OK,
            r#"{"result":null,"error":"boom"}"#,
            Duration::ZERO,
        )
        .await;

        let response = client(&fake.url).deck_names_and_ids().await;

        assert_eq!(
            response,
            AnkiResponse {
                result: Value::Null,
                error: Some("boom".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn non_success_status_is_an_http_error() {
        let fake = FakeAnki::start(StatusCode::INTERNAL_SERVER_ERROR, "", Duration::ZERO).await;

        let response = client(&fake.url).deck_names_and_ids().await;

        assert_eq!(
            response.error.as_deref(),
            Some("HTTP 500 Internal Server Error")
        );
        assert_eq!(response.result, Value::Null);
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_an_error() {
        let fake = FakeAnki::start(StatusCode::OK, "not json", Duration::ZERO).await;

        let response = client(&fake.url).deck_names_and_ids().await;

        assert_eq!(response.error.as_deref(), Some(INVALID_JSON));
    }

    #[tokio::test]
    async fn a_slow_server_times_out() {
        let fake = FakeAnki::start(
            StatusCode::OK,
            r#"{"result":[],"error":null}"#,
            Duration::from_millis(500),
        )
        .await;

        let response = client(&fake.url)
            .invoke("deckNamesAndIds", None, Duration::from_millis(50))
            .await;

        assert_eq!(response.error.as_deref(), Some(TIMED_OUT));
    }

    #[tokio::test]
    async fn a_refused_connection_is_fetch_failed() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind a port to drop");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        let anki = client(&format!("http://127.0.0.1:{port}"));

        // Long timeout: Windows takes a while to refuse, and that must not read as a timeout.
        let response = anki
            .invoke("deckNamesAndIds", None, Duration::from_secs(10))
            .await;

        assert_eq!(response.error.as_deref(), Some(FETCH_FAILED));
        assert_eq!(response.result, Value::Null);
    }

    #[test]
    fn deck_query_escapes_quotes() {
        assert_eq!(
            deck_query("Proxi::A \"b\""),
            r#"deck:"Proxi::A \"b\"""#
        );
        assert_eq!(deck_query("Proxi::Demo"), r#"deck:"Proxi::Demo""#);
    }

    #[test]
    fn resolve_anki_url_prefers_the_flag_then_a_non_empty_env() {
        assert_eq!(
            resolve_anki_url(Some("http://f"), Some("http://e")),
            "http://f"
        );
        assert_eq!(resolve_anki_url(None, Some("http://e")), "http://e");
        assert_eq!(resolve_anki_url(Some(""), Some("")), DEFAULT_ANKI_URL);
        assert_eq!(resolve_anki_url(None, None), DEFAULT_ANKI_URL);
        assert_eq!(resolve_anki_url(Some(""), Some("http://e")), "http://e");
        // Only "" counts as absent, as in Node: whitespace is not trimmed.
        assert_eq!(resolve_anki_url(Some("  "), None), "  ");
    }
}
