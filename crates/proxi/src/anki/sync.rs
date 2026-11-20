//! AnkiConnect sync (port of `syncToAnkiConnect`, `anki-sync.ts:575-793`).
//!
//! TSV first as the permanent local record, then a reachability probe, `createDeck`, the note types
//! and one `addNotes` batch. Anki offline is not an error: the TSV stays and the run exits 0.

use std::io::Write;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::js::{array_index, parse_json};
use crate::lesson::js::{js_concat, js_to_string, js_trim, js_truthy, stringify_compact};

use super::cards::{self, ExportOptions};
use super::format::format_default_deck_name;
use super::ping;

#[derive(Default)]
pub struct SyncOptions {
    pub cards: Option<Value>,
    pub nodes: Option<Value>,
    pub lesson_path: Option<String>,
    pub topic: Option<Value>,
    pub slug: Option<Value>,
    pub deck_name: Option<Value>,
    pub anki_url: Option<String>,
    /// Raw `--timeout`: Node's `parseInt` runs on it before `setTimeout` clamps.
    pub timeout: Option<String>,
    pub allow_duplicates: bool,
    pub base_dir: Option<String>,
}

pub struct SyncResult {
    pub synced: bool,
    /// `js_to_string` of the deck: Node interpolates the raw value into its lines.
    pub deck_name: String,
    pub total_cards: usize,
    pub added_count: usize,
    pub duplicate_count: usize,
    /// Node's `failedCount`: every card on a failed run, 0 on success.
    pub failed_count: usize,
    pub tsv_export_path: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

/// The cards payload as Node sees it: a list, or a value whose missing `.length`/`.map` changes the
/// error text and the offline count.
enum CardList {
    Items(Vec<Value>),
    NotArray(String),
}

impl CardList {
    fn from_option(cards: Option<&Value>) -> CardList {
        match cards {
            Some(value) if js_truthy(Some(value)) => match value {
                Value::Array(items) => CardList::Items(items.clone()),
                Value::String(text) => CardList::NotArray(text.encode_utf16().count().to_string()),
                _ => CardList::NotArray("undefined".to_string()),
            },
            _ => CardList::Items(Vec::new()),
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, CardList::Items(items) if items.is_empty())
    }

    /// `cards.length` in a template string.
    fn length_text(&self) -> String {
        match self {
            CardList::Items(items) => items.len().to_string(),
            CardList::NotArray(text) => text.clone(),
        }
    }

