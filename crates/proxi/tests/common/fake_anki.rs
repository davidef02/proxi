//! A fake AnkiConnect over plain HTTP: the subset of tests/mock-anki.ts the Anki routes use.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Map, Value};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct FakeCard {
    pub card_id: i64,
    pub note: i64,
    pub deck_name: String,
    pub tags: Vec<String>,
    pub interval: i64,
    pub lapses: i64,
    /// `(id, ease)` of each review, in the order the fake reports them.
    pub reviews: Vec<(i64, i64)>,
}

pub fn card(
    card_id: i64,
    note: i64,
    deck: &str,
    tags: &[&str],
    interval: i64,
    lapses: i64,
    reviews: &[(i64, i64)],
) -> FakeCard {
    FakeCard {
        card_id,
        note,
        deck_name: deck.to_string(),
        tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        interval,
        lapses,
        reviews: reviews.to_vec(),
    }
}

#[derive(Default)]
pub struct FakeState {
    pub cards: Vec<FakeCard>,
    pub deck_names_and_ids: Option<Value>,
    pub deck_stats: Option<Value>,
    pub response_error: Option<String>,
    pub gui_deck_review_error: Option<String>,
    /// When set, `getReviewsOfCards` answers this result instead of deriving it from `cards`.
    pub reviews_result: Option<Value>,
    pub find_cards_delay: Duration,
    /// Parsed request bodies in arrival order, so a test can check that `params` is absent.
    pub requests: Vec<Value>,
}

pub struct FakeAnki {
    pub url: String,
    state: Arc<Mutex<FakeState>>,
    task: JoinHandle<()>,
}

impl FakeAnki {
    pub async fn start(state: FakeState) -> Self {
        let state = Arc::new(Mutex::new(state));
        let app = Router::new().fallback(handle).with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake anki");
        let addr = listener.local_addr().expect("fake anki address");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            url: format!("http://{addr}"),
            state,
            task,
        }
    }

    pub fn requests(&self) -> Vec<Value> {
        self.state.lock().expect("fake anki lock").requests.clone()
    }

    pub fn actions(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("fake anki lock")
            .requests
            .iter()
            .map(|request| action_of(request))
            .collect()
    }

    pub fn update(&self, f: impl FnOnce(&mut FakeState)) {
        f(&mut self.state.lock().expect("fake anki lock"));
    }
}

impl Drop for FakeAnki {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(state): State<Arc<Mutex<FakeState>>>, body: Bytes) -> Response {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => {
            let error = json!({"result": null, "error": "Invalid JSON"});
            return (StatusCode::BAD_REQUEST, axum::Json(error)).into_response();
        }
    };
    let action = action_of(&payload);
    let params = payload.get("params").cloned().unwrap_or(Value::Null);
    let delay = {
        let mut state = state.lock().expect("fake anki lock");
        state.requests.push(payload);
        state.find_cards_delay
    };
    // Sleep outside the lock: the import test needs the second caller to arrive meanwhile.
    if action == "findCards" && !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let answer = {
        let state = state.lock().expect("fake anki lock");
        answer_for(&state, &action, &params)
    };
    axum::Json(answer).into_response()
}

fn action_of(payload: &Value) -> String {
    payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn answer_for(state: &FakeState, action: &str, params: &Value) -> Value {
    if let Some(error) = &state.response_error {
        return json!({"result": null, "error": error});
    }
    match action {
        "findCards" => json!({"result": find_cards(state, params), "error": null}),
        "cardsInfo" => json!({"result": cards_info(state, params), "error": null}),
        "notesInfo" => json!({"result": notes_info(state, params), "error": null}),
        "getReviewsOfCards" => json!({"result": reviews_of_cards(state, params), "error": null}),
        "deckNamesAndIds" => json!({"result": deck_names_and_ids(state), "error": null}),
        "getDeckStats" => json!({"result": deck_stats(state, params), "error": null}),
        "guiDeckReview" => match &state.gui_deck_review_error {
            Some(error) => json!({"result": null, "error": error}),
            None => json!({"result": true, "error": null}),
        },
        other => json!({"result": null, "error": format!("Unsupported action {other}")}),
    }
}

/// Handles `deck:"Proxi::Demo"` only, unescaping `\"` inside the name.
fn find_cards(state: &FakeState, params: &Value) -> Value {
    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
    let Some(rest) = query.strip_prefix("deck:\"") else {
        return json!([]);
    };
    let Some(inner) = rest.strip_suffix('"') else {
        return json!([]);
    };
    let deck = inner.replace("\\\"", "\"");
    let nested = format!("{deck}::");
    let ids: Vec<i64> = state
        .cards
        .iter()
        .filter(|card| card.deck_name == deck || card.deck_name.starts_with(&nested))
        .map(|card| card.card_id)
        .collect();
    json!(ids)
}

fn cards_info(state: &FakeState, params: &Value) -> Value {
    let mut result = Vec::new();
    for id in requested(params, "cards") {
        let found = state.cards.iter().find(|card| json!(card.card_id) == id);
        if let Some(card) = found {
            result.push(json!({
                "cardId": card.card_id,
                "note": card.note,
                "deckName": card.deck_name,
                "interval": card.interval,
                "lapses": card.lapses,
                "factor": 2500,
            }));
        }
    }
    Value::Array(result)
}

fn notes_info(state: &FakeState, params: &Value) -> Value {
    let mut result = Vec::new();
    for note in requested(params, "notes") {
        let found = state.cards.iter().find(|card| json!(card.note) == note);
        if let Some(card) = found {
            result.push(json!({"noteId": note, "tags": card.tags}));
        }
    }
    Value::Array(result)
}

fn reviews_of_cards(state: &FakeState, params: &Value) -> Value {
    if let Some(value) = &state.reviews_result {
        return value.clone();
    }
    let mut result = Map::new();
    for id in requested(params, "cards") {
        let key = match &id {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let reviews: Vec<Value> = state
            .cards
            .iter()
            .find(|card| json!(card.card_id) == id)
            .map(|card| {
                card.reviews
                    .iter()
                    .map(|(review_id, ease)| {
                        json!({
                            "id": review_id,
                            "usn": 0,
                            "ease": ease,
                            "ivl": card.interval,
                            "lastIvl": 0,
                            "factor": 2500,
                            "time": 1000,
                            "type": 1,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        result.insert(key, Value::Array(reviews));
    }
    Value::Object(result)
}

fn deck_names_and_ids(state: &FakeState) -> Value {
    if let Some(value) = &state.deck_names_and_ids {
        return value.clone();
    }
    let mut names: Vec<&str> = Vec::new();
    for card in &state.cards {
        if !card.deck_name.is_empty() && !names.contains(&card.deck_name.as_str()) {
            names.push(card.deck_name.as_str());
        }
    }
    let mut result = Map::new();
    for (index, name) in names.iter().enumerate() {
        result.insert((*name).to_string(), json!(index + 1));
    }
    Value::Object(result)
}

fn deck_stats(state: &FakeState, params: &Value) -> Value {
    if let Some(value) = &state.deck_stats {
        return value.clone();
    }
    let mut result = Map::new();
    for deck in params
        .get("decks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        result.insert(
            deck.to_string(),
            json!({
                "deck_id": 1,
                "name": deck,
                "new_count": 0,
                "learn_count": 0,
                "review_count": 0,
                "total_in_deck": 0,
            }),
        );
    }
    Value::Object(result)
}

fn requested(params: &Value, key: &str) -> Vec<Value> {
    params
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}
