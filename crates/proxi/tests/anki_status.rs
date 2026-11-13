mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use proxi::anki::{fetch_anki_status, invoke, CourseCounts, DEFAULT_TIMEOUT};

fn zero() -> CourseCounts {
    CourseCounts {
        due: 0,
        learning: 0,
        new_cards: 0,
    }
}

#[tokio::test]
async fn counts_due_new_learning_from_the_root_deck_only() {
    let anki = common::fake_anki(|request| {
        let action = request["action"].as_str().unwrap_or("");
        let result = if action == "deckNamesAndIds" {
            json!({ "Altro": 9, "Proxi": 1, "Proxi::Vue": 2, "Proxi::Vue::Proxi": 3 })
        } else {
            json!({
                "3": { "deck_id": 3, "name": "Proxi", "new_count": 0, "learn_count": 0, "review_count": 0, "total_in_deck": 1 },
                "1": { "deck_id": 1, "name": "Proxi", "new_count": 2, "learn_count": 1, "review_count": 3, "total_in_deck": 0 },
                "2": { "deck_id": 2, "name": "Vue", "new_count": 2, "learn_count": 1, "review_count": 3, "total_in_deck": 7 }
            })
        };
        common::FakeReply {
            status: 200,
            body: json!({ "result": result, "error": null }).to_string(),
            delay: Duration::ZERO,
        }
    });

    let status = fetch_anki_status(
        &anki.url,
        DEFAULT_TIMEOUT,
        || Vec::<String>::new(),
        |course| format!("Proxi::{course}"),
    )
    .await
    .expect("status");

    assert_eq!(status.due, 3);
    assert_eq!(status.new_cards, 2);
    assert_eq!(status.learning, 1);
    assert_eq!(status.total, 8);
    assert_eq!(status.deck_count, 3);
    assert!(status.by_course.is_empty());
}

#[tokio::test]
async fn maps_one_get_deck_stats_batch_to_by_course_and_zero_fills() {
    let anki = common::fake_anki_scripted(&json!({
        "deckNamesAndIds": { "result": { "Proxi::vue": 2 }, "error": null },
        "getDeckStats": {
            "result": {
                "2": { "deck_id": 2, "name": "Vue", "new_count": 2, "learn_count": 1, "review_count": 3, "total_in_deck": 9 }
            },
            "error": null
        }
    }));

    let status = fetch_anki_status(
        &anki.url,
        DEFAULT_TIMEOUT,
        || vec!["vue".to_string(), "rust".to_string()],
        |course| format!("Proxi::{course}"),
    )
    .await
    .expect("status");

    assert_eq!(
        status.by_course,
        vec![
            (
                "vue".to_string(),
                CourseCounts {
                    due: 3,
                    learning: 1,
                    new_cards: 2,
                },
            ),
            ("rust".to_string(), zero()),
        ]
    );
    let requests = anki.requests();
    let stats: Vec<_> = requests
        .iter()
        .filter(|request| request["action"] == "getDeckStats")
        .collect();
    assert_eq!(
        stats.len(),
        1,
        "one batch for every course, never one call per course"
    );
    assert_eq!(stats[0]["params"]["decks"], json!(["Proxi::vue"]));
}

#[tokio::test]
async fn zero_fills_planned_courses_when_no_proxi_deck_exists() {
    let anki = common::fake_anki_scripted(&json!({
        "deckNamesAndIds": { "result": { "Altro": 9 }, "error": null }
    }));

    let status = fetch_anki_status(
        &anki.url,
        DEFAULT_TIMEOUT,
        || vec!["vue".to_string(), "rust".to_string()],
        |course| format!("Proxi::{course}"),
    )
    .await
    .expect("status");

    assert_eq!(status.deck_count, 0);
    // A course whose deck does not exist yet still gets an entry, so the board can hide its badge.
    assert_eq!(
        status.by_course,
        vec![("vue".to_string(), zero()), ("rust".to_string(), zero())]
    );
    let requests = anki.requests();
    assert_eq!(requests.len(), 1, "no Proxi deck: nothing to ask getDeckStats about");
}

