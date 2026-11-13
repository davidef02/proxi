//! The shared Anki route seed: the data root of app/test/api-anki.test.ts and its Node golden.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::fake_anki::{card, FakeCard, FakeState};
use proxi::course_data::progress_file;

pub const CURRICULUM: &str = r#"{"chapters":[{"title":"Demo","slug":"demo","lessons":[{"title":"Lesson T","slug":"t","plannedNodes":2},{"title":"Lesson U","slug":"u","plannedNodes":1}]},{"title":"Vuoto","slug":"vuoto","lessons":[]}]}"#;

pub const SEED_LINE: &str = "{\"at\":\"2023-11-14T00:00:00.000Z\",\"sessionId\":\"s1\",\"course\":\"demo\",\"lessonSlug\":\"t\",\"nodeRefs\":[\"demo/t#alpha\"],\"kind\":\"check\",\"correct\":true}\n";

/// Both a refused connection and Windows' slow refusal are acceptable.
pub const OFFLINE_ERRORS: [&str; 2] = ["fetch failed", "AnkiConnect request timed out"];

pub fn lesson_doc(lesson_number: i64, title: &str, slug: &str, node_ids: &[&str]) -> Value {
    json!({
        "metadata": {
            "lessonNumber": lesson_number,
            "topic": "Demo",
            "title": title,
            "slug": slug,
            "dateCreated": "",
            "dateModified": "",
            "version": "1",
        },
        "nodes": node_ids
            .iter()
            .map(|node_id| json!({ "nodeId": node_id, "title": node_id, "tier": "derivation", "content": "x" }))
            .collect::<Vec<_>>(),
    })
}

pub fn write_lesson(data_root: &Path, course: &str, filename: &str, doc: Value) {
    let dir = data_root.join("courses").join(course).join("lessons");
    fs::create_dir_all(&dir).expect("create lessons dir");
    fs::write(
        dir.join(filename),
        format!(r#"<script type="application/json" id="lesson-data">{doc}</script>"#),
    )
    .expect("write lesson");
}

/// The api-anki seed: Demo and Vuoto planned, orfano only on disk, one check already logged.
pub fn seed(data_root: &Path) {
    fs::write(data_root.join("curriculum.json"), CURRICULUM).expect("write curriculum");
    write_lesson(
        data_root,
        "demo",
        "0001-t.html",
        lesson_doc(1, "Lesson T", "t", &["alpha", "dup"]),
    );
    write_lesson(
        data_root,
        "demo",
        "0002-u.html",
        lesson_doc(2, "Lesson U", "u", &["dup"]),
    );
    write_lesson(data_root, "orfano", "0001-o.html", lesson_doc(1, "O", "o", &["x"]));
    fs::write(progress_file(data_root, "demo"), SEED_LINE).expect("seed progress log");
}

pub fn open_state() -> FakeState {
    FakeState {
        deck_names_and_ids: Some(json!({
            "Proxi": 1,
            "Proxi::Demo": 2,
            "Proxi::Altro": 3,
            "Default": 4,
        })),
        deck_stats: Some(json!({
            "1": { "deck_id": 1, "name": "Proxi", "new_count": 5, "learn_count": 2, "review_count": 4, "total_in_deck": 0 },
            "2": { "deck_id": 2, "name": "Demo", "new_count": 5, "learn_count": 2, "review_count": 4, "total_in_deck": 10 },
            "3": { "deck_id": 3, "name": "Altro", "new_count": 1, "learn_count": 0, "review_count": 0, "total_in_deck": 3 },
        })),
        ..FakeState::default()
    }
}

/// The six cards of the Node golden: two skipped, four reviews written.
pub fn golden_cards() -> Vec<FakeCard> {
    vec![
        card(
            1001,
            2001,
            "Proxi::Demo",
            &["node::alpha"],
            10,
            0,
            &[(1_700_000_002_000, 1), (1_700_000_001_000, 3)],
        ),
        card(
            1002,
            2002,
            "Proxi::Demo",
            &["NODE::dup", "lesson::u"],
            -600,
            2,
            &[(1_700_000_003_000, 4), (1_700_000_004_000, 0)],
        ),
        card(
            1003,
            2003,
            "Proxi::Demo",
            &["node::dup"],
            1,
            0,
            &[(1_700_000_005_000, 3)],
        ),
        card(1004, 2004, "Proxi::Demo", &[], 1, 0, &[(1_700_000_006_000, 3)]),
        card(
            1005,
            2005,
            "Proxi::Demo::Sub",
            &["node::alpha", "lesson::t"],
            1,
            0,
            &[(1_700_000_000_500, 2)],
        ),
        card(
            1006,
            2006,
            "Altro::Demo",
            &["node::alpha"],
            1,
            0,
            &[(1_700_000_007_000, 3)],
        ),
    ]
}

pub fn golden_fixture() -> PathBuf {
    super::repo_root()
        .join("app")
        .join("test")
        .join("fixtures")
        .join("anki")
        .join("import-progress.jsonl")
}
