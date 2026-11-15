//! The concept catalog and its tools: the graph's propositions joined with the course and birth
//! lesson they came from, over the lexical/semantic ranking.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use indexmap::IndexMap;
use serde_json::Value;

use crate::chapter;
use crate::course_data::{STANDALONE_COURSE_SLUG, STANDALONE_COURSE_TITLE};
use crate::graph::derive::{birth_session_of, Record, Snapshot};
use crate::graph::store::GraphStore;
use crate::js;
use crate::knowledge;
use crate::tools::ToolOutcome;

/// Same cap as `SEARCH_LIMIT` in app/server/graph/concepts.ts.
pub const SEARCH_LIMIT: usize = 10;
/// Same cap as `BROWSE_LIMIT` in app/server/graph/concepts.ts.
pub const BROWSE_LIMIT: usize = 100;
pub const SEMANTIC_UNAVAILABLE: &str =
    "(Semantic search unavailable: the embedding model is not downloaded yet. Lexical results only.)";
/// A lexical match outranks a bare cosine: the model is good at meaning, not at exact names.
pub const LEXICAL_BONUS: f64 = 0.15;

#[derive(Debug, Clone, PartialEq)]
pub struct NodeText {
    pub id: String,
    pub name: String,
    pub statement: String,
    pub aliases: Vec<String>,
}

