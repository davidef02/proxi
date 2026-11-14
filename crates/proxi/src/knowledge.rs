//! Knowledge map over the lessons on disk: statuses, attempts and dependency edges.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::js;
use crate::lesson;

const PROGRESS_KINDS: [&str; 4] = ["probe", "predict", "check", "review"];
const SHAKY_LAPSES: f64 = 3.0;
const MATURE_DAYS: f64 = 21.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Taught,
    Solid,
    Shaky,
    Missed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Taught => "taught",
            Status::Solid => "solid",
            Status::Shaky => "shaky",
            Status::Missed => "missed",
        }
    }

    /// Worst-status order used by the graph store: missed beats shaky beats solid.
    pub fn rank(self) -> u8 {
        match self {
            Status::Taught => 0,
            Status::Solid => 1,
            Status::Shaky => 2,
            Status::Missed => 3,
        }
    }
}

/// The graded or reviewed lines of a course's ledger; anything else is not progress.
pub fn read_progress(data_root: &Path, course: &str) -> Vec<Value> {
    let file = data_root
        .join("courses")
        .join(course)
        .join("progress.jsonl");
    let Ok(raw) = fs::read(&file) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&raw);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let Ok(rec) = js::parse(line) else {
            continue;
        };
        let Some(obj) = rec.as_object() else {
            continue;
        };
        let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("");
        if !PROGRESS_KINDS.contains(&kind) || !obj.get("nodeRefs").is_some_and(Value::is_array) {
            continue;
        }
        out.push(rec);
    }
    out
}

/// Anki's verdict on the latest review of every card: shaky while a card is floored.
pub fn anki_status(reviews: &[&Value]) -> Status {
    if shaky_cards(reviews).is_empty() {
        Status::Solid
    } else {
        Status::Shaky
    }
}

fn shaky_cards<'a>(reviews: &[&'a Value]) -> Vec<&'a Value> {
    let mut groups: Vec<(Value, Vec<&'a Value>)> = Vec::new();
    for &review in reviews {
        let key = card_key(review);
        match groups.iter().position(|(k, _)| *k == key) {
            Some(i) => groups[i].1.push(review),
            None => groups.push((key, vec![review])),
        }
    }

    let mut shaky = Vec::new();
    for (_, group) in groups {
        let mut latest = group[0];
        for &cur in &group[1..] {
            let cmp = at_of(cur).cmp(at_of(latest));
            if cmp == Ordering::Greater
                || (cmp == Ordering::Equal && review_id(cur) > review_id(latest))
            {
                latest = cur;
            }
        }
        if is_shaky(latest) {
            shaky.push(latest);
        }
    }
    shaky
}

/// `cardId ?? reviewId ?? "default"`: ids of any JSON type, and 5 differs from "5".
fn card_key(review: &Value) -> Value {
    for key in ["cardId", "reviewId"] {
        if let Some(value) = review.get(key) {
            if !value.is_null() {
                return value.clone();
            }
        }
    }
    Value::String("default".to_string())
}

fn at_of(record: &Value) -> &str {
    record.get("at").and_then(Value::as_str).unwrap_or("")
}

fn review_id(record: &Value) -> f64 {
    number_or_zero(record.get("reviewId"))
}

fn number_or_zero(value: Option<&Value>) -> f64 {
    match value {
        Some(v) => js::to_number(Some(v)),
        None => 0.0,
    }
}

fn is_shaky(review: &Value) -> bool {
    let floored = matches!(review.get("ease"), Some(Value::Number(n)) if n.as_f64() == Some(1.0));
    let lapses = number_or_zero(review.get("lapses"));
    let interval = number_or_zero(review.get("interval"));
    floored || (lapses >= SHAKY_LAPSES && interval < MATURE_DAYS)
}

/// A JSON `null` counts as present: `correct !== undefined` is the graded marker.
fn is_graded(record: &Value) -> bool {
    record.get("correct").is_some()
        || matches!(record.get("dontKnow"), Some(Value::Bool(true)))
}

fn graded_status(record: &Value) -> Status {
    if !js::truthy(record.get("correct")) || js::truthy(record.get("dontKnow")) {
        return Status::Missed;
    }
    let confidence = match record.get("confidence") {
        None | Some(Value::Null) => 70.0,
        value => js::to_number(value),
    };
    if confidence >= 50.0 {
        Status::Solid
    } else {
        Status::Shaky
    }
}

