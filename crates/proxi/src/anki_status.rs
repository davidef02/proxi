//! Anki status and review start (port of homepage.ts fetchAnkiStatus/fetchDeckStatus).

use std::path::Path;

use serde_json::{Map, Value};

use crate::anki_client::AnkiClient;
use crate::course_data::{course_deck_name_for, course_slugs, DECK_ROOT};
use crate::protocol::{AnkiCourseStats, AnkiStatusResponse, JsNumber};

/// `String(v)`: strings stay bare, everything else is its JSON text.
fn js_key(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => match number.as_f64() {
            // serde reads `1.0` as a float; JS prints it as "1".
            Some(float) if float.fract() == 0.0 && float.abs() <= 9_007_199_254_740_991.0 => {
                (float as i64).to_string()
            }
            _ => number.to_string(),
        },
        other => other.to_string(),
    }
}

/// JS `entry[key] || 0`: missing, non-numeric and zero all count as zero.
fn count(entry: Option<&Value>, key: &str) -> f64 {
    entry
        .and_then(|entry| entry.get(key))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value != 0.0)
        .unwrap_or(0.0)
}

/// JS `Object.values(map)[0]`: array-index keys enumerate first, smallest index first.
fn first_value(map: &Map<String, Value>) -> Option<&Value> {
    let mut first: Option<(u64, &Value)> = None;
    for (key, value) in map {
        if let Some(index) = array_index(key) {
            if first.is_none_or(|(smallest, _)| index < smallest) {
                first = Some((index, value));
            }
        }
    }
    first
        .map(|(_, value)| value)
        .or_else(|| map.values().next())
}

