//! Course data the Anki routes read: curriculum, lessons, progress log.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::Value;

// Duplicated from app/shared/identity.mjs: renaming it orphans every synced deck.
pub const DECK_ROOT: &str = "Proxi";
pub const STANDALONE_COURSE_SLUG: &str = "lezioni-singole";
pub const STANDALONE_COURSE_TITLE: &str = "Lezioni singole";

/// `^[a-z0-9]+(?:-[a-z0-9]+)*$`, by hand: no regex crate in the binary.
pub fn valid_course(slug: &str) -> bool {
    !slug.is_empty()
        && slug.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

pub fn read_chapters(data_root: &Path) -> Result<Vec<Value>, String> {
    let path = data_root.join("curriculum.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let Some(object) = parsed.as_object() else {
        return Ok(Vec::new());
    };
    match object.get("chapters") {
        None => Ok(Vec::new()),
        Some(Value::Array(chapters)) => Ok(chapters.clone()),
        // A malformed plan is refused, not degraded: planChapter writes the result back.
        Some(_) => Err(format!(
            "Malformed {}: \"chapters\" must be an array",
            path.display()
        )),
    }
}

/// "Proxi::<chapter title>" for course under data_root (directory name if the plan has no title).
pub fn course_deck_name_for(data_root: &Path, course: &str) -> String {
    let title = read_chapters(data_root)
        .ok()
        .and_then(|chapters| {
            chapters
                .into_iter()
                .find(|chapter| chapter.get("slug").and_then(Value::as_str) == Some(course))
        })
        .and_then(|chapter| {
            chapter
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|title| !title.is_empty());
    let title = title.or_else(|| {
        (course == STANDALONE_COURSE_SLUG).then(|| STANDALONE_COURSE_TITLE.to_string())
    });
    format!(
        "{DECK_ROOT}::{}",
        clean_deck_title(title.as_deref().unwrap_or(course))
    )
}

/// JS `text.replace(/::|[\\/]/g, " ")` then `trim()`: separators collapse to one space.
fn clean_deck_title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ':' if chars.peek() == Some(&':') => {
                chars.next();
                out.push(' ');
            }
            '\\' | '/' => out.push(' '),
            other => out.push(other),
        }
    }
    js_trim(&out).to_string()
}

/// JS `String.prototype.trim` also drops the BOM, which `char::is_whitespace` does not.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}')
}

/// `courses/<course>/lessons/file.html` → (data root, course), as Node's path parts check it.
fn course_of(lesson_path: &Path) -> Option<(PathBuf, String)> {
    let resolved = crate::data_root::resolve_path(lesson_path);
    let lessons = resolved.parent()?;
    if lessons.file_name() != Some(OsStr::new("lessons")) {
        return None;
    }
    let course_dir = lessons.parent()?;
    let course = course_dir.file_name()?.to_string_lossy().into_owned();
    let courses = course_dir.parent()?;
    if courses.file_name() != Some(OsStr::new("courses")) {
        return None;
    }
    Some((courses.parent()?.to_path_buf(), course))
}

/// One deck per course: "Proxi::<chapter title>", None for a lesson outside a course directory.
pub fn course_deck_name(lesson_path: &Path) -> Option<String> {
    course_of(lesson_path).map(|(base, course)| course_deck_name_for(&base, &course))
}

/// Course and lesson tags for a lesson in a course; [] outside one, no lesson tag when the slug is empty.
pub fn course_lesson_tags(lesson_path: &Path, lesson_slug: Option<&Value>) -> Vec<String> {
    let Some((_base, course)) = course_of(lesson_path) else {
        return Vec::new();
    };
    let mut tags = vec![format!("course::{course}")];
    if let Some(slug) = lesson_slug
        && crate::lesson::js::js_truthy(Some(slug))
    {
        tags.push(format!("lesson::{}", crate::lesson::js::js_to_string(slug)));
    }
    tags
}

#[derive(Clone, Debug, PartialEq)]
pub struct LessonDoc {
    pub course: String,
    pub filename: String,
    pub slug: String,
    pub lesson_number: f64,
    pub node_ids: Vec<String>,
}

const ISLAND_OPEN: &str = r#"<script type="application/json" id="lesson-data">"#;

pub fn parse_island(html: &str) -> Result<Option<Value>, String> {
    let Some(start) = html.find(ISLAND_OPEN) else {
        return Ok(None);
    };
    let rest = &html[start + ISLAND_OPEN.len()..];
    // The Node regex needs the closing tag: without it this is not an island.
    let Some(end) = rest.find("</script>") else {
        return Ok(None);
    };
    serde_json::from_str(js_trim(&rest[..end]))
        .map(Some)
        .map_err(|error| format!("Malformed #lesson-data island: {error}"))
}

