//! Anki review import into progress.jsonl (port of app/server/anki-import.ts).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::watch;

use crate::anki_client::{deck_query, AnkiClient};
use crate::course_data::{course_deck_name_for, known_anki_review_ids, progress_file, scan_lessons};
use crate::events::iso_from_millis;
use crate::protocol::{AnkiImportResult, AnkiReviewRecord};

pub async fn import_reviews(
    client: &AnkiClient,
    data_root: &Path,
    course: &str,
) -> AnkiImportResult {
    let deck = course_deck_name_for(data_root, course);

    let found = client.find_cards(&deck_query(&deck)).await;
    if let Some(error) = found.error {
        return AnkiImportResult::offline(error);
    }
    let Some(card_ids) = found.result.as_array() else {
        return AnkiImportResult::offline("Failed to find cards");
    };
    if card_ids.is_empty() {
        return AnkiImportResult {
            online: true,
            imported: 0,
            skipped: 0,
            error: None,
        };
    }

    let info = client.cards_info(card_ids).await;
    if let Some(error) = info.error {
        return AnkiImportResult::offline(error);
    }
    let Some(card_entries) = info.result.as_array() else {
        return AnkiImportResult::offline("Failed to fetch card info");
    };
    let cards: Vec<&Value> = card_entries
        .iter()
        .filter(|card| card.is_object())
        .collect();

    let mut note_ids: Vec<Value> = Vec::new();
    for card in &cards {
        if let Some(note) = card.get("note") {
            if !note_ids.contains(note) {
                note_ids.push(note.clone());
            }
        }
    }

    let notes = client.notes_info(&note_ids).await;
    if let Some(error) = notes.error {
        return AnkiImportResult::offline(error);
    }
    let Some(note_entries) = notes.result.as_array() else {
        return AnkiImportResult::offline("Failed to fetch note info");
    };
    // Later duplicates keep their first position but take the last tags, like a JS Map.set.
    let mut note_tags: Vec<(Value, Vec<String>)> = Vec::new();
    for note in note_entries.iter().filter(|note| note.is_object()) {
        let Some(note_id) = note.get("noteId") else {
            continue;
        };
        let tags: Vec<String> = note
            .get("tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        match note_tags.iter_mut().find(|(id, _)| id == note_id) {
            Some(entry) => entry.1 = tags,
            None => note_tags.push((note_id.clone(), tags)),
        }
    }

    let reviews = client.get_reviews_of_cards(card_ids).await;
    if let Some(error) = reviews.error {
        return AnkiImportResult::offline(error);
    }
    let Some(reviews_by_card) = reviews.result.as_object() else {
        return AnkiImportResult::offline("Failed to fetch reviews");
    };

    let mut known = known_anki_review_ids(data_root, course);
    let lessons: Vec<_> = scan_lessons(data_root)
        .into_iter()
        .filter(|lesson| lesson.course == course)
        .collect();

    let mut skipped: u64 = 0;
    let mut mapped: Vec<(&Value, String, String)> = Vec::new();
    for card in &cards {
        let tags = card
            .get("note")
            .and_then(|note| note_tags.iter().find(|(id, _)| id == note))
            .map(|(_, tags)| tags.as_slice())
            .unwrap_or_default();

        let mut node_tags: Vec<String> = Vec::new();
        for tag in tags {
            if has_prefix(tag, "node::") {
                let node_id = tag[6..].to_string();
                if !node_tags.contains(&node_id) {
                    node_tags.push(node_id);
                }
            }
        }

        // Exactly one node tag or nothing can be attributed to the graph.
        if node_tags.len() != 1 {
            skipped += 1;
            continue;
        }
        let node_id = &node_tags[0];

        let lesson_tag = tags.iter().find(|tag| has_prefix(tag, "lesson::"));

        let resolved = match lesson_tag.and_then(|tag| {
            lessons.iter().find(|lesson| {
                lesson.slug.as_str() == &tag[8..] && lesson.node_ids.contains(node_id)
            })
        }) {
            Some(lesson) => lesson,
            None => {
                let candidates: Vec<_> = lessons
                    .iter()
                    .filter(|lesson| lesson.node_ids.contains(node_id))
                    .collect();
                if candidates.len() != 1 {
                    skipped += 1;
                    continue;
                }
                candidates[0]
            }
        };

        mapped.push((card, node_id.clone(), resolved.slug.clone()));
    }

    let mut new_records: Vec<AnkiReviewRecord> = Vec::new();
    for (card, node_id, lesson_slug) in &mapped {
        let card_id = card.get("cardId");
        let key = card_id
            .map(js_string)
            .unwrap_or_else(|| "undefined".to_string());
        let card_reviews = reviews_by_card
            .get(&key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();

        for review in card_reviews.iter().filter(|review| review.is_object()) {
            let Some(ease) = review.get("ease").and_then(Value::as_f64) else {
                continue;
            };
            if !(1.0..=4.0).contains(&ease) {
                continue;
            }
            let id = review
                .get("id")
                .and_then(Value::as_f64)
                .filter(|id| id.fract() == 0.0 && id.abs() <= i64::MAX as f64)
                .map(|id| id as i64);
            let Some(id) = id else {
                // Node's `new Date(id).toISOString()` throws a RangeError here.
                return AnkiImportResult::offline("Invalid time value");
            };
            if known.contains(&id) {
                continue;
            }

            new_records.push(AnkiReviewRecord {
                at: iso_from_millis(id),
                session_id: "anki".to_string(),
                course: course.to_string(),
                lesson_slug: lesson_slug.clone(),
                node_refs: vec![format!("{course}/{lesson_slug}#{node_id}")],
                kind: "review".to_string(),
                source: "anki".to_string(),
                review_id: id,
                card_id: card_id.cloned(),
                ease: review.get("ease").cloned().unwrap_or(Value::Null),
                lapses: card.get("lapses").cloned(),
                interval: card.get("interval").cloned(),
                correct: ease > 1.0,
            });
            known.insert(id);
        }
    }

    let course_dir = data_root.join("courses").join(course);
    if new_records.is_empty() || !course_dir.exists() {
        return AnkiImportResult {
            online: true,
            imported: 0,
            skipped,
            error: None,
        };
    }

    new_records.sort_by_key(|record| record.review_id);
    let mut text = String::new();
    for record in &new_records {
        match serde_json::to_string(record) {
            Ok(line) => {
                text.push_str(&line);
                text.push('\n');
            }
            Err(error) => return AnkiImportResult::offline(error.to_string()),
        }
    }

    // One append: the port keeps Node's "all lines or nothing" write.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(progress_file(data_root, course));
    let mut file = match file {
        Ok(file) => file,
        Err(error) => return AnkiImportResult::offline(error.to_string()),
    };
    if let Err(error) = file.write_all(text.as_bytes()) {
        return AnkiImportResult::offline(error.to_string());
    }

    AnkiImportResult {
        online: true,
        imported: new_records.len() as u64,
        skipped,
        error: None,
    }
}

/// ASCII-case-insensitive prefix check; the tag slice after it is still byte-safe.
fn has_prefix(tag: &str, prefix: &str) -> bool {
    tag.len() >= prefix.len()
        && tag.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// JS `String(value)`: the key AnkiConnect uses for a card id.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// One running import per course: concurrent HTTP callers share its result, as Node shared the promise.
#[derive(Clone, Default)]
pub struct ImportRegistry {
    inflight: Arc<Mutex<HashMap<String, watch::Receiver<Option<AnkiImportResult>>>>>,
}

impl ImportRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn import(
        &self,
        client: AnkiClient,
        data_root: PathBuf,
        course: String,
    ) -> AnkiImportResult {
        let key = format!("{}|{}", data_root.display(), course);
        let (mut rx, started) = {
            let mut inflight = self.inflight.lock().expect("import registry lock");
            match inflight.get(&key).cloned() {
                Some(rx) => (rx, None),
                None => {
                    let (tx, rx) = watch::channel(None);
                    inflight.insert(key.clone(), rx.clone());
                    (rx, Some(tx))
                }
            }
        };

        if let Some(tx) = started {
            // Own task: a dropped HTTP request must not cancel the import half-way.
            let registry = self.clone();
            tokio::spawn(async move {
                let result = import_reviews(&client, &data_root, &course).await;
                // Drop the key before publishing: a later caller starts a fresh run, like Node.
                registry
                    .inflight
                    .lock()
                    .expect("import registry lock")
                    .remove(&key);
                let _ = tx.send(Some(result));
            });
        }

        match rx.wait_for(|value| value.is_some()).await {
            // The filter above only lets a sent value through.
            Ok(value) => value.as_ref().expect("import result").clone(),
            Err(_) => AnkiImportResult::offline("Import interrotto"),
        }
    }
}
