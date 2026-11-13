//! Port of the `extensions/anki-client.ts` calls plus `fetchAnkiStatus` from `extensions/homepage.ts`.

use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

use crate::identity::DECK_ROOT;

pub const DEFAULT_ANKI_URL: &str = "http://127.0.0.1:8765";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2000);
const ANKI_VERSION: u64 = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct AnkiResponse {
    pub result: Option<Value>,
    pub error: Option<String>,
}

/// One JSON-RPC action. A dead Anki is an `error`, never an exception.
pub async fn invoke(url: &str, action: &str, params: Option<Value>, timeout: Duration) -> AnkiResponse {
    let mut request = Map::new();
    request.insert("action".to_string(), Value::String(action.to_string()));
    request.insert("version".to_string(), Value::from(ANKI_VERSION));
    if let Some(params) = params {
        request.insert("params".to_string(), params);
    }
    let body = serde_json::to_vec(&Value::Object(request)).expect("AnkiConnect request is JSON");

    // A refused loopback connect can take ~2s to surface on Windows: a hair shorter connect phase lets
    // the refusal beat the whole-request timeout, so it still reads as "fetch failed" like Node.
    let connect_timeout = if timeout > Duration::from_millis(200) {
        timeout - Duration::from_millis(100)
    } else {
        timeout
    };
    let client = match reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return AnkiResponse {
                result: None,
                error: Some(error.to_string()),
            };
        }
    };

    let response = client
        .post(url)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(body)
        .timeout(timeout)
        .send()
        .await;

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return AnkiResponse {
                result: None,
                error: Some(reqwest_error_text(&error, url)),
            };
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        return AnkiResponse {
            result: None,
            error: Some(format!(
                "HTTP {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            )),
        };
    }

    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return AnkiResponse {
                result: None,
                error: Some(reqwest_error_text(&error, url)),
            };
        }
    };

    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(body)) => AnkiResponse {
            result: body.get("result").filter(|value| !value.is_null()).cloned(),
            error: error_text(body.get("error")),
        },
        Ok(_) => AnkiResponse {
            result: None,
            error: None,
        },
        Err(error) => AnkiResponse {
            result: None,
            error: Some(error.to_string()),
        },
    }
}

fn reqwest_error_text(error: &reqwest::Error, url: &str) -> String {
    if error.is_connect() {
        // undici's text for refused/reset connections.
        "fetch failed".to_string()
    } else if error.is_timeout() {
        "AnkiConnect request timed out".to_string()
    } else if error.is_builder() {
        format!("Failed to parse URL from {url}")
    } else {
        "fetch failed".to_string()
    }
}

/// AnkiConnect's error field: JS truthiness decides whether there was one.
fn error_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::Bool(false) => None,
        Value::String(text) if text.is_empty() => None,
        Value::Number(number) if number.as_f64() == Some(0.0) => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

fn is_js_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(flag) => !flag,
        Value::Number(number) => number.as_f64() == Some(0.0),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CourseCounts {
    pub due: i64,
    pub learning: i64,
    pub new_cards: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnkiStatus {
    pub due: i64,
    pub new_cards: i64,
    pub learning: i64,
    pub total: i64,
    pub deck_count: usize,
    pub generated_at: SystemTime,
    pub by_course: Vec<(String, CourseCounts)>,
}

/// Deck stats for the board. `course_slugs` and `deck_name_for` keep this module free of the plan scan.
pub async fn fetch_anki_status<S, D>(
    url: &str,
    timeout: Duration,
    course_slugs: S,
    deck_name_for: D,
) -> Result<AnkiStatus, String>
where
    S: FnOnce() -> Vec<String>,
    D: Fn(&str) -> String,
{
    let names = invoke(url, "deckNamesAndIds", None, timeout).await;
    if let Some(error) = names.error {
        return Err(error);
    }
    let deck_ids = match names.result {
        Some(Value::Object(map)) => map,
        // JS arrays are objects too, and this one has no deck ids in it.
        Some(Value::Array(_)) => Map::new(),
        _ => return Err("AnkiConnect returned no deck list".to_string()),
    };

    let slugs = course_slugs();

    let deck_key = |deck: &str| -> Option<String> {
        match deck_ids.get(deck)? {
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        }
    };

    let fill = |stats: Option<&Map<String, Value>>| -> Vec<(String, CourseCounts)> {
        slugs
            .iter()
            .map(|slug| {
                let entry = deck_key(&deck_name_for(slug))
                    .and_then(|id| stats.and_then(|stats| stats.get(&id)));
                let counts = entry
                    .map(|entry| CourseCounts {
                        due: count_field(entry, "review_count"),
                        learning: count_field(entry, "learn_count"),
                        new_cards: count_field(entry, "new_count"),
                    })
                    .unwrap_or(CourseCounts {
                        due: 0,
                        learning: 0,
                        new_cards: 0,
                    });
                (slug.clone(), counts)
            })
            .collect()
    };

    let decks: Vec<String> = deck_ids
        .keys()
        .filter(|name| is_proxi_deck(name))
        .cloned()
        .collect();
    if decks.is_empty() {
        return Ok(AnkiStatus {
            due: 0,
            new_cards: 0,
            learning: 0,
            total: 0,
            deck_count: 0,
            generated_at: SystemTime::now(),
            by_course: fill(None),
        });
    }

    let stats = invoke(
        url,
        "getDeckStats",
        Some(serde_json::json!({ "decks": decks })),
        timeout,
    )
    .await;
    if let Some(error) = stats.error {
        return Err(error);
    }
    let stats_result = match stats.result {
        Some(value) if !is_js_falsy(&value) => value,
        _ => return Err("AnkiConnect returned no deck stats".to_string()),
    };

    let entries: Vec<&Value> = match &stats_result {
        Value::Object(map) => map.values().collect(),
        _ => Vec::new(),
    };
    // Anki rolls subdecks (and its daily limits) into the parent's due/new/learn counts, so the root alone is what Anki
    // shows; summing it with its children double-counts. total_in_deck is per deck (no subdecks), so that one is summed.
    // Matched by id: getDeckStats names are leaf names, so a "Proxi::X::Proxi" subdeck would also say "Proxi".
    let root = deck_key(DECK_ROOT).and_then(|id| {
        stats_result
            .as_object()
            .and_then(|stats| stats.get(&id))
    });
    let counted: Vec<&Value> = match root {
        Some(root) => vec![root],
        None => entries.clone(),
    };

    Ok(AnkiStatus {
        due: counted.iter().map(|entry| count_field(entry, "review_count")).sum(),
        new_cards: counted.iter().map(|entry| count_field(entry, "new_count")).sum(),
        learning: counted.iter().map(|entry| count_field(entry, "learn_count")).sum(),
        total: entries
            .iter()
            .map(|entry| count_field(entry, "total_in_deck"))
            .sum(),
        deck_count: entries.len(),
        generated_at: SystemTime::now(),
        by_course: fill(stats_result.as_object()),
    })
}

fn is_proxi_deck(name: &str) -> bool {
    name == DECK_ROOT
        || name
            .strip_prefix(DECK_ROOT)
            .is_some_and(|rest| rest.starts_with("::"))
}

fn count_field(entry: &Value, field: &str) -> i64 {
    entry.get(field).and_then(Value::as_i64).unwrap_or(0)
}