/// Every readable lesson artifact under courses/<course>/lessons/*.html, parsed from its island.
pub fn scan_lessons(data_root: &Path) -> Vec<LessonDoc> {
    let courses_dir = data_root.join("courses");
    let Ok(entries) = std::fs::read_dir(&courses_dir) else {
        return Vec::new();
    };
    let mut courses: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    courses.sort();

    let mut lessons = Vec::new();
    for course in courses {
        let lessons_dir = courses_dir.join(&course).join("lessons");
        if !lessons_dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&lessons_dir) else {
            continue;
        };
        let mut files: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|filename| filename.ends_with(".html"))
            .collect();
        files.sort();

        for filename in files {
            let html = match std::fs::read_to_string(lessons_dir.join(&filename)) {
                Ok(html) => html,
                Err(error) => {
                    // One unreadable artifact must not take the whole board down: name it and move on.
                    eprintln!("Ignorata {course}/{filename}: {error}");
                    continue;
                }
            };
            let island = match parse_island(&html) {
                Ok(island) => island,
                Err(error) => {
                    eprintln!("Ignorata {course}/{filename}: {error}");
                    continue;
                }
            };
            let Some(Value::Object(island)) = island else {
                continue;
            };
            let Some(Value::Object(metadata)) = island.get("metadata") else {
                continue;
            };
            let Some(Value::Array(nodes)) = island.get("nodes") else {
                continue;
            };
            let Some(slug) = metadata.get("slug").and_then(Value::as_str) else {
                continue;
            };
            lessons.push(LessonDoc {
                course: course.clone(),
                filename,
                slug: slug.to_string(),
                lesson_number: metadata
                    .get("lessonNumber")
                    .and_then(Value::as_f64)
                    .unwrap_or(f64::NAN),
                node_ids: nodes
                    .iter()
                    .filter_map(|node| node.get("nodeId").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect(),
            });
        }
    }

    // Stable: equal lesson numbers keep file order. Byte order for courses, not localeCompare.
    lessons.sort_by(|a, b| {
        a.course.cmp(&b.course).then_with(|| {
            a.lesson_number
                .partial_cmp(&b.lesson_number)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    lessons
}

/// Every course that should have a deck: the plan's chapters, then course dirs already on disk.
pub fn course_slugs(data_root: &Path) -> Vec<String> {
    let mut slugs: Vec<String> = Vec::new();
    if let Ok(chapters) = read_chapters(data_root) {
        for chapter in chapters {
            if let Some(slug) = chapter.get("slug").and_then(Value::as_str) {
                if !slugs.iter().any(|known| known == slug) {
                    slugs.push(slug.to_string());
                }
            }
        }
    }
    for lesson in scan_lessons(data_root) {
        if !slugs.iter().any(|known| known == &lesson.course) {
            slugs.push(lesson.course);
        }
    }
    slugs
}

pub fn progress_file(data_root: &Path, course: &str) -> PathBuf {
    data_root.join("courses").join(course).join("progress.jsonl")
}

/// The Anki review ids already in the course progress log, so an import stays idempotent.
pub fn known_anki_review_ids(data_root: &Path, course: &str) -> HashSet<i64> {
    let mut known = HashSet::new();
    let Ok(raw) = std::fs::read_to_string(progress_file(data_root, course)) else {
        return known;
    };
    for line in raw.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(rec) = rec.as_object() else {
            continue;
        };
        if !rec.get("nodeRefs").is_some_and(Value::is_array) {
            continue;
        }
        let Some(kind) = rec.get("kind").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(kind, "probe" | "predict" | "check" | "review") {
            continue;
        }
        if rec.get("source").and_then(Value::as_str) != Some("anki") {
            continue;
        }
        let Some(id) = rec.get("reviewId").and_then(Value::as_f64) else {
            continue;
        };
        if id.fract() != 0.0 || id < i64::MIN as f64 || id > i64::MAX as f64 {
            continue;
        }
        known.insert(id as i64);
    }
    known
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
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
            "proxi-course-data-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn write_curriculum(data_root: &Path, chapters: Value) {
        fs::write(
            data_root.join("curriculum.json"),
            json!({ "chapters": chapters }).to_string(),
        )
        .expect("write curriculum");
    }

    fn lesson_doc(number: f64, slug: &str, nodes: &[&str]) -> Value {
        json!({
            "metadata": {
                "lessonNumber": number,
                "topic": "Demo",
                "title": "Lesson",
                "slug": slug,
            },
            "nodes": nodes
                .iter()
                .map(|id| json!({ "nodeId": id, "title": "N", "tier": "axiom", "content": "x" }))
                .collect::<Vec<_>>(),
        })
    }

    fn write_lesson(data_root: &Path, course: &str, filename: &str, island: Value) {
        let dir = data_root.join("courses").join(course).join("lessons");
        fs::create_dir_all(&dir).expect("create lessons dir");
        fs::write(
            dir.join(filename),
            format!(r#"<script type="application/json" id="lesson-data">{island}</script>"#),
        )
        .expect("write lesson");
    }

    #[test]
    fn valid_course_matches_the_slug_rule() {
        for slug in ["demo", "a-b-1"] {
            assert!(valid_course(slug), "{slug}");
        }
        for slug in ["", "Bad", "a--b", "-a", "a-", "a_b", "a\n"] {
            assert!(!valid_course(slug), "{slug}");
        }
    }

    #[test]
    fn read_chapters_refuses_only_a_bad_chapters_value() {
        let t = temp_dir("chapters");
        assert_eq!(read_chapters(&t.0).unwrap(), Vec::<Value>::new());

        fs::write(t.0.join("curriculum.json"), r#"{"chapters":null}"#).unwrap();
        assert!(read_chapters(&t.0).is_err());

        fs::write(t.0.join("curriculum.json"), "[1]").unwrap();
        assert_eq!(read_chapters(&t.0).unwrap(), Vec::<Value>::new());

        fs::write(t.0.join("curriculum.json"), "{not json").unwrap();
        assert!(read_chapters(&t.0).is_err());
    }

    #[test]
    fn course_deck_name_uses_the_plan_title_or_the_directory() {
        let t = temp_dir("deck-name");
        write_curriculum(
            &t.0,
            json!([
                { "title": "Demo", "slug": "demo" },
                { "title": "A::B/C\\D ", "slug": "altre" },
            ]),
        );
        assert_eq!(course_deck_name_for(&t.0, "demo"), "Proxi::Demo");
        assert_eq!(course_deck_name_for(&t.0, "altre"), "Proxi::A B C D");
        assert_eq!(course_deck_name_for(&t.0, "orfano"), "Proxi::orfano");
        assert_eq!(
            course_deck_name_for(&t.0, STANDALONE_COURSE_SLUG),
            "Proxi::Lezioni singole"
        );

        fs::write(t.0.join("curriculum.json"), "{broken").unwrap();
        assert_eq!(course_deck_name_for(&t.0, "demo"), "Proxi::demo");

        write_curriculum(&t.0, json!([{ "title": 42, "slug": "demo" }]));
        assert_eq!(course_deck_name_for(&t.0, "demo"), "Proxi::demo");

        fs::remove_file(t.0.join("curriculum.json")).unwrap();
        assert_eq!(course_deck_name_for(&t.0, "demo"), "Proxi::demo");
    }

    #[test]
    fn course_lesson_tags_outside_and_inside() {
        let t = temp_dir("lesson-tags");
        let loose = t.0.join("lessons").join("0001-x.html");
        assert_eq!(
            course_lesson_tags(&loose, Some(&json!("x"))),
            Vec::<String>::new()
        );
        assert_eq!(
            course_lesson_tags(Path::new("some/random/path.html"), Some(&json!("x"))),
            Vec::<String>::new()
        );

        let in_course = t.0.join("courses/demo/lessons/0001-x.html");
        assert_eq!(
            course_lesson_tags(&in_course, Some(&json!("x"))),
            vec!["course::demo".to_string(), "lesson::x".to_string()]
        );
        assert_eq!(
            course_lesson_tags(&in_course, None),
            vec!["course::demo".to_string()]
        );
        assert_eq!(
            course_lesson_tags(&in_course, Some(&json!(""))),
            vec!["course::demo".to_string()]
        );
    }

    #[test]
    fn course_deck_name_uses_chapter_title() {
        let t = temp_dir("chapter-deck");
        write_curriculum(
            &t.0,
            json!([{ "title": "Vue.js & Vetrine", "slug": "vue-vetrine", "lessons": [] }]),
        );
        assert_eq!(
            course_deck_name(&t.0.join("courses/vue-vetrine/lessons/0001-a.html")),
            Some("Proxi::Vue.js & Vetrine".to_string())
        );
        assert_eq!(
            course_deck_name_for(&t.0, "missing-course"),
            "Proxi::missing-course"
        );
        assert_eq!(
            course_deck_name(&t.0.join("courses/lezioni-singole/lessons/a.html")),
            Some("Proxi::Lezioni singole".to_string())
        );
        assert_eq!(course_deck_name(&t.0.join("lessons/a.html")), None);
    }

    #[test]
    fn parse_island_reads_only_the_lesson_data_script() {
        assert_eq!(parse_island("<p>prose</p>").unwrap(), None);

        let html = r#"<script type="application/json" id="lesson-data"> {"metadata":{"slug":"t"},"nodes":[]} </script>"#;
        assert_eq!(
            parse_island(html).unwrap(),
            Some(json!({ "metadata": { "slug": "t" }, "nodes": [] }))
        );

        assert!(
            parse_island(
                r#"<script type="application/json" id="lesson-data">{oops</script>"#
            )
            .is_err()
        );

        // No closing tag: the Node regex does not match either.
        assert_eq!(
            parse_island(r#"<script type="application/json" id="lesson-data">{"metadata":{}}"#)
                .unwrap(),
            None
        );
    }

    #[test]
    fn scan_lessons_skips_non_artifacts_and_sorts_by_course_then_number() {
        let t = temp_dir("scan");
        write_lesson(&t.0, "alpha", "0002-late.html", lesson_doc(1.0, "late", &["a"]));
        write_lesson(&t.0, "alpha", "0001-early.html", lesson_doc(2.0, "early", &["b"]));
        write_lesson(&t.0, "beta", "0001-b.html", json!({
            "metadata": { "lessonNumber": 1, "slug": "b" },
            "nodes": [{ "nodeId": "x" }, { "title": "no id" }, { "nodeId": 7 }],
        }));

        // Prose lesson, malformed island and a non-html file: all skipped.
        let alpha_lessons = t.0.join("courses").join("alpha").join("lessons");
        fs::write(alpha_lessons.join("0003-prose.html"), "<p>prose</p>").unwrap();
        fs::write(
            alpha_lessons.join("0004-broken.html"),
            r#"<script type="application/json" id="lesson-data">{oops</script>"#,
        )
        .unwrap();
        fs::write(alpha_lessons.join("notes.txt"), "not html").unwrap();

        // A course directory without lessons: skipped.
        fs::create_dir_all(t.0.join("courses").join("gamma")).unwrap();
        fs::write(t.0.join("courses").join("gamma").join("MISSION.md"), "x").unwrap();

        assert_eq!(
            scan_lessons(&t.0),
            vec![
                LessonDoc {
                    course: "alpha".to_string(),
                    filename: "0002-late.html".to_string(),
                    slug: "late".to_string(),
                    lesson_number: 1.0,
                    node_ids: vec!["a".to_string()],
                },
                LessonDoc {
                    course: "alpha".to_string(),
                    filename: "0001-early.html".to_string(),
                    slug: "early".to_string(),
                    lesson_number: 2.0,
                    node_ids: vec!["b".to_string()],
                },
                LessonDoc {
                    course: "beta".to_string(),
                    filename: "0001-b.html".to_string(),
                    slug: "b".to_string(),
                    lesson_number: 1.0,
                    node_ids: vec!["x".to_string()],
                },
            ]
        );
    }

    #[test]
    fn course_slugs_lists_the_plan_then_the_orphans_once_each() {
        let t = temp_dir("slugs");
        write_curriculum(
            &t.0,
            json!([
                { "title": "Demo", "slug": "demo", "lessons": [] },
                { "title": "Vuoto", "slug": "vuoto", "lessons": [] },
            ]),
        );
        write_lesson(&t.0, "demo", "0001-t.html", lesson_doc(1.0, "t", &["alpha"]));
        write_lesson(&t.0, "orfano", "0001-o.html", lesson_doc(1.0, "o", &["x"]));

        assert_eq!(course_slugs(&t.0), vec!["demo", "vuoto", "orfano"]);
    }

    #[test]
    fn known_anki_review_ids_reads_only_complete_anki_reviews() {
        let t = temp_dir("known");
        assert_eq!(
            known_anki_review_ids(&t.0, "demo"),
            HashSet::new(),
            "no progress file yet"
        );

        let dir = t.0.join("courses").join("demo");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("progress.jsonl"),
            concat!(
                "{\"nodeRefs\":[\"demo/t#alpha\"],\"kind\":\"check\"}\r\n",
                "\n",
                "   \n",
                "garbage\n",
                "{\"nodeRefs\":[],\"kind\":\"review\",\"source\":\"teach\",\"reviewId\":7}\n",
                "{\"nodeRefs\":[],\"kind\":\"review\",\"source\":\"anki\",\"reviewId\":5}\r\n",
                "{\"nodeRefs\":[],\"kind\":\"review\",\"source\":\"anki\",\"reviewId\":\"6\"}\n",
                "{\"nodeRefs\":[],\"kind\":\"other\",\"source\":\"anki\",\"reviewId\":8}\n",
            ),
        )
        .unwrap();

        assert_eq!(
            known_anki_review_ids(&t.0, "demo"),
            HashSet::from([5_i64])
        );
    }

    #[test]
    fn deck_root_matches_the_identity_module() {
        let identity =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/shared/identity.mjs");
        let text = fs::read_to_string(identity).expect("read identity.mjs");
        assert!(text.contains("export const DECK_ROOT = \"Proxi\";"));
        assert_eq!(DECK_ROOT, "Proxi");
    }
}
