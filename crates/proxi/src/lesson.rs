//! Lesson listing, reading and document bodies (app/server/lesson.ts, homepage.ts, planstate.ts).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::data_root::resolve_path;
use crate::html_lesson::{
    build_sintesi_document, parse_lesson_document, parse_reference_document,
};
use crate::js;

const MALFORMED: &str = "Malformed #lesson-data island";

fn skip_ws(s: &str, mut i: usize) -> usize {
    while let Some(c) = s[i..].chars().next() {
        if js::is_js_ws(c) {
            i += c.len_utf8();
        } else {
            break;
        }
    }
    i
}

fn course_ok(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn lesson_file_ok(name: &str) -> bool {
    name.len() >= 6
        && name.ends_with(".html")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

pub fn lesson_rel(rel: &str) -> Option<String> {
    let normalised = rel.replace('\\', "/");
    if normalised.contains("..") {
        return None;
    }
    let parts: Vec<&str> = normalised.split('/').collect();
    if parts.len() != 4 || parts[0] != "courses" || parts[2] != "lessons" {
        return None;
    }
    if !course_ok(parts[1]) || !lesson_file_ok(parts[3]) {
        return None;
    }
    Some(normalised)
}

/// A lesson or a reference sheet: what the in-app document reader may open.
pub fn doc_rel(rel: &str) -> Option<String> {
    let normalised = rel.replace('\\', "/");
    if normalised.contains("..") {
        return None;
    }
    let parts: Vec<&str> = normalised.split('/').collect();
    if parts.len() != 4 || parts[0] != "courses" || !matches!(parts[2], "lessons" | "reference") {
        return None;
    }
    if !course_ok(parts[1]) || !lesson_file_ok(parts[3]) {
        return None;
    }
    Some(normalised)
}

pub fn lesson_abs(root: &Path, rel: &str) -> Result<PathBuf, String> {
    match lesson_rel(rel) {
        Some(normalised) => Ok(resolve_path(&root.join(normalised))),
        None => Err(format!(
            "Bad lesson path \"{rel}\": expected courses/<course>/lessons/<file>.html."
        )),
    }
}

/// `courses/x/lessons/0003-the-slug.html` (or the bare file name) -> `the-slug`.
pub fn slug_of_lesson(p: &str) -> String {
    let name = Path::new(p)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let without_number = strip_leading_number(name);
    without_number
        .strip_suffix(".html")
        .unwrap_or(without_number)
        .to_string()
}

fn strip_leading_number(name: &str) -> &str {
    let digits = name.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && name.as_bytes().get(digits) == Some(&b'-') {
        &name[digits + 1..]
    } else {
        name
    }
}

/// Last line of a record: `<!-- lesson: <slug> -->` (the slug may be empty).
pub fn parse_marker(content: &str) -> Option<String> {
    let body = content.trim_end_matches(js::is_js_ws);
    if !body.ends_with("-->") {
        return None;
    }
    let open = body.rfind("<!--")?;
    let mut i = skip_ws(body, open + 4);
    if !body[i..].starts_with("lesson:") {
        return None;
    }
    i += "lesson:".len();
    let close = body.len() - "-->".len();
    if i > close {
        return None;
    }
    let inner = &body[i..close];
    if inner.contains('>') {
        return None;
    }
    Some(js::js_trim(inner).to_string())
}

#[derive(Debug, Clone)]
pub struct Record {
    pub file: String,
    pub marker: Option<String>,
}

fn is_record_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 8
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[4] == b'-'
        && name.ends_with(".md")
}

pub fn read_records(root: &Path, course: &str) -> Vec<Record> {
    let dir = root.join("courses").join(course).join("learning-records");
    if !dir.exists() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| is_record_name(name))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let content = fs::read(dir.join(&file))
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            Record {
                marker: parse_marker(&content),
                file,
            }
        })
        .collect()
}