impl NodeText {
    pub fn from_record(record: &Record) -> Self {
        let text = |key: &str| {
            record
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let aliases = match record.get("aliases") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        NodeText {
            id: text("id"),
            name: text("name"),
            statement: text("statement"),
            aliases,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedHit {
    pub id: String,
    pub cosine: f64,
}

/// The text a concept is embedded as. Change the recipe only here.
pub fn concept_text(node: &NodeText) -> String {
    let aliases = node.aliases.join(", ");
    [node.name.as_str(), node.statement.as_str(), aliases.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(". ")
}

/// Port of `fuzzyScore` (app/shared/graph/fuzzy.ts): every token must hit one field.
pub fn fuzzy_score(query: &str, fields: &[&str]) -> Option<i64> {
    let lowered = query.trim().to_lowercase();
    let tokens: Vec<&str> = lowered.split_whitespace().collect();
    if tokens.is_empty() {
        return Some(0);
    }
    let hay: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();

    let mut total = 0;
    for token in &tokens {
        let mut best: i64 = -1;
        for field in &hay {
            if field == token {
                best = best.max(100);
            } else if field.starts_with(token) {
                best = best.max(60);
            } else if let Some(byte_at) = field.find(token) {
                // JS indexOf counts UTF-16 code units, not chars or bytes.
                let at = field[..byte_at].encode_utf16().count() as i64;
                best = best.max(30 - at.min(27));
            }
        }
        if best < 0 {
            return None;
        }
        total += best;
    }
    let joined = tokens.join(" ");
    if hay.iter().any(|field| field.contains(&joined)) {
        total += 50;
    }
    Some(total)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredConcept {
    pub index: usize,
    pub score: f64,
}

/// The fields a query is matched against: name, statement, then aliases.
fn fields(node: &NodeText) -> Vec<&str> {
    let mut all = Vec::with_capacity(2 + node.aliases.len());
    all.push(node.name.as_str());
    all.push(node.statement.as_str());
    all.extend(node.aliases.iter().map(String::as_str));
    all
}

pub fn rank_concepts(
    query: &str,
    nodes: &[NodeText],
    cosines: Option<&[RankedHit]>,
    min_score: f64,
    limit: usize,
) -> Vec<ScoredConcept> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let cap = limit.clamp(1, SEARCH_LIMIT);
    let mut scored: Vec<ScoredConcept> = Vec::new();
    if let Some(hits) = cosines {
        // Later duplicates win, like building the Map in searchConcepts.
        let by_id: HashMap<&str, f64> = hits
            .iter()
            .map(|hit| (hit.id.as_str(), hit.cosine))
            .collect();
        for (index, node) in nodes.iter().enumerate() {
            let cosine = by_id.get(node.id.as_str()).copied().unwrap_or(0.0);
            let lexical = fuzzy_score(query, &fields(node)).is_some();
            if cosine >= min_score || lexical {
                scored.push(ScoredConcept {
                    index,
                    score: cosine + if lexical { LEXICAL_BONUS } else { 0.0 },
                });
            }
        }
    } else {
        for (index, node) in nodes.iter().enumerate() {
            if let Some(score) = fuzzy_score(query, &fields(node)) {
                scored.push(ScoredConcept {
                    index,
                    score: score as f64,
                });
            }
        }
    }
    // Stable sort: equal scores keep ledger order.
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    scored.truncate(cap);
    scored
}

/// What a caller's semantic search answers: cosines at best, or why it is off.
pub type Ranker<'a> = &'a dyn Fn(&str, &[NodeText]) -> Result<Vec<RankedHit>, String>;

/// The read-only concept catalog, built per tool call so the system prompt never carries the list.
pub struct ConceptCatalog {
    pub snap: Snapshot,
    /// Curriculum chapter slugs in plan order, standalone course excluded.
    pub course_order: Vec<String>,
    /// slug -> title; the standalone course is always present.
    pub course_titles: HashMap<String, String>,
    /// "<course>/<lesson file stem>#<nodeId>" -> MapNode.key (same key rule as provenance_status).
    pub map_keys: HashMap<String, String>,
}

pub fn load_catalog(data_root: &Path) -> ConceptCatalog {
    // A malformed curriculum is not fatal here: course dirs still name themselves.
    let curriculum = chapter::read_curriculum(&chapter::curriculum_file(data_root, None))
        .unwrap_or_default();

    // Unreadable lessons cost the MapNode keys, not the catalog.
    let map_keys = match knowledge::build_knowledge_map(data_root) {
        Ok(map) => map
            .nodes
            .into_iter()
            .filter_map(|node| {
                let key = node.json.get("key")?.as_str()?.to_string();
                Some((node.provenance_key, key))
            })
            .collect(),
        Err(_) => HashMap::new(),
    };

    // Infallible in Rust: unreadable ledger lines just read as no concepts, the ported store's rule.
    let snap = GraphStore::new(data_root.to_path_buf()).read_snapshot();

    let course_order = curriculum
        .chapters
        .iter()
        .map(chapter::chapter_slug)
        .filter(|slug| slug != STANDALONE_COURSE_SLUG)
        .collect();
    let mut course_titles = HashMap::new();
    for chapter in &curriculum.chapters {
        let title = chapter.get("title");
        // A missing or null title joins as undefined in Node: fall back to the slug.
        if title.is_some_and(|title| !title.is_null()) {
            course_titles.insert(chapter::chapter_slug(chapter), js::js_string(title));
        }
    }
    // The standalone course is always titled by the constant, whatever the plan says.
    course_titles.insert(
        STANDALONE_COURSE_SLUG.to_string(),
        STANDALONE_COURSE_TITLE.to_string(),
    );

    ConceptCatalog {
        snap,
        course_order,
        course_titles,
        map_keys,
    }
}

/// `birthSessionOf` is None when a node has no provenance: never split the raw result.
fn birth_of(node: &Record) -> String {
    birth_session_of(node).unwrap_or_default()
}

fn course_of(node: &Record) -> String {
    let birth = birth_of(node);
    birth.split('/').next().unwrap_or_default().to_string()
}

fn course_title(cat: &ConceptCatalog, slug: &str) -> String {
    if slug.is_empty() {
        return "(no lesson)".to_string();
    }
    cat.course_titles
        .get(slug)
        .cloned()
        .unwrap_or_else(|| slug.to_string())
}

fn lesson_label(cat: &ConceptCatalog, chat_id: &str) -> String {
    if chat_id.is_empty() {
        return "-".to_string();
    }
    cat.snap
        .sessions
        .get(chat_id)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| chat_id.to_string())
}

pub fn status_it(status: Option<&str>) -> &'static str {
    match status {
        None | Some("taught") => "insegnato",
        Some("solid") => "solido",
        Some("shaky") => "fragile",
        Some("missed") => "mancato",
        // Node's STATUS_IT of an unknown status is undefined, which joins as empty.
        Some(_) => "",
    }
}

fn concept_line(cat: &ConceptCatalog, node: &Record) -> String {
    // A missing or null record field joins as "" (Node's array join).
    let field = |key: &str| {
        node.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let parts = [
        field("id"),
        field("name"),
        status_it(node.get("status").and_then(Value::as_str)).to_string(),
        course_title(cat, &course_of(node)),
        lesson_label(cat, &birth_of(node)),
    ];
    format!("- {}", parts.join(" | "))
}

/// Plan order first, other course dirs alphabetically, the standalone course last.
fn compare_courses(cat: &ConceptCatalog, a: &str, b: &str) -> Ordering {
    let ia = cat.course_order.iter().position(|slug| slug == a);
    let ib = cat.course_order.iter().position(|slug| slug == b);
    if ia.is_some() || ib.is_some() {
        return match (ia, ib) {
            (Some(x), Some(y)) => x.cmp(&y),
            // a is planned and b is not: a comes first
            (Some(_), None) => Ordering::Less,
            _ => Ordering::Greater,
        };
    }
    if a == STANDALONE_COURSE_SLUG || b == STANDALONE_COURSE_SLUG {
        if a == STANDALONE_COURSE_SLUG {
            return if b == STANDALONE_COURSE_SLUG {
                Ordering::Equal
            } else {
                Ordering::Greater
            };
        }
        return Ordering::Less;
    }
    js::locale_compare(a, b)
}

pub fn search_concepts(
    cat: &ConceptCatalog,
    query: &str,
    limit: usize,
    ranker: Option<Ranker>,
    min_score: f64,
) -> ToolOutcome {
    if js::js_trim(query).is_empty() {
        return ToolOutcome::error("Empty query: pass words from the concept's name or statement.");
    }
    let nodes: Vec<NodeText> = cat.snap.propositions.iter().map(NodeText::from_record).collect();
    let (cosines, note) = match ranker {
        Some(ranker) => match ranker(query, &nodes) {
            Ok(hits) => (Some(hits), String::new()),
            Err(_) => (None, format!("\n{SEMANTIC_UNAVAILABLE}")),
        },
        None => (None, String::new()),
    };
    let hits = rank_concepts(query, &nodes, cosines.as_deref(), min_score, limit);
    if hits.is_empty() {
        return ToolOutcome::ok(format!("No concept matches \"{query}\".{note}"));
    }
    let mut text = format!(
        "{} concept(s), best first (id | name | status | course | lesson):",
        hits.len()
    );
    for hit in &hits {
        text.push('\n');
        text.push_str(&concept_line(cat, &cat.snap.propositions[hit.index]));
    }
    text.push_str(&note);
    ToolOutcome::ok(text)
}

fn concept_ref(cat: &ConceptCatalog, id: &str) -> String {
    match cat
        .snap
        .propositions
        .iter()
        .find(|node| node.get("id").and_then(Value::as_str) == Some(id))
    {
        Some(node) => {
            let field = |key: &str| node.get(key).and_then(Value::as_str).unwrap_or("").to_string();
            format!("{} | {}", field("id"), field("name"))
        }
        None => id.to_string(),
    }
}

pub fn open_concept(cat: &ConceptCatalog, id: &str) -> ToolOutcome {
    let Some(node) = cat
        .snap
        .propositions
        .iter()
        .find(|node| node.get("id").and_then(Value::as_str) == Some(id))
    else {
        return ToolOutcome::error(format!(
            "Unknown concept id \"{id}\". Use search_concepts or browse_concepts to find ids."
        ));
    };
    let field = |key: &str| node.get(key).and_then(Value::as_str).unwrap_or("");

    let mut lines = vec![
        format!("# {} ({id})", field("name")),
        format!("Statement: {}", field("statement")),
        format!("Status: {}", status_it(node.get("status").and_then(Value::as_str))),
    ];
    let aliases: Vec<&str> = node
        .get("aliases")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(|alias| alias.as_str().unwrap_or("")).collect())
        .unwrap_or_default();
    if !aliases.is_empty() {
        lines.push(format!("Aliases: {}", aliases.join(", ")));
    }

    let mut courses: Vec<String> = Vec::new();
    let mut lessons: Vec<String> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for point in node
        .get("provenance")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let chat_id = point.get("chatId").and_then(Value::as_str).unwrap_or("");
        let course = chat_id.split('/').next().unwrap_or("");
        if !courses.iter().any(|seen| seen == course) {
            courses.push(course.to_string());
        }
        if !lessons.iter().any(|seen| seen == chat_id) {
            lessons.push(chat_id.to_string());
        }
        let message_id = point.get("messageId").and_then(Value::as_str).unwrap_or("");
        if message_id.is_empty() {
            continue;
        }
        let key = format!("{chat_id}#{message_id}");
        if let Some(key) = cat.map_keys.get(&key) {
            if !keys.iter().any(|seen| seen == key) {
                keys.push(key.clone());
            }
        }
    }
    lines.push(format!(
        "Courses: {}",
        if courses.is_empty() {
            "(none)".to_string()
        } else {
            courses
                .iter()
                .map(|course| format!("{} ({course})", course_title(cat, course)))
                .collect::<Vec<_>>()
                .join("; ")
        }
    ));
    lines.push(format!(
        "Lessons: {}",
        if lessons.is_empty() {
            "(none)".to_string()
        } else {
            lessons
                .iter()
                .map(|lesson| format!("{} ({lesson})", lesson_label(cat, lesson)))
                .collect::<Vec<_>>()
                .join("; ")
        }
    ));
    lines.push(if keys.is_empty() {
        "MapNode keys: none".to_string()
    } else {
        format!("MapNode keys (for problem.nodeKeys): {}", keys.join(", "))
    });

    let mut premises: Vec<String> = Vec::new();
    for chain in &cat.snap.chains {
        if chain.get("conclusion").and_then(Value::as_str) != Some(id) {
            continue;
        }
        for premise in chain
            .get("premises")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let premise = premise.as_str().unwrap_or("");
            if !premises.iter().any(|seen| seen == premise) {
                premises.push(premise.to_string());
            }
        }
    }
    lines.push(if premises.is_empty() {
        "Premises: (none)".to_string()
    } else {
        "Premises:".to_string()
    });
    for premise in &premises {
        lines.push(format!("- {}", concept_ref(cat, premise)));
    }

    let mut consequences: Vec<String> = Vec::new();
    for chain in &cat.snap.chains {
        let follows = chain
            .get("premises")
            .and_then(Value::as_array)
            .is_some_and(|premises| premises.iter().any(|p| p.as_str() == Some(id)));
        let conclusion = chain.get("conclusion").and_then(Value::as_str).unwrap_or("");
        if follows && !consequences.iter().any(|seen| seen == conclusion) {
            consequences.push(conclusion.to_string());
        }
    }
    lines.push(if consequences.is_empty() {
        "Consequences: (none)".to_string()
    } else {
        "Consequences:".to_string()
    });
    for consequence in &consequences {
        lines.push(format!("- {}", concept_ref(cat, consequence)));
    }

    ToolOutcome::ok(lines.join("\n"))
}

pub fn browse_concepts(
    cat: &ConceptCatalog,
    course: Option<&str>,
    lesson: Option<&str>,
) -> ToolOutcome {
    // Empty strings are Node's falsy course/lesson: absent.
    let course = course.filter(|course| !course.is_empty());
    let lesson = lesson.filter(|lesson| !lesson.is_empty());

    let Some(course) = course else {
        if lesson.is_some() {
            return ToolOutcome::error("Pass course with lesson.");
        }
        let mut counts: IndexMap<String, usize> = IndexMap::new();
        for node in &cat.snap.propositions {
            *counts.entry(course_of(node)).or_insert(0) += 1;
        }
        if counts.is_empty() {
            return ToolOutcome::ok("The knowledge graph is empty: no concept yet.");
        }
        let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
        rows.sort_by(|a, b| compare_courses(cat, &a.0, &b.0));
        let mut text = String::from("Courses (slug | title | concepts):");
        for (slug, count) in rows {
            text.push('\n');
            text.push_str(&format!(
                "- {slug} | {} | {count} concepts",
                course_title(cat, &slug)
            ));
        }
        return ToolOutcome::ok(text);
    };

    let members: Vec<&Record> = cat
        .snap
        .propositions
        .iter()
        .filter(|node| course_of(node) == course)
        .collect();
    let Some(lesson) = lesson else {
        if members.is_empty() {
            let mut with_concepts: Vec<String> = Vec::new();
            for node in &cat.snap.propositions {
                let slug = course_of(node);
                if !with_concepts.iter().any(|seen| seen == &slug) {
                    with_concepts.push(slug);
                }
            }
            with_concepts.sort_by(|a, b| compare_courses(cat, a, b));
            let list = if with_concepts.is_empty() {
                "(none)".to_string()
            } else {
                with_concepts.join(", ")
            };
            return ToolOutcome::error(format!(
                "No concepts in course \"{course}\". Courses with concepts: {list}."
            ));
        }
        let mut counts: IndexMap<String, usize> = IndexMap::new();
        for node in &members {
            *counts.entry(birth_of(node)).or_insert(0) += 1;
        }
        let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
        // JS string sort compares UTF-16 code units.
        rows.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        let mut text = format!(
            "Lessons of {} (lesson | label | concepts):",
            course_title(cat, course)
        );
        for (chat_id, count) in rows {
            text.push('\n');
            text.push_str(&format!(
                "- {chat_id} | {} | {count} concepts",
                lesson_label(cat, &chat_id)
            ));
        }
        return ToolOutcome::ok(text);
    };

    // lesson may be the full chatId or just the stem
    let prefix = format!("{course}/");
    let stem = lesson.strip_prefix(prefix.as_str()).unwrap_or(lesson);
    let chat_id = format!("{course}/{stem}");
    let born: Vec<&Record> = members
        .into_iter()
        .filter(|node| birth_of(node) == chat_id)
        .collect();
    if born.is_empty() {
        return ToolOutcome::error(format!(
            "No concepts born in lesson \"{chat_id}\". Call browse_concepts with course \"{course}\" to list its lessons."
        ));
    }
    let mut lines = vec![format!(
        "Concepts of {} (id | name | status | course | lesson):",
        lesson_label(cat, &chat_id)
    )];
    for node in born.iter().take(BROWSE_LIMIT) {
        lines.push(concept_line(cat, node));
    }
    if born.len() > BROWSE_LIMIT {
        lines.push(format!(
            "(+{} more: use search_concepts)",
            born.len() - BROWSE_LIMIT
        ));
    }
    ToolOutcome::ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use serde_json::json;

    use super::*;
    use crate::embeddings::EmbeddingConfig;

    const DEMO_LESSON: &str = "demo/0001-demo";

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-concepts-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn make_data(tag: &str) -> PathBuf {
        let root = temp_dir(tag);
        fs::write(
            root.join("curriculum.json"),
            json!({"chapters": [{"title": "Demo", "slug": "demo", "lessons": []}]}).to_string(),
        )
        .unwrap();
        root
    }

    fn born_in(chat_id: &str) -> Value {
        json!([{ "chatId": chat_id, "messageId": "" }])
    }

    fn prop(id: &str, name: &str) -> Value {
        prop_in(id, name, born_in(DEMO_LESSON))
    }

    fn prop_in(id: &str, name: &str, provenance: Value) -> Value {
        json!({
            "id": id,
            "name": name,
            "statement": format!("{name} vale."),
            "aliases": [],
            "createdAt": "2026-01-01T00:00:00.000Z",
            "provenance": provenance,
        })
    }

    fn chain(id: &str, premises: &[&str], conclusion: &str) -> Value {
        json!({
            "id": id,
            "premises": premises,
            "conclusion": conclusion,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "provenance": [],
        })
    }

    fn write_graph(root: &Path, nodes: &[Value], chains: &[Value]) {
        let dir = root.join("graph");
        fs::create_dir_all(&dir).unwrap();
        let jsonl = |rows: &[Value]| {
            let mut text = rows.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
            if !rows.is_empty() {
                text.push('\n');
            }
            text
        };
        fs::write(dir.join("nodes.jsonl"), jsonl(nodes)).unwrap();
        fs::write(dir.join("chains.jsonl"), jsonl(chains)).unwrap();
    }

    fn write_lesson(
        root: &Path,
        course: &str,
        filename: &str,
        slug: &str,
        title: &str,
        nodes: &[(&str, &str)],
        lesson_number: i64,
    ) {
        let dir = root.join("courses").join(course).join("lessons");
        fs::create_dir_all(&dir).unwrap();
        let nodes: Vec<Value> = nodes
            .iter()
            .map(|(node_id, title)| {
                json!({"nodeId": node_id, "title": title, "tier": "axiom", "content": "x"})
            })
            .collect();
        let doc = json!({
            "metadata": {
                "lessonNumber": lesson_number,
                "topic": title,
                "title": title,
                "slug": slug,
                "dateCreated": "2026-01-01T00:00:00.000Z",
                "dateModified": "2026-01-02T00:00:00.000Z",
                "version": "1",
            },
            "nodes": nodes,
        });
        fs::write(
            dir.join(filename),
            format!("<script type=\"application/json\" id=\"lesson-data\">{doc}</script>"),
        )
        .unwrap();
    }

    fn min_score(root: &Path) -> f64 {
        EmbeddingConfig::load(root).expect("embedding config").min_score
    }

    fn demo_catalog(tag: &str) -> (PathBuf, ConceptCatalog) {
        let root = make_data(tag);
        write_graph(
            &root,
            &[
                prop_in("n1", "Uno", born_in("demo/0001-demo")),
                prop_in("n2", "Due", born_in("demo/0001-demo")),
                prop_in("n3", "Tre", born_in("demo/0002-secondo")),
                prop_in("n4", "Quattro", born_in("altra/0001-terza")),
                prop_in("n5", "Cinque", born_in("lezioni-singole/0001-sola")),
            ],
            &[],
        );
        let cat = load_catalog(&root);
        (root, cat)
    }

    fn node(id: &str, name: &str, statement: &str, aliases: &[&str]) -> NodeText {
        NodeText {
            id: id.to_string(),
            name: name.to_string(),
            statement: statement.to_string(),
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
        }
    }

    fn hits(entries: &[(&str, f64)]) -> Vec<RankedHit> {
        entries
            .iter()
            .map(|(id, cosine)| RankedHit {
                id: id.to_string(),
                cosine: *cosine,
            })
            .collect()
    }

    fn ids(nodes: &[NodeText], scored: &[ScoredConcept]) -> Vec<String> {
        scored.iter().map(|hit| nodes[hit.index].id.clone()).collect()
    }

    #[test]
    fn fuzzy_score_ranks_equality_prefix_substring_and_contiguity() {
        assert_eq!(fuzzy_score("glm flash", &["z-ai/glm-5.3-flash"]), Some(42));
        assert_eq!(fuzzy_score("glm flash", &["glm flash model"]), Some(136));
        assert_eq!(fuzzy_score("props", &["Props"]), Some(150));
        assert_eq!(fuzzy_score("prop", &["Props"]), Some(110));
        assert_eq!(
            fuzzy_score(
                "ohm resistenza",
                &[
                    "Legge fondamentale",
                    "La corrente segue la legge di Ohm.",
                    "resistenza elettrica",
                ]
            ),
            Some(63)
        );
    }

    #[test]
    fn fuzzy_score_measures_find_positions_in_utf16_units() {
        assert_eq!(fuzzy_score("x", &["àààx"]), Some(77));
        assert_eq!(fuzzy_score("x", &["𝒜𝒜x"]), Some(76));
        assert_eq!(fuzzy_score("è", &["perché è così"]), Some(73));
    }

    #[test]
    fn fuzzy_score_returns_none_when_a_token_misses_every_field() {
        assert_eq!(fuzzy_score("zzz", &["Props"]), None);
        assert_eq!(fuzzy_score("   ", &["Props"]), Some(0));
    }

    #[test]
    fn concept_text_joins_the_non_empty_parts() {
        assert_eq!(
            concept_text(&node(
                "n_computed",
                "Proprietà calcolate",
                "Valore derivato e in cache",
                &["computed", "valore derivato"]
            )),
            "Proprietà calcolate. Valore derivato e in cache. computed, valore derivato"
        );
        assert_eq!(
            concept_text(&node("n_props", "Props", "Dati passati dal genitore", &[])),
            "Props. Dati passati dal genitore"
        );
        assert_eq!(
            concept_text(&node("n_empty", "Solo nome", "", &["alias"])),
            "Solo nome. alias"
        );
    }

    #[test]
    fn from_record_reads_only_string_aliases() {
        let record: Record = serde_json::from_value(serde_json::json!({
            "id": "n_1",
            "name": "Props",
            "statement": "Dati passati",
            "aliases": ["props", 7, null, "dati"],
        }))
        .unwrap();
        assert_eq!(
            NodeText::from_record(&record),
            node("n_1", "Props", "Dati passati", &["props", "dati"])
        );

        let missing: Record = serde_json::from_value(serde_json::json!({
            "id": "n_2",
            "name": 42,
        }))
        .unwrap();
        assert_eq!(NodeText::from_record(&missing), node("n_2", "", "", &[]));
    }

    #[test]
    fn rank_concepts_orders_by_cosine_and_adds_the_lexical_bonus() {
        let nodes = vec![
            node("n_a", "Alfa", "", &[]),
            node("n_b", "Beta", "", &[]),
            node("n_c", "Passo veloce", "", &[]),
            node("n_d", "Zeta", "", &[]),
            node("n_e", "Passo lento", "", &[]),
        ];
        let cosines = hits(&[
            ("n_a", 0.8),
            ("n_b", 0.5),
            ("n_c", 0.4),
            ("n_d", 0.1),
            ("n_e", 0.1),
        ]);

        let scored = rank_concepts("passo", &nodes, Some(&cosines), 0.3, 10);
        // n_c's bonus beats n_b; n_d is under minScore and not lexical, so it is dropped.
        assert_eq!(ids(&nodes, &scored), ["n_a", "n_c", "n_b", "n_e"]);
    }

    #[test]
    fn rank_concepts_lets_a_later_duplicate_cosine_win() {
        let nodes = vec![node("n_a", "Alfa", "", &[])];
        let cosines = hits(&[("n_a", 0.1), ("n_a", 0.9)]);

        let scored = rank_concepts("zzz", &nodes, Some(&cosines), 0.5, 10);
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].score, 0.9);
    }

    #[test]
    fn rank_concepts_matches_aliases() {
        let nodes = vec![node("n_a", "Alfa", "", &["passo"])];
        let cosines = hits(&[("n_a", 0.0)]);

        // the alias keeps a node under minScore and adds the lexical bonus
        let scored = rank_concepts("passo", &nodes, Some(&cosines), 0.3, 10);
        assert_eq!(ids(&nodes, &scored), ["n_a"]);
        assert_eq!(scored[0].score, LEXICAL_BONUS);
    }

    #[test]
    fn rank_concepts_keeps_input_order_on_ties() {
        let nodes = vec![
            node("n_first", "Alfa", "", &[]),
            node("n_second", "Beta", "", &[]),
        ];
        let cosines = hits(&[("n_first", 0.5), ("n_second", 0.5)]);

        let scored = rank_concepts("zzz", &nodes, Some(&cosines), 0.3, 10);
        assert_eq!(ids(&nodes, &scored), ["n_first", "n_second"]);
    }

    #[test]
    fn rank_concepts_caps_to_the_search_limit() {
        let nodes: Vec<NodeText> = (0..12)
            .map(|i| node(&format!("n_{i}"), &format!("Nodo {i}"), "", &[]))
            .collect();
        let cosines: Vec<RankedHit> = nodes
            .iter()
            .map(|n| RankedHit {
                id: n.id.clone(),
                cosine: 0.9,
            })
            .collect();

        assert_eq!(rank_concepts("zzz", &nodes, Some(&cosines), 0.0, 50).len(), 10);
        assert_eq!(rank_concepts("zzz", &nodes, Some(&cosines), 0.0, 3).len(), 3);
        assert_eq!(rank_concepts("zzz", &nodes, Some(&cosines), 0.0, 0).len(), 1);
    }

    #[test]
    fn rank_concepts_falls_back_to_lexical_scores() {
        let nodes = vec![
            node("n_a", "Alfa", "", &[]),
            node("n_b", "Passo veloce", "", &[]),
            node("n_c", "Passo", "il primo passo", &[]),
        ];

        let scored = rank_concepts("passo", &nodes, None, 0.3, 10);
        // Exact field (100 + 50) beats a prefix (60 + 50); Alfa does not match at all.
        assert_eq!(ids(&nodes, &scored), ["n_c", "n_b"]);
    }

    #[test]
    fn rank_concepts_returns_nothing_for_a_blank_query() {
        let nodes = vec![node("n_a", "Alfa", "", &[])];
        let cosines = hits(&[("n_a", 1.0)]);
        assert!(rank_concepts("   ", &nodes, Some(&cosines), 0.0, 10).is_empty());
        assert!(rank_concepts("", &nodes, None, 0.0, 10).is_empty());
    }

    #[test]
    fn rank_concepts_counts_a_node_missing_from_the_cosines_as_zero() {
        let nodes = vec![
            node("n_absent", "Alfa", "", &[]),
            node("n_hit", "Beta", "", &[]),
        ];
        let cosines = hits(&[("n_hit", 0.9)]);

        let with_min = rank_concepts("zzz", &nodes, Some(&cosines), 0.2, 10);
        assert_eq!(ids(&nodes, &with_min), ["n_hit"]);

        let without_min = rank_concepts("zzz", &nodes, Some(&cosines), 0.0, 10);
        assert_eq!(ids(&nodes, &without_min), ["n_hit", "n_absent"]);
        assert_eq!(without_min[1].score, 0.0);
    }

    #[test]
    fn search_constants_match_the_node_ranking() {
        let source = include_str!("../../../app/server/graph/concepts.ts");
        assert!(source.contains("export const SEARCH_LIMIT = 10;"));
        assert!(source.contains("export const BROWSE_LIMIT = 100;"));
        assert!(source.contains("export const LEXICAL_BONUS = 0.15;"));
        assert!(source.contains("the embedding model is not downloaded yet. Lexical results only.)"));
        assert_eq!(SEARCH_LIMIT, 10);
        assert_eq!(BROWSE_LIMIT, 100);
        assert_eq!(LEXICAL_BONUS, 0.15);
        assert!(SEMANTIC_UNAVAILABLE.contains("Lexical results only."));
    }

    #[test]
    fn status_it_names_the_statuses_and_blanks_an_unknown_one() {
        assert_eq!(status_it(None), "insegnato");
        assert_eq!(status_it(Some("solid")), "solido");
        assert_eq!(status_it(Some("shaky")), "fragile");
        assert_eq!(status_it(Some("taught")), "insegnato");
        assert_eq!(status_it(Some("missed")), "mancato");
        assert_eq!(status_it(Some("other")), "");
    }

    #[test]
    fn search_caps_at_ten_with_five_fields_and_insegnato() {
        let root = make_data("search-cap");
        let nodes: Vec<Value> = (1..=15)
            .map(|i| prop(&format!("n_{i}"), &format!("Passo {i}")))
            .collect();
        write_graph(&root, &nodes, &[]);
        let cat = load_catalog(&root);
        let min = min_score(&root);

        let out = search_concepts(&cat, "passo", SEARCH_LIMIT, None, min);
        assert!(!out.is_error);
        let lines: Vec<&str> = out.text.split('\n').collect();
        assert_eq!(lines.len(), SEARCH_LIMIT + 1);
        assert_eq!(
            lines[0],
            "10 concept(s), best first (id | name | status | course | lesson):"
        );
        for line in &lines[1..] {
            let fields: Vec<&str> = line.split(" | ").collect();
            assert_eq!(fields.len(), 5, "{line}");
            assert_eq!(fields[2], "insegnato", "{line}");
            assert_eq!(fields[3], "Demo", "{line}");
        }

        assert_eq!(
            search_concepts(&cat, "passo", 3, None, min)
                .text
                .split('\n')
                .count(),
            4
        );
        // the cap never rises above SEARCH_LIMIT
        assert_eq!(
            search_concepts(&cat, "passo", 50, None, min)
                .text
                .split('\n')
                .count(),
            SEARCH_LIMIT + 1
        );

        let empty = search_concepts(&cat, "   ", SEARCH_LIMIT, None, min);
        assert!(empty.is_error);
        assert!(empty.text.starts_with("Empty query"), "{}", empty.text);

        let none = search_concepts(&cat, "zzz", SEARCH_LIMIT, None, min);
        assert!(!none.is_error);
        assert_eq!(none.text, "No concept matches \"zzz\".");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_proposition_with_no_provenance_is_searchable_and_names_no_lesson() {
        let root = make_data("search-no-provenance");
        write_graph(&root, &[prop_in("n_x", "Senza lezione", json!([]))], &[]);
        let cat = load_catalog(&root);

        let out = search_concepts(&cat, "senza", SEARCH_LIMIT, None, min_score(&root));
        assert!(!out.is_error);
        let line = out.text.split('\n').nth(1).expect("a hit line");
        assert!(line.ends_with(" | (no lesson) | -"), "{line}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_matches_the_statement_and_the_aliases_not_only_the_name() {
        let root = make_data("search-fields");
        let mut ohm = prop("n_ohm", "Legge fondamentale");
        ohm["statement"] = json!("La corrente segue la legge di Ohm.");
        ohm["aliases"] = json!(["resistenza elettrica"]);
        write_graph(&root, &[ohm], &[]);
        let cat = load_catalog(&root);

        for query in ["ohm", "resistenza"] {
            let out = search_concepts(&cat, query, SEARCH_LIMIT, None, min_score(&root));
            assert!(!out.is_error);
            assert!(
                out.text.contains("n_ohm | Legge fondamentale"),
                "\"{query}\" did not find the concept: {}",
                out.text
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_ranker_keeps_lexical_hits_and_adds_the_note() {
        let root = make_data("search-fallback");
        write_graph(&root, &[prop("n_1", "Primo passo")], &[]);
        let cat = load_catalog(&root);
        let min = min_score(&root);
        let ranker: Ranker<'_> = &|_query, _nodes| Err("model missing".to_string());

        let hit = search_concepts(&cat, "passo", SEARCH_LIMIT, Some(ranker), min);
        assert!(!hit.is_error);
        assert!(hit.text.contains("n_1 | Primo passo"), "{}", hit.text);
        assert!(hit.text.ends_with(SEMANTIC_UNAVAILABLE), "{}", hit.text);

        let none = search_concepts(&cat, "zzz", SEARCH_LIMIT, Some(ranker), min);
        assert!(!none.is_error);
        assert!(none.text.ends_with(SEMANTIC_UNAVAILABLE), "{}", none.text);
        assert!(
            none.text.starts_with("No concept matches \"zzz\""),
            "{}",
            none.text
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_semantic_ranker_orders_by_cosine_and_adds_the_lexical_bonus() {
        let root = make_data("search-semantic");
        write_graph(
            &root,
            &[
                prop("n_a", "Alfa"),
                prop("n_b", "Beta"),
                prop("n_c", "Passo veloce"),
                prop("n_d", "Zeta"),
                prop("n_e", "Passo lento"),
            ],
            &[],
        );
        let cat = load_catalog(&root);
        let cosine: HashMap<&str, f64> = [
            ("n_a", 0.8),
            ("n_b", 0.5),
            ("n_c", 0.4),
            ("n_d", 0.1),
            ("n_e", 0.1),
        ]
        .into_iter()
        .collect();
        let ranker: Ranker<'_> = &|_query, nodes| {
            Ok(nodes
                .iter()
                .map(|node| RankedHit {
                    id: node.id.clone(),
                    cosine: cosine.get(node.id.as_str()).copied().unwrap_or(0.0),
                })
                .collect())
        };

        let out = search_concepts(&cat, "passo", SEARCH_LIMIT, Some(ranker), min_score(&root));
        assert!(!out.is_error, "{}", out.text);
        let ids: Vec<&str> = out
            .text
            .split('\n')
            .skip(1)
            .map(|line| {
                line.strip_prefix("- ")
                    .unwrap_or(line)
                    .split(" | ")
                    .next()
                    .unwrap_or("")
            })
            .collect();
        // n_c's bonus beats n_b; n_d is under minScore and not lexical, so it is dropped;
        // n_e is under minScore too but matches lexically, so it is kept.
        assert_eq!(ids, ["n_a", "n_c", "n_b", "n_e"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ac7_an_added_concept_is_found_and_a_deleted_one_is_gone_without_restart() {
        let root = make_data("search-ac7");
        write_graph(&root, &[prop("n_base", "Concetto base")], &[]);
        let min = min_score(&root);
        let ranker: Ranker<'_> = &|_query, nodes| {
            Ok(nodes
                .iter()
                .map(|node| RankedHit {
                    id: node.id.clone(),
                    cosine: if node.name.contains("nuovo") { 1.0 } else { 0.0 },
                })
                .collect())
        };

        let mut cat = load_catalog(&root);
        assert!(
            !search_concepts(&cat, "nuovo", SEARCH_LIMIT, Some(ranker), min)
                .text
                .contains("n_nuovo")
        );

        write_graph(
            &root,
            &[
                prop("n_base", "Concetto base"),
                prop("n_nuovo", "Concetto nuovo"),
            ],
            &[],
        );
        cat = load_catalog(&root);
        assert!(
            search_concepts(&cat, "nuovo", SEARCH_LIMIT, Some(ranker), min)
                .text
                .contains("n_nuovo | Concetto nuovo")
        );

        GraphStore::new(root.clone()).delete_node("n_nuovo").unwrap();
        cat = load_catalog(&root);
        assert!(
            !search_concepts(&cat, "nuovo", SEARCH_LIMIT, Some(ranker), min)
                .text
                .contains("n_nuovo")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn open_shows_the_concept_its_provenance_map_keys_and_chain_neighbours() {
        let root = make_data("open-full");
        write_lesson(
            &root,
            "demo",
            "0001-demo.html",
            "demo",
            "Demo",
            &[("alpha", "Alfa"), ("beta", "Beta"), ("gamma", "Gamma")],
            1,
        );
        let chat_id = "demo/0001-demo";
        write_graph(
            &root,
            &[
                prop_in("n_a", "Alfa", json!([{ "chatId": chat_id, "messageId": "alpha" }])),
                prop_in("n_b", "Beta", json!([{ "chatId": chat_id, "messageId": "beta" }])),
                prop_in("n_c", "Gamma", json!([{ "chatId": chat_id, "messageId": "gamma" }])),
            ],
            &[chain("arg_1", &["n_a", "n_b"], "n_c")],
        );
        let cat = load_catalog(&root);

        let c = open_concept(&cat, "n_c");
        assert!(!c.is_error);
        assert!(c.text.starts_with("# Gamma (n_c)"), "{}", c.text);
        assert!(c.text.contains("Statement: Gamma vale."), "{}", c.text);
        assert!(c.text.contains("Status: insegnato"), "{}", c.text);
        assert!(c.text.contains("Courses: Demo (demo)"), "{}", c.text);
        assert!(
            c.text.contains("Lessons: Lezione 1 · Demo (demo/0001-demo)"),
            "{}",
            c.text
        );
        assert!(
            c.text
                .contains("MapNode keys (for problem.nodeKeys): demo/demo#gamma"),
            "{}",
            c.text
        );
        assert!(
            c.text.contains("Premises:\n- n_a | Alfa\n- n_b | Beta"),
            "{}",
            c.text
        );
        assert!(c.text.contains("Consequences: (none)"), "{}", c.text);

        let a = open_concept(&cat, "n_a");
        assert!(a.text.contains("Premises: (none)"), "{}", a.text);
        assert!(a.text.contains("Consequences:\n- n_c | Gamma"), "{}", a.text);

        let bad = open_concept(&cat, "nope");
        assert!(bad.is_error);
        assert_eq!(
            bad.text,
            "Unknown concept id \"nope\". Use search_concepts or browse_concepts to find ids."
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn open_lists_aliases_every_distinct_course_and_lesson_and_says_when_no_key_exists() {
        let root = make_data("open-multi");
        let mut multi = prop_in(
            "n_multi",
            "Multi",
            json!([
                { "chatId": "demo/0001-demo", "messageId": "missing-node" },
                { "chatId": "altra/0002-due", "messageId": "" },
                { "chatId": "demo/0001-demo", "messageId": "missing-node" },
            ]),
        );
        multi["aliases"] = json!(["sinonimo uno", "sinonimo due"]);
        write_graph(&root, &[multi], &[]);
        let cat = load_catalog(&root);

        let out = open_concept(&cat, "n_multi");
        assert!(!out.is_error);
        assert!(
            out.text.contains("Aliases: sinonimo uno, sinonimo due"),
            "{}",
            out.text
        );
        // birth point first, duplicates collapsed, unknown slugs keep their slug
        assert!(
            out.text.contains("Courses: Demo (demo); altra (altra)"),
            "{}",
            out.text
        );
        assert!(
            out.text.contains(
                "Lessons: demo/0001-demo (demo/0001-demo); altra/0002-due (altra/0002-due)"
            ),
            "{}",
            out.text
        );
        assert!(out.text.contains("MapNode keys: none"), "{}", out.text);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_with_no_input_lists_courses_with_counts_standalone_last() {
        let (root, cat) = demo_catalog("browse-courses");
        let out = browse_concepts(&cat, None, None);
        assert!(!out.is_error);
        let lines: Vec<&str> = out.text.split('\n').collect();
        assert_eq!(lines[0], "Courses (slug | title | concepts):");
        assert_eq!(
            lines[1..],
            [
                "- demo | Demo | 3 concepts",
                "- altra | altra | 1 concepts",
                "- lezioni-singole | Lezioni singole | 1 concepts"
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_of_a_course_lists_its_birth_lessons_with_counts() {
        let (root, cat) = demo_catalog("browse-lessons");
        let out = browse_concepts(&cat, Some("demo"), None);
        assert!(!out.is_error);
        let lines: Vec<&str> = out.text.split('\n').collect();
        assert_eq!(lines[0], "Lessons of Demo (lesson | label | concepts):");
        assert_eq!(
            lines[1..],
            [
                "- demo/0001-demo | demo/0001-demo | 2 concepts",
                "- demo/0002-secondo | demo/0002-secondo | 1 concepts"
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_of_a_lesson_accepts_the_stem_or_the_full_id() {
        let (root, cat) = demo_catalog("browse-concepts");
        let stem = browse_concepts(&cat, Some("demo"), Some("0001-demo"));
        let full = browse_concepts(&cat, Some("demo"), Some("demo/0001-demo"));
        assert!(!stem.is_error);
        assert_eq!(stem.text, full.text);
        let lines: Vec<&str> = stem.text.split('\n').collect();
        assert_eq!(
            lines[0],
            "Concepts of demo/0001-demo (id | name | status | course | lesson):"
        );
        assert_eq!(
            lines[1..],
            [
                "- n1 | Uno | insegnato | Demo | demo/0001-demo",
                "- n2 | Due | insegnato | Demo | demo/0001-demo"
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_errors_are_readable_for_an_unknown_course_lesson_or_a_lesson_without_course() {
        let (root, cat) = demo_catalog("browse-errors");
        let no_course = browse_concepts(&cat, Some("nope"), None);
        assert!(no_course.is_error);
        assert_eq!(
            no_course.text,
            "No concepts in course \"nope\". Courses with concepts: demo, altra, lezioni-singole."
        );

        let no_lesson = browse_concepts(&cat, Some("demo"), Some("9999-nope"));
        assert!(no_lesson.is_error);
        assert_eq!(
            no_lesson.text,
            "No concepts born in lesson \"demo/9999-nope\". Call browse_concepts with course \"demo\" to list its lessons."
        );

        let dangling = browse_concepts(&cat, None, Some("0001-demo"));
        assert!(dangling.is_error);
        assert_eq!(dangling.text, "Pass course with lesson.");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_of_an_empty_graph_says_so() {
        let root = make_data("browse-empty");
        let cat = load_catalog(&root);
        let out = browse_concepts(&cat, None, None);
        assert!(!out.is_error);
        assert_eq!(out.text, "The knowledge graph is empty: no concept yet.");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_truncates_a_long_lesson_with_a_pointer_to_search() {
        let root = make_data("browse-limit");
        let nodes: Vec<Value> = (0..BROWSE_LIMIT + 2)
            .map(|i| prop(&format!("n_b{i}"), &format!("Concetto {i}")))
            .collect();
        write_graph(&root, &nodes, &[]);
        let cat = load_catalog(&root);

        let out = browse_concepts(&cat, Some("demo"), Some("0001-demo"));
        assert!(!out.is_error);
        let lines: Vec<&str> = out.text.split('\n').collect();
        assert_eq!(lines.len(), 1 + BROWSE_LIMIT + 1);
        assert_eq!(lines.last(), Some(&"(+2 more: use search_concepts)"));
        let _ = fs::remove_dir_all(&root);
    }
}
