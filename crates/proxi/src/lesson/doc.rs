//! Lesson documents: island, numbering, template, init and append.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::data_root::resolve_path;
use crate::events::iso_now;
use crate::lesson::{APP_NAME, js, node_path, render};
use crate::slug::{lesson_slug, slugify};

pub const ISLAND_OPEN: &str = "<script type=\"application/json\" id=\"lesson-data\">";
pub const SCRIPT_CLOSE: &str = "</script>";
pub const INJECTION_POINT: &str = "<!-- NODES_INJECTION_POINT -->";
pub const TEMPLATE_REL: &str = "assets/lesson-template.html";

/// Reads the embedded lesson document out of a lesson artifact's data island.
/// `Ok(None)` means no island: a hand-authored lesson, not an artifact.
pub fn parse_lesson_document(raw: &str) -> Result<Option<Value>, String> {
    let Some(open) = raw.find(ISLAND_OPEN) else {
        return Ok(None);
    };
    let inner_start = open + ISLAND_OPEN.len();
    let Some(offset) = raw[inner_start..].find(SCRIPT_CLOSE) else {
        return Ok(None);
    };
    let inner = &raw[inner_start..inner_start + offset];
    serde_json::from_str(js::js_trim(inner))
        .map(Some)
        .map_err(|e| format!("Malformed #lesson-data island: {e}"))
}

pub fn next_lesson_number(lessons_dir: &str) -> u64 {
    let Ok(entries) = fs::read_dir(lessons_dir) else {
        return 1;
    };
    let mut max_number = 0;
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if let Some(number) = numbered_lesson(&name) {
            max_number = max_number.max(number);
        }
    }
    max_number + 1
}

/// `^(\d{4})-[^.]+\.html$`: anything else in the folder is not a lesson artifact.
fn numbered_lesson(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".html")?;
    let digits = stem.get(..4)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rest = stem.get(4..)?.strip_prefix('-')?;
    if rest.is_empty() || rest.contains('.') {
        return None;
    }
    digits.parse().ok()
}

pub fn format_lesson_filename(number: u64, slug: &str) -> String {
    format!("{number:04}-{slug}.html")
}

pub fn find_lesson_by_slug(lessons_dir: &str, slug: &str) -> Option<String> {
    let mut names: Vec<String> = fs::read_dir(lessons_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    let name = names
        .into_iter()
        .find(|name| is_numbered_slug(name, slug))?;
    Some(node_path::join(lessons_dir, &name))
}

/// `^\d+-<slug>\.html$` anchored on the whole name: `--slug come-nginx` must not
/// resolve `0009-come-nginx-trova-il-backend.html` and rewrite it.
fn is_numbered_slug(name: &str, slug: &str) -> bool {
    let Some(stem) = name.strip_suffix(".html") else {
        return false;
    };
    let Some(digits) = stem
        .strip_suffix(slug)
        .and_then(|rest| rest.strip_suffix('-'))
    else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// First existing template wins, so a data folder's `assets/` overrides the repo's.
pub fn load_template(base_dir: &str, repo_root: &Path) -> Result<String, String> {
    let candidates = [
        resolve_path(&Path::new(base_dir).join(TEMPLATE_REL)),
        resolve_path(Path::new(TEMPLATE_REL)),
        repo_root.join(TEMPLATE_REL),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return read_text(&candidate);
        }
    }
    Err("Could not locate assets/lesson-template.html".to_string())
}

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub topic: String,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub dag: Option<String>,
    pub target_goal: Option<String>,
    pub base_dir: Option<String>,
    pub lessons_dir: Option<String>,
    pub repo_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitResult {
    pub file_path: String,
    pub lesson_number: u64,
    pub slug: String,
}

impl InitResult {
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "filePath".to_string(),
            Value::String(self.file_path.clone()),
        );
        map.insert("lessonNumber".to_string(), Value::from(self.lesson_number));
        map.insert("slug".to_string(), Value::String(self.slug.clone()));
        Value::Object(map)
    }
}