/// The record closing a lesson: its marker wins, a marker-less record closes by number only.
pub fn find_record(records: &[Record], course: &str, slug: &str, number: &str) -> Option<String> {
    let padding = 4usize.saturating_sub(number.chars().count());
    let numbered = format!("{}{number}-", "0".repeat(padding));
    let hit = records
        .iter()
        .find(|record| record.marker.as_deref() == Some(slug))
        .or_else(|| {
            records
                .iter()
                .find(|record| record.marker.is_none() && record.file.starts_with(&numbered))
        })?;
    Some(format!(
        "courses/{course}/learning-records/{}",
        hit.file
    ))
}

pub fn closing_record(root: &Path, course: &str, slug: &str, number: &str) -> Option<String> {
    find_record(&read_records(root, course), course, slug, number)
}

/// `N1`, `N2`... in first-appearance order, for ids defined with `[`.
pub fn parse_dag_ids(dag: &str) -> Vec<String> {
    let bytes = dag.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'N' && (i == 0 || !js::is_word_byte(bytes[i - 1])) {
            let mut digits_end = i + 1;
            while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
                digits_end += 1;
            }
            if digits_end > i + 1 {
                let mut after_ws = digits_end;
                while let Some(c) = dag[after_ws..].chars().next() {
                    if js::is_js_ws(c) {
                        after_ws += c.len_utf8();
                    } else {
                        break;
                    }
                }
                if after_ws < bytes.len() && bytes[after_ws] == b'[' {
                    let id = dag[i..digits_end].to_string();
                    if !out.contains(&id) {
                        out.push(id);
                    }
                }
            }
        }
        i += 1;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LessonStatus {
    Taught,
    InProgress,
    Closed,
}

#[derive(Debug, Clone)]
pub struct LessonSummary {
    pub course: String,
    pub filename: String,
    pub lesson_number: Option<Value>,
    pub slug: Option<Value>,
    pub title: Option<Value>,
    pub node_count: usize,
}

impl LessonSummary {
    pub fn slug_str(&self) -> String {
        js::js_string(self.slug.as_ref())
    }
}

#[derive(Debug, Clone)]
pub struct Lesson {
    /// Repo-relative path.
    pub rel: String,
    pub abs: PathBuf,
    pub summary: LessonSummary,
    pub doc: Value,
    /// Nodes in the plan DAG (0 without a plan).
    pub dag_nodes: usize,
    /// Plan nodes not captured yet, closed or not.
    pub pending_nodes: usize,
    /// Repo-relative learning record that closes it.
    pub record: Option<String>,
    pub status: LessonStatus,
}

pub fn summarize(course: &str, filename: &str, doc: &Value) -> Result<LessonSummary, String> {
    let metadata = doc.get("metadata").filter(|value| value.is_object());
    let nodes = doc.get("nodes").and_then(Value::as_array);
    let (Some(metadata), Some(nodes)) = (metadata, nodes) else {
        return Err(format!("{MALFORMED}: metadata and nodes are required"));
    };
    let title = metadata
        .get("title")
        .filter(|value| js::truthy(value))
        .or_else(|| metadata.get("topic"))
        .cloned();
    Ok(LessonSummary {
        course: course.to_string(),
        filename: filename.to_string(),
        lesson_number: metadata.get("lessonNumber").cloned(),
        slug: metadata.get("slug").cloned(),
        title,
        node_count: nodes.len(),
    })
}

