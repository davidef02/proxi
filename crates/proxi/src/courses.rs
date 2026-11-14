//! Course overview and standalone lessons (app/server/courses.ts, extensions/chapter.ts).

use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::data_root::{has_data, resolve_path};
use crate::js;
use crate::lesson::{Lesson, LessonStatus, list_lessons};

pub const STANDALONE_COURSE_SLUG: &str = "lezioni-singole";

/// MISSION.md written into courses/lezioni-singole/ when the container is created.
pub const STANDALONE_MISSION: &str = concat!(
    "# Lezioni singole\n",
    "\n",
    "## Why\n",
    "\n",
    "Studiare argomenti liberi e autocontenuti fuori dai corsi strutturati, mantenendo la stessa profondità dai primi principi.\n",
    "\n",
    "## Success looks like\n",
    "\n",
    "Ogni lezione singola installa un'intuizione solida e verificata sui concetti scelti dallo studente.\n",
    "\n",
    "## Constraints\n",
    "\n",
    "- Una lezione copre un solo argomento ben delimitato.\n",
    "\n",
    "## Out of scope\n",
    "\n",
    "- Percorsi multi-lezione: se il tema richiede un intero corso, proporre una sessione di pianificazione.\n",
);

/// `Syntax` is a curriculum.json that is not JSON: the server answers 400 like Node's SyntaxError.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoursesError {
    Syntax,
    Other(String),
}

/// Creates courses/lezioni-singole/ (lessons, reference, learning-records) + MISSION.md, create-only.
/// On read paths it stays off a data folder nobody has touched yet: creating courses/ there would
/// flip `has_data` to true and hide the first-run sync.
pub fn ensure_standalone_course(root: &Path, force: bool) -> io::Result<()> {
    let root = resolve_path(root);
    if !force && !has_data(&root) && !root.join("courses").exists() {
        return Ok(());
    }
    let dir = root.join("courses").join(STANDALONE_COURSE_SLUG);
    for sub in ["lessons", "reference", "learning-records"] {
        fs::create_dir_all(dir.join(sub))?;
    }
    let mission = dir.join("MISSION.md");
    if !mission.exists() {
        fs::write(mission, STANDALONE_MISSION)?;
    }
    Ok(())
}

/// First paragraph under "## Why" in MISSION.md (else the first paragraph after the title),
/// markdown stripped.
pub fn mission_destination(root: &Path, slug: &str) -> Option<String> {
    let raw = fs::read(root.join("courses").join(slug).join("MISSION.md")).ok()?;
    let md = String::from_utf8_lossy(&raw);
    let paragraph = paragraphs(&mission_body(&md))
        .into_iter()
        .find(|p| !p.is_empty() && !p.starts_with('#'))?;
    Some(truncate(&strip_markdown(&paragraph)))
}

/// `^## +Why\b` on one line; the match stops before a trailing `\r`.
fn is_why_header(line: &str) -> bool {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let Some(rest) = line.strip_prefix("##") else {
        return false;
    };
    let spaces = rest.len() - rest.trim_start_matches(' ').len();
    if spaces == 0 {
        return false;
    }
    let Some(after) = rest[spaces..].strip_prefix("Why") else {
        return false;
    };
    after.is_empty() || !js::is_word_byte(after.as_bytes()[0])
}

/// Text after the first `## Why` header up to the next one; without a header, the file minus its title.
fn mission_body(md: &str) -> String {
    let lines: Vec<&str> = md.split('\n').collect();
    if let Some(first) = lines.iter().position(|line| is_why_header(line)) {
        let end = lines
            .iter()
            .enumerate()
            .skip(first + 1)
            .find(|(_, line)| is_why_header(line))
            .map(|(idx, _)| idx)
            .unwrap_or(lines.len());
        return lines[first + 1..end].join("\n");
    }
    // No `## Why`: blank the first line starting with '#' but keep its newline (Node's
    // `md.replace(/^#.*$/m, "")`), so the paragraphs on either side stay separate.
    let mut out = Vec::with_capacity(lines.len());
    let mut stripped = false;
    for line in &lines {
        if !stripped && line.starts_with('#') {
            stripped = true;
            out.push("");
            continue;
        }
        out.push(*line);
    }
    out.join("\n")
}

