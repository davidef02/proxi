//! Lesson artifacts on disk: courses/<course>/lessons/*.html and their #lesson-data island.

use std::cmp::Ordering;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::js;

const ISLAND_OPEN: &str = "<script type=\"application/json\" id=\"lesson-data\">";
const ISLAND_CLOSE: &str = "</script>";

/// Reads the embedded lesson document out of a lesson artifact's data island, `None` when there is none.
pub fn parse_lesson_document(html: &str) -> Result<Option<Value>, String> {
    let Some(start) = html.find(ISLAND_OPEN) else {
        return Ok(None);
    };
    let rest = &html[start + ISLAND_OPEN.len()..];
    let Some(end) = rest.find(ISLAND_CLOSE) else {
        return Ok(None);
    };
    match js::parse(rest[..end].trim()) {
        Ok(doc) => Ok(Some(doc)),
        Err(e) => Err(format!("Malformed #lesson-data island: {e}")),
    }
}

/// A parsed lesson artifact: where it lives on disk plus its data island.
#[derive(Debug)]
pub struct LessonDoc {
    pub course: String,
    pub filename: String,
    pub doc: Value,
}

impl LessonDoc {
    pub fn rel(&self) -> String {
        format!("courses/{}/lessons/{}", self.course, self.filename)
    }

    /// The file name without its `.html` suffix: the lesson's identity inside the course.
    pub fn stem(&self) -> &str {
        self.filename.strip_suffix(".html").unwrap_or(&self.filename)
    }

    pub fn metadata(&self) -> &Value {
        self.doc.get("metadata").unwrap_or(&Value::Null)
    }

    pub fn lesson_number(&self) -> Option<&Value> {
        self.metadata().get("lessonNumber")
    }

    pub fn slug(&self) -> Option<&Value> {
        self.metadata().get("slug")
    }

    /// The display title; a blank title falls back to the topic, like the homepage summary.
    pub fn title(&self) -> Option<&Value> {
        let title = self.metadata().get("title");
        if js::truthy(title) {
            title
        } else {
            self.metadata().get("topic")
        }
    }

    pub fn nodes(&self) -> &[Value] {
        match self.doc.get("nodes") {
            Some(Value::Array(nodes)) => nodes,
            _ => &[],
        }
    }
}

/// Walks every `courses/<course>/lessons/*.html` and keeps the ones carrying a data island.
pub fn scan_lesson_docs(data_root: &Path) -> Result<Vec<LessonDoc>, String> {
    let courses = data_root.join("courses");
    if !courses.exists() {
        return Ok(Vec::new());
    }
    let course_names = sorted_entries(&courses)?;

    let mut docs = Vec::new();
    for course in course_names {
        let lessons = courses.join(&course).join("lessons");
        if !lessons.is_dir() {
            continue;
        }
        for filename in sorted_entries(&lessons)? {
            if !filename.ends_with(".html") {
                continue;
            }
            let raw = match fs::read(lessons.join(&filename)) {
                Ok(bytes) => bytes,
                Err(e) => {
                    // One unreadable artifact must not take the whole board down.
                    eprintln!("Ignorata {course}/{filename}: {e}");
                    continue;
                }
            };
            let html = String::from_utf8_lossy(&raw);
            let doc = match parse_lesson_document(&html) {
                Ok(Some(doc)) if js::truthy(Some(&doc)) => doc,
                Ok(_) => continue,
                Err(e) => {
                    eprintln!("Ignorata {course}/{filename}: {e}");
                    continue;
                }
            };
            let rel = format!("courses/{course}/lessons/{filename}");
            if doc.get("metadata").is_none_or(Value::is_null) {
                return Err(format!("{rel}: metadata mancante"));
            }
            match doc.get("nodes") {
                Some(Value::Array(nodes)) => {
                    // Node's summary reads `n.tier` of every node outside its per-file try.
                    if nodes.iter().any(Value::is_null) {
                        return Err(format!("{rel}: nodo null"));
                    }
                }
                _ => return Err(format!("{rel}: nodes non è un array")),
            }
            docs.push(LessonDoc {
                course: course.clone(),
                filename,
                doc,
            });
        }
    }
    docs.sort_by(|a, b| {
        a.course
            .as_bytes()
            .cmp(b.course.as_bytes())
            .then_with(|| lesson_order(a, b))
    });
    Ok(docs)
}