fn build(root: &Path, rel: &str, summary: LessonSummary, doc: Value, records: &[Record]) -> Lesson {
    let dag = doc
        .get("metadata")
        .and_then(|metadata| metadata.get("initialDag"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let dag_nodes = parse_dag_ids(dag).len();
    let record = find_record(
        records,
        &summary.course,
        &summary.slug_str(),
        &js::js_string(summary.lesson_number.as_ref()),
    );
    let pending_nodes = dag_nodes.saturating_sub(summary.node_count);
    let status = if record.is_some() {
        LessonStatus::Closed
    } else if pending_nodes > 0 {
        LessonStatus::InProgress
    } else {
        LessonStatus::Taught
    };
    Lesson {
        rel: rel.to_string(),
        abs: resolve_path(&root.join(rel)),
        summary,
        doc,
        dag_nodes,
        pending_nodes,
        record,
        status,
    }
}

fn dir_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// Every readable lesson artifact, each parsed once. Unreadable ones are skipped (the scan warns).
pub fn list_lessons(root: &Path) -> Vec<Lesson> {
    let root = resolve_path(root);
    let courses_dir = root.join("courses");
    if !courses_dir.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for course in dir_names(&courses_dir) {
        let lessons_dir = courses_dir.join(&course).join("lessons");
        if !lessons_dir.is_dir() {
            continue;
        }
        let records = read_records(&root, &course);
        for filename in dir_names(&lessons_dir) {
            if !filename.ends_with(".html") {
                continue;
            }
            let raw = match fs::read(lessons_dir.join(&filename)) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) => {
                    eprintln!("Ignorata {course}/{filename}: {e}");
                    continue;
                }
            };
            let doc = match parse_lesson_document(&raw) {
                Ok(Some(doc)) => doc,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("Ignorata {course}/{filename}: {e}");
                    continue;
                }
            };
            let summary = match summarize(&course, &filename, &doc) {
                Ok(summary) => summary,
                Err(e) => {
                    eprintln!("Ignorata {course}/{filename}: {e}");
                    continue;
                }
            };
            let rel = format!("courses/{course}/lessons/{filename}");
            out.push(build(&root, &rel, summary, doc, &records));
        }
    }
    // Stable: course byte order, then lesson number (a missing number compares equal).
    out.sort_by(|a, b| {
        a.summary.course.cmp(&b.summary.course).then_with(|| {
            let left = a.summary.lesson_number.as_ref().and_then(Value::as_f64);
            let right = b.summary.lesson_number.as_ref().and_then(Value::as_f64);
            match (left, right) {
                (Some(left), Some(right)) => {
                    left.partial_cmp(&right).unwrap_or(std::cmp::Ordering::Equal)
                }
                _ => std::cmp::Ordering::Equal,
            }
        })
    });
    out
}

/// One lesson by repo-relative path. `Ok(None)` when missing or without an island.
pub fn read_lesson(root: &Path, rel: &str) -> Result<Option<Lesson>, String> {
    let root = resolve_path(root);
    let abs = lesson_abs(&root, rel)?;
    if !abs.is_file() {
        return Ok(None);
    }
    let raw = fs::read(&abs).map_err(|e| e.to_string())?;
    let Some(doc) = parse_lesson_document(&String::from_utf8_lossy(&raw))? else {
        return Ok(None);
    };
    let normalised = rel.replace('\\', "/");
    let course = normalised.split('/').nth(1).unwrap_or("").to_string();
    let filename = normalised
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let summary = summarize(&course, &filename, &doc)?;
    Ok(Some(build(
        &root,
        &normalised,
        summary,
        doc,
        &read_records(&root, &course),
    )))
}

/// Raw HTML of a lesson file, for artifacts without a readable island (prose lessons).
pub fn read_lesson_html(root: &Path, rel: &str) -> Result<Option<String>, String> {
    let abs = lesson_abs(root, rel)?;
    if !abs.exists() {
        return Ok(None);
    }
    // Node's readFileSync on a directory is EISDIR; Rust may report something else on Windows.
    if abs.is_dir() {
        return Err("EISDIR: illegal operation on a directory, read".into());
    }
    match fs::read(&abs) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) => Err(e.to_string()),
    }
}

fn last_ci(hay: &str, needle: &str) -> Option<usize> {
    let mut from = 0;
    let mut last = None;
    while let Some(found) = js::find_ci(hay, needle, from) {
        last = Some(found);
        from = found + 1;
    }
    last
}

fn inner_tag<'a>(src: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut from = 0;
    while let Some(pos) = js::find_ci(src, &open, from) {
        let after = pos + open.len();
        if after < src.len() && js::is_word_byte(src.as_bytes()[after]) {
            from = after;
            continue;
        }
        let gt = after + src[after..].find('>')?;
        let body = &src[gt + 1..];
        let end = last_ci(body, &close)?;
        return Some(&body[..end]);
    }
    None
}