pub fn init_lesson(o: &InitOptions) -> Result<InitResult, String> {
    let base = base_dir(&o.base_dir)?;
    let lessons_dir = match truthy(&o.lessons_dir) {
        Some(dir) => dir.to_string(),
        None => resolve_string(&Path::new(&base).join("lessons")),
    };
    if !Path::new(&lessons_dir).exists() {
        fs::create_dir_all(&lessons_dir).map_err(|e| e.to_string())?;
    }

    let lesson_number = next_lesson_number(&lessons_dir);
    let title = truthy(&o.title)
        .map(str::to_owned)
        .unwrap_or_else(|| o.topic.clone());
    // The slug comes from the TITLE: the plan only knows the title, so slugging the topic
    // would orphan the artifact whenever topic and title differ.
    let slug = match truthy(&o.slug) {
        Some(slug) => slugify(slug),
        None => lesson_slug(&title),
    };
    let file_path = node_path::join(&lessons_dir, &format_lesson_filename(lesson_number, &slug));

    // Slug is the join identity within a course: a twin is invisible on the board, so refuse it.
    if let Some(twin) = find_lesson_by_slug(&lessons_dir, &slug) {
        return Err(format!(
            "A lesson with slug \"{slug}\" already exists in this course: {}. Reuse it, or give this lesson a distinct title.",
            node_path::basename(&twin)
        ));
    }

    let template = load_template(&base, &o.repo_root)?;
    let now = iso_now();
    let date = &now[..10];
    let dag = truthy(&o.dag).unwrap_or("");

    let mut metadata = Map::new();
    metadata.insert("lessonNumber".to_string(), Value::from(lesson_number));
    metadata.insert("topic".to_string(), Value::String(o.topic.clone()));
    metadata.insert("title".to_string(), Value::String(title.clone()));
    metadata.insert("slug".to_string(), Value::String(slug.clone()));
    if let Some(description) = &o.description {
        metadata.insert(
            "description".to_string(),
            Value::String(description.clone()),
        );
    }
    metadata.insert("dateCreated".to_string(), Value::String(now.clone()));
    metadata.insert("dateModified".to_string(), Value::String(now.clone()));
    metadata.insert("initialDag".to_string(), Value::String(dag.to_string()));
    if let Some(goal) = &o.target_goal {
        metadata.insert("targetGoal".to_string(), Value::String(goal.clone()));
    }
    metadata.insert("version".to_string(), Value::String("1.0.0".to_string()));

    let mut doc = Map::new();
    doc.insert("metadata".to_string(), Value::Object(metadata));
    doc.insert("nodes".to_string(), Value::Array(Vec::new()));
    let doc = Value::Object(doc);

    let dag_section = if dag.is_empty() {
        String::new()
    } else {
        format!(
            "<section class=\"diagram-container\" id=\"dagContainer\"><div class=\"diagram-title\">Curriculum Dependency Graph</div><pre class=\"mermaid\">\n{dag}\n</pre></section>"
        )
    };
    let goal = truthy(&o.description)
        .or_else(|| truthy(&o.target_goal))
        .unwrap_or("");

    let html = template
        .replace("<!-- LESSON_TITLE_SLOT -->", &render::escape_html(&title))
        .replace("<!-- APP_NAME_SLOT -->", APP_NAME)
        .replace("<!-- LESSON_GOAL_SLOT -->", &render::escape_html(goal))
        .replace("<!-- LESSON_DATE_SLOT -->", &format!("Generated: {date}"))
        .replace(
            "<!-- LESSON_NUMBER_SLOT -->",
            &format!("{lesson_number:04}"),
        )
        .replace("<!-- DAG_CONTAINER_SLOT -->", &dag_section)
        .replace("<!-- DATA_ISLAND_SLOT -->", &js::render_data_island(&doc));

    fs::write(Path::new(&file_path), html).map_err(|e| e.to_string())?;

    Ok(InitResult {
        file_path,
        lesson_number,
        slug,
    })
}

#[derive(Debug, Clone)]
pub struct AppendOptions {
    pub lesson_path: Option<String>,
    pub slug: Option<String>,
    pub node: Value,
    pub base_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppendResult {
    pub file_path: String,
    pub node_count: usize,
    pub node_id: Option<Value>,
}

impl AppendResult {
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "filePath".to_string(),
            Value::String(self.file_path.clone()),
        );
        map.insert(
            "lessonPath".to_string(),
            Value::String(self.file_path.clone()),
        );
        map.insert(
            "filepath".to_string(),
            Value::String(self.file_path.clone()),
        );
        map.insert("nodeCount".to_string(), Value::from(self.node_count));
        // JSON.stringify drops an undefined nodeId.
        if let Some(node_id) = &self.node_id {
            map.insert("nodeId".to_string(), node_id.clone());
        }
        Value::Object(map)
    }
}

pub fn append_node(o: AppendOptions) -> Result<AppendResult, String> {
    let base = base_dir(&o.base_dir)?;
    let lessons_dir = resolve_string(&Path::new(&base).join("lessons"));

    let mut file_path = truthy(&o.lesson_path).map(str::to_owned);
    if file_path.is_none()
        && let Some(slug) = truthy(&o.slug)
    {
        file_path = find_lesson_by_slug(&lessons_dir, slug);
    }
    let found = file_path
        .as_deref()
        .is_some_and(|path| Path::new(path).exists());
    if !found {
        // `filePath || options.slug`: a present-but-empty slug still shows as nothing.
        let target = file_path.as_deref().or(o.slug.as_deref());
        return Err(format!(
            "Lesson file not found: {}",
            target.unwrap_or("undefined")
        ));
    }
    let file_path = file_path.expect("checked above");

    let raw = read_text(Path::new(&file_path))?;
    let mut doc = parse_lesson_document(&raw)?.ok_or_else(|| {
        format!("Corrupted lesson file: missing #lesson-data island in {file_path}")
    })?;
    if !doc.get("metadata").is_some_and(Value::is_object)
        || !doc.get("nodes").is_some_and(Value::is_array)
    {
        return Err("Malformed #lesson-data island: expected metadata and nodes".to_string());
    }
    if !o.node.is_object() {
        return Err("Invalid node payload: expected a JSON object".to_string());
    }
    let mut node = o.node;

    node["capturedAt"] = Value::String(iso_now());
    let content_needs_fill = {
        let content = node.get("content");
        !js::js_truthy(content)
            || content.is_some_and(|c| c.as_str().is_some_and(|s| js::js_trim(s).is_empty()))
    };
    if content_needs_fill {
        let backfill = node
            .get("steps")
            .and_then(Value::as_array)
            .filter(|steps| !steps.is_empty())
            .map(|steps| {
                steps
                    .iter()
                    .map(|step| js::js_join_item(step.get("content")))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            });
        if let Some(content) = backfill {
            node["content"] = Value::String(content);
        }
    }

    let existing = doc["nodes"]
        .as_array()
        .expect("validated nodes")
        .iter()
        .position(|n| strict_eq(n.get("nodeId"), node.get("nodeId")));
    let nodes = doc["nodes"].as_array_mut().expect("validated nodes");
    let index = match existing {
        Some(index) => {
            nodes[index] = node;
            index
        }
        None => {
            nodes.push(node);
            nodes.len() - 1
        }
    };

    doc["metadata"]["dateModified"] = Value::String(iso_now());

    let node = &doc["nodes"][index];
    let node_html = render::render_node_html(node, index)?;
    let node_id = node.get("nodeId").cloned();

    let mut updated_html = raw;
    let mut replaced = false;
    if existing.is_some() {
        // The markup holds the id escaped; `n1(v2)` must not be interpolated raw.
        let needle = format!(
            "<article class=\"cognitive-node\" id=\"node-{}\"",
            render::escape_html(js::text(node_id.as_ref()))
        );
        if let Some(start) = updated_html.find(&needle)
            && let Some(offset) = updated_html[start + needle.len()..].find("</article>")
        {
            let end = start + needle.len() + offset + "</article>".len();
            updated_html.replace_range(start..end, js::js_trim(&node_html));
            replaced = true;
        }
    }
    if !replaced {
        updated_html = updated_html.replacen(
            INJECTION_POINT,
            &format!("{}\n      {INJECTION_POINT}", js::js_trim(&node_html)),
            1,
        );
    }

    let updated_html = replace_island(&updated_html, &js::render_data_island(&doc));
    fs::write(Path::new(&file_path), updated_html).map_err(|e| e.to_string())?;

    Ok(AppendResult {
        file_path,
        node_count: doc["nodes"].as_array().expect("validated nodes").len(),
        node_id,
    })
}