fn lesson_order(a: &LessonDoc, b: &LessonDoc) -> Ordering {
    let diff = js::to_number(a.lesson_number()) - js::to_number(b.lesson_number());
    if diff.is_nan() || diff == 0.0 {
        Ordering::Equal
    } else if diff < 0.0 {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

/// Directory entry names, sorted the way JS `.sort()` sorts ASCII ones.
fn sorted_entries(dir: &Path) -> Result<Vec<String>, String> {
    let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-lesson-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn lesson_html(metadata: &Value, nodes: &Value) -> String {
        format!(
            "<html><body><script type=\"application/json\" id=\"lesson-data\">{}</script></body></html>",
            json!({ "metadata": metadata, "nodes": nodes })
        )
    }

    fn write_lesson(root: &Path, course: &str, filename: &str, html: &str) {
        let dir = root.join("courses").join(course).join("lessons");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(filename), html).unwrap();
    }

    fn metadata(lesson_number: u32, slug: &str, title: &str, topic: &str) -> Value {
        json!({
            "lessonNumber": lesson_number,
            "slug": slug,
            "title": title,
            "topic": topic,
            "dateCreated": "",
            "dateModified": "",
            "version": "1",
        })
    }

    #[test]
    fn parses_the_data_island_or_reports_it_malformed() {
        assert_eq!(parse_lesson_document("<html><body>appunti</body></html>"), Ok(None));
        let open = "<script type=\"application/json\" id=\"lesson-data\">";
        assert_eq!(parse_lesson_document(&format!("{open}{{\"a\":1}}")), Ok(None));

        let doc = parse_lesson_document(&lesson_html(&json!({"slug": "t"}), &json!([])))
            .unwrap()
            .unwrap();
        assert_eq!(doc["metadata"]["slug"], json!("t"));

        let err = parse_lesson_document(&format!("{open}{{not json}}</script>")).unwrap_err();
        assert!(err.contains("Malformed"), "{err}");
    }

    #[test]
    fn scans_courses_in_order_and_lessons_by_number() {
        let root = temp_dir("scan");
        write_lesson(&root, "b", "0002-y.html", &lesson_html(&metadata(2, "y", "Y", ""), &json!([])));
        write_lesson(&root, "b", "0001-x.html", &lesson_html(&metadata(1, "x", "X", ""), &json!([])));
        write_lesson(&root, "a", "0002-y.html", &lesson_html(&metadata(2, "y", "Y", ""), &json!([])));
        write_lesson(&root, "a", "0001-x.html", &lesson_html(&metadata(1, "x", "X", ""), &json!([])));

        let docs = scan_lesson_docs(&root).unwrap();
        let got: Vec<(String, u64)> = docs
            .iter()
            .map(|d| (d.course.clone(), d.lesson_number().unwrap().as_u64().unwrap()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a".to_string(), 1),
                ("a".to_string(), 2),
                ("b".to_string(), 1),
                ("b".to_string(), 2)
            ]
        );
        assert_eq!(docs[0].rel(), "courses/a/lessons/0001-x.html");
        assert_eq!(docs[0].stem(), "0001-x");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_foreign_and_broken_artifacts() {
        let root = temp_dir("skip");
        write_lesson(&root, "demo", "0001-x.html", &lesson_html(&metadata(1, "x", "X", ""), &json!([])));
        write_lesson(&root, "demo", "notes.txt", "plain text");
        write_lesson(&root, "demo", "prose.html", "<html><body>nessuna isola</body></html>");
        write_lesson(
            &root,
            "demo",
            "broken.html",
            "<script type=\"application/json\" id=\"lesson-data\">{oops</script>",
        );
        write_lesson(&root, "demo", "null.html", "<script type=\"application/json\" id=\"lesson-data\">null</script>");

        let docs = scan_lesson_docs(&root).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].filename, "0001-x.html");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_courses_folder_is_empty_and_a_file_is_an_error() {
        let root = temp_dir("missing");
        assert!(scan_lesson_docs(&root).unwrap().is_empty());

        fs::write(root.join("courses"), "not a folder").unwrap();
        assert!(scan_lesson_docs(&root).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lesson_without_metadata_or_nodes_is_an_error() {
        let root = temp_dir("shape");
        write_lesson(
            &root,
            "demo",
            "0001-x.html",
            "<script type=\"application/json\" id=\"lesson-data\">{\"nodes\":[]}</script>",
        );
        let err = scan_lesson_docs(&root).unwrap_err();
        assert_eq!(err, "courses/demo/lessons/0001-x.html: metadata mancante");

        write_lesson(
            &root,
            "demo",
            "0001-x.html",
            "<script type=\"application/json\" id=\"lesson-data\">{\"metadata\":{}}</script>",
        );
        let err = scan_lesson_docs(&root).unwrap_err();
        assert_eq!(err, "courses/demo/lessons/0001-x.html: nodes non è un array");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_null_node_fails_the_scan() {
        let root = temp_dir("null-node");
        write_lesson(
            &root,
            "demo",
            "0001-x.html",
            &lesson_html(&metadata(1, "x", "X", ""), &json!([{ "nodeId": "a" }, null])),
        );
        let err = scan_lesson_docs(&root).unwrap_err();
        assert_eq!(err, "courses/demo/lessons/0001-x.html: nodo null");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_blank_title_falls_back_to_the_topic() {
        let doc = parse_lesson_document(&lesson_html(&metadata(1, "t", "", "Topic"), &json!([])))
            .unwrap()
            .unwrap();
        let lesson = LessonDoc {
            course: "demo".to_string(),
            filename: "0001-t.html".to_string(),
            doc,
        };
        assert_eq!(lesson.title(), Some(&json!("Topic")));
        assert_eq!(lesson.slug(), Some(&json!("t")));
    }
}