fn close_span_end(src: &str, close: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(pos) = js::find_ci(src, close, at) {
        let mut end = pos + close.len();
        end = skip_ws(src, end);
        if end < src.len() && src.as_bytes()[end] == b'>' {
            return Some(end + 1);
        }
        at = pos + 1;
    }
    None
}

/// `/<(script|style)\b[\s\S]*?<\/\1\s*>/gi` in one left-to-right pass: the earliest boundary-checked
/// `<script` or `<style` wins and is stripped through the closing tag of that same tag.
fn strip_script_and_style(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let Some((pos, tag, after)) = next_script_or_style(src, i) else {
            out.push_str(&src[i..]);
            break;
        };
        let close = format!("</{tag}");
        if let Some(end) = close_span_end(src, &close, after) {
            out.push_str(&src[i..pos]);
            i = end;
            continue;
        }
        // No closing tag (or not a tag at all): keep it and keep scanning.
        out.push_str(&src[i..after]);
        i = after;
    }
    out
}

/// Earliest `<script`/`<style` at or after `from` whose tag name ends at a word boundary.
fn next_script_or_style(src: &str, from: usize) -> Option<(usize, &'static str, usize)> {
    let mut hit: Option<(usize, &'static str, usize)> = None;
    // `script` first like the regex alternation on ties (they cannot share a start).
    for tag in ["script", "style"] {
        let open = format!("<{tag}");
        let mut at = from;
        while let Some(pos) = js::find_ci(src, &open, at) {
            let after = pos + open.len();
            if after >= src.len() || !js::is_word_byte(src.as_bytes()[after]) {
                if hit.is_none_or(|(best, _, _)| pos < best) {
                    hit = Some((pos, tag, after));
                }
                break;
            }
            // `<scriptx`: not the tag, keep looking for a later real one.
            at = after;
        }
    }
    hit
}

fn strip_link_tags(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let Some(pos) = js::find_ci(src, "<link", i) else {
            out.push_str(&src[i..]);
            break;
        };
        let after = pos + "<link".len();
        let boundary_ok = after >= src.len() || !js::is_word_byte(src.as_bytes()[after]);
        if boundary_ok {
            if let Some(rel) = src[after..].find('>') {
                out.push_str(&src[i..pos]);
                i = after + rel + 1;
                continue;
            }
        }
        out.push_str(&src[i..after]);
        i = after;
    }
    out
}

fn title_of(src: &str) -> Option<String> {
    let start = js::find_ci(src, "<title>", 0)? + "<title>".len();
    let body = &src[start..];
    let end = js::find_ci(body, "</title>", 0)?;
    Some(js::js_trim(&body[..end]).to_string())
}

pub fn read_doc_body(root: &Path, rel: &str) -> Option<Value> {
    let normalised = doc_rel(rel)?;
    let abs = resolve_path(&root.join(&normalised));
    if !abs.is_file() {
        return None;
    }
    let bytes = fs::read(&abs).ok()?;
    let src = String::from_utf8_lossy(&bytes);
    let mut html = inner_tag(&src, "main")
        .or_else(|| inner_tag(&src, "body"))
        .unwrap_or(&src)
        .to_string();
    html = strip_script_and_style(&html);
    html = strip_link_tags(&html);

    let mut out = Map::new();
    if let Some(title) = title_of(&src) {
        out.insert("title".into(), Value::String(title));
    }
    out.insert("html".into(), Value::String(html));
    Some(Value::Object(out))
}

/// `courses/<course>/reference/<slug>.html` -> (course, slug), refusing traversal.
fn reference_parts(rel: &str) -> Option<(&str, &str)> {
    if rel.contains("..") {
        return None;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() != 4 || parts[0] != "courses" || parts[2] != "reference" {
        return None;
    }
    if !course_ok(parts[1]) {
        return None;
    }
    let file = parts[3];
    if file.len() < 6 || !file.ends_with(".html") {
        return None;
    }
    let slug = &file[..file.len() - ".html".len()];
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return None;
    }
    Some((parts[1], slug))
}

