mod common;

use std::fs;
use std::path::Path;

use serde_json::json;

use proxi::anki::{CourseCounts, DEFAULT_TIMEOUT};
use proxi::chapter::{curriculum_file, plan_chapter};
use proxi::homepage::{board_anki_status, course_deck_name_for, courses_dir_for};

/// A minimal artifact: only the island matters for the board's course scan.
fn write_lesson(base_dir: &Path, course: &str, lesson_number: i64, slug: &str, title: &str) {
    let dir = base_dir.join("courses").join(course).join("lessons");
    fs::create_dir_all(&dir).expect("create lessons dir");
    let doc = json!({
        "metadata": {
            "lessonNumber": lesson_number,
            "topic": course,
            "title": title,
            "slug": slug
        },
        "nodes": [{ "nodeId": "n0", "title": "Nodo", "tier": "axiom", "content": "..." }]
    });
    fs::write(
        dir.join(format!("{:04}-{slug}.html", lesson_number)),
        format!(
            "<html><body><h1>{title}</h1>\n<script type=\"application/json\" id=\"lesson-data\">\n{}\n</script>\n</body></html>",
            serde_json::to_string_pretty(&doc).expect("doc json")
        ),
    )
    .expect("write lesson");
}

fn counts(due: i64, learning: i64, new_cards: i64) -> CourseCounts {
    CourseCounts {
        due,
        learning,
        new_cards,
    }
}

fn lessons_json(raw: &str) -> Vec<serde_json::Value> {
    serde_json::from_str(raw).expect("lessons json")
}

#[tokio::test]
async fn includes_a_course_that_exists_only_on_disk() {
    let t = common::temp_dir("board-anki-disk");
    let base = &t.0;
    let file = curriculum_file(base, None);
    plan_chapter(
        &file,
        "Vue",
        None,
        &lessons_json(r#"[{"title":"Il template"}]"#),
    )
    .expect("plans");
    write_lesson(base, "go", 1, "goroutine", "Goroutine");

    let anki = common::fake_anki_scripted(&json!({
        "deckNamesAndIds": { "result": { "Proxi::Vue": 2, "Proxi::go": 3 }, "error": null },
        "getDeckStats": {
            "result": {
                "2": { "deck_id": 2, "name": "Vue", "new_count": 2, "learn_count": 1, "review_count": 3, "total_in_deck": 9 },
                "3": { "deck_id": 3, "name": "go", "new_count": 7, "learn_count": 0, "review_count": 0, "total_in_deck": 7 }
            },
            "error": null
        }
    }));

    let mut warn = Vec::new();
    let status = board_anki_status(
        &anki.url,
        DEFAULT_TIMEOUT,
        base,
        &courses_dir_for(base, None),
        &file,
        &mut warn,
    )
    .await
    .expect("status");

    assert_eq!(
        status.by_course,
        vec![
            ("vue".to_string(), counts(3, 1, 2)),
            // No curriculum chapter: the deck falls back to the folder slug, not dropped from the map.
            ("go".to_string(), counts(0, 0, 7)),
        ]
    );
}

#[tokio::test]
async fn still_lists_courses_on_disk_when_the_plan_is_unreadable() {
    let t = common::temp_dir("board-anki-broken-plan");
    let base = &t.0;
    // A broken plan must cost the badges of planned-only courses, not the whole board.
    fs::write(base.join("curriculum.json"), r#"{"chapters":{}}"#).expect("write plan");
    write_lesson(base, "go", 1, "goroutine", "Goroutine");

    let anki = common::fake_anki_scripted(&json!({
        "deckNamesAndIds": { "result": { "Proxi::go": 3 }, "error": null },
        "getDeckStats": {
            "result": {
                "3": { "deck_id": 3, "name": "go", "new_count": 4, "learn_count": 1, "review_count": 2, "total_in_deck": 7 }
            },
            "error": null
        }
    }));

    let mut warn = Vec::new();
    let status = board_anki_status(
        &anki.url,
        DEFAULT_TIMEOUT,
        base,
        &courses_dir_for(base, None),
        &curriculum_file(base, None),
        &mut warn,
    )
    .await
    .expect("status");

    assert_eq!(status.by_course, vec![("go".to_string(), counts(2, 1, 4))]);
}

#[tokio::test]
async fn scans_courses_from_an_explicit_directory() {
    let t = common::temp_dir("board-anki-alt");
    let base = &t.0;
    let file = curriculum_file(base, None);
    plan_chapter(
        &file,
        "Vue",
        None,
        &lessons_json(r#"[{"title":"Il template"}]"#),
    )
    .expect("plans");
    // The taught course lives outside <baseDir>/courses: only the coursesDir option can find it.
    write_lesson(&base.join("alt"), "go", 1, "goroutine", "Goroutine");
    let courses_dir = base.join("alt").join("courses");

    let anki = common::fake_anki_scripted(&json!({
        "deckNamesAndIds": { "result": { "Proxi::Vue": 2, "Proxi::go": 3 }, "error": null },
        "getDeckStats": {
            "result": {
                "2": { "deck_id": 2, "name": "Vue", "new_count": 2, "learn_count": 1, "review_count": 3, "total_in_deck": 9 },
                "3": { "deck_id": 3, "name": "go", "new_count": 7, "learn_count": 0, "review_count": 0, "total_in_deck": 7 }
            },
            "error": null
        }
    }));

    let mut warn = Vec::new();
    let status = board_anki_status(
        &anki.url,
        DEFAULT_TIMEOUT,
        base,
        &courses_dir,
        &file,
        &mut warn,
    )
    .await
    .expect("status");

    assert_eq!(
        status.by_course,
        vec![
            ("vue".to_string(), counts(3, 1, 2)),
            ("go".to_string(), counts(0, 0, 7)),
        ]
    );
}

#[test]
fn course_deck_name_uses_the_chapter_title_and_sanitizes_it() {
    let t = common::temp_dir("board-anki-deck-names");
    let base = &t.0;
    let file = curriculum_file(base, None);
    plan_chapter(
        &file,
        "Probabilità",
        None,
        &lessons_json(r#"[{"title":"Eventi"}]"#),
    )
    .expect("plans");
    plan_chapter(
        &file,
        "A::B/C\\D",
        Some("weird"),
        &lessons_json(r#"[{"title":"X"}]"#),
    )
    .expect("plans");

    assert_eq!(course_deck_name_for(base, "probabilita"), "Proxi::Probabilità");
    assert_eq!(course_deck_name_for(base, "weird"), "Proxi::A B C D");
    // No plan entry: the directory name.
    assert_eq!(course_deck_name_for(base, "docker"), "Proxi::docker");
    assert_eq!(
        course_deck_name_for(base, "lezioni-singole"),
        "Proxi::Lezioni singole"
    );
}
