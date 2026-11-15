//! A fake AnkiConnect for the sync tests: the subset of `tests/mock-anki.ts` the sync uses.
//!
//! Included with `#[path = "common/fake_http.rs"] mod fake_http;` then
//! `#[path = "common/mock_anki.rs"] mod mock_anki;`.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use proxi::js::{first_value, js_string, truthy};

use crate::fake_http::{FakeHttp, Reply};

pub struct MockAnkiState {
    /// Reject a note whose deck, model and first field value match an accepted note, like Anki
    /// with allowDuplicate false and duplicateScope deck.
    pub reject_duplicates: bool,
    /// Note types the mock profile has; `None` answers "Unsupported action".
    pub models: Option<Vec<(String, Vec<String>)>>,
    pub added_notes: Vec<Value>,
    pub created_decks: Vec<Value>,
    next_note_id: i64,
}

impl MockAnkiState {
    pub fn new() -> Self {
        MockAnkiState {
            reject_duplicates: false,
            models: None,
            added_notes: Vec::new(),
            created_decks: Vec::new(),
            next_note_id: 2001,
        }
    }

    pub fn answer(&mut self, body: &str) -> (u16, Value) {
        let payload: Value = if body.is_empty() {
            json!({})
        } else {
            match serde_json::from_str(body) {
                Ok(payload) => payload,
                Err(error) => {
                    return (
                        400,
                        json!({"result": null, "error": error.to_string()}),
                    );
                }
            }
        };
        let action = payload.get("action");
        let params = payload.get("params").cloned().unwrap_or(Value::Null);

        match action.and_then(Value::as_str) {
            Some("version") => (200, json!({"result": 6, "error": null})),
            Some("createDeck") => {
                let deck = match params.get("deck") {
                    Some(deck) if truthy(Some(deck)) => deck.clone(),
                    _ => Value::from("Default"),
                };
                self.created_decks.push(deck);
                (200, json!({"result": 1725128400000i64, "error": null}))
            }
            Some("addNotes") => {
                let notes = params
                    .get("notes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut result = Vec::new();
                for note in notes {
                    if self.reject_duplicates && self.is_duplicate(&note) {
                        result.push(Value::Null);
                        continue;
                    }
                    self.added_notes.push(note);
                    result.push(json!(self.next_note_id));
                    self.next_note_id += 1;
                }
                (200, json!({"result": result, "error": null}))
            }
            Some("modelNames") if self.models.is_some() => {
                let names: Vec<String> = self
                    .models
                    .as_ref()
                    .expect("models checked")
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect();
                (200, json!({"result": names, "error": null}))
            }
            Some("modelFieldNames") if self.models.is_some() => {
                let name = params.get("modelName").and_then(Value::as_str);
                let fields = name.and_then(|name| {
                    self.models
                        .as_ref()
                        .expect("models checked")
                        .iter()
                        .find(|(model, _)| model == name)
                        .map(|(_, fields)| fields.clone())
                });
                (200, json!({"result": fields, "error": null}))
            }
            _ => (
                200,
                json!({"result": null, "error": format!("Unsupported action {}", js_string(action))}),
            ),
        }
    }

    /// Anki's duplicate rule: same deck, model and first field value, unless the note opts out.
    fn is_duplicate(&self, note: &Value) -> bool {
        if note.get("options").and_then(|options| options.get("allowDuplicate"))
            == Some(&Value::Bool(true))
        {
            return false;
        }
        self.added_notes.iter().any(|added| {
            added.get("deckName") == note.get("deckName")
                && added.get("modelName") == note.get("modelName")
                && first_field(added) == first_field(note)
        })
    }
}

impl Default for MockAnkiState {
    fn default() -> Self {
        Self::new()
    }
}

/// First value of the `fields` object as JS sees it (`Object.values(fields)[0]`): array-index keys
/// enumerate first, ascending, then the rest in insertion order. Anki's `sortField` stand-in.
fn first_field(note: &Value) -> Option<&Value> {
    match note.get("fields") {
        Some(Value::Object(fields)) => first_value(fields),
        _ => None,
    }
}

pub struct MockAnki {
    pub http: FakeHttp,
    state: Arc<Mutex<MockAnkiState>>,
}

impl MockAnki {
    pub fn start(reject_duplicates: bool, models: Option<&[(&str, [&str; 2])]>) -> MockAnki {
        let mut initial = MockAnkiState::new();
        initial.reject_duplicates = reject_duplicates;
        initial.models = models.map(|models| {
            models
                .iter()
                .map(|(name, fields)| {
                    (
                        (*name).to_string(),
                        fields.iter().map(|field| (*field).to_string()).collect(),
                    )
                })
                .collect()
        });
        let state = Arc::new(Mutex::new(initial));
        let inner = Arc::clone(&state);
        let http = FakeHttp::start(move |request| {
            let (status, value) = inner
                .lock()
                .expect("mock anki state")
                .answer(&request.body);
            Reply::json(status, value)
        });
        MockAnki { http, state }
    }

    pub fn url(&self) -> &str {
        &self.http.url
    }

    /// Parsed request bodies in arrival order.
    pub fn requests(&self) -> Vec<Value> {
        self.http
            .requests()
            .iter()
            .map(|request| request.json())
            .collect()
    }

    pub fn actions(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| js_string(request.get("action")))
            .collect()
    }

    /// Forgets the HTTP log; the notes and decks the mock accepted stay.
    pub fn clear(&self) {
        self.http.clear();
    }

    pub fn added_notes(&self) -> Vec<Value> {
        self.state
            .lock()
            .expect("mock anki state")
            .added_notes
            .clone()
    }
}