/// Maximal runs of lines that are not whitespace-only, trimmed.
fn paragraphs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    for line in body.split('\n') {
        if js::js_trim(line).is_empty() {
            if !run.is_empty() {
                out.push(run.join("\n"));
                run.clear();
            }
        } else {
            run.push(line);
        }
    }
    if !run.is_empty() {
        out.push(run.join("\n"));
    }
    out.into_iter()
        .map(|p| js::js_trim(&p).to_string())
        .collect()
}

/// Drops `*`, `_` and backticks, turns `[label](target)` into the label, then collapses whitespace.
fn strip_markdown(para: &str) -> String {
    let without_marks: String = para
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '`'))
        .collect();
    js::collapse_ws(&replace_links(&without_marks))
}

fn replace_links(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(close) = s[i + 1..].find(']').map(|rel| i + 1 + rel) {
                // `[^\]]+` needs at least one label char, then `](` must follow.
                if close > i + 1 && bytes.get(close + 1) == Some(&b'(') {
                    if let Some(paren) = s[close + 2..].find(')') {
                        out.push_str(&s[i + 1..close]);
                        i = close + 2 + paren + 1;
                        continue;
                    }
                }
            }
        }
        let ch = s[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn truncate(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 240 {
        return text.to_string();
    }
    let head = &chars[..239];
    // `slice(0, 239).replace(/\s+\S*$/, "")`: cut at the last whitespace run, if any.
    let Some(last_ws) = head.iter().rposition(|c| js::is_js_ws(*c)) else {
        let kept: String = head.iter().collect();
        return format!("{kept}…");
    };
    let mut cut = last_ws;
    while cut > 0 && js::is_js_ws(head[cut - 1]) {
        cut -= 1;
    }
    let kept: String = head[..cut].iter().collect();
    format!("{kept}…")
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: Option<Value>,
    pub slug: Option<Value>,
    pub lessons: Vec<Value>,
}

/// `extensions/chapter.ts:60-74`: a missing file is no plan, a wrong shape is an error.
pub fn read_curriculum(root: &Path) -> Result<Vec<Chapter>, CoursesError> {
    let root = resolve_path(root);
    let path = root.join("curriculum.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path).map_err(|e| CoursesError::Other(e.to_string()))?;
    // Structured numbers, like JSON.parse + JSON.stringify: `2.0` in the file is `2` in the plan.
    let parsed = js::parse_json(&raw).map_err(|_| CoursesError::Syntax)?;
    let Some(object) = parsed.as_object() else {
        return Ok(Vec::new());
    };
    let Some(chapters) = object.get("chapters") else {
        return Ok(Vec::new());
    };
    let Some(chapters) = chapters.as_array() else {
        return Err(CoursesError::Other(format!(
            "Malformed {}: \"chapters\" must be an array",
            path.display()
        )));
    };
    Ok(chapters.iter().map(chapter_of).collect())
}

fn chapter_of(value: &Value) -> Chapter {
    Chapter {
        title: value.get("title").cloned(),
        slug: value.get("slug").cloned(),
        lessons: value
            .get("lessons")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

#[derive(Debug)]
pub struct Match {
    /// Chapter index and its plan entries: the planned value and the lesson claiming it.
    pub chapters: Vec<(usize, Vec<(Value, Option<usize>)>)>,
    /// Lesson indices no plan entry claims, in lesson order.
    pub orphans: Vec<usize>,
}

/// Joins the plan onto the lessons on disk. A plan entry is claimed only by a lesson of its course.
pub fn match_curriculum(chapters: &[Chapter], lessons: &[Lesson]) -> Match {
    let mut by_key: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (idx, lesson) in lessons.iter().enumerate() {
        let key = format!("{}/{}", lesson.summary.course, lesson.summary.slug_str());
        // First wins, and the shadowed file stays in `orphans`: two artifacts with one slug is a
        // broken state, and hiding one of them is how it stays unnoticed.
        by_key.entry(key).or_insert(idx);
    }

    let mut claimed = vec![false; lessons.len()];
    let mut matched = Vec::with_capacity(chapters.len());
    for (chapter_idx, chapter) in chapters.iter().enumerate() {
        let mut entries = Vec::with_capacity(chapter.lessons.len());
        for planned in &chapter.lessons {
            let key = format!(
                "{}/{}",
                js::js_string(chapter.slug.as_ref()),
                js::js_string(planned.get("slug"))
            );
            let lesson_idx = by_key.get(&key).copied();
            if let Some(idx) = lesson_idx {
                claimed[idx] = true;
            }
            entries.push((planned.clone(), lesson_idx));
        }
        matched.push((chapter_idx, entries));
    }

    Match {
        chapters: matched,
        orphans: (0..lessons.len()).filter(|&idx| !claimed[idx]).collect(),
    }
}

pub fn to_course_lesson(root: &Path, lesson: &Lesson) -> Map<String, Value> {
    let summary = &lesson.summary;
    let mut out = Map::new();
    if let Some(title) = &summary.title {
        out.insert("title".into(), title.clone());
    }
    if let Some(slug) = &summary.slug {
        out.insert("slug".into(), slug.clone());
    }
    out.insert("lessonPath".into(), Value::String(lesson.rel.clone()));
    if let Some(number) = &summary.lesson_number {
        out.insert("lessonNumber".into(), number.clone());
    }
    out.insert("nodeCount".into(), json!(summary.node_count));
    let reference = format!(
        "courses/{}/reference/{}.html",
        summary.course,
        summary.slug_str()
    );
    if root.join(&reference).exists() {
        out.insert("referencePath".into(), Value::String(reference));
    }
    out.insert(
        "inProgress".into(),
        Value::Bool(lesson.status == LessonStatus::InProgress),
    );
    out
}

fn matched_lesson(root: &Path, lesson: &Lesson, planned: &Value) -> Value {
    let taught = to_course_lesson(root, lesson);
    // Node spreads the taught entry then overwrites `title` in place; a fresh map keeps `title`
    // first even when only the plan has one, and `Map::remove` would swap the last key into its slot.
    let mut entry = Map::new();
    if let Some(title) = planned.get("title") {
        entry.insert("title".into(), title.clone());
    }
    for (key, value) in taught {
        if key != "title" {
            entry.insert(key, value);
        }
    }
    if let Some(nodes) = planned.get("plannedNodes") {
        entry.insert("plannedNodes".into(), nodes.clone());
    }
    Value::Object(entry)
}

fn planned_lesson(planned: &Value) -> Value {
    let mut entry = Map::new();
    for key in ["title", "slug", "plannedNodes"] {
        if let Some(value) = planned.get(key) {
            entry.insert(key.into(), value.clone());
        }
    }
    Value::Object(entry)
}

fn insert_flags(course: &mut Map<String, Value>, root: &Path, slug: &str) {
    let dir = root.join("courses").join(slug);
    course.insert(
        "hasExercises".into(),
        Value::Bool(dir.join("exercises").join("package.json").exists()),
    );
    course.insert(
        "hasMission".into(),
        Value::Bool(dir.join("MISSION.md").exists()),
    );
    if let Some(destination) = mission_destination(root, slug) {
        course.insert("destination".into(), Value::String(destination));
    }
}

/// The status board: the plan's chapters with their taught lessons, then the orphan courses.
pub fn list_courses(root: &Path) -> Result<Vec<Value>, CoursesError> {
    let root = resolve_path(root);
    ensure_standalone_course(&root, false).map_err(|e| CoursesError::Other(e.to_string()))?;
    let lessons = list_lessons(&root);
    let chapters = read_curriculum(&root)?;
    let matched = match_curriculum(&chapters, &lessons);

    let mut out = Vec::new();
    let mut listed: Vec<String> = Vec::new();
    for (chapter_idx, entries) in &matched.chapters {
        let chapter = &chapters[*chapter_idx];
        let slug = js::js_string(chapter.slug.as_ref());
        if slug == STANDALONE_COURSE_SLUG {
            continue;
        }
        let mut course = Map::new();
        if let Some(slug_value) = &chapter.slug {
            course.insert("slug".into(), slug_value.clone());
        }
        if let Some(title) = &chapter.title {
            course.insert("title".into(), title.clone());
        }
        let lessons_out = entries
            .iter()
            .map(|(planned, lesson_idx)| match lesson_idx {
                Some(idx) => matched_lesson(&root, &lessons[*idx], planned),
                None => planned_lesson(planned),
            })
            .collect();
        course.insert("lessons".into(), Value::Array(lessons_out));
        let orphans = orphans_of(&matched, &lessons, &root, &slug);
        course.insert("orphans".into(), Value::Array(orphans));
        insert_flags(&mut course, &root, &slug);
        listed.push(slug);
        out.push(Value::Object(course));
    }

    // Taught course directories the plan does not name still get a card of their own.
    let mut seen: Vec<String> = Vec::new();
    for &idx in &matched.orphans {
        let slug = lessons[idx].summary.course.clone();
        if seen.contains(&slug) {
            continue;
        }
        seen.push(slug.clone());
        if slug == STANDALONE_COURSE_SLUG || listed.contains(&slug) {
            continue;
        }
        let mut course = Map::new();
        course.insert("slug".into(), Value::String(slug.clone()));
        course.insert("title".into(), Value::String(slug.clone()));
        course.insert("lessons".into(), Value::Array(Vec::new()));
        let orphans = orphans_of(&matched, &lessons, &root, &slug);
        course.insert("orphans".into(), Value::Array(orphans));
        insert_flags(&mut course, &root, &slug);
        out.push(Value::Object(course));
    }
    Ok(out)
}

fn orphans_of(match_result: &Match, lessons: &[Lesson], root: &Path, slug: &str) -> Vec<Value> {
    match_result
        .orphans
        .iter()
        .filter(|&&idx| lessons[idx].summary.course == slug)
        .map(|&idx| Value::Object(to_course_lesson(root, &lessons[idx])))
        .collect()
}

/// The lessons of courses/lezioni-singole/, in lesson order. Provisioning on read like `list_courses`.
pub fn list_standalone_lessons(root: &Path) -> Result<Vec<Value>, CoursesError> {
    let root = resolve_path(root);
    ensure_standalone_course(&root, false).map_err(|e| CoursesError::Other(e.to_string()))?;
    Ok(list_lessons(&root)
        .iter()
        .filter(|lesson| lesson.summary.course == STANDALONE_COURSE_SLUG)
        .map(|lesson| Value::Object(to_course_lesson(&root, lesson)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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
            "proxi-courses-{tag}-{}-{nanos}-{n}",
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

    fn lesson_html(num: usize, slug: &str, title: &str, nodes: usize, dag: &str) -> String {
        let doc = serde_json::json!({
            "metadata": {
                "lessonNumber": num,
                "topic": title,
                "title": title,
                "slug": slug,
                "dateCreated": "",
                "dateModified": "",
                "version": "1",
                "initialDag": dag
            },
            "nodes": (1..=nodes).map(|i| serde_json::json!({
                "nodeId": format!("n{i}"),
                "title": format!("Nodo {i}"),
                "tier": "axiom",
                "content": "x"
            })).collect::<Vec<_>>()
        });
        format!(r#"<html><script type="application/json" id="lesson-data">{doc}</script></html>"#)
    }

    fn slugs(courses: &[Value]) -> Vec<&str> {
        courses
            .iter()
            .map(|course| course["slug"].as_str().unwrap_or(""))
            .collect()
    }

    #[test]
    fn list_courses_planned_orphans_flags_in_progress() {
        let temp = temp_dir("listing");
        let root = &temp.0;
        write(
            root,
            "curriculum.json",
            &serde_json::json!({ "chapters": [{ "title": "Demo", "slug": "demo", "lessons": [
                { "title": "Prima", "slug": "prima", "plannedNodes": 2 },
                { "title": "Seconda", "slug": "seconda" },
            ]}] })
            .to_string(),
        );
        write(
            root,
            "courses/demo/MISSION.md",
            "# M\n\n## Why\n\nImparare **Vue** `ref`\nin due righe.\n\nSecondo paragrafo.\n",
        );
        write(root, "courses/demo/exercises/package.json", "{}");
        write(
            root,
            "courses/demo/lessons/0001-prima.html",
            &lesson_html(1, "prima", "Prima", 1, DAG),
        );
        write(root, "courses/demo/reference/prima.html", "<html/>");
        write(
            root,
            "courses/demo/lessons/0002-extra.html",
            &lesson_html(2, "extra", "Extra", 1, ""),
        );
        write(
            root,
            "courses/loose/lessons/0001-solo.html",
            &lesson_html(1, "solo", "Solo", 1, ""),
        );

        let listing = list_courses(root).expect("courses");
        assert_eq!(slugs(&listing), vec!["demo", "loose"]);
        let demo = &listing[0];
        assert_eq!(demo["hasMission"], json!(true));
        assert_eq!(demo["hasExercises"], json!(true));
        assert_eq!(demo["destination"], json!("Imparare Vue ref in due righe."));
        assert!(listing[1].get("destination").is_none());
        assert_eq!(demo["lessons"].as_array().unwrap().len(), 2);
        let prima = &demo["lessons"][0];
        assert_eq!(
            prima["lessonPath"],
            json!("courses/demo/lessons/0001-prima.html")
        );
        assert_eq!(prima["nodeCount"], json!(1));
        assert_eq!(prima["inProgress"], json!(true));
        assert_eq!(
            prima["referencePath"],
            json!("courses/demo/reference/prima.html")
        );
        assert_eq!(prima["plannedNodes"], json!(2));
        assert!(demo["lessons"][1].get("lessonPath").is_none());
        assert_eq!(demo["orphans"].as_array().unwrap().len(), 1);
        assert_eq!(demo["orphans"][0]["slug"], json!("extra"));
        assert_eq!(demo["orphans"][0]["inProgress"], json!(false));
        assert_eq!(listing[1]["lessons"].as_array().unwrap().len(), 0);

        // A learning record closes the lesson even with DAG nodes left (by number, or by the slug marker).
        write(root, "courses/demo/learning-records/0001-x.md", "# x\n");
        assert_eq!(
            list_courses(root).unwrap()[0]["lessons"][0]["inProgress"],
            json!(false)
        );
        fs::remove_file(root.join("courses/demo/learning-records/0001-x.md"))
            .expect("remove record");
        write(
            root,
            "courses/demo/learning-records/0007-y.md",
            "# y\n\n<!-- lesson: prima -->\n",
        );
        assert_eq!(
            list_courses(root).unwrap()[0]["lessons"][0]["inProgress"],
            json!(false)
        );
        assert_eq!(listing[1]["orphans"][0]["slug"], json!("solo"));
        assert_eq!(listing[1]["hasMission"], json!(false));
    }

    #[test]
    fn list_courses_provisions_standalone_only_when_data_exists() {
        let empty = temp_dir("empty");
        assert!(list_courses(&empty.0).unwrap().is_empty());
        assert!(
            !has_data(&empty.0),
            "listing an untouched data folder must not create courses/"
        );
        assert!(list_standalone_lessons(&empty.0).unwrap().is_empty());
        assert!(!has_data(&empty.0));

        let temp = temp_dir("standalone");
        let root = &temp.0;
        write(
            root,
            "curriculum.json",
            &serde_json::json!({ "chapters": [{ "title": "Demo", "slug": "demo", "lessons": [] }] })
                .to_string(),
        );
        write(
            root,
            "courses/demo/MISSION.md",
            "# M\n\n## Why\n\nImparare Vue.\n",
        );

        let courses = list_courses(root).unwrap();
        let standalone_dir = root.join("courses").join(STANDALONE_COURSE_SLUG);
        assert!(standalone_dir.join("MISSION.md").exists());
        assert_eq!(
            fs::read_to_string(standalone_dir.join("MISSION.md")).unwrap(),
            STANDALONE_MISSION
        );
        assert_eq!(
            slugs(&courses),
            vec!["demo"],
            "lezioni-singole is not a structured course"
        );

        // create-only: an existing MISSION.md (the learner's own words) is never overwritten
        write(
            root,
            "courses/lezioni-singole/MISSION.md",
            "# Mio contenitore\n\n## Why\n\nMarker.\n",
        );
        list_courses(root).unwrap();
        assert!(
            fs::read_to_string(standalone_dir.join("MISSION.md"))
                .unwrap()
                .contains("Marker."),
            "MISSION.md is create-only"
        );

        let dir = format!("courses/{STANDALONE_COURSE_SLUG}");
        write(
            root,
            &format!("{dir}/lessons/0001-trasformata-di-fourier.html"),
            &lesson_html(
                1,
                "trasformata-di-fourier",
                "La trasformata di Fourier scompone un segnale",
                2,
                "",
            ),
        );
        write(
            root,
            &format!("{dir}/reference/trasformata-di-fourier.html"),
            "<html/>",
        );
        write(
            root,
            &format!("{dir}/learning-records/0001-chiusa.md"),
            "# chiusa\n\n<!-- lesson: trasformata-di-fourier -->\n",
        );
        write(
            root,
            &format!("{dir}/lessons/0002-entropia.html"),
            &lesson_html(
                2,
                "entropia",
                "L'entropia di Shannon misura la sorpresa media",
                0,
                "graph TD\nN1[a] --> N2[b]",
            ),
        );
        // even a chapter in the plan must not pull the container into the structured list
        write(
            root,
            "curriculum.json",
            &serde_json::json!({ "chapters": [
                { "title": "Demo", "slug": "demo", "lessons": [] },
                { "title": "Lezioni singole", "slug": STANDALONE_COURSE_SLUG, "lessons": [] },
            ]})
            .to_string(),
        );
        assert_eq!(slugs(&list_courses(root).unwrap()), vec!["demo"]);

        let standalone = list_standalone_lessons(root).unwrap();
        let lessons: Vec<&str> = standalone
            .iter()
            .map(|lesson| lesson["slug"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(lessons, vec!["trasformata-di-fourier", "entropia"]);
        let fourier = &standalone[0];
        assert_eq!(
            fourier["lessonPath"],
            json!(format!("{dir}/lessons/0001-trasformata-di-fourier.html"))
        );
        assert_eq!(
            fourier["title"],
            json!("La trasformata di Fourier scompone un segnale")
        );
        assert_eq!(
            fourier["referencePath"],
            json!(format!("{dir}/reference/trasformata-di-fourier.html"))
        );
        assert_eq!(fourier["inProgress"], json!(false));
        assert_eq!(standalone[1]["inProgress"], json!(true));
    }

    #[test]
    fn course_lesson_key_order() {
        let temp = temp_dir("key-order");
        let root = &temp.0;
        write(
            root,
            "curriculum.json",
            &serde_json::json!({ "chapters": [{ "title": "Demo", "slug": "demo", "lessons": [
                { "title": "Prima", "slug": "prima", "plannedNodes": 2 },
                { "title": "Seconda", "slug": "seconda" },
            ]}] })
            .to_string(),
        );
        write(
            root,
            "courses/demo/lessons/0001-prima.html",
            &lesson_html(1, "prima", "Prima", 1, DAG),
        );
        write(root, "courses/demo/reference/prima.html", "<html/>");
        write(
            root,
            "courses/demo/lessons/0002-extra.html",
            &lesson_html(2, "extra", "Extra", 1, ""),
        );

        let listing = list_courses(root).unwrap();
        let lessons = listing[0]["lessons"].as_array().unwrap();
        assert_eq!(
            serde_json::to_string(&lessons[0]).unwrap(),
            r#"{"title":"Prima","slug":"prima","lessonPath":"courses/demo/lessons/0001-prima.html","lessonNumber":1,"nodeCount":1,"referencePath":"courses/demo/reference/prima.html","inProgress":true,"plannedNodes":2}"#
        );
        assert_eq!(
            serde_json::to_string(&lessons[1]).unwrap(),
            r#"{"title":"Seconda","slug":"seconda"}"#
        );
        assert_eq!(
            serde_json::to_string(&listing[0]["orphans"][0]).unwrap(),
            r#"{"title":"Extra","slug":"extra","lessonPath":"courses/demo/lessons/0002-extra.html","lessonNumber":2,"nodeCount":1,"inProgress":false}"#
        );
    }

    #[test]
    fn matched_lesson_keeps_title_first_without_a_planned_title() {
        let temp = temp_dir("planned-without-title");
        let root = &temp.0;
        // The plan names the lesson but carries no title: the taught title is dropped like Node's
        // `title: undefined`, and every remaining key stays in `toCourseLesson` order.
        write(
            root,
            "curriculum.json",
            r#"{"chapters":[{"title":"Demo","slug":"demo","lessons":[{"slug":"prima","plannedNodes":2}]}]}"#,
        );
        write(
            root,
            "courses/demo/lessons/0001-prima.html",
            &lesson_html(1, "prima", "Prima", 1, DAG),
        );
        let listing = list_courses(root).unwrap();
        assert_eq!(
            serde_json::to_string(&listing[0]["lessons"][0]).unwrap(),
            r#"{"slug":"prima","lessonPath":"courses/demo/lessons/0001-prima.html","lessonNumber":1,"nodeCount":1,"inProgress":true,"plannedNodes":2}"#
        );

        // The plan has a title, the lesson's island has none: `title` is still first.
        write(
            root,
            "curriculum.json",
            r#"{"chapters":[{"title":"Demo","slug":"demo","lessons":[{"title":"Prima","slug":"prima"}]}]}"#,
        );
        let untitled = r#"<html><script type="application/json" id="lesson-data">{"metadata":{"lessonNumber":1,"title":"","slug":"prima"},"nodes":[]}</script></html>"#;
        write(root, "courses/demo/lessons/0001-prima.html", untitled);
        let listing = list_courses(root).unwrap();
        assert_eq!(
            serde_json::to_string(&listing[0]["lessons"][0]).unwrap(),
            r#"{"title":"Prima","slug":"prima","lessonPath":"courses/demo/lessons/0001-prima.html","lessonNumber":1,"nodeCount":0,"inProgress":false}"#
        );
    }

    #[test]
    fn mission_destination_reads_the_first_why_paragraph() {
        let temp = temp_dir("mission");
        let root = &temp.0;
        write(
            root,
            "courses/mark/MISSION.md",
            "# M\n\n## Why\n\nImparare **Vue** `ref` [in](http://x) due righe.\n\nSecondo.\n",
        );
        assert_eq!(
            mission_destination(root, "mark").as_deref(),
            Some("Imparare Vue ref in due righe.")
        );

        // No `## Why`: the first paragraph after the title.
        write(root, "courses/plain/MISSION.md", "# Titolo\n\nIntro qui.\n");
        assert_eq!(
            mission_destination(root, "plain").as_deref(),
            Some("Intro qui.")
        );

        // A `#` line in the middle disappears but leaves its newline: paragraphs stay separate.
        write(
            root,
            "courses/between/MISSION.md",
            "Intro\n# Titolo\nAltro\n",
        );
        assert_eq!(
            mission_destination(root, "between").as_deref(),
            Some("Intro")
        );

        write(
            root,
            "courses/headings/MISSION.md",
            "# Titolo\n\n## Altro\n\n### Terzo\n",
        );
        assert_eq!(mission_destination(root, "headings"), None);

        let long = "parola ".repeat(50);
        write(
            root,
            "courses/long/MISSION.md",
            &format!("# L\n\n## Why\n\n{long}\n"),
        );
        let destination = mission_destination(root, "long").expect("destination");
        assert!(destination.ends_with('…'), "{destination}");
        assert!(
            destination.chars().count() <= 240,
            "{}",
            destination.chars().count()
        );

        write(
            root,
            "courses/crlf/MISSION.md",
            "# M\r\n\r\n## Why\r\n\r\nDestinazione.\r\n",
        );
        assert_eq!(
            mission_destination(root, "crlf").as_deref(),
            Some("Destinazione.")
        );

        assert_eq!(mission_destination(root, "missing"), None);
    }

    #[test]
    fn read_curriculum_validates_the_shape() {
        let temp = temp_dir("curriculum");
        let root = &temp.0;
        assert!(read_curriculum(root).unwrap().is_empty());

        write(root, "curriculum.json", "{");
        assert!(matches!(read_curriculum(root), Err(CoursesError::Syntax)));

        for body in [r#"{"chapters":{}}"#, r#"{"chapters":null}"#] {
            write(root, "curriculum.json", body);
            match read_curriculum(root) {
                Err(CoursesError::Other(message)) => assert_eq!(
                    message,
                    format!(
                        "Malformed {}: \"chapters\" must be an array",
                        root.join("curriculum.json").display()
                    ),
                    "{body}"
                ),
                other => panic!("{body}: {other:?}"),
            }
        }

        for body in ["{}", "null"] {
            write(root, "curriculum.json", body);
            assert!(read_curriculum(root).unwrap().is_empty(), "{body}");
        }

        write(
            root,
            "curriculum.json",
            r#"{"chapters":[{"title":"A","slug":"a"}]}"#,
        );
        let chapters = read_curriculum(root).unwrap();
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title, Some(json!("A")));
        assert_eq!(chapters[0].slug, Some(json!("a")));
        assert!(chapters[0].lessons.is_empty());
    }

    #[test]
    fn planned_values_keep_json_numbers() {
        let temp = temp_dir("planned-numbers");
        let root = &temp.0;
        write(
            root,
            "curriculum.json",
            r#"{"chapters":[{"title":"A","slug":"a","lessons":[{"title":"P","slug":"p","plannedNodes":2.0}]}]}"#,
        );
        let listing = list_courses(root).unwrap();
        let text = serde_json::to_string(&listing).unwrap();
        assert!(text.contains("\"plannedNodes\":2}"), "{text}");
        assert!(!text.contains("2.0"), "{text}");
    }
}