/// A key JS enumerates as an array index: digits, no leading zero, below 2^32 - 1.
fn array_index(key: &str) -> Option<u64> {
    if key.is_empty()
        || !key.bytes().all(|byte| byte.is_ascii_digit())
        || (key.len() > 1 && key.starts_with('0'))
    {
        return None;
    }
    let index: u64 = key.parse().ok()?;
    (index < 4_294_967_295).then_some(index)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeckStatus {
    pub due: f64,
    pub new_cards: f64,
    pub learning: f64,
    pub total: f64,
    pub exists: bool,
}

fn zero_status(exists: bool) -> DeckStatus {
    DeckStatus {
        due: 0.0,
        new_cards: 0.0,
        learning: 0.0,
        total: 0.0,
        exists,
    }
}

/// Due/new/learning of one deck (subdecks included, as Anki shows it). A missing deck is 0 due, not an error.
pub async fn fetch_deck_status(client: &AnkiClient, deck: &str) -> Result<DeckStatus, String> {
    let names = client.deck_names_and_ids().await;
    if let Some(error) = names.error.as_deref().filter(|error| !error.is_empty()) {
        return Err(error.to_string());
    }
    let Some(deck_ids) = names.result.as_object() else {
        return Err("AnkiConnect returned no deck list".to_string());
    };
    let Some(deck_id) = deck_ids.get(deck) else {
        return Ok(zero_status(false));
    };

    let stats = client.get_deck_stats(&[deck.to_string()]).await;
    if let Some(error) = stats.error.as_deref().filter(|error| !error.is_empty()) {
        return Err(error.to_string());
    }
    let Some(stats) = stats.result.as_object() else {
        return Err("AnkiConnect returned no deck stats".to_string());
    };
    let Some(entry) = stats.get(&js_key(deck_id)).or_else(|| first_value(stats)) else {
        return Ok(zero_status(true));
    };
    Ok(DeckStatus {
        due: count(Some(entry), "review_count"),
        new_cards: count(Some(entry), "new_count"),
        learning: count(Some(entry), "learn_count"),
        total: count(Some(entry), "total_in_deck"),
        exists: true,
    })
}

pub async fn status_for_course(
    client: &AnkiClient,
    data_root: &Path,
    course: &str,
) -> AnkiStatusResponse {
    let deck = course_deck_name_for(data_root, course);
    match fetch_deck_status(client, &deck).await {
        Ok(status) => AnkiStatusResponse {
            online: true,
            due: Some(JsNumber(status.due)),
            new_cards: Some(JsNumber(status.new_cards)),
            learning: Some(JsNumber(status.learning)),
            total: Some(JsNumber(status.total)),
            deck: Some(deck),
            deck_exists: Some(status.exists),
            ..Default::default()
        },
        Err(error) => AnkiStatusResponse {
            online: false,
            error: Some(error),
            deck: Some(deck),
            ..Default::default()
        },
    }
}

fn offline(error: &str) -> AnkiStatusResponse {
    AnkiStatusResponse {
        online: false,
        error: Some(error.to_string()),
        ..Default::default()
    }
}

pub async fn status_all(client: &AnkiClient, data_root: &Path) -> AnkiStatusResponse {
    let names = client.deck_names_and_ids().await;
    if let Some(error) = names.error.as_deref().filter(|error| !error.is_empty()) {
        return offline(error);
    }
    let Some(deck_ids) = names.result.as_object() else {
        return offline("AnkiConnect returned no deck list");
    };

    let slugs = course_slugs(data_root);
    // The same batch the totals need, keyed by deck id: one read fills every course badge.
    let fill = |stats: Option<&Map<String, Value>>| {
        let mut by_course = Map::new();
        for slug in &slugs {
            let deck_id = deck_ids.get(&course_deck_name_for(data_root, slug));
            let entry = deck_id.and_then(|id| stats.and_then(|stats| stats.get(&js_key(id))));
            let course = AnkiCourseStats {
                due: JsNumber(count(entry, "review_count")),
                learning: JsNumber(count(entry, "learn_count")),
                new_cards: JsNumber(count(entry, "new_count")),
            };
            // Never a dead end: a course without a deck still shows zero.
            by_course.insert(
                slug.clone(),
                serde_json::to_value(course).unwrap_or(Value::Null),
            );
        }
        by_course
    };

    let prefix = format!("{DECK_ROOT}::");
    let decks: Vec<String> = deck_ids
        .keys()
        .filter(|name| name.as_str() == DECK_ROOT || name.starts_with(&prefix))
        .cloned()
        .collect();
    if decks.is_empty() {
        return AnkiStatusResponse {
            online: true,
            due: Some(JsNumber(0.0)),
            new_cards: Some(JsNumber(0.0)),
            learning: Some(JsNumber(0.0)),
            total: Some(JsNumber(0.0)),
            by_course: Some(fill(None)),
            ..Default::default()
        };
    }

    let stats = client.get_deck_stats(&decks).await;
    if let Some(error) = stats.error.as_deref().filter(|error| !error.is_empty()) {
        return offline(error);
    }
    let Some(stats) = stats.result.as_object() else {
        return offline("AnkiConnect returned no deck stats");
    };

    let entries: Vec<&Value> = stats.values().collect();
    // Anki rolls subdecks (and its daily limits) into the parent's due/new/learn counts, so the root alone
    // is what Anki shows; total_in_deck is per deck and is summed.
    let root = deck_ids.get(DECK_ROOT).and_then(|id| stats.get(&js_key(id)));
    let counted: Vec<&Value> = match root {
        Some(root) => vec![root],
        None => entries.clone(),
    };
    let sum = |key: &str, entries: &[&Value]| -> f64 {
        entries.iter().map(|entry| count(Some(*entry), key)).sum()
    };
    AnkiStatusResponse {
        online: true,
        due: Some(JsNumber(sum("review_count", &counted))),
        new_cards: Some(JsNumber(sum("new_count", &counted))),
        learning: Some(JsNumber(sum("learn_count", &counted))),
        total: Some(JsNumber(sum("total_in_deck", &entries))),
        by_course: Some(fill(Some(stats))),
        ..Default::default()
    }
}

#[derive(Debug, PartialEq)]
pub enum ReviewStart {
    Started,
    NoDeck,
    Failed(String),
}

/// Opens the deck in Anki: a missing deck asks nothing more, an Anki error is reported as such.
pub async fn start_review(client: &AnkiClient, deck: &str) -> ReviewStart {
    let status = match fetch_deck_status(client, deck).await {
        Ok(status) => status,
        Err(error) => return ReviewStart::Failed(error),
    };
    if !status.exists {
        return ReviewStart::NoDeck;
    }
    let review = client.gui_deck_review(deck).await;
    match review.error.as_deref().filter(|error| !error.is_empty()) {
        Some(error) => ReviewStart::Failed(error.to_string()),
        None => ReviewStart::Started,
    }
}