    /// Node's `cards.length` for the sync counts: a non-array's `.length` is a string length or
    /// `undefined`, not a card count, so both counts stay 0 there.
    fn count(&self) -> usize {
        match self {
            CardList::Items(items) => items.len(),
            CardList::NotArray(_) => 0,
        }
    }
}

pub fn sync_to_anki_connect(
    options: &SyncOptions,
    warn: &mut dyn Write,
) -> Result<SyncResult, String> {
    // 1. The TSV is the permanent local record; a failed export only warns.
    let export = match cards::export_deck_tsv(&ExportOptions {
        cards: options.cards.clone(),
        nodes: options.nodes.clone(),
        lesson_path: options.lesson_path.clone(),
        topic: options.topic.clone(),
        slug: options.slug.clone(),
        deck_name: options.deck_name.clone(),
        out_file: None,
        base_dir: options.base_dir.clone(),
    }) {
        Ok(export) => Some(export),
        Err(message) => {
            let _ = writeln!(
                warn,
                "[anki-sync] TSV export skipped: {}",
                js_error_message(&message)
            );
            None
        }
    };

    let mut cards = CardList::from_option(options.cards.as_ref());
    let mut deck = match options.deck_name.as_ref() {
        Some(value) if js_truthy(Some(value)) => value.clone(),
        _ => match &export {
            Some(export) => Value::String(export.deck_name.clone()),
            None => {
                let topic = options
                    .topic
                    .as_ref()
                    .filter(|value| js_truthy(Some(*value)))
                    .cloned()
                    .unwrap_or_else(|| Value::String("Lesson".to_string()));
                let Value::String(topic) = topic else {
                    return Err("TypeError: topic.replace is not a function".to_string());
                };
                Value::String(format_default_deck_name(&topic))
            }
        },
    };

    if cards.is_empty() {
        if let Some(lesson_path) = options.lesson_path.as_deref().filter(|p| !p.is_empty()) {
            let extracted = cards::extract_cards_from_lesson(lesson_path)?;
            deck = match options.deck_name.as_ref() {
                Some(value) if js_truthy(Some(value)) => value.clone(),
                _ => Value::String(extracted.deck_name.clone()),
            };
            cards = CardList::Items(extracted.cards);
        }
    }

    // 2. Probe before any JSON-RPC call: Node's ping owns the offline wording.
    let raw_timeout = ping::parse_int_timeout(options.timeout.as_deref());
    let timeout = ping::clamp_timer(raw_timeout);
    let url = options
        .anki_url
        .as_deref()
        .filter(|url| !url.is_empty())
        .unwrap_or(ping::DEFAULT_URL);
    let probe = ping::ping_anki_connect(url, timeout);
    if !probe.online {
        let error = probe.error.unwrap_or_default();
        let total_cards = cards.count();
        let warning = format!(
            "[anki-sync] Note: AnkiConnect offline ({url} - {error}). Saved {} cards to {}.",
            cards.length_text(),
            export
                .as_ref()
                .map(|export| export.file_path.as_str())
                .unwrap_or("./decks")
        );
        return Ok(SyncResult {
            synced: false,
            deck_name: js_to_string(&deck),
            total_cards,
            added_count: 0,
            duplicate_count: 0,
            failed_count: total_cards,
            tsv_export_path: export.map(|export| export.file_path),
            warning: Some(warning),
            error: Some(error),
        });
    }

    // 3. createDeck is idempotent in Anki, so a failure here is reported, not retried.
    let response = call_anki_connect(url, "createDeck", json!({ "deck": deck.clone() }), timeout);
    if let Some(error) = response_error(&response)? {
        let error = js_to_string(&error);
        let total_cards = cards.count();
        return Ok(SyncResult {
            synced: false,
            deck_name: js_to_string(&deck),
            total_cards,
            added_count: 0,
            duplicate_count: 0,
            failed_count: total_cards,
            tsv_export_path: export.map(|export| export.file_path),
            warning: Some(format!(
                "[anki-sync] Failed to create deck '{}': {error}",
                js_to_string(&deck)
            )),
            error: Some(error),
        });
    }

    let models = resolve_note_models(url, timeout)?;
    let notes = build_notes(&cards, &deck, &models, options.allow_duplicates)?;

    // addNotes gets twice the timeout: Anki writes media and checks duplicates per note.
    let notes_timeout = ping::clamp_timer(raw_timeout.map(|value| value.saturating_mul(2)));
    let response = call_anki_connect(url, "addNotes", json!({ "notes": notes }), notes_timeout);
    if let Some(error) = response_error(&response)? {
        let error = js_to_string(&error);
        let total_cards = cards.count();
        return Ok(SyncResult {
            synced: false,
            deck_name: js_to_string(&deck),
            total_cards,
            added_count: 0,
            duplicate_count: 0,
            failed_count: total_cards,
            tsv_export_path: export.map(|export| export.file_path),
            warning: Some(format!("[anki-sync] addNotes failed: {error}")),
            error: Some(error),
        });
    }

    let (added_count, duplicate_count) = count_note_ids(response_result(&response)?)?;
    Ok(SyncResult {
        synced: true,
        deck_name: js_to_string(&deck),
        total_cards: cards.count(),
        added_count,
        duplicate_count,
        failed_count: 0,
        tsv_export_path: export.map(|export| export.file_path),
        warning: None,
        error: None,
    })
}

/// Node's `err.message`: the text after the exception name.
fn js_error_message(text: &str) -> &str {
    for prefix in ["Error: ", "TypeError: ", "SyntaxError: ", "RangeError: "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return rest;
        }
    }
    text
}