#[derive(Debug, Clone)]
pub struct RebuildOptions {
    pub lesson_path: Option<String>,
    pub slug: Option<String>,
    pub base_dir: Option<String>,
    pub lessons_dir: Option<String>,
    pub repo_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildResult {
    pub file_path: String,
    pub node_count: usize,
}

impl RebuildResult {
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "filePath".to_string(),
            Value::String(self.file_path.clone()),
        );
        map.insert("nodeCount".to_string(), Value::from(self.node_count));
        Value::Object(map)
    }
}

/// Port of `renderFullLesson`: the template filled from the island, nodes included.
pub fn render_full_lesson(doc: &Value, template: &str) -> Result<String, String> {
    let member = |key: &str| doc.get("metadata").and_then(|metadata| metadata.get(key));
    let title = or_truthy(member("title"), member("topic"));
    let goal = or_truthy(member("description"), member("targetGoal"));
    let date = match member("dateCreated") {
        Some(value) if js::js_truthy(Some(value)) => js::js_to_string(value),
        _ => iso_now(),
    };
    let date: String = date.chars().take(10).collect();
    let dag = match member("initialDag") {
        Some(value) if js::js_truthy(Some(value)) => js::js_to_string(value),
        _ => String::new(),
    };

    let dag_section = if dag.is_empty() {
        String::new()
    } else {
        format!(
            "<section class=\"diagram-container\" id=\"dagContainer\">\n      <div class=\"diagram-title\">Curriculum Dependency Graph</div>\n      <pre class=\"mermaid\">\n{dag}\n</pre>\n    </section>"
        )
    };

    let nodes = doc
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("Malformed #lesson-data island: expected metadata and nodes")?;
    let nodes_html = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| render::render_node_html(node, index))
        .collect::<Result<Vec<_>, String>>()?
        .join("\n      ");

    let lesson_number = js::js_concat(member("lessonNumber"));
    let pad = 4usize.saturating_sub(lesson_number.chars().count());
    let lesson_number = format!("{}{lesson_number}", "0".repeat(pad));

    Ok(template
        .replace("<!-- LESSON_TITLE_SLOT -->", &esc(title))
        .replace("<!-- APP_NAME_SLOT -->", APP_NAME)
        .replace("<!-- LESSON_GOAL_SLOT -->", &esc(goal))
        .replace("<!-- LESSON_DATE_SLOT -->", &format!("Generated: {date}"))
        .replace("<!-- LESSON_NUMBER_SLOT -->", &lesson_number)
        .replace("<!-- DAG_CONTAINER_SLOT -->", &dag_section)
        .replace(
            INJECTION_POINT,
            &format!("{nodes_html}\n      {INJECTION_POINT}"),
        )
        .replace("<!-- DATA_ISLAND_SLOT -->", &js::render_data_island(doc)))
}

/// Port of `rebuildLesson`: re-renders an artifact from its island, in place.
pub fn rebuild_lesson(o: &RebuildOptions) -> Result<RebuildResult, String> {
    let base = base_dir(&o.base_dir)?;
    let lessons_dir = match truthy(&o.lessons_dir) {
        Some(dir) => dir.to_string(),
        None => resolve_string(&Path::new(&base).join("lessons")),
    };

    let mut file_path = truthy(&o.lesson_path).map(str::to_owned);
    if file_path.is_none()
        && let Some(slug) = truthy(&o.slug)
    {
        file_path = find_lesson_by_slug(&lessons_dir, slug);
    }
    let found = file_path
        .as_deref()
        .is_some_and(|path| Path::new(path).exists());
    if !found {
        // `filePath || options.slug`, same as append: an empty --slug prints as nothing.
        let target = file_path.as_deref().or(o.slug.as_deref());
        return Err(format!(
            "Lesson not found: {}",
            target.unwrap_or("undefined")
        ));
    }
    let file_path = file_path.expect("checked above");

    let raw = read_text(Path::new(&file_path))?;
    let doc = parse_lesson_document(&raw)?
        .filter(|doc| js::js_truthy(Some(doc)))
        .ok_or_else(|| {
            format!("Corrupted lesson file: missing #lesson-data island in {file_path}")
        })?;
    if !doc.get("metadata").is_some_and(Value::is_object)
        || !doc.get("nodes").is_some_and(Value::is_array)
    {
        return Err("Malformed #lesson-data island: expected metadata and nodes".to_string());
    }

    let template = load_template(&base, &o.repo_root)?;
    let html = render_full_lesson(&doc, &template)?;
    // The island is re-rendered as parsed: Node does not touch dateModified here.
    fs::write(Path::new(&file_path), html).map_err(|e| e.to_string())?;

    Ok(RebuildResult {
        file_path,
        node_count: doc["nodes"].as_array().expect("validated nodes").len(),
    })
}

/// JS `a || b`.
fn or_truthy<'a>(a: Option<&'a Value>, b: Option<&'a Value>) -> Option<&'a Value> {
    if js::js_truthy(a) { a } else { b }
}

