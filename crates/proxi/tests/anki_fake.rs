//! The Rust fake AnkiConnect answers the calls the Anki routes make.

mod common;

use common::fake_anki::{card, FakeAnki, FakeState};
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