#[derive(Clone)]
struct NoteModel {
    name: Value,
    fields: [String; 2],
}

struct NoteModels {
    basic: NoteModel,
    cloze: NoteModel,
}

fn default_models() -> NoteModels {
    NoteModels {
        basic: NoteModel {
            name: Value::String("Basic".to_string()),
            fields: ["Front".to_string(), "Back".to_string()],
        },
        cloze: NoteModel {
            name: Value::String("Cloze".to_string()),
            fields: ["Text".to_string(), "Extra".to_string()],
        },
    }
}

/// One note per card, in order; a null card fails like `card.type` does.
fn build_notes(
    cards: &CardList,
    deck: &Value,
    models: &NoteModels,
    allow_duplicates: bool,
) -> Result<Vec<Value>, String> {
    let items = match cards {
        CardList::NotArray(_) => return Err("TypeError: cards.map is not a function".to_string()),
        CardList::Items(items) => items,
    };

    let mut notes = Vec::with_capacity(items.len());
    for card in items {
        if card.is_null() {
            return Err("TypeError: Cannot read properties of null (reading 'type')".to_string());
        }
        let basic = card.get("type").and_then(Value::as_str) == Some("basic");
        let model = if basic { &models.basic } else { &models.cloze };
        let first = if basic {
            card.get("front").cloned()
        } else {
            card.get("text").cloned()
        };
        // `card.extra || ""` keeps the empty field, so only a basic card can lose the second one.
        let second = if basic {
            card.get("back").cloned()
        } else {
            match card.get("extra") {
                Some(value) if js_truthy(Some(value)) => Some(value.clone()),
                _ => Some(Value::String(String::new())),
            }
        };

        let mut field_entries = Vec::with_capacity(2);
        if model.fields[0] == model.fields[1] {
            if let Some(value) = second {
                field_entries.push((model.fields[0].clone(), value));
            }
        } else {
            if let Some(value) = first {
                field_entries.push((model.fields[0].clone(), value));
            }
            if let Some(value) = second {
                field_entries.push((model.fields[1].clone(), value));
            }
        }

        let mut note = Map::new();
        note.insert("deckName".to_string(), deck.clone());
        note.insert("modelName".to_string(), model.name.clone());
        note.insert("fields".to_string(), fields_object(field_entries));
        if let Some(tags) = card.get("tags") {
            note.insert("tags".to_string(), tags.clone());
        }
        note.insert(
            "options".to_string(),
            json!({ "allowDuplicate": allow_duplicates, "duplicateScope": "deck" }),
        );
        notes.push(Value::Object(note));
    }
    Ok(notes)
}

/// The `fields` object Node writes as `{[f0]: v0, [f1]: v1}`: V8 enumerates array-index keys first,
/// ascending, then the rest in insertion order, and `JSON.stringify` follows that order.
fn fields_object(mut entries: Vec<(String, Value)>) -> Value {
    entries.sort_by_key(|(key, _)| match array_index(key) {
        Some(index) => (0, index),
        None => (1, 0),
    });
    Value::Object(entries.into_iter().collect())
}

/// `result || []` then one added per number, one duplicate per other item.
fn count_note_ids(result: Option<Value>) -> Result<(usize, usize), String> {
    let ids = match result.filter(|value| js_truthy(Some(value))) {
        Some(value) => value,
        None => Value::Array(Vec::new()),
    };
    match ids {
        Value::Array(items) => {
            let added = items.iter().filter(|item| item.is_number()).count();
            Ok((added, items.len() - added))
        }
        // A string is iterable in JS: one duplicate per code point.
        Value::String(text) => Ok((0, text.chars().count())),
        _ => Err("TypeError: noteIds is not iterable".to_string()),
    }
}

