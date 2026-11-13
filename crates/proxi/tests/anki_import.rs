//! Anki review import: the cases of app/test/anki-import.test.ts plus parity with the Node golden.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use common::fake_anki::{card, FakeAnki, FakeCard, FakeState};
use proxi::anki_client::AnkiClient;
use proxi::anki_import::{import_reviews, ImportRegistry};
use proxi::course_data::progress_file;

const SEED_LINE: &str = "{\"at\":\"2023-11-14T00:00:00.000Z\",\"sessionId\":\"s1\",\"course\":\"demo\",\"lessonSlug\":\"t\",\"nodeRefs\":[\"demo/t#alpha\"],\"kind\":\"check\",\"correct\":true}\n";
/// Both a refused connection and Windows' slow refusal are acceptable.
const OFFLINE_ERRORS: [&str; 2] = ["fetch failed", "AnkiConnect request timed out"];

fn lesson_doc(lesson_number: i64, title: &str, slug: &str, node_ids: &[&str]) -> Value {
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

fn write_lesson(data_root: &Path, course: &str, filename: &str, doc: Value) {
    let dir = data_root.join("courses").join(course).join("lessons");
    fs::create_dir_all(&dir).expect("create lessons dir");
    fs::write(
        dir.join(filename),
        format!(r#"<script type="application/json" id="lesson-data">{doc}</script>"#),
    )
    .expect("write lesson");
}

/// The data root of the old TypeScript test: one planned course with two lessons.
fn make_test_repo(tag: &str) -> common::TempDir {
    let root = common::temp_dir(tag);
    let data = &root.0;
    fs::write(
        data.join("curriculum.json"),
        json!({
            "chapters": [{
                "title": "Demo",
                "slug": "demo",
                "lessons": [
                    { "title": "Lesson T", "slug": "t", "plannedNodes": 5 },
                    { "title": "Lesson U", "slug": "u", "plannedNodes": 1 },
                ],
            }],
        })
        .to_string(),
    )
    .expect("write curriculum");
    write_lesson(
        data,
        "demo",
        "0001-t.html",
        lesson_doc(1, "Lesson T", "t", &["alpha", "beta", "gamma", "delta", "dup"]),
    );
    write_lesson(data, "demo", "0002-u.html", lesson_doc(2, "Lesson U", "u", &["dup"]));
    fs::write(data.join("courses/demo/MISSION.md"), "# Mission\n").expect("write mission");
    fs::write(data.join("courses/demo/NOTES.md"), "# Notes\n").expect("write notes");
    root
}

/// The data root of app/test/api-anki.test.ts, whose import golden this port must reproduce.
fn seed_golden(tag: &str) -> common::TempDir {
    let root = common::temp_dir(tag);
    let data = &root.0;
    fs::write(
        data.join("curriculum.json"),
        json!({
            "chapters": [
                {
                    "title": "Demo",
                    "slug": "demo",
                    "lessons": [
                        { "title": "Lesson T", "slug": "t", "plannedNodes": 2 },
                        { "title": "Lesson U", "slug": "u", "plannedNodes": 1 },
                    ],
                },
                { "title": "Vuoto", "slug": "vuoto", "lessons": [] },
            ],
        })
        .to_string(),
    )
    .expect("write curriculum");
    write_lesson(data, "demo", "0001-t.html", lesson_doc(1, "Lesson T", "t", &["alpha", "dup"]));
    write_lesson(data, "demo", "0002-u.html", lesson_doc(2, "Lesson U", "u", &["dup"]));
    write_lesson(data, "orfano", "0001-o.html", lesson_doc(1, "O", "o", &["x"]));
    fs::write(progress_file(data, "demo"), SEED_LINE).expect("seed progress log");
    root
}

/// The six cards of the Node golden: the two ambiguous ones are skipped, four reviews written.
fn golden_cards() -> Vec<FakeCard> {
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

fn read_lines(data_root: &Path) -> Vec<Value> {
    let raw = fs::read_to_string(progress_file(data_root, "demo")).expect("read progress log");
    raw.lines()
        .map(|line| serde_json::from_str(line).expect("progress line is JSON"))
        .collect()
}

async fn start_anki(state: FakeState) -> (FakeAnki, AnkiClient) {
    let fake = FakeAnki::start(state).await;
    let client = AnkiClient::new(&fake.url).expect("anki client");
    (fake, client)
}

fn find_query(fake: &FakeAnki) -> String {
    fake.requests()
        .into_iter()
        .find(|request| request["action"] == "findCards")
        .expect("findCards request")["params"]["query"]
        .as_str()
        .expect("query string")
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn instability_cards_import_eight_reviews() {
    let root = make_test_repo("import-eight");
    let mut state = FakeState::default();
    state.cards = vec![
        card(1001, 2001, "Proxi::Demo", &["node::alpha"], 10, 0, &[
            (1_700_000_001_000, 3),
            (1_700_000_002_000, 1),
        ]),
        card(1002, 2002, "Proxi::Demo", &["node::beta"], 10, 3, &[
            (1_700_000_003_000, 3),
            (1_700_000_004_000, 3),
        ]),
        card(1003, 2003, "Proxi::Demo", &["node::gamma"], 30, 3, &[
            (1_700_000_005_000, 3),
            (1_700_000_006_000, 3),
        ]),
        card(1004, 2004, "Proxi::Demo", &["node::delta"], 5, 0, &[
            (1_700_000_007_000, 3),
            (1_700_000_008_000, 2),
        ]),
    ];
    let (fake, anki) = start_anki(state).await;

    let result = import_reviews(&anki, &root.0, "demo").await;

    assert_eq!(result.online, true);
    assert_eq!(result.imported, 8);
    assert_eq!(result.skipped, 0);
    assert_eq!(find_query(&fake), "deck:\"Proxi::Demo\"");

    let lines = read_lines(&root.0);
    let expected = [
        (1_700_000_001_000_i64, "alpha", 3, 0, 10, true),
        (1_700_000_002_000, "alpha", 1, 0, 10, false),
        (1_700_000_003_000, "beta", 3, 3, 10, true),
        (1_700_000_004_000, "beta", 3, 3, 10, true),
        (1_700_000_005_000, "gamma", 3, 3, 30, true),
        (1_700_000_006_000, "gamma", 3, 3, 30, true),
        (1_700_000_007_000, "delta", 3, 0, 5, true),
        (1_700_000_008_000, "delta", 2, 0, 5, true),
    ];
    assert_eq!(lines.len(), expected.len());
    for (line, (review_id, node_id, ease, lapses, interval, correct)) in lines.iter().zip(expected)
    {
        assert_eq!(line["reviewId"], review_id);
        assert_eq!(line["lessonSlug"], "t");
        assert_eq!(line["nodeRefs"], json!([format!("demo/t#{node_id}")]));
        assert_eq!(line["ease"], ease);
        assert_eq!(line["lapses"], lapses);
        assert_eq!(line["interval"], interval);
        assert_eq!(line["correct"], correct);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn second_import_is_idempotent() {
    let root = make_test_repo("import-idempotent");
    let mut state = FakeState::default();
    state.cards = vec![card(
        1001,
        2001,
        "Proxi::Demo",
        &["node::alpha"],
        10,
        0,
        &[(1_700_000_001_000, 3)],
    )];
    let (_fake, anki) = start_anki(state).await;

    let first = import_reviews(&anki, &root.0, "demo").await;
    assert_eq!(first.online, true);
    assert_eq!(first.imported, 1);
    let before = fs::read(progress_file(&root.0, "demo")).expect("read progress log");

    let second = import_reviews(&anki, &root.0, "demo").await;
    assert_eq!(second.online, true);
    assert_eq!(second.imported, 0);
    assert_eq!(
        fs::read(progress_file(&root.0, "demo")).expect("read progress log"),
        before
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn skips_untagged_unknown_and_ambiguous_and_resolves_lesson_tag() {
    let root = make_test_repo("import-skips");
    let mut state = FakeState::default();
    state.cards = vec![
        card(1001, 2001, "Proxi::Demo", &[], 10, 0, &[(1_700_000_001_000, 3)]),
        card(
            1002,
            2002,
            "Proxi::Demo",
            &["node::nope"],
            10,
            0,
            &[(1_700_000_002_000, 3)],
        ),
        card(
            1003,
            2003,
            "Proxi::Demo",
            &["node::dup"],
            10,
            0,
            &[(1_700_000_003_000, 3)],
        ),
        card(
            1004,
            2004,
            "Proxi::Demo",
            &["node::alpha"],
            10,
            0,
            &[(1_700_000_004_000, 3)],
        ),
        card(
            1005,
            2005,
            "Proxi::Demo",
            &["node::dup", "lesson::u"],
            10,
            0,
            &[(1_700_000_005_000, 3)],
        ),
    ];
    let (_fake, anki) = start_anki(state).await;

    let result = import_reviews(&anki, &root.0, "demo").await;

    assert_eq!(result.online, true);
    assert_eq!(result.skipped, 3);
    assert_eq!(result.imported, 2);

    let lines = read_lines(&root.0);
    assert_eq!(lines.len(), 2);
    let dup = lines
        .iter()
        .find(|line| line["reviewId"].as_i64() == Some(1_700_000_005_000))
        .expect("the lesson::u review");
    assert_eq!(dup["nodeRefs"], json!(["demo/u#dup"]));
    assert_eq!(dup["lessonSlug"], "u");

    let alpha = lines
        .iter()
        .find(|line| line["reviewId"].as_i64() == Some(1_700_000_004_000))
        .expect("the alpha review");
    assert_eq!(alpha["nodeRefs"], json!(["demo/t#alpha"]));
    assert_eq!(alpha["lessonSlug"], "t");
}

#[tokio::test(flavor = "multi_thread")]
async fn ignores_cards_outside_the_course_deck() {
    let root = make_test_repo("import-outside");
    let mut state = FakeState::default();
    state.cards = vec![card(
        1001,
        2001,
        "Altro::Demo",
        &["node::alpha"],
        10,
        0,
        &[(1_700_000_001_000, 3)],
    )];
    let (fake, anki) = start_anki(state).await;

    let result = import_reviews(&anki, &root.0, "demo").await;

    assert_eq!(result.online, true);
    assert_eq!(result.imported, 0);
    assert_eq!(find_query(&fake), "deck:\"Proxi::Demo\"");
    assert!(!progress_file(&root.0, "demo").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_anki_is_offline_without_writing() {
    let root = make_test_repo("import-dead");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind a port to drop");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let anki = AnkiClient::new(format!("http://127.0.0.1:{port}")).expect("anki client");

    let result = import_reviews(&anki, &root.0, "demo").await;

    assert_eq!(result.online, false);
    assert_eq!(result.imported, 0);
    assert_eq!(result.skipped, 0);
    let error = result.error.as_deref().expect("error string");
    assert!(OFFLINE_ERRORS.contains(&error), "{error}");
    assert!(!progress_file(&root.0, "demo").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_imports_share_one_run() {
    let root = make_test_repo("import-concurrent");
    let mut state = FakeState::default();
    state.cards = vec![card(
        1001,
        2001,
        "Proxi::Demo",
        &["node::alpha"],
        10,
        0,
        &[(1_700_000_001_000, 3)],
    )];
    // Slow enough that the second caller arrives while the first run is still running.
    state.find_cards_delay = Duration::from_millis(300);
    let (_fake, anki) = start_anki(state).await;
    let registry = ImportRegistry::new();

    let (first, second) = tokio::join!(
        registry.import(anki.clone(), root.0.clone(), "demo".to_string()),
        registry.import(anki.clone(), root.0.clone(), "demo".to_string()),
    );

    assert_eq!(first, second);
    assert_eq!(first.online, true);
    assert_eq!(first.imported, 1);

    let written = fs::read_to_string(progress_file(&root.0, "demo")).expect("read progress log");
    assert_eq!(written.lines().count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn golden_matches_node_fixture() {
    let root = seed_golden("import-golden");
    let mut state = FakeState::default();
    state.cards = golden_cards();
    let (_fake, anki) = start_anki(state).await;

    let result = import_reviews(&anki, &root.0, "demo").await;

    assert_eq!(result.online, true);
    assert_eq!(result.imported, 4);
    assert_eq!(result.skipped, 2);
    let fixture = common::repo_root().join("app/test/fixtures/anki/import-progress.jsonl");
    assert_eq!(
        fs::read(progress_file(&root.0, "demo")).expect("read progress log"),
        fs::read(fixture).expect("read the Node golden"),
        "the port must write exactly the Node golden, byte for byte"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ease_outside_1_to_4_is_ignored() {
    let root = make_test_repo("import-ease");
    let mut state = FakeState::default();
    state.cards = vec![card(
        1001,
        2001,
        "Proxi::Demo",
        &["node::alpha"],
        10,
        0,
        &[(1_700_000_001_000, 0), (1_700_000_002_000, 5)],
    )];
    let (_fake, anki) = start_anki(state).await;

    let result = import_reviews(&anki, &root.0, "demo").await;

    assert_eq!(result.online, true);
    assert_eq!(result.imported, 0);
    assert_eq!(result.skipped, 0, "an unusable review is not a skipped card");
    assert!(!progress_file(&root.0, "demo").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn no_course_directory_writes_nothing() {
    let root = make_test_repo("import-no-course-dir");
    fs::write(
        root.0.join("curriculum.json"),
        json!({
            "chapters": [
                { "title": "Demo", "slug": "demo" },
                // Same title, so its deck is Proxi::Demo too, but there is no directory on disk.
                { "title": "Demo", "slug": "other" },
            ],
        })
        .to_string(),
    )
    .expect("write curriculum");
    let mut state = FakeState::default();
    state.cards = vec![card(
        1001,
        2001,
        "Proxi::Demo",
        &["node::alpha"],
        10,
        0,
        &[(1_700_000_001_000, 3)],
    )];
    let (_fake, anki) = start_anki(state).await;

    let result = import_reviews(&anki, &root.0, "other").await;

    assert_eq!(result.online, true);
    assert_eq!(result.imported, 0);
    assert!(!root.0.join("courses/other/progress.jsonl").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn registry_runs_again_after_completion() {
    let root = make_test_repo("import-registry-again");
    let mut state = FakeState::default();
    state.cards = vec![card(
        1001,
        2001,
        "Proxi::Demo",
        &["node::alpha"],
        10,
        0,
        &[(1_700_000_001_000, 3)],
    )];
    let (_fake, anki) = start_anki(state).await;
    let registry = ImportRegistry::new();

    let first = registry
        .import(anki.clone(), root.0.clone(), "demo".to_string())
        .await;
    assert_eq!(first.imported, 1);

    let second = registry
        .import(anki.clone(), root.0.clone(), "demo".to_string())
        .await;
    assert_eq!(second.online, true);
    assert_eq!(second.imported, 0);
    assert_eq!(read_lines(&root.0).len(), 1);
}