/// One map node: the provenance key the graph store joins on plus the JSON `/api/map` emits.
#[derive(Debug)]
pub struct MapNode {
    pub provenance_key: String,
    pub status: Status,
    pub json: Map<String, Value>,
}

#[derive(Debug)]
pub struct KnowledgeMap {
    pub nodes: Vec<MapNode>,
    pub edges: Vec<(String, String)>,
}

impl KnowledgeMap {
    pub fn to_json(&self) -> Value {
        let nodes: Vec<Value> = self
            .nodes
            .iter()
            .map(|n| Value::Object(n.json.clone()))
            .collect();
        let edges: Vec<Value> = self
            .edges
            .iter()
            .map(|(from, to)| {
                let mut edge = Map::new();
                edge.insert("from".to_string(), Value::String(from.clone()));
                edge.insert("to".to_string(), Value::String(to.clone()));
                Value::Object(edge)
            })
            .collect();
        let mut out = Map::new();
        out.insert("nodes".to_string(), Value::Array(nodes));
        out.insert("edges".to_string(), Value::Array(edges));
        Value::Object(out)
    }
}

pub fn build_knowledge_map(data_root: &Path) -> Result<KnowledgeMap, String> {
    let docs = lesson::scan_lesson_docs(data_root)?;
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut progress: HashMap<String, Vec<Value>> = HashMap::new();

    for doc in &docs {
        let lesson_nodes = doc.nodes();
        if lesson_nodes.is_empty() {
            continue;
        }
        // Read a course's ledger once: every lesson of the chapter shares it.
        if !progress.contains_key(&doc.course) {
            progress.insert(doc.course.clone(), read_progress(data_root, &doc.course));
        }
        let records = &progress[&doc.course];
        let slug = js::display(doc.slug());
        let key_of = |id: &str| format!("{}/{}#{}", doc.course, slug, id);
        let ids: Vec<&Value> = lesson_nodes
            .iter()
            .filter_map(|node| node.get("nodeId"))
            .collect();

        for (index, node) in lesson_nodes.iter().enumerate() {
            let node_id = node.get("nodeId");
            let key = key_of(&js::display(node_id));

            let mine = records_for_node(records, &key, doc.slug(), node_id, index);
            let reviews: Vec<&Value> = mine
                .iter()
                .copied()
                .filter(|r| r.get("kind").and_then(Value::as_str) == Some("review"))
                .collect();
            let graded: Vec<&Value> = mine
                .iter()
                .copied()
                .filter(|r| r.get("kind").and_then(Value::as_str) != Some("review") && is_graded(r))
                .collect();
            let status = if !reviews.is_empty() {
                anki_status(&reviews)
            } else if let Some(latest) = graded.last() {
                graded_status(latest)
            } else {
                Status::Taught
            };
            let attempts = graded.len() + reviews.len();

            let mut json = Map::new();
            json.insert("key".to_string(), Value::String(key.clone()));
            json.insert("course".to_string(), Value::String(doc.course.clone()));
            if let Some(slug) = doc.slug() {
                json.insert("lessonSlug".to_string(), slug.clone());
            }
            if let Some(title) = doc.title() {
                json.insert("lessonTitle".to_string(), title.clone());
            }
            if let Some(number) = doc.lesson_number() {
                json.insert("lessonNumber".to_string(), number.clone());
            }
            json.insert("lessonPath".to_string(), Value::String(doc.rel()));
            if let Some(id) = node_id {
                json.insert("nodeId".to_string(), id.clone());
            }
            if let Some(title) = node.get("title") {
                json.insert("title".to_string(), title.clone());
            }
            if let Some(tier) = node.get("tier") {
                json.insert("tier".to_string(), tier.clone());
            }
            json.insert(
                "status".to_string(),
                Value::String(status.as_str().to_string()),
            );
            json.insert("attempts".to_string(), Value::from(attempts as u64));
            if let Some(last) = mine.last().and_then(|r| r.get("at")) {
                json.insert("lastAt".to_string(), last.clone());
            }
            nodes.push(MapNode {
                provenance_key: format!(
                    "{}/{}#{}",
                    doc.course,
                    doc.stem(),
                    js::display(node_id)
                ),
                status,
                json,
            });

            for dependency in dependency_values(node.get("dependencies"))? {
                if ids.iter().any(|id| **id == dependency) {
                    edges.push((key_of(&js::display(Some(&dependency))), key.clone()));
                }
            }
        }
    }

    Ok(KnowledgeMap { nodes, edges })
}