/// Node's `escapeHtml`: falsy values render as nothing.
fn esc(v: Option<&Value>) -> String {
    match v {
        Some(value) if js::js_truthy(Some(value)) => render::escape_html(&js::js_to_string(value)),
        _ => String::new(),
    }
}

/// Node replaces the first island in the page; without one the HTML stays as it is.
fn replace_island(html: &str, island: &str) -> String {
    let Some(open) = html.find(ISLAND_OPEN) else {
        return html.to_string();
    };
    let inner_start = open + ISLAND_OPEN.len();
    let Some(offset) = html[inner_start..].find(SCRIPT_CLOSE) else {
        return html.to_string();
    };
    let end = inner_start + offset + SCRIPT_CLOSE.len();
    let mut out = String::with_capacity(html.len() - (end - open) + island.len());
    out.push_str(&html[..open]);
    out.push_str(island);
    out.push_str(&html[end..]);
    out
}

/// JS `===` on two JSON members: missing and null are distinct, numbers compare as doubles.
fn strict_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Value::Null), Some(Value::Null)) => true,
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => a == b,
        (Some(Value::Number(a)), Some(Value::Number(b))) => a.as_f64() == b.as_f64(),
        (Some(Value::String(a)), Some(Value::String(b))) => a == b,
        _ => false,
    }
}

/// JS truthiness for the CLI's optional strings: present and non-empty.
pub(crate) fn truthy(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|text| !text.is_empty())
}

pub(crate) fn base_dir(value: &Option<String>) -> Result<String, String> {
    match truthy(value) {
        Some(dir) => Ok(dir.to_string()),
        None => Ok(as_string(&env::current_dir().map_err(|e| e.to_string())?)),
    }
}

pub(crate) fn resolve_string(path: &Path) -> String {
    as_string(&resolve_path(path))
}

