//! The Rust fake AnkiConnect: `fake_anki` answers the calls the Anki routes make, `mock_anki` the
//! ones `proxi anki sync` makes.

mod common;

#[path = "common/fake_http.rs"]
mod fake_http;

#[path = "common/mock_anki.rs"]
mod mock_anki;

use common::fake_anki::{card, FakeAnki, FakeState};
use mock_anki::{MockAnki, MockAnkiState};
use reqwest::header::CONTENT_TYPE;
use reqwest::Client;
use serde_json::{json, Value};

async fn invoke(client: &Client, url: &str, body: Value) -> Value {
    let text = client
        .post(url)
        .header(CONTENT_TYPE, "application/json")
        .body(serde_json::to_string(&body).expect("serialise request"))
        .send()
        .await
        .expect("request fake anki")
        .text()
        .await
        .expect("fake anki body");
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("fake anki sent {text:?}: {error}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn find_cards_matches_the_deck_prefix_and_unsupported_actions_are_reported() {
    let mut state = FakeState::default();
    state.cards = vec![
        card(1001, 2001, "Proxi::Demo", &["node::alpha"], 10, 0, &[]),
        card(1002, 2002, "Proxi::Demo::Sub", &[], 1, 0, &[]),
        card(1003, 2003, "Altro::Demo", &[], 1, 0, &[]),
    ];
    let fake = FakeAnki::start(state).await;
    let client = Client::new();

    let found = invoke(
        &client,
        &fake.url,
        json!({"action": "findCards", "version": 6, "params": {"query": "deck:\"Proxi::Demo\""}}),
    )
    .await;
    assert_eq!(found, json!({"result": [1001, 1002], "error": null}));

    let unknown = invoke(&client, &fake.url, json!({"action": "nope", "version": 6})).await;
    assert_eq!(unknown, json!({"result": null, "error": "Unsupported action nope"}));

    assert_eq!(fake.actions(), ["findCards", "nope"]);
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["params"]["query"], "deck:\"Proxi::Demo\"");
    assert!(requests[1].get("params").is_none());
}

/// A basic note as the sync sends it, `options` included.
const NOTE: &str = r#"{"deckName":"Proxi::Demo","modelName":"Basic","fields":{"Front":"Q","Back":"A"},"options":{"allowDuplicate":false,"duplicateScope":"deck"}}"#;

fn add_notes(notes: &str) -> String {
    format!(r#"{{"action":"addNotes","version":6,"params":{{"notes":[{notes}]}}}}"#)
}

fn post(url: &str, body: &str) -> Value {
    let text = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .expect("blocking client")
        .post(url)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .body(body.to_string())
        .send()
        .expect("post mock anki")
        .text()
        .expect("mock anki body");
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("mock anki sent {text:?}: {error}"))
}

#[test]
fn version_and_create_deck_answer_the_sync_sequence() {
    let fake = MockAnki::start(false, None);

    assert_eq!(
        post(fake.url(), r#"{"action":"version","version":6}"#),
        json!({"result": 6, "error": null})
    );
    assert_eq!(
        post(
            fake.url(),
            r#"{"action":"createDeck","version":6,"params":{"deck":"Proxi::Demo"}}"#
        ),
        json!({"result": 1725128400000i64, "error": null})
    );
    assert_eq!(fake.actions(), ["version", "createDeck"]);
    assert_eq!(fake.requests()[1]["params"]["deck"], "Proxi::Demo");
}

#[test]
fn reject_duplicates_answers_null_unless_allow_duplicate() {
    let fake = MockAnki::start(true, None);

    assert_eq!(post(fake.url(), &add_notes(NOTE))["result"], json!([2001]));
    assert_eq!(post(fake.url(), &add_notes(NOTE))["result"], json!([null]));

    let allowed = NOTE.replace("\"allowDuplicate\":false", "\"allowDuplicate\":true");
    assert_eq!(post(fake.url(), &add_notes(&allowed))["result"], json!([2002]));

    assert_eq!(fake.added_notes().len(), 2);
}

#[test]
fn duplicate_detection_reads_the_first_field_in_js_order() {
    let fake = MockAnki::start(true, None);

    let first = NOTE.replace(
        r#""fields":{"Front":"Q","Back":"A"}"#,
        r#""fields":{"2":"Q","1":"A"}"#,
    );
    let second = NOTE.replace(
        r#""fields":{"Front":"Q","Back":"A"}"#,
        r#""fields":{"1":"A","2":"R"}"#,
    );
    assert_eq!(post(fake.url(), &add_notes(&first))["result"], json!([2001]));
    assert_eq!(post(fake.url(), &add_notes(&second))["result"], json!([null]));
}

#[test]
fn the_same_note_twice_in_one_batch_keeps_only_the_first() {
    let fake = MockAnki::start(true, None);

    let batch = format!("{NOTE},{NOTE}");
    assert_eq!(post(fake.url(), &add_notes(&batch))["result"], json!([2001, null]));
    assert_eq!(fake.added_notes().len(), 1);
}

#[test]
fn without_rejection_every_note_gets_a_fresh_id() {
    let fake = MockAnki::start(false, None);

    assert_eq!(post(fake.url(), &add_notes(NOTE))["result"], json!([2001]));
    assert_eq!(post(fake.url(), &add_notes(NOTE))["result"], json!([2002]));
    assert_eq!(fake.added_notes().len(), 2);
}

#[test]
fn model_names_and_fields_follow_the_profile() {
    let fake = MockAnki::start(
        false,
        Some(&[
            ("Basilare", ["Fronte", "Retro"]),
            ("Testo da completare", ["Testo", "Extra"]),
        ]),
    );

    assert_eq!(
        post(fake.url(), r#"{"action":"modelNames","version":6,"params":{}}"#),
        json!({"result": ["Basilare", "Testo da completare"], "error": null})
    );
    assert_eq!(
        post(
            fake.url(),
            r#"{"action":"modelFieldNames","version":6,"params":{"modelName":"Basilare"}}"#
        ),
        json!({"result": ["Fronte", "Retro"], "error": null})
    );
    assert_eq!(
        post(
            fake.url(),
            r#"{"action":"modelFieldNames","version":6,"params":{"modelName":"Nope"}}"#
        ),
        json!({"result": null, "error": null})
    );
}

#[test]
fn unsupported_actions_are_reported() {
    let fake = MockAnki::start(false, None);

    assert_eq!(
        post(fake.url(), r#"{"action":"modelNames","version":6,"params":{}}"#),
        json!({"result": null, "error": "Unsupported action modelNames"})
    );
    assert_eq!(
        post(fake.url(), ""),
        json!({"result": null, "error": "Unsupported action undefined"})
    );
}

#[test]
fn invalid_json_is_a_400() {
    let mut state = MockAnkiState::new();
    let (status, answer) = state.answer("{");

    assert_eq!(status, 400);
    assert_eq!(answer["result"], Value::Null);
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
}