/// All records of one node, oldest first: by key, or by node id / `N<i>` inside the same lesson.
fn records_for_node<'a>(
    records: &'a [Value],
    key: &str,
    slug: Option<&Value>,
    node_id: Option<&Value>,
    index: usize,
) -> Vec<&'a Value> {
    let n_ref = format!("N{}", index + 1);
    let mut mine: Vec<&'a Value> = records
        .iter()
        .filter(|record| {
            let Some(refs) = record.get("nodeRefs").and_then(Value::as_array) else {
                return false;
            };
            if refs.iter().any(|r| r.as_str() == Some(key)) {
                return true;
            }
            record.get("lessonSlug") == slug
                && (node_id.is_some_and(|id| refs.iter().any(|r| r == id))
                    || refs.iter().any(|r| r.as_str() == Some(n_ref.as_str())))
        })
        .collect();
    mine.sort_by(|a, b| at_of(a).cmp(at_of(b)));
    mine
}

/// `n.dependencies ?? []` iterated with `for…of`: arrays by element, strings by code point.
fn dependency_values(dependencies: Option<&Value>) -> Result<Vec<Value>, String> {
    match dependencies {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(Value::String(s)) => Ok(s
            .chars()
            .map(|c| Value::String(c.to_string()))
            .collect()),
        Some(_) => Err("dependencies non iterabile".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-knowledge-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn metadata(lesson_number: u32, slug: &str, title: &str, topic: &str) -> Value {
        json!({
            "lessonNumber": lesson_number,
            "topic": topic,
            "title": title,
            "slug": slug,
            "dateCreated": "",
            "dateModified": "",
            "version": "1",
        })
    }

    fn write_lesson(dir: &Path, filename: &str, metadata: &Value, nodes: &Value) {
        let doc = json!({ "metadata": metadata, "nodes": nodes });
        fs::write(
            dir.join(filename),
            format!("<script type=\"application/json\" id=\"lesson-data\">{doc}</script>"),
        )
        .unwrap();
    }

    fn make_repo(tag: &str) -> PathBuf {
        let root = temp_dir(tag);
        let dir = root.join("courses").join("demo").join("lessons");
        fs::create_dir_all(&dir).unwrap();
        write_lesson(
            &dir,
            "0001-t.html",
            &metadata(1, "t", "T", "T"),
            &json!([
                { "nodeId": "alpha", "title": "A", "tier": "axiom", "content": "x" },
                { "nodeId": "beta", "title": "B", "tier": "derivation", "content": "x", "dependencies": ["alpha", "ghost"] },
                { "nodeId": "gamma", "title": "C", "tier": "derivation", "content": "x" },
            ]),
        );
        write_lesson(
            &dir,
            "0002-prose.html",
            &metadata(2, "prose", "P", "P"),
            &json!([]),
        );
        root
    }

    fn rec(overrides: Value) -> Value {
        let mut base = json!({
            "at": "2026-01-01T00:00:00Z",
            "sessionId": "s",
            "course": "demo",
            "lessonSlug": "t",
            "nodeRefs": [],
            "kind": "check",
        });
        for (key, value) in overrides.as_object().unwrap() {
            base[key.as_str()] = value.clone();
        }
        base
    }

    fn anki_rec(overrides: Value) -> Value {
        let mut anki = json!({
            "kind": "review",
            "source": "anki",
            "sessionId": "anki",
            "nodeRefs": ["demo/t#alpha"],
        });
        for (key, value) in overrides.as_object().unwrap() {
            anki[key.as_str()] = value.clone();
        }
        rec(anki)
    }

    fn append_progress(root: &Path, record: &Value) {
        let file = root.join("courses").join("demo").join("progress.jsonl");
        let mut text = fs::read_to_string(&file).unwrap_or_default();
        text.push_str(&format!("{record}\n"));
        fs::write(file, text).unwrap();
    }

    fn node<'a>(map: &'a KnowledgeMap, node_id: &str) -> &'a MapNode {
        map.nodes
            .iter()
            .find(|n| n.json.get("nodeId").and_then(Value::as_str) == Some(node_id))
            .unwrap_or_else(|| panic!("missing node {node_id}"))
    }

    fn field<'a>(node: &'a MapNode, key: &str) -> &'a Value {
        node.json.get(key).unwrap_or_else(|| panic!("missing field {key}"))
    }

    #[test]
    fn no_progress_teaches_every_node_and_skips_unknown_dependencies() {
        let root = make_repo("taught");
        let map = build_knowledge_map(&root).unwrap();
        assert_eq!(map.nodes.len(), 3);
        assert!(
            map.nodes
                .iter()
                .all(|n| n.status == Status::Taught && field(n, "attempts") == &json!(0))
        );
        assert_eq!(field(&map.nodes[0], "key"), &json!("demo/t#alpha"));
        assert_eq!(
            field(&map.nodes[0], "lessonPath"),
            &json!("courses/demo/lessons/0001-t.html")
        );
        assert_eq!(
            map.edges,
            vec![("demo/t#alpha".to_string(), "demo/t#beta".to_string())]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn status_derived_from_progress_matching_node_id_n_index_and_key() {
        let root = make_repo("progress");
        append_progress(
            &root,
            &rec(json!({"nodeRefs": ["alpha"], "correct": true, "confidence": 90})),
        );
        append_progress(
            &root,
            &rec(json!({"nodeRefs": ["N2"], "correct": true, "confidence": 20, "at": "2026-01-02T00:00:00Z"})),
        );
        append_progress(&root, &rec(json!({"nodeRefs": ["N3"], "correct": true})));
        append_progress(
            &root,
            &rec(json!({"nodeRefs": ["N3"], "correct": false, "dontKnow": true, "at": "2026-01-03T00:00:00Z"})),
        );
        let file = root.join("courses").join("demo").join("progress.jsonl");
        let mut text = fs::read_to_string(&file).unwrap();
        text.push_str("not json\n");
        fs::write(&file, text).unwrap();

        let map = build_knowledge_map(&root).unwrap();
        assert_eq!(node(&map, "alpha").status, Status::Solid);
        assert_eq!(field(node(&map, "alpha"), "attempts"), &json!(1));
        assert_eq!(node(&map, "beta").status, Status::Shaky);
        assert_eq!(
            field(node(&map, "beta"), "lastAt"),
            &json!("2026-01-02T00:00:00Z")
        );
        assert_eq!(node(&map, "gamma").status, Status::Missed);
        assert_eq!(field(node(&map, "gamma"), "attempts"), &json!(2));
        assert_eq!(read_progress(&root, "demo").len(), 4);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn practice_and_exercise_lines_never_grade_a_node() {
        let root = make_repo("practice");
        let file = root.join("courses").join("demo").join("progress.jsonl");
        let mut text = String::new();
        text.push_str(
            &json!({"kind": "practice", "nodeRefs": ["demo/t#beta"], "outcome": "solved", "correct": true, "at": "2026-01-01T00:00:00Z", "sessionId": "s", "course": "demo"}).to_string(),
        );
        text.push('\n');
        text.push_str(
            &json!({"kind": "exercise", "nodeRefs": ["demo/t#gamma"], "correct": true, "at": "2026-01-01T00:00:00Z", "sessionId": "s", "course": "demo"}).to_string(),
        );
        text.push('\n');
        fs::write(&file, text).unwrap();
        append_progress(&root, &rec(json!({"nodeRefs": ["alpha"], "correct": true})));

        let map = build_knowledge_map(&root).unwrap();
        assert_eq!(node(&map, "alpha").status, Status::Solid);
        assert_eq!(node(&map, "beta").status, Status::Taught);
        assert_eq!(field(node(&map, "beta"), "attempts"), &json!(0));
        assert_eq!(node(&map, "gamma").status, Status::Taught);
        assert_eq!(field(node(&map, "gamma"), "attempts"), &json!(0));
        assert_eq!(read_progress(&root, "demo").len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_progress_of_a_missing_course_is_empty() {
        let root = make_repo("empty");
        assert!(read_progress(&root, "nope").is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_check_then_a_later_anki_review_is_solid() {
        let root = make_repo("case-a");
        append_progress(
            &root,
            &rec(json!({"nodeRefs": ["alpha"], "correct": false, "at": "2026-01-01T00:00:00Z"})),
        );
        append_progress(
            &root,
            &anki_rec(json!({"ease": 3, "at": "2026-01-02T00:00:00Z"})),
        );

        let map = build_knowledge_map(&root).unwrap();
        assert_eq!(node(&map, "alpha").status, Status::Solid);
        assert_eq!(field(node(&map, "alpha"), "attempts"), &json!(2));
        assert_eq!(
            field(node(&map, "alpha"), "lastAt"),
            &json!("2026-01-02T00:00:00Z")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_confident_check_without_reviews_is_solid() {
        let root = make_repo("case-b");
        append_progress(
            &root,
            &rec(json!({"nodeRefs": ["alpha"], "correct": true, "confidence": 90, "at": "2026-01-01T00:00:00Z"})),
        );

        let map = build_knowledge_map(&root).unwrap();
        assert_eq!(node(&map, "alpha").status, Status::Solid);
        assert_eq!(field(node(&map, "alpha"), "attempts"), &json!(1));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reviews_win_over_a_later_check() {
        let root = make_repo("case-c");
        append_progress(
            &root,
            &anki_rec(json!({"ease": 3, "at": "2026-01-01T00:00:00Z"})),
        );
        append_progress(
            &root,
            &rec(json!({"nodeRefs": ["alpha"], "correct": false, "at": "2026-01-05T00:00:00Z"})),
        );

        let map = build_knowledge_map(&root).unwrap();
        assert_eq!(node(&map, "alpha").status, Status::Solid);
        assert_eq!(field(node(&map, "alpha"), "attempts"), &json!(2));
        assert_eq!(
            field(node(&map, "alpha"), "lastAt"),
            &json!("2026-01-05T00:00:00Z")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn checks_are_read_in_at_order_not_append_order() {
        let root = make_repo("case-d");
        append_progress(
            &root,
            &rec(json!({"nodeRefs": ["alpha"], "correct": false, "at": "2026-01-05T00:00:00Z"})),
        );
        append_progress(
            &root,
            &rec(json!({"nodeRefs": ["alpha"], "correct": true, "confidence": 90, "at": "2026-01-02T00:00:00Z"})),
        );

        let map = build_knowledge_map(&root).unwrap();
        assert_eq!(node(&map, "alpha").status, Status::Missed);
        assert_eq!(field(node(&map, "alpha"), "attempts"), &json!(2));
        assert_eq!(
            field(node(&map, "alpha"), "lastAt"),
            &json!("2026-01-05T00:00:00Z")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn anki_status_cases() {
        let cases: Vec<(Vec<Value>, Status)> = vec![
            (vec![anki_rec(json!({"ease": 1}))], Status::Shaky),
            (
                vec![anki_rec(json!({"ease": 3, "lapses": 3, "interval": 10}))],
                Status::Shaky,
            ),
            (
                vec![anki_rec(json!({"ease": 3, "lapses": 3, "interval": 30}))],
                Status::Solid,
            ),
            (
                vec![anki_rec(json!({"ease": 2, "lapses": 0, "interval": 5}))],
                Status::Solid,
            ),
            (
                vec![
                    anki_rec(json!({"cardId": 1, "ease": 1})),
                    anki_rec(json!({"cardId": 2, "ease": 3, "lapses": 0, "interval": 30})),
                ],
                Status::Shaky,
            ),
            (
                vec![
                    anki_rec(json!({"cardId": 1, "ease": 1, "at": "2026-01-01T00:00:00Z", "reviewId": 100})),
                    anki_rec(json!({"cardId": 1, "ease": 3, "interval": 30, "lapses": 0, "at": "2026-01-02T00:00:00Z", "reviewId": 101})),
                ],
                Status::Solid,
            ),
        ];

        for (reviews, expected) in cases {
            let refs: Vec<&Value> = reviews.iter().collect();
            assert_eq!(anki_status(&refs), expected, "{reviews:?}");
        }
    }

    #[test]
    fn node_json_has_the_protocol_keys_in_order() {
        let root = make_repo("shape");
        let map = build_knowledge_map(&root).unwrap();
        assert_eq!(
            serde_json::to_string(&Value::Object(node(&map, "alpha").json.clone())).unwrap(),
            "{\"key\":\"demo/t#alpha\",\"course\":\"demo\",\"lessonSlug\":\"t\",\"lessonTitle\":\"T\",\"lessonNumber\":1,\"lessonPath\":\"courses/demo/lessons/0001-t.html\",\"nodeId\":\"alpha\",\"title\":\"A\",\"tier\":\"axiom\",\"status\":\"taught\",\"attempts\":0}"
        );
        assert_eq!(node(&map, "alpha").provenance_key, "demo/0001-t#alpha");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lesson_without_a_nodes_array_is_an_error() {
        let root = temp_dir("no-nodes");
        let dir = root.join("courses").join("demo").join("lessons");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("0001-x.html"),
            "<script type=\"application/json\" id=\"lesson-data\">{\"metadata\":{\"slug\":\"x\"}}</script>",
        )
        .unwrap();

        let err = build_knowledge_map(&root).unwrap_err();
        assert_eq!(err, "courses/demo/lessons/0001-x.html: nodes non è un array");
        let _ = fs::remove_dir_all(&root);
    }
}