fn resolve_note_models(url: &str, timeout_ms: u64) -> Result<NoteModels, String> {
    let response = call_anki_connect(url, "modelNames", json!({}), timeout_ms);
    if response_error(&response)?.is_some() {
        return Ok(default_models());
    }
    let Some(Value::Array(names)) = response_result(&response)? else {
        return Ok(default_models());
    };

    let defaults = default_models();
    let basic = resolve_note_model(url, timeout_ms, &names, &defaults.basic, "Basic", is_basic)?;
    let cloze = resolve_note_model(url, timeout_ms, &names, &defaults.cloze, "Cloze", is_cloze)?;
    Ok(NoteModels { basic, cloze })
}

fn resolve_note_model(
    url: &str,
    timeout_ms: u64,
    names: &[Value],
    fallback: &NoteModel,
    exact: &str,
    loose: fn(&str) -> bool,
) -> Result<NoteModel, String> {
    let name = names
        .iter()
        .find(|value| value.as_str() == Some(exact))
        .or_else(|| names.iter().find(|value| loose(&js_to_string(value))));
    let Some(name) = name else {
        return Ok(fallback.clone());
    };

    let with_fallback_fields = |name: &Value| NoteModel {
        name: name.clone(),
        fields: fallback.fields.clone(),
    };
    let response = call_anki_connect(
        url,
        "modelFieldNames",
        json!({ "modelName": name }),
        timeout_ms,
    );
    if response_error(&response)?.is_some() {
        return Ok(with_fallback_fields(name));
    }
    let Some(result) = response_result(&response)?.filter(|value| js_truthy(Some(value))) else {
        return Ok(with_fallback_fields(name));
    };

    let fields = match &result {
        Value::Array(items) if items.len() >= 2 => {
            [js_to_string(&items[0]), js_to_string(&items[1])]
        }
        Value::Array(_) => fallback.fields.clone(),
        Value::String(text) => {
            // JS indexes a string by UTF-16 unit, and `.length` counts units.
            let units: Vec<u16> = text.encode_utf16().collect();
            if units.len() < 2 {
                fallback.fields.clone()
            } else {
                [
                    String::from_utf16_lossy(&units[..1]),
                    String::from_utf16_lossy(&units[1..2]),
                ]
            }
        }
        other => [js_concat(other.get("0")), js_concat(other.get("1"))],
    };
    Ok(NoteModel {
        name: name.clone(),
        fields,
    })
}

fn is_basic(name: &str) -> bool {
    name.eq_ignore_ascii_case("basic") || name.eq_ignore_ascii_case("basilare")
}

fn is_cloze(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("cloze") || lower.contains("completare")
}

/// `callAnkiConnect`: one JSON-RPC POST, transport failures folded into the envelope.
fn call_anki_connect(url: &str, action: &str, params: Value, timeout_ms: u64) -> Value {
    let mut body = Map::new();
    body.insert("action".to_string(), Value::String(action.to_string()));
    body.insert("version".to_string(), Value::from(6));
    body.insert("params".to_string(), params);
    let body = stringify_compact(&Value::Object(body));

    let failed = |error: String| json!({ "result": null, "error": error });
    let client = match reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(timeout_ms))
        .build()
    {
        Ok(client) => client,
        Err(_) => return failed("fetch failed".to_string()),
    };
    let response = match client
        .post(url)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(body)
        .send()
    {
        Ok(response) => response,
        Err(error) => return failed(call_transport_error(&error)),
    };
    let status = response.status();
    if !status.is_success() {
        return failed(format!(
            "HTTP {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        ));
    }
    let bytes = match response.bytes() {
        Ok(bytes) => bytes,
        Err(error) => return failed(call_transport_error(&error)),
    };
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = js_trim(&text);
    if trimmed.is_empty() {
        return failed("Unexpected end of JSON input".to_string());
    }
    match parse_json(trimmed) {
        Ok(value) => value,
        Err(error) => failed(error),
    }
}

fn call_transport_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "AnkiConnect request timed out".to_string()
    } else {
        "fetch failed".to_string()
    }
}