fn as_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn read_text(path: &Path) -> Result<String, String> {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
        let path = env::temp_dir().join(format!(
            "proxi-lesson-doc-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn repo_root() -> PathBuf {
        resolve_path(&Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."))
    }

    fn init_options(dir: &Path, topic: &str, title: &str) -> InitOptions {
        InitOptions {
            topic: topic.to_string(),
            title: Some(title.to_string()),
            slug: None,
            description: None,
            dag: None,
            target_goal: None,
            base_dir: Some(as_string(dir)),
            lessons_dir: None,
            repo_root: repo_root(),
        }
    }

    fn append_options(dir: &Path, lesson_path: &str, node: Value) -> AppendOptions {
        AppendOptions {
            lesson_path: Some(lesson_path.to_string()),
            slug: None,
            node,
            base_dir: Some(as_string(dir)),
        }
    }

    fn rebuild_options(dir: &Path, lesson_path: Option<&str>) -> RebuildOptions {
        RebuildOptions {
            lesson_path: lesson_path.map(str::to_owned),
            slug: None,
            base_dir: Some(as_string(dir)),
            lessons_dir: None,
            repo_root: repo_root(),
        }
    }

    #[test]
    fn init_writes_numbered_lesson_with_filled_slots() {
        let tmp = temp_dir("init");
        let options = InitOptions {
            topic: "Calculus: Derivatives & Rates of Change".to_string(),
            title: Some("Understanding Derivatives from First Principles".to_string()),
            slug: None,
            description: Some(
                "Motivated discovery of rates of change, tangents, and the power rule."
                    .to_string(),
            ),
            dag: Some(
                "flowchart TD\n  A[\"Instantaneous Rate\"] --> B[\"Difference Quotient\"]\n  B --> C[\"Power Rule\"]"
                    .to_string(),
            ),
            target_goal: None,
            base_dir: Some(as_string(&tmp.0)),
            lessons_dir: None,
            repo_root: repo_root(),
        };

        let result = init_lesson(&options).expect("init");

        assert_eq!(result.lesson_number, 1);
        assert_eq!(
            result.slug,
            "understanding-derivatives-from-first-principles"
        );
        let expected = tmp
            .0
            .join("lessons")
            .join("0001-understanding-derivatives-from-first-principles.html");
        assert_eq!(Path::new(&result.file_path), expected.as_path());
        assert!(expected.exists());

        let html = fs::read_to_string(&expected).expect("read lesson");
        assert!(
            html.contains("<title>Understanding Derivatives from First Principles | Proxi</title>")
        );
        assert!(html.contains("content=\"Personalized standalone lesson synthesized by Proxi\""));
        assert!(!html.contains("APP_NAME_SLOT"));
        assert!(html.to_lowercase().contains("<!doctype html>"));
        assert!(html.contains("https://cdn.jsdelivr.net/npm/katex"));
        assert!(html.contains("renderMathInElement"));
        assert!(html.contains("https://cdn.jsdelivr.net/npm/mermaid"));
        assert!(html.contains("<pre class=\"mermaid\">"));
        assert!(html.contains("flowchart TD"));
        assert!(html.contains("Instantaneous Rate"));
        assert!(html.contains("data-theme"));
        assert!(html.contains("--bg"));
        assert!(html.contains("--fg"));
        assert!(html.contains("id=\"lesson-nodes\""));

        let doc = parse_lesson_document(&html)
            .expect("island parses")
            .expect("island exists");
        let metadata = doc["metadata"].as_object().expect("metadata object");
        let keys: Vec<&str> = metadata.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "lessonNumber",
                "topic",
                "title",
                "slug",
                "description",
                "dateCreated",
                "dateModified",
                "initialDag",
                "version"
            ]
        );
        assert!(!metadata.contains_key("targetGoal"));
        let date_created = metadata["dateCreated"].as_str().expect("dateCreated");
        assert!(html.contains(&format!("Generated: {}", &date_created[..10])));
    }

    #[test]
    fn init_numbers_sequentially_after_existing_files() {
        let tmp = temp_dir("numbering");
        let lessons = tmp.0.join("lessons");
        fs::create_dir_all(&lessons).expect("lessons dir");
        fs::write(lessons.join("0001-linear-algebra.html"), "<html></html>").expect("seed");

        let mut first = init_options(&tmp.0, "Calculus", "Calculus Part 1");
        first.dag = Some("flowchart TD\n  A --> B".to_string());
        let first = init_lesson(&first).expect("first init");
        assert_eq!(first.lesson_number, 2);
        assert_eq!(
            node_path::basename(&first.file_path),
            "0002-calculus-part-1.html"
        );

        let mut second = init_options(&tmp.0, "Information Theory", "Entropy & Compression");
        second.dag = Some("flowchart TD\n  A --> B".to_string());
        let second = init_lesson(&second).expect("second init");
        assert_eq!(second.lesson_number, 3);
        assert_eq!(
            node_path::basename(&second.file_path),
            "0003-entropy-and-compression.html"
        );

        let odd = temp_dir("numbering-ignored");
        let odd_lessons = odd.0.join("lessons");
        fs::create_dir_all(&odd_lessons).expect("odd lessons dir");
        for name in ["12345-x.html", "0009-a.b.html", "0009-.html", "notes.txt"] {
            fs::write(odd_lessons.join(name), "<html></html>").expect("seed odd file");
        }
        assert_eq!(next_lesson_number(&as_string(&odd_lessons)), 1);
    }

    #[test]
    fn init_refuses_duplicate_slug() {
        let tmp = temp_dir("duplicate");
        init_lesson(&init_options(&tmp.0, "Docker", "Compose e consegna")).expect("first lesson");

        let err = init_lesson(&init_options(&tmp.0, "Docker", "Compose e consegna"))
            .expect_err("duplicate slug");
        assert_eq!(
            err,
            "A lesson with slug \"compose-e-consegna\" already exists in this course: 0001-compose-e-consegna.html. Reuse it, or give this lesson a distinct title."
        );
    }

    #[test]
    fn find_by_slug_is_anchored() {
        let tmp = temp_dir("find-slug");
        let long = init_lesson(&init_options(
            &tmp.0,
            "Docker",
            "Come nginx trova il backend",
        ))
        .expect("lesson");
        let lessons_dir = Path::new(&long.file_path)
            .parent()
            .expect("parent")
            .to_string_lossy()
            .into_owned();

        assert_eq!(find_lesson_by_slug(&lessons_dir, "come-nginx"), None);
        assert_eq!(
            find_lesson_by_slug(&lessons_dir, "come-nginx-trova-il-backend"),
            Some(long.file_path.clone())
        );

        let one = temp_dir("find-one");
        let lessons = one.0.join("lessons");
        fs::create_dir_all(&lessons).expect("lessons dir");
        fs::write(lessons.join("0001-1-a.html"), "<html></html>").expect("seed");
        assert_eq!(
            find_lesson_by_slug(&as_string(&lessons), "1-a"),
            Some(node_path::join(&as_string(&lessons), "0001-1-a.html"))
        );
    }

    #[test]
    fn init_empty_description_and_goal_keep_keys() {
        let tmp = temp_dir("empty-description");
        let mut options = init_options(&tmp.0, "Topic", "Titolo");
        options.description = Some(String::new());
        options.target_goal = Some("g".to_string());

        let result = init_lesson(&options).expect("init");
        let html = fs::read_to_string(&result.file_path).expect("read lesson");
        let doc = parse_lesson_document(&html)
            .expect("island parses")
            .expect("island exists");
        assert_eq!(doc["metadata"]["description"], json!(""));
        assert_eq!(doc["metadata"]["targetGoal"], json!("g"));
        assert!(html.contains(
            "<p id=\"lessonGoal\" style=\"font-size: 1.15rem; color: var(--text-secondary);\">g</p>"
        ));
    }

    #[test]
    fn data_folder_template_wins() {
        let tmp = temp_dir("template");
        let assets = tmp.0.join("assets");
        fs::create_dir_all(&assets).expect("assets dir");
        fs::write(
            assets.join("lesson-template.html"),
            "T:<!-- LESSON_TITLE_SLOT -->|<!-- DATA_ISLAND_SLOT -->",
        )
        .expect("override template");

        let result = init_lesson(&init_options(&tmp.0, "Topic", "Titolo")).expect("init");
        let html = fs::read_to_string(&result.file_path).expect("read lesson");
        assert!(html.starts_with("T:Titolo|<script"), "got {html}");
    }

    #[test]
    fn island_survives_closing_script_tag() {
        let tmp = temp_dir("island");
        let init = init_lesson(&init_options(&tmp.0, "Script Tags", "Script Tags")).expect("init");

        append_node(append_options(
            &tmp.0,
            &init.file_path,
            json!({
                "nodeId": "closing-tag",
                "title": "A node with </script> in its title",
                "tier": "derivation",
                "content": "Inline JS ends at </script>, which would terminate the island unescaped."
            }),
        ))
        .expect("append");

        let raw = fs::read_to_string(&init.file_path).expect("read lesson");
        let doc = parse_lesson_document(&raw)
            .expect("island parses")
            .expect("island exists");
        let nodes = doc["nodes"].as_array().expect("nodes");
        assert_eq!(nodes.len(), 1);
        assert!(
            nodes[0]["content"]
                .as_str()
                .expect("content")
                .contains("</script>")
        );
        assert!(
            nodes[0]["title"]
                .as_str()
                .expect("title")
                .contains("</script>")
        );
        assert!(raw.contains("\\u003c/script>"));
    }

    #[test]
    fn append_adds_nodes_and_keeps_previous() {
        let tmp = temp_dir("append");
        let mut options = init_options(&tmp.0, "Derivatives", "Calculus Derivatives");
        options.dag = Some("flowchart TD\n  A --> B".to_string());
        let init = init_lesson(&options).expect("init");

        let node1 = json!({
            "nodeId": "rate-of-change-definition",
            "title": "Average Rate of Change",
            "tier": "unconditional-truth",
            "motivation": "Understanding baseline rates",
            "content": "Average speed over any interval is strictly total distance divided by elapsed time.",
            "math": ["\\bar{v} = \\frac{\\Delta x}{\\Delta t}"],
            "quiz": {
                "question": "What is the average velocity over interval $\\Delta t$?",
                "options": ["1. $\\frac{\\Delta x}{\\Delta t}$", "2. $\\Delta x \\cdot \\Delta t$", "3. $\\Delta t$"],
                "correctIndex": 1,
                "explanation": "By definition, average rate of change is the ratio of output delta to input delta."
            }
        });
        assert_eq!(
            append_node(append_options(&tmp.0, &init.file_path, node1))
                .expect("append 1")
                .node_count,
            1
        );

        let html = fs::read_to_string(&init.file_path).expect("read lesson");
        assert!(html.contains("Average Rate of Change"));
        assert!(html.contains("Unconditional Truth") || html.contains("unconditional-truth"));
        assert!(html.contains("\\frac{\\Delta x}{\\Delta t}"));
        assert!(html.contains("Average speed over any interval"));

        let node2 = json!({
            "nodeId": "instantaneous-velocity-limit",
            "title": "Instantaneous Velocity via Limit",
            "tier": "derivation",
            "motivation": "Going from macroscopic to infinitesimal",
            "content": "As the time window shrinks toward zero, the secant line approaches the tangent line.",
            "math": ["v(t) = \\lim_{\\Delta t \\to 0} \\frac{x(t + \\Delta t) - x(t)}{\\Delta t}"],
            "visuals": [{ "type": "svg-inline", "content": "<svg viewBox=\"0 0 100 100\"><circle cx=\"50\" cy=\"50\" r=\"40\" fill=\"none\" stroke=\"currentColor\"/></svg>" }],
            "quiz": {
                "question": "What happens as $\\Delta t \\to 0$?",
                "options": [
                    "1. The secant slope converges to the instantaneous tangent slope.",
                    "2. The slope becomes infinite unconditionally.",
                    "3. The secant line becomes undefined."
                ],
                "correctIndex": 1,
                "explanation": "The difference quotient limit represents the derivative at that exact point."
            }
        });
        assert_eq!(
            append_node(append_options(&tmp.0, &init.file_path, node2))
                .expect("append 2")
                .node_count,
            2
        );

        let html = fs::read_to_string(&init.file_path).expect("read lesson");
        assert!(html.contains("rate-of-change-definition"));
        assert!(html.contains("Average Rate of Change"));
        assert!(html.contains("instantaneous-velocity-limit"));
        assert!(html.contains("Instantaneous Velocity via Limit"));
        assert!(html.contains("Derivation"));
        assert!(html.contains("<svg viewBox=\"0 0 100 100\">"));
        assert!(html.contains("<details"));
        assert!(html.contains("The secant slope converges"));

        let second = html
            .find("<article class=\"cognitive-node\" id=\"node-instantaneous-velocity-limit\"")
            .expect("second article");
        let marker = html.find(INJECTION_POINT).expect("injection point");
        assert!(second < marker);
    }

    #[test]
    fn append_preserves_latex_and_mermaid() {
        let tmp = temp_dir("latex");
        let mut options = init_options(
            &tmp.0,
            "Advanced Calculus",
            "Integration by Parts & Multivariable Limits",
        );
        options.dag = Some(
            "flowchart TD\n  A[\"$$\\int u dv$$\"] --> B[\"$$\\iint_R f(x,y) dA$$\"]".to_string(),
        );
        let init = init_lesson(&options).expect("init");

        let node = json!({
            "nodeId": "complex-math-node",
            "title": "Integral Definition & Multi-line Proof",
            "tier": "derivation",
            "motivation": "Formalizing multi-dimensional calculus",
            "content": "Consider the multi-line transformation:\n\n$$\\begin{aligned} \\int u \\, dv &= u v - \\int v \\, du \\\\ \\sum_{k=1}^n k^2 &= \\frac{n(n+1)(2n+1)}{6} \\end{aligned}$$\n\nNotice how $\\lim_{x \\to 0} \\frac{\\sin x}{x} = 1$.",
            "math": [
                "\\int_{-\\infty}^{\\infty} e^{-x^2} dx = \\sqrt{\\pi}",
                "\\frac{\\partial^2 f}{\\partial x^2} + \\frac{\\partial^2 f}{\\partial y^2} = 0"
            ]
        });
        append_node(append_options(&tmp.0, &init.file_path, node)).expect("append");

        let html = fs::read_to_string(&init.file_path).expect("read lesson");
        assert!(html.contains("\\begin{aligned}"));
        assert!(html.contains("\\int u"));
        assert!(html.contains("\\frac{n(n+1)(2n+1)}{6}"));
        assert!(html.contains("\\lim_{x"));
        assert!(html.contains("\\sqrt{\\pi}"));
        assert!(html.contains("\\partial^2 f"));
        assert!(html.contains("$$\\begin{aligned}"));
        assert!(html.contains("$\\lim_{x"));
        assert!(html.contains("flowchart TD"));
        assert!(html.contains("$$\\int u dv$$"));
    }

    #[test]
    fn append_replaces_existing_node_in_place() {
        let tmp = temp_dir("replace");
        let init = init_lesson(&init_options(&tmp.0, "T", "Titolo")).expect("init");

        append_node(append_options(
            &tmp.0,
            &init.file_path,
            json!({"nodeId": "n1", "title": "Primo", "tier": "axiom", "content": "uno"}),
        ))
        .expect("append n1");
        append_node(append_options(
            &tmp.0,
            &init.file_path,
            json!({"nodeId": "n2", "title": "Secondo", "tier": "axiom", "content": "due"}),
        ))
        .expect("append n2");
        let result = append_node(append_options(
            &tmp.0,
            &init.file_path,
            json!({"nodeId": "n1", "title": "Primo rivisto", "tier": "axiom", "content": "uno"}),
        ))
        .expect("replace n1");

        assert_eq!(result.node_count, 2);
        let html = fs::read_to_string(&init.file_path).expect("read lesson");
        assert_eq!(html.matches("id=\"node-n1\"").count(), 1);
        assert!(html.contains("Primo rivisto"));
        let first = html.find("id=\"node-n1\"").expect("n1");
        let second = html.find("id=\"node-n2\"").expect("n2");
        assert!(first < second);
        assert!(html.contains("Node 01"));

        let doc = parse_lesson_document(&html)
            .expect("island parses")
            .expect("island exists");
        assert_eq!(doc["nodes"][0]["title"], json!("Primo rivisto"));
    }

    #[test]
    fn append_overwrites_captured_at_in_place_and_backfills_content() {
        let tmp = temp_dir("captured-at");
        let init = init_lesson(&init_options(&tmp.0, "T", "Titolo")).expect("init");
        let node: Value = serde_json::from_str(
            r#"{"nodeId":"s","capturedAt":"old","content":"  ","steps":[{"content":"A"},{"x":1},{"content":"B"}],"extra":{"z":1.0}}"#,
        )
        .expect("payload");
        append_node(append_options(&tmp.0, &init.file_path, node)).expect("append");

        let raw = fs::read_to_string(&init.file_path).expect("read lesson");
        let doc = parse_lesson_document(&raw)
            .expect("island parses")
            .expect("island exists");
        let node = doc["nodes"][0].as_object().expect("node object");
        let keys: Vec<&str> = node.keys().map(String::as_str).collect();
        assert_eq!(keys, ["nodeId", "capturedAt", "content", "steps", "extra"]);
        let captured_at = node["capturedAt"].as_str().expect("capturedAt");
        assert_eq!(captured_at.len(), 24);
        assert!(captured_at.ends_with('Z'));
        assert_eq!(node["content"], json!("A\n\n\n\nB"));
        let start = raw.find(ISLAND_OPEN).expect("island open") + ISLAND_OPEN.len();
        let end = start + raw[start..].find(SCRIPT_CLOSE).expect("island close");
        let island = &raw[start..end];
        assert!(island.contains("\"z\": 1\n"), "got island {island}");
        assert!(!island.contains("\"z\": 1.0"));
    }

    #[test]
    fn append_errors_match_node() {
        let tmp = temp_dir("append-errors");
        let base = Some(as_string(&tmp.0));

        let missing = tmp.0.join("lessons").join("9999-nope.html");
        let missing_path = as_string(&missing);
        let err = append_node(append_options(&tmp.0, &missing_path, json!({})))
            .expect_err("missing lesson");
        assert_eq!(err, format!("Lesson file not found: {missing_path}"));

        let err = append_node(AppendOptions {
            lesson_path: None,
            slug: None,
            node: json!({}),
            base_dir: base.clone(),
        })
        .expect_err("no target");
        assert_eq!(err, "Lesson file not found: undefined");

        let err = append_node(AppendOptions {
            lesson_path: None,
            slug: Some("nope".to_string()),
            node: json!({}),
            base_dir: base.clone(),
        })
        .expect_err("unknown slug");
        assert_eq!(err, "Lesson file not found: nope");

        let handmade = tmp.0.join("handmade.html");
        fs::write(&handmade, "<html></html>").expect("handmade lesson");
        let handmade_path = as_string(&handmade);
        let err =
            append_node(append_options(&tmp.0, &handmade_path, json!({}))).expect_err("no island");
        assert_eq!(
            err,
            format!("Corrupted lesson file: missing #lesson-data island in {handmade_path}")
        );

        let broken = tmp.0.join("broken.html");
        fs::write(&broken, format!("{ISLAND_OPEN}\n{{\n{SCRIPT_CLOSE}")).expect("broken island");
        let err = append_node(append_options(&tmp.0, &as_string(&broken), json!({})))
            .expect_err("malformed island");
        assert!(
            err.starts_with("Malformed #lesson-data island: "),
            "got {err}"
        );

        let wrong_shape = tmp.0.join("wrong-shape.html");
        fs::write(
            &wrong_shape,
            format!("{ISLAND_OPEN}\n{{\"metadata\": 1, \"nodes\": []}}\n{SCRIPT_CLOSE}"),
        )
        .expect("wrong shape island");
        let err = append_node(append_options(&tmp.0, &as_string(&wrong_shape), json!({})))
            .expect_err("wrong shape");
        assert_eq!(
            err,
            "Malformed #lesson-data island: expected metadata and nodes"
        );

        let init = init_lesson(&init_options(&tmp.0, "Valid", "Valida")).expect("init");
        let before = fs::read(&init.file_path).expect("read before");
        for payload in ["5", "\"s\"", "[]", "null"] {
            let node: Value = serde_json::from_str(payload).expect("payload");
            let err = append_node(append_options(&tmp.0, &init.file_path, node))
                .expect_err("non-object payload");
            assert_eq!(err, "Invalid node payload: expected a JSON object");
        }
        assert_eq!(fs::read(&init.file_path).expect("read after"), before);
    }

    #[test]
    fn rebuild_keeps_proxi_title_and_fills_app_name_slot() {
        let tmp = temp_dir("rebuild-title");
        let init = init_lesson(&init_options(&tmp.0, "Reti", "Il modello OSI")).expect("init");

        let result =
            rebuild_lesson(&rebuild_options(&tmp.0, Some(&init.file_path))).expect("rebuild");
        assert_eq!(result.file_path, init.file_path);
        assert_eq!(result.node_count, 0);

        let html = fs::read_to_string(&init.file_path).expect("read lesson");
        assert!(
            html.contains("<title>Il modello OSI | Proxi</title>"),
            "got {html}"
        );
        assert!(!html.contains("APP_NAME_SLOT"));
    }

    #[test]
    fn render_full_lesson_adds_marker_newline_without_nodes() {
        let doc = json!({"metadata": {}, "nodes": []});
        let html = render_full_lesson(&doc, "A<!-- NODES_INJECTION_POINT -->B").expect("render");
        assert_eq!(html, "A\n      <!-- NODES_INJECTION_POINT -->B");
    }

    #[test]
    fn render_full_lesson_fills_slots_and_multiline_dag() {
        let template = "<!-- LESSON_TITLE_SLOT -->|<!-- APP_NAME_SLOT -->|<!-- LESSON_GOAL_SLOT -->|<!-- LESSON_DATE_SLOT -->|<!-- LESSON_NUMBER_SLOT -->|<!-- DAG_CONTAINER_SLOT -->|<!-- NODES_INJECTION_POINT -->|<!-- DATA_ISLAND_SLOT -->";
        let doc = json!({
            "metadata": {
                "lessonNumber": 7,
                "title": "",
                "topic": "T <x>",
                "description": "",
                "targetGoal": "G",
                "dateCreated": "2026-01-02T03:04:05.000Z",
                "initialDag": "flowchart TD"
            },
            "nodes": []
        });

        let html = render_full_lesson(&doc, template).expect("render");

        assert!(html.contains("T &lt;x&gt;"), "got {html}");
        assert!(html.contains("|Proxi|"), "got {html}");
        assert!(html.contains("|G|"), "got {html}");
        assert!(html.contains("Generated: 2026-01-02"), "got {html}");
        assert!(html.contains("|0007|"), "got {html}");
        let dag_section = "<section class=\"diagram-container\" id=\"dagContainer\">\n      <div class=\"diagram-title\">Curriculum Dependency Graph</div>\n      <pre class=\"mermaid\">\nflowchart TD\n</pre>\n    </section>";
        assert!(html.contains(dag_section), "got {html}");
        assert!(
            html.contains("<script type=\"application/json\" id=\"lesson-data\">"),
            "got {html}"
        );
    }

    #[test]
    fn rebuild_rerenders_appended_nodes_from_the_island() {
        let tmp = temp_dir("rebuild-nodes");
        let init = init_lesson(&init_options(&tmp.0, "Calcolo", "Derivate")).expect("init");
        append_node(append_options(
            &tmp.0,
            &init.file_path,
            json!({"nodeId": "n1", "title": "Primo", "tier": "axiom", "content": "uno"}),
        ))
        .expect("append 1");
        append_node(append_options(
            &tmp.0,
            &init.file_path,
            json!({"nodeId": "n2", "title": "Secondo", "tier": "derivation", "content": "due"}),
        ))
        .expect("append 2");
        let before = fs::read_to_string(&init.file_path).expect("read before");

        let result =
            rebuild_lesson(&rebuild_options(&tmp.0, Some(&init.file_path))).expect("rebuild");
        assert_eq!(result.node_count, 2);

        let html = fs::read_to_string(&init.file_path).expect("read after");
        let first = html
            .find("<article class=\"cognitive-node\" id=\"node-n1\"")
            .expect("n1 article");
        let second = html
            .find("<article class=\"cognitive-node\" id=\"node-n2\"")
            .expect("n2 article");
        let marker = html.find(INJECTION_POINT).expect("injection point");
        assert!(first < second && second < marker);
        assert_eq!(
            parse_lesson_document(&html).expect("island parses"),
            parse_lesson_document(&before).expect("island parses")
        );

        rebuild_lesson(&rebuild_options(&tmp.0, Some(&init.file_path))).expect("rebuild again");
        assert_eq!(
            fs::read_to_string(&init.file_path).expect("read twice"),
            html
        );
    }

    #[test]
    fn rebuild_errors_match_node() {
        let tmp = temp_dir("rebuild-errors");
        fs::create_dir_all(tmp.0.join("lessons")).expect("lessons dir");

        let err = rebuild_lesson(&rebuild_options(&tmp.0, None)).expect_err("no target");
        assert_eq!(err, "Lesson not found: undefined");

        let err = rebuild_lesson(&RebuildOptions {
            slug: Some("nope".to_string()),
            ..rebuild_options(&tmp.0, None)
        })
        .expect_err("unknown slug");
        assert_eq!(err, "Lesson not found: nope");

        let missing = tmp.0.join("lessons").join("9999-nope.html");
        let missing_path = as_string(&missing);
        let err = rebuild_lesson(&rebuild_options(&tmp.0, Some(&missing_path)))
            .expect_err("missing lesson");
        assert_eq!(err, format!("Lesson not found: {missing_path}"));

        let handmade = tmp.0.join("handmade.html");
        fs::write(&handmade, "<html></html>").expect("handmade lesson");
        let handmade_path = as_string(&handmade);
        let err =
            rebuild_lesson(&rebuild_options(&tmp.0, Some(&handmade_path))).expect_err("no island");
        assert_eq!(
            err,
            format!("Corrupted lesson file: missing #lesson-data island in {handmade_path}")
        );

        let null_island = tmp.0.join("null-island.html");
        fs::write(&null_island, format!("{ISLAND_OPEN}\nnull\n{SCRIPT_CLOSE}"))
            .expect("null island");
        let null_path = as_string(&null_island);
        let err =
            rebuild_lesson(&rebuild_options(&tmp.0, Some(&null_path))).expect_err("falsy island");
        assert_eq!(
            err,
            format!("Corrupted lesson file: missing #lesson-data island in {null_path}")
        );

        let broken = tmp.0.join("broken.html");
        fs::write(&broken, format!("{ISLAND_OPEN}\n{{\n{SCRIPT_CLOSE}")).expect("broken island");
        let before = fs::read(&broken).expect("read before");
        let err = rebuild_lesson(&rebuild_options(&tmp.0, Some(&as_string(&broken))))
            .expect_err("malformed island");
        assert!(
            err.starts_with("Malformed #lesson-data island: "),
            "got {err}"
        );
        assert_eq!(fs::read(&broken).expect("read after"), before);
    }
}
