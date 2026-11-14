//! Anki status and review start against the fake AnkiConnect: Node's exact response text.

mod common;

use common::anki_seed::{open_state, seed};
use common::fake_anki::{FakeAnki, FakeState};
use common::TempDir;
use proxi::anki_client::{AnkiClient, FETCH_FAILED, TIMED_OUT};
use proxi::anki_status::{start_review, status_all, status_for_course, ReviewStart};
use proxi::protocol::AnkiStatusResponse;
use serde_json::json;

async fn setup(state: FakeState) -> (TempDir, FakeAnki, AnkiClient) {
    let data = common::temp_dir("anki-status");
    seed(&data.0);
    let fake = FakeAnki::start(state).await;
    let client = AnkiClient::new(&fake.url).expect("build anki client");
    (data, fake, client)
}

/// A port nothing listens on: Anki is closed.
fn closed_client() -> AnkiClient {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a port to drop");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    AnkiClient::new(format!("http://127.0.0.1:{port}")).expect("build anki client")
}

fn serialise(status: &AnkiStatusResponse) -> String {
    serde_json::to_string(status).expect("serialise status")
}

#[tokio::test(flavor = "multi_thread")]
async fn open_anki_reports_the_root_totals_and_every_course() {
    let (data, fake, client) = setup(open_state()).await;

    let text = serialise(&status_all(&client, &data.0).await);

    assert_eq!(
        text,
        r#"{"online":true,"due":4,"newCards":5,"learning":2,"total":13,"byCourse":{"demo":{"due":4,"learning":2,"newCards":5},"vuoto":{"due":0,"learning":0,"newCards":0},"orfano":{"due":0,"learning":0,"newCards":0}}}"#
    );
    // `params` is absent from deckNamesAndIds, as in Node.
    assert_eq!(
        fake.requests(),
        vec![
            json!({"action": "deckNamesAndIds", "version": 6}),
            json!({
                "action": "getDeckStats",
                "version": 6,
                "params": {"decks": ["Proxi", "Proxi::Demo", "Proxi::Altro"]},
            }),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_root_deck_the_due_counts_sum_every_proxi_entry() {
    let mut state = open_state();
    state.deck_names_and_ids = Some(json!({ "Proxi::Demo": 2, "Proxi::Altro": 3 }));
    state.deck_stats = Some(json!({
        "2": { "deck_id": 2, "name": "Demo", "new_count": 5, "learn_count": 2, "review_count": 4, "total_in_deck": 10 },
        "3": { "deck_id": 3, "name": "Altro", "new_count": 1, "learn_count": 0, "review_count": 0, "total_in_deck": 3 },
    }));
    let (data, _fake, client) = setup(state).await;

    let text = serialise(&status_all(&client, &data.0).await);

    assert_eq!(
        text,
        r#"{"online":true,"due":4,"newCards":6,"learning":2,"total":13,"byCourse":{"demo":{"due":4,"learning":2,"newCards":5},"vuoto":{"due":0,"learning":0,"newCards":0},"orfano":{"due":0,"learning":0,"newCards":0}}}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_proxi_deck_reports_zeros_without_asking_for_stats() {
    let mut state = FakeState::default();
    state.deck_names_and_ids = Some(json!({ "Default": 1 }));
    let (data, fake, client) = setup(state).await;

    let text = serialise(&status_all(&client, &data.0).await);

    assert_eq!(
        text,
        r#"{"online":true,"due":0,"newCards":0,"learning":0,"total":0,"byCourse":{"demo":{"due":0,"learning":0,"newCards":0},"vuoto":{"due":0,"learning":0,"newCards":0},"orfano":{"due":0,"learning":0,"newCards":0}}}"#
    );
    assert_eq!(fake.actions(), ["deckNamesAndIds"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn course_status_matches_the_node_text_and_a_missing_deck_is_not_an_error() {
    let (data, _fake, client) = setup(open_state()).await;

    assert_eq!(
        serialise(&status_for_course(&client, &data.0, "demo").await),
        r#"{"online":true,"due":4,"newCards":5,"learning":2,"total":10,"deck":"Proxi::Demo","deckExists":true}"#
    );
    assert_eq!(
        serialise(&status_for_course(&client, &data.0, "vuoto").await),
        r#"{"online":true,"due":0,"newCards":0,"learning":0,"total":0,"deck":"Proxi::Vuoto","deckExists":false}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_keyed_by_another_id_fall_back_to_the_first_entry() {
    let mut state = open_state();
    state.deck_stats = Some(json!({
        "99": { "deck_id": 99, "name": "Whatever", "new_count": 7, "learn_count": 1, "review_count": 2, "total_in_deck": 5 },
    }));
    let (data, _fake, client) = setup(state).await;

    assert_eq!(
        serialise(&status_for_course(&client, &data.0, "demo").await),
        r#"{"online":true,"due":2,"newCards":7,"learning":1,"total":5,"deck":"Proxi::Demo","deckExists":true}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_fall_back_in_javascript_key_order() {
    let mut state = open_state();
    // Demo's deck id is missing from the stats, so Node's Object.values(stats)[0] picks
    // the smallest canonical array index, not the first inserted key.
    state.deck_names_and_ids = Some(json!({ "Proxi": 1, "Proxi::Demo": 3 }));
    state.deck_stats = Some(json!({
        "10": { "deck_id": 10, "name": "Ten", "new_count": 10, "learn_count": 0, "review_count": 10, "total_in_deck": 1 },
        "2": { "deck_id": 2, "name": "Two", "new_count": 2, "learn_count": 0, "review_count": 2, "total_in_deck": 1 },
    }));
    let (data, _fake, client) = setup(state).await;

    assert_eq!(
        serialise(&status_for_course(&client, &data.0, "demo").await),
        r#"{"online":true,"due":2,"newCards":2,"learning":0,"total":1,"deck":"Proxi::Demo","deckExists":true}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_anki_is_offline_for_all_and_for_one_course() {
    let data = common::temp_dir("anki-status-closed");
    seed(&data.0);
    let client = closed_client();

    let all = status_all(&client, &data.0).await;
    let all_error = all.error.clone().expect("error text");
    assert!(
        matches!(all_error.as_str(), FETCH_FAILED | TIMED_OUT),
        "{all_error}"
    );
    assert_eq!(
        serialise(&all),
        format!(r#"{{"online":false,"error":"{all_error}"}}"#)
    );

    let course = status_for_course(&client, &data.0, "demo").await;
    let course_error = course.error.clone().expect("error text");
    assert!(
        matches!(course_error.as_str(), FETCH_FAILED | TIMED_OUT),
        "{course_error}"
    );
    assert_eq!(
        serialise(&course),
        format!(r#"{{"online":false,"error":"{course_error}","deck":"Proxi::Demo"}}"#)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_anki_error_keeps_its_text() {
    let mut state = open_state();
    state.response_error = Some("boom".to_string());
    let (data, fake, client) = setup(state).await;

    assert_eq!(
        serialise(&status_all(&client, &data.0).await),
        r#"{"online":false,"error":"boom"}"#
    );
    assert_eq!(fake.actions(), ["deckNamesAndIds"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_review_opens_the_course_deck() {
    let (_data, fake, client) = setup(open_state()).await;

    assert_eq!(
        start_review(&client, "Proxi::Demo").await,
        ReviewStart::Started
    );
    assert_eq!(
        fake.actions(),
        ["deckNamesAndIds", "getDeckStats", "guiDeckReview"]
    );
    assert_eq!(
        fake.requests().last(),
        Some(&json!({
            "action": "guiDeckReview",
            "version": 6,
            "params": {"name": "Proxi::Demo"},
        }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_deck_does_not_open_anything() {
    let (_data, fake, client) = setup(open_state()).await;

    assert_eq!(
        start_review(&client, "Proxi::Vuoto").await,
        ReviewStart::NoDeck
    );
    assert_eq!(fake.actions(), ["deckNamesAndIds"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gui_deck_review_error_is_reported() {
    let (_data, fake, client) = setup(open_state()).await;
    fake.update(|state| state.gui_deck_review_error = Some("boom".to_string()));

    assert_eq!(
        start_review(&client, "Proxi::Demo").await,
        ReviewStart::Failed("boom".to_string())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_anki_fails_the_review() {
    let client = closed_client();

    let ReviewStart::Failed(error) = start_review(&client, "Proxi::Demo").await else {
        panic!("a closed Anki must fail the review");
    };
    assert!(matches!(error.as_str(), FETCH_FAILED | TIMED_OUT), "{error}");
}