pub fn read_reference_document(root: &Path, rel: &str) -> Result<Option<Value>, String> {
    let normalised = rel.replace('\\', "/");
    let Some((course, file_slug)) = reference_parts(&normalised) else {
        return Ok(None);
    };
    let abs_ref = resolve_path(&root.join(&normalised));
    if abs_ref.is_file() {
        if let Ok(bytes) = fs::read(&abs_ref) {
            let src = String::from_utf8_lossy(&bytes);
            if let Some(doc) = parse_reference_document(&src) {
                if js::truthy(&doc) {
                    return Ok(Some(doc));
                }
            }
        }
    }

    // Fallback: the matching lesson artifact plus its learning record.
    let lessons_dir = root.join("courses").join(course).join("lessons");
    if !lessons_dir.exists() {
        return Ok(None);
    }
    let wanted = format!("{file_slug}.html");
    let candidate = dir_names(&lessons_dir)
        .into_iter()
        .filter(|name| name.ends_with(".html"))
        .find(|name| slug_of_lesson(name) == file_slug || *name == wanted);
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    let lesson_path = lessons_dir.join(candidate);
    let raw = fs::read(&lesson_path).map_err(|e| e.to_string())?;
    let Some(doc) = parse_lesson_document(&String::from_utf8_lossy(&raw))? else {
        return Ok(None);
    };
    build_sintesi_document(&doc, &lesson_path).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    const DAG: &str = "graph TD\nN1[a] --> N2[b]\nN2 --> N3[c]";

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
            "proxi-lesson-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        fs::write(path, content).expect("write file");
    }

    fn lesson_html(num: usize, slug: &str, nodes: usize, dag: &str) -> String {
        let doc = serde_json::json!({
            "metadata": {
                "lessonNumber": num,
                "topic": slug,
                "title": slug,
                "slug": slug,
                "dateCreated": "",
                "dateModified": "",
                "version": "1",
                "initialDag": dag,
                "targetGoal": "g"
            },
            "nodes": (1..=nodes).map(|i| serde_json::json!({
                "nodeId": format!("n{i}"),
                "title": format!("Nodo {i}"),
                "tier": "axiom",
                "content": "x"
            })).collect::<Vec<_>>()
        });
        format!(
            r#"<html><body><script type="application/json" id="lesson-data">{doc}</script></body></html>"#
        )
    }

    #[test]
    fn path_rule_accepts_only_lesson_files() {
        assert_eq!(
            lesson_rel("courses/demo/lessons/0001-a.html").as_deref(),
            Some("courses/demo/lessons/0001-a.html")
        );
        assert_eq!(
            lesson_rel("courses\\demo\\lessons\\0001-a.html").as_deref(),
            Some("courses/demo/lessons/0001-a.html")
        );
        for bad in [
            "../x/lessons/0001-a.html",
            "courses/../lessons/0001-a.html",
            "courses/demo/lessons/../../x.html",
            "courses/demo/reference/a.html",
            "courses/demo/lessons/a.md",
            "courses/demo/lessons/sub/a.html",
            "lessons/0001-a.html",
            "/etc/passwd",
            "",
        ] {
            assert_eq!(lesson_rel(bad), None, "{bad}");
        }

        let err = lesson_abs(Path::new("/r"), "courses/demo/lessons/../x.html").unwrap_err();
        assert!(err.contains("Bad lesson path"), "{err}");
        assert_eq!(
            lesson_abs(Path::new("/r"), "courses/demo/lessons/0001-a.html").unwrap(),
            resolve_path(&Path::new("/r").join("courses/demo/lessons/0001-a.html"))
        );
    }

    #[test]
    fn read_lesson_parses_once_with_its_record() {
        let temp = temp_dir("read");
        let root = &temp.0;
        write(
            root,
            "courses/demo/lessons/0001-a.html",
            &lesson_html(1, "a", 1, DAG),
        );
        write(root, "courses/demo/lessons/0002-prosa.html", "<html>solo prosa</html>");
        write(
            root,
            "courses/demo/lessons/0003-rotta.html",
            r#"<script type="application/json" id="lesson-data">{nope</script>"#,
        );
        write(root, "courses/demo/learning-records/0009-x.md", "# x\n\n<!-- lesson: a -->\n");

        let lesson = read_lesson(root, "courses/demo/lessons/0001-a.html")
            .unwrap()
            .expect("lesson");
        assert_eq!(lesson.summary.slug_str(), "a");
        assert_eq!(lesson.doc["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(lesson.dag_nodes, 3);
        assert_eq!(lesson.pending_nodes, 2);
        assert_eq!(
            lesson.record.as_deref(),
            Some("courses/demo/learning-records/0009-x.md")
        );

        assert!(read_lesson(root, "courses/demo/lessons/0099-none.html")
            .unwrap()
            .is_none());
        assert!(read_lesson(root, "courses/demo/lessons/0002-prosa.html")
            .unwrap()
            .is_none());
        let html = read_lesson_html(root, "courses/demo/lessons/0002-prosa.html")
            .unwrap()
            .expect("html");
        assert!(html.contains("prosa"), "{html}");
        assert!(read_lesson_html(root, "courses/demo/lessons/0099-none.html")
            .unwrap()
            .is_none());
        let err = read_lesson(root, "courses/demo/lessons/0003-rotta.html").unwrap_err();
        assert!(err.contains("Malformed"), "{err}");
        let err = read_lesson(root, "../x.html").unwrap_err();
        assert!(err.contains("Bad lesson path"), "{err}");

        fs::create_dir_all(root.join("courses/demo/lessons/0005-dir.html")).expect("dir lesson");
        let err = read_lesson_html(root, "courses/demo/lessons/0005-dir.html").unwrap_err();
        assert!(err.contains("EISDIR"), "{err}");
        assert!(read_lesson(root, "courses/demo/lessons/0005-dir.html")
            .unwrap()
            .is_none());

        let slugs: Vec<String> = list_lessons(root)
            .iter()
            .map(|lesson| lesson.summary.slug_str())
            .collect();
        assert_eq!(slugs, vec!["a"]);
    }

    #[test]
    fn status_taught_in_progress_closed() {
        let temp = temp_dir("status");
        let root = &temp.0;
        let status = |slug: &str| {
            list_lessons(root)
                .into_iter()
                .find(|lesson| lesson.summary.slug_str() == slug)
                .expect("lesson")
                .status
        };
        write(root, "courses/demo/lessons/0001-a.html", &lesson_html(1, "a", 1, DAG));
        write(root, "courses/demo/lessons/0002-b.html", &lesson_html(2, "b", 3, DAG));
        write(root, "courses/demo/lessons/0003-c.html", &lesson_html(3, "c", 1, DAG));

        assert_eq!(status("a"), LessonStatus::InProgress);
        assert_eq!(status("b"), LessonStatus::Taught);

        // A record carrying another lesson's marker closes that lesson, never the one sharing its number.
        write(root, "courses/demo/learning-records/0003-x.md", "# x\n\n<!-- lesson: a -->\n");
        assert_eq!(status("a"), LessonStatus::Closed);
        assert_eq!(status("c"), LessonStatus::InProgress);

        // The empty marker (plan/practice record) names no lesson.
        write(root, "courses/demo/learning-records/0002-y.md", "# y\n\n<!-- lesson: -->\n");
        assert_eq!(status("b"), LessonStatus::Taught);

        // Legacy: a record with no marker at all closes the lesson with its number.
        write(root, "courses/demo/learning-records/0001-z.md", "# z\n");
        assert_eq!(
            closing_record(root, "demo", "a", "1").as_deref(),
            Some("courses/demo/learning-records/0003-x.md")
        );
        write(root, "courses/demo/learning-records/0003-x.md", "# x\n");
        assert_eq!(status("c"), LessonStatus::Closed);
    }

    #[test]
    fn doc_rel_and_read_doc_body() {
        assert_eq!(
            doc_rel("courses/c/reference/x.html").as_deref(),
            Some("courses/c/reference/x.html")
        );
        assert_eq!(
            doc_rel("courses/c/lessons/0001-a.html").as_deref(),
            Some("courses/c/lessons/0001-a.html")
        );
        assert_eq!(doc_rel("courses/c/exercises/x.html"), None);
        assert_eq!(doc_rel("courses/c/reference/../../x.html"), None);

        let temp = temp_dir("doc");
        let root = &temp.0;
        write(
            root,
            "courses/c/reference/x.html",
            "<html><head><title>T</title><style>a{}</style><link rel=x></head><body><script>1</script><main><h2>Ciao</h2></main></body></html>",
        );
        let body = read_doc_body(root, "courses/c/reference/x.html").expect("body");
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            r#"{"title":"T","html":"<h2>Ciao</h2>"}"#
        );

        // The earliest `<script`/`<style` wins and is stripped to its own close tag, like Node's
        // single alternation pass: the `<script>` inside the style is not a nesting point.
        write(
            root,
            "courses/c/reference/y.html",
            "<main><style>/* <script> */</style><p>x</p><script></script></main>",
        );
        let body = read_doc_body(root, "courses/c/reference/y.html").expect("body");
        assert_eq!(body["html"], serde_json::json!("<p>x</p>"));
        assert!(read_doc_body(root, "courses/c/reference/nope.html").is_none());
    }

    #[test]
    fn read_reference_document_island_and_fallback() {
        let temp = temp_dir("reference");
        let root = &temp.0;

        let ref_doc = serde_json::json!({
            "title": "Sintesi A",
            "topic": "A",
            "slug": "a",
            "nodes": [{ "nodeId": "n1", "number": 1, "title": "Nodo 1", "tier": "unconditional-truth", "summary": "Fatto 1" }],
            "formulas": [],
            "misconceptions": []
        });
        write(
            root,
            "courses/c/reference/a.html",
            &format!(
                r#"<html><body><script type="application/json" id="reference-data">{ref_doc}</script></body></html>"#
            ),
        );
        let parsed = read_reference_document(root, "courses/c/reference/a.html")
            .unwrap()
            .expect("reference island");
        assert_eq!(parsed["title"], serde_json::json!("Sintesi A"));
        assert_eq!(parsed["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["nodes"][0]["summary"], serde_json::json!("Fatto 1"));

        write(root, "courses/c/lessons/0002-b.html", &lesson_html(2, "b", 2, DAG));
        write(
            root,
            "courses/c/learning-records/0002-b.md",
            "# Rec\n\n## Evidence\n\n- Learner slip A\n\n<!-- lesson: b -->\n",
        );
        let fallback = read_reference_document(root, "courses/c/reference/b.html")
            .unwrap()
            .expect("fallback");
        assert_eq!(fallback["title"], serde_json::json!("b"));
        assert_eq!(fallback["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(fallback["initialDag"], serde_json::json!(DAG));
        assert!(fallback["misconceptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["description"]
                .as_str()
                .unwrap_or("")
                .contains("Learner slip A")));

        assert!(read_reference_document(root, "courses/c/reference/nope.html")
            .unwrap()
            .is_none());
        assert!(read_reference_document(root, "courses/c/lessons/b.html")
            .unwrap()
            .is_none());
    }

    #[test]
    fn parse_dag_ids_lists_ids_in_first_appearance_order() {
        assert_eq!(
            parse_dag_ids("graph TD\nN1[a] --> N2[b]\nN2 --> N3[c]"),
            vec!["N1", "N2", "N3"]
        );
        assert!(parse_dag_ids("xN1[a]").is_empty());
        assert_eq!(
            parse_dag_ids("N1 [a]\nN1 --> N10 [b]"),
            vec!["N1".to_string(), "N10".to_string()]
        );
    }

    #[test]
    fn parse_marker_reads_the_closing_marker() {
        assert_eq!(parse_marker("<!-- lesson: -->").as_deref(), Some(""));
        assert_eq!(
            parse_marker("# x\n\n<!-- lesson: a -->\n").as_deref(),
            Some("a")
        );
        assert_eq!(parse_marker("# x\n"), None);
        assert_eq!(parse_marker("<!-- lesson: a --> trailing\n"), None);
    }
}