#[tokio::test]
async fn ignores_decks_under_another_root() {
    let anki = common::fake_anki_scripted(&json!({
        "deckNamesAndIds": {
            "result": { "Proxi": 1, "Proxi::Demo": 2, "Altro::Demo": 3 },
            "error": null
        },
        "getDeckStats": { "result": {}, "error": null }
    }));

    fetch_anki_status(
        &anki.url,
        DEFAULT_TIMEOUT,
        || Vec::<String>::new(),
        |course| course.to_string(),
    )
    .await
    .expect("status");

    let requests = anki.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["params"]["decks"], json!(["Proxi", "Proxi::Demo"]));
}

#[tokio::test]
async fn invoke_sends_action_version_and_only_present_params() {
    let anki = common::fake_anki_scripted(&json!({
        "deckNamesAndIds": { "result": {}, "error": null },
        "getDeckStats": { "result": {}, "error": null }
    }));

    let response = invoke(&anki.url, "deckNamesAndIds", None, DEFAULT_TIMEOUT).await;
    assert_eq!(response.error, None);
    assert_eq!(response.result, Some(json!({})));

    invoke(
        &anki.url,
        "getDeckStats",
        Some(json!({ "decks": ["Proxi"] })),
        DEFAULT_TIMEOUT,
    )
    .await;

    let requests = anki.requests();
    assert_eq!(
        requests[0].to_string(),
        r#"{"action":"deckNamesAndIds","version":6}"#
    );
    assert_eq!(
        requests[1].to_string(),
        r#"{"action":"getDeckStats","version":6,"params":{"decks":["Proxi"]}}"#
    );
}

#[tokio::test]
async fn closed_port_is_fetch_failed() {
    let url = common::closed_port_url();

    let error = fetch_anki_status(
        &url,
        DEFAULT_TIMEOUT,
        || Vec::<String>::new(),
        |course| course.to_string(),
    )
    .await
    .expect_err("closed port");
    assert_eq!(error, "fetch failed");

    let response = invoke(&url, "deckNamesAndIds", None, DEFAULT_TIMEOUT).await;
    assert_eq!(response.result, None);
    assert_eq!(response.error.as_deref(), Some("fetch failed"));
}

#[tokio::test]
async fn http_error_keeps_node_status_text() {
    let anki = common::fake_anki(|_| common::FakeReply {
        status: 500,
        body: "{}".to_string(),
        delay: Duration::ZERO,
    });

    let response = invoke(&anki.url, "deckNamesAndIds", None, DEFAULT_TIMEOUT).await;
    assert_eq!(
        response.error.as_deref(),
        Some("HTTP 500 Internal Server Error")
    );
}

#[tokio::test]
async fn slow_anki_times_out() {
    let anki = common::fake_anki(|_| common::FakeReply {
        status: 200,
        body: json!({ "result": {}, "error": null }).to_string(),
        delay: Duration::from_millis(500),
    });

    let response = invoke(
        &anki.url,
        "deckNamesAndIds",
        None,
        Duration::from_millis(100),
    )
    .await;
    assert_eq!(
        response.error.as_deref(),
        Some("AnkiConnect request timed out")
    );
}

#[tokio::test]
async fn anki_error_fails_the_status_without_scanning_courses() {
    let anki = common::fake_anki_scripted(&json!({
        "deckNamesAndIds": { "result": null, "error": "boom" }
    }));
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);

    let error = fetch_anki_status(
        &anki.url,
        DEFAULT_TIMEOUT,
        move || {
            seen.fetch_add(1, Ordering::SeqCst);
            Vec::<String>::new()
        },
        |course| course.to_string(),
    )
    .await
    .expect_err("boom");

    assert_eq!(error, "boom");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