fn response_error(response: &Value) -> Result<Option<Value>, String> {
    match response {
        Value::Null => Err(
            "TypeError: Cannot read properties of null (reading 'error')".to_string(),
        ),
        Value::Object(map) => Ok(map
            .get("error")
            .filter(|value| js_truthy(Some(*value)))
            .cloned()),
        _ => Ok(None),
    }
}

fn response_result(response: &Value) -> Result<Option<Value>, String> {
    match response {
        Value::Null => Err(
            "TypeError: Cannot read properties of null (reading 'result')".to_string(),
        ),
        Value::Object(map) => Ok(map.get("result").cloned()),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Read};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "proxi-anki-sync-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    /// A throwaway AnkiConnect that answers by action, so the sync's call order does not matter.
    struct FakeAnki {
        url: String,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeAnki {
        fn start(note_ids: Value) -> FakeAnki {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake anki");
            let port = listener.local_addr().expect("fake anki addr").port();
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    // A non-blocking accept keeps drop() from hanging on a silent socket.
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            answer(stream, &note_ids);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
            FakeAnki {
                url: format!("http://127.0.0.1:{port}"),
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for FakeAnki {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn answer(mut stream: TcpStream, note_ids: &Value) {
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some(value) = line
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|rest| rest.trim().parse().ok())
            {
                length = value;
            }
        }
        let mut body = vec![0u8; length];
        let _ = reader.read_exact(&mut body);
        let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let model = request
            .get("params")
            .and_then(|params| params.get("modelName"));
        let reply = match request.get("action").and_then(Value::as_str) {
            Some("version") => json!({ "result": 6, "error": null }),
            Some("createDeck") => json!({ "result": true, "error": null }),
            Some("modelNames") => json!({ "result": ["Basic", "Cloze"], "error": null }),
            Some("modelFieldNames") if model == Some(&json!("Cloze")) => {
                json!({ "result": ["Text", "Extra"], "error": null })
            }
            Some("modelFieldNames") => json!({ "result": ["Front", "Back"], "error": null }),
            Some("addNotes") => json!({ "result": note_ids, "error": null }),
            other => json!({ "result": null, "error": format!("Unsupported action {other:?}") }),
        };
        let text = serde_json::to_string(&reply).expect("fake anki reply");
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        let _ = stream.flush();
    }

    fn notes_for(
        cards: Value,
        models: &NoteModels,
        allow_duplicates: bool,
    ) -> Result<Vec<Value>, String> {
        build_notes(
            &CardList::from_option(Some(&cards)),
            &json!("Proxi::Demo"),
            models,
            allow_duplicates,
        )
    }

    #[test]
    fn js_error_message_strips_the_exception_name() {
        assert_eq!(js_error_message("TypeError: x"), "x");
        assert_eq!(
            js_error_message("Error: Lesson file not found"),
            "Lesson file not found"
        );
        assert_eq!(js_error_message("SyntaxError: x"), "x");
        assert_eq!(js_error_message("RangeError: x"), "x");
        assert_eq!(js_error_message("plain"), "plain");
        assert_eq!(js_error_message("TypeError: Error: x"), "Error: x");
    }

    #[test]
    fn basic_and_cloze_notes_serialise_like_node() {
        let cards = json!([
            {"type": "basic", "front": "Q?", "back": "A.", "tags": ["t"]},
            {"type": "cloze", "text": "Il {{c1::x}}", "extra": "e", "tags": null}
        ]);
        let notes = notes_for(cards, &default_models(), false).unwrap();

        assert_eq!(
            stringify_compact(&notes[0]),
            r#"{"deckName":"Proxi::Demo","modelName":"Basic","fields":{"Front":"Q?","Back":"A."},"tags":["t"],"options":{"allowDuplicate":false,"duplicateScope":"deck"}}"#
        );
        assert_eq!(
            stringify_compact(&notes[1]),
            r#"{"deckName":"Proxi::Demo","modelName":"Cloze","fields":{"Text":"Il {{c1::x}}","Extra":"e"},"tags":null,"options":{"allowDuplicate":false,"duplicateScope":"deck"}}"#
        );
    }

    #[test]
    fn cloze_without_extra_keeps_an_empty_field() {
        let notes = notes_for(
            json!([{"type": "cloze", "text": "x", "tags": []}]),
            &default_models(),
            false,
        )
        .unwrap();
        assert_eq!(notes[0]["fields"], json!({ "Text": "x", "Extra": "" }));
    }

    #[test]
    fn a_non_object_card_takes_the_cloze_branch() {
        let notes = notes_for(json!([5]), &default_models(), false).unwrap();
        assert_eq!(
            stringify_compact(&notes[0]),
            r#"{"deckName":"Proxi::Demo","modelName":"Cloze","fields":{"Extra":""},"options":{"allowDuplicate":false,"duplicateScope":"deck"}}"#
        );
    }

    #[test]
    fn a_missing_basic_value_is_dropped_from_fields() {
        let notes = notes_for(
            json!([{"type": "basic", "back": "A."}]),
            &default_models(),
            true,
        )
        .unwrap();
        assert_eq!(
            stringify_compact(&notes[0]),
            r#"{"deckName":"Proxi::Demo","modelName":"Basic","fields":{"Back":"A."},"options":{"allowDuplicate":true,"duplicateScope":"deck"}}"#
        );
    }

    #[test]
    fn equal_field_names_keep_only_the_second_value() {
        let models = NoteModels {
            basic: NoteModel {
                name: json!("Basic"),
                fields: ["Front".to_string(), "Front".to_string()],
            },
            ..default_models()
        };
        let notes = notes_for(
            json!([{"type": "basic", "front": "F", "back": "B"}]),
            &models,
            false,
        )
        .unwrap();
        assert_eq!(notes[0]["fields"], json!({ "Front": "B" }));
    }

    #[test]
    fn integer_like_field_names_serialise_in_js_order() {
        let models = NoteModels {
            basic: NoteModel {
                name: json!("Basic"),
                fields: ["2".to_string(), "1".to_string()],
            },
            ..default_models()
        };
        let notes = notes_for(
            json!([{"type": "basic", "front": "F", "back": "B"}]),
            &models,
            false,
        )
        .unwrap();
        assert_eq!(
            stringify_compact(&notes[0]),
            r#"{"deckName":"Proxi::Demo","modelName":"Basic","fields":{"1":"B","2":"F"},"options":{"allowDuplicate":false,"duplicateScope":"deck"}}"#
        );

        let models = NoteModels {
            basic: NoteModel {
                name: json!("Basic"),
                fields: ["Back".to_string(), "1".to_string()],
            },
            ..default_models()
        };
        let notes = notes_for(
            json!([{"type": "basic", "front": "F", "back": "B"}]),
            &models,
            false,
        )
        .unwrap();
        assert_eq!(
            stringify_compact(&notes[0]),
            r#"{"deckName":"Proxi::Demo","modelName":"Basic","fields":{"1":"B","Back":"F"},"options":{"allowDuplicate":false,"duplicateScope":"deck"}}"#
        );
    }

    #[test]
    fn tags_missing_is_omitted_and_null_is_kept() {
        let notes = notes_for(
            json!([
                {"type": "cloze", "text": "a"},
                {"type": "cloze", "text": "b", "tags": null}
            ]),
            &default_models(),
            false,
        )
        .unwrap();
        assert!(notes[0].get("tags").is_none(), "{}", notes[0]);
        assert_eq!(notes[1]["tags"], Value::Null);
    }

    #[test]
    fn a_null_card_is_a_fatal_error() {
        assert_eq!(
            notes_for(json!([null]), &default_models(), false).unwrap_err(),
            "TypeError: Cannot read properties of null (reading 'type')"
        );
    }

    #[test]
    fn non_array_cards_fail_like_node() {
        let number = CardList::from_option(Some(&json!(5)));
        assert_eq!(number.length_text(), "undefined");
        assert_eq!(
            build_notes(&number, &json!("D"), &default_models(), false).unwrap_err(),
            "TypeError: cards.map is not a function"
        );

        let text = CardList::from_option(Some(&json!("ab")));
        assert_eq!(text.length_text(), "2");

        for falsy in [None, Some(json!(null)), Some(json!([])), Some(json!(""))] {
            let empty = CardList::from_option(falsy.as_ref());
            assert!(empty.is_empty());
            assert_eq!(empty.length_text(), "0");
        }
    }

    #[test]
    fn note_ids_count_numbers_as_added_and_the_rest_as_duplicates() {
        assert_eq!(
            count_note_ids(Some(json!([2001, null, "x"]))).unwrap(),
            (1, 2)
        );
        assert_eq!(count_note_ids(Some(json!("ab"))).unwrap(), (0, 2));
        assert_eq!(count_note_ids(None).unwrap(), (0, 0));
        assert_eq!(count_note_ids(Some(json!(0))).unwrap(), (0, 0));
        assert_eq!(count_note_ids(Some(json!([]))).unwrap(), (0, 0));
        assert_eq!(
            count_note_ids(Some(json!(true))).unwrap_err(),
            "TypeError: noteIds is not iterable"
        );
        assert_eq!(
            count_note_ids(Some(json!(5))).unwrap_err(),
            "TypeError: noteIds is not iterable"
        );
        assert_eq!(
            count_note_ids(Some(json!({ "a": 1 }))).unwrap_err(),
            "TypeError: noteIds is not iterable"
        );
    }

    #[test]
    fn offline_sync_counts_every_card_as_failed() {
        let dir = temp_dir("offline-counts");
        let mut warn = Vec::new();
        let result = sync_to_anki_connect(
            &SyncOptions {
                cards: Some(json!([
                    { "type": "basic", "front": "Q1", "back": "A1" },
                    { "type": "cloze", "text": "Il {{c1::x}}" }
                ])),
                topic: Some(json!("Demo")),
                anki_url: Some("http://127.0.0.1:1".to_string()),
                timeout: Some("10000".to_string()),
                base_dir: Some(dir.0.display().to_string()),
                ..Default::default()
            },
            &mut warn,
        )
        .unwrap();

        assert!(!result.synced);
        assert_eq!(result.error.as_deref(), Some("fetch failed"));
        assert_eq!(result.total_cards, 2);
        assert_eq!(result.added_count, 0);
        assert_eq!(result.duplicate_count, 0);
        assert_eq!(result.failed_count, 2);
    }

    #[test]
    fn sync_counts_cards_and_duplicates_against_anki() {
        let dir = temp_dir("counts");
        // One null for the card Anki already has.
        let fake = FakeAnki::start(json!([2001, null, 2003]));
        let mut warn = Vec::new();

        let result = sync_to_anki_connect(
            &SyncOptions {
                cards: Some(json!([
                    { "type": "basic", "front": "Q1", "back": "A1" },
                    { "type": "basic", "front": "Q2", "back": "A2" },
                    { "type": "cloze", "text": "Il {{c1::x}}", "extra": "e" }
                ])),
                topic: Some(json!("Demo")),
                anki_url: Some(fake.url.clone()),
                base_dir: Some(dir.0.display().to_string()),
                ..Default::default()
            },
            &mut warn,
        )
        .unwrap();

        assert!(result.synced);
        assert_eq!(result.total_cards, 3);
        assert_eq!(result.added_count, 2);
        assert_eq!(result.duplicate_count, 1);
        assert_eq!(result.failed_count, 0);
    }

    #[test]
    fn a_non_array_card_list_counts_no_cards() {
        // Node's `.length` on a non-array is a string length or undefined, not a card count.
        assert_eq!(CardList::from_option(Some(&json!("ab"))).count(), 0);
        assert_eq!(CardList::from_option(Some(&json!(5))).count(), 0);
        assert_eq!(CardList::from_option(Some(&json!([]))).count(), 0);
        assert_eq!(
            CardList::from_option(Some(&json!([{ "type": "cloze" }]))).count(),
            1
        );
    }
}
