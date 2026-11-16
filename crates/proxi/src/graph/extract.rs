//! The map extractor's inputs (port of `app/server/graph/extract.ts`): the lesson text the extractor
//! reads (`htmlToText`, `readLessonText`), the anchor lookup, the coverage ledger and the source list.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::events;
use crate::graph::store::{validate_findings, GraphStore};
use crate::jobs::roles::{read_prompt, Role, RoleEnv};
use crate::js;
use crate::lesson::artifact::{lesson_rel, read_lesson, read_lesson_html};
use crate::protocol::GlobalEvent;
use crate::sessions::{invalid_type, issue};
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolOutcome};
use crate::vitest::truncate_utf16;

/// Node's `ENTITIES` table (`extract.ts:36-38`): the exact case wins, the lowercase spelling is the fallback.
const ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", " "),
    ("agrave", "à"),
    ("egrave", "è"),
    ("igrave", "ì"),
    ("ograve", "ò"),
    ("ugrave", "ù"),
    ("eacute", "é"),
    ("Agrave", "À"),
    ("Egrave", "È"),
    ("Eacute", "É"),
    ("hellip", "…"),
    ("mdash", "—"),
    ("ndash", "–"),
];

/// Tags whose whole block disappears, `head` included.
const RAW_BLOCKS: [&str; 4] = ["script", "style", "svg", "head"];

/// Closing tags that read as a paragraph break.
const BLOCK_CLOSERS: [&str; 14] = [
    "p",
    "div",
    "li",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "tr",
    "pre",
    "blockquote",
    "section",
    "article",
];

/// The lesson's visible text: raw blocks and markup gone, breaks as newlines, entities decoded.
pub fn html_to_text(html: &str) -> String {
    let text = drop_raw_blocks(html);
    let text = breaks_to_newlines(&text);
    let text = js::strip_tags(&text);
    let text = decode_entities(&text);
    let text = drop_blanks_before_newlines(&text);
    let text = collapse_newlines(&text);
    js::js_trim(&text).to_string()
}

/// `/<(script|style|svg|head)[\s\S]*?<\/\1>/gi` -> `" "`.
///
/// Node's pattern has no name boundary, so `<header>` opens a `head` block that runs to the first
/// `</head>`, and `</header>` never closes it: left as is, the coverage fingerprint must not move.
/// A block with no closing tag is left for the tag stripper.
fn drop_raw_blocks(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut kept_from = 0;
    let mut at = 0;
    while let Some(rel) = html[at..].find('<') {
        let open = at + rel;
        at = open + 1;
        let Some(name) = RAW_BLOCKS
            .iter()
            .find(|name| starts_with_ci(&html[open + 1..], name))
        else {
            continue;
        };
        let closer = format!("</{name}>");
        let Some(end) = js::find_ci(html, &closer, open + 1 + name.len()) else {
            continue;
        };
        out.push_str(&html[kept_from..open]);
        out.push(' ');
        kept_from = end + closer.len();
        at = kept_from;
    }
    out.push_str(&html[kept_from..]);
    out
}

/// `/<br\s*\/?>|<\/(p|div|li|h[1-6]|tr|pre|blockquote|section|article)>/gi` -> `"\n"`.
fn breaks_to_newlines(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut kept_from = 0;
    let mut at = 0;
    while let Some(rel) = html[at..].find('<') {
        let open = at + rel;
        match break_tag_end(&html[open..]) {
            Some(len) => {
                out.push_str(&html[kept_from..open]);
                out.push('\n');
                at = open + len;
                kept_from = at;
            }
            None => at = open + 1,
        }
    }
    out.push_str(&html[kept_from..]);
    out
}

/// Byte length of the tag at `s` when it is one Node turns into a newline; `s` starts with `<`.
fn break_tag_end(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('<')?;
    if let Some(after) = strip_prefix_ci(rest, "br") {
        let mut blanks = 0;
        for c in after.chars() {
            if !js::is_js_ws(c) {
                break;
            }
            blanks += c.len_utf8();
        }
        let after = after[blanks..]
            .strip_prefix('/')
            .unwrap_or(&after[blanks..]);
        return after.strip_prefix('>').map(|tail| s.len() - tail.len());
    }
    let after = rest.strip_prefix('/')?;
    BLOCK_CLOSERS.iter().find_map(|name| {
        let tail = strip_prefix_ci(after, name)?.strip_prefix('>')?;
        Some(s.len() - tail.len())
    })
}

/// `&(#x?[0-9a-f]+|\w+);` decoded, case-insensitively; anything else is left alone.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(rel) = text[at..].find('&') {
        let start = at + rel;
        match entity_at(&text[start..]) {
            Some((len, decoded)) => {
                out.push_str(&text[at..start]);
                out.push_str(&decoded);
                at = start + len;
            }
            None => {
                out.push_str(&text[at..=start]);
                at = start + 1;
            }
        }
    }
    out.push_str(&text[at..]);
    out
}

/// `(byte length including `&` and `;`, decoded text)` for the entity at `s`, which starts with `&`.
fn entity_at(s: &str) -> Option<(usize, String)> {
    let body = &s[1..];
    let bytes = body.as_bytes();
    let numeric = bytes.first() == Some(&b'#');
    let first_digit = if numeric && matches!(bytes.get(1), Some(b'x' | b'X')) {
        2
    } else if numeric {
        1
    } else {
        0
    };
    let mut end = first_digit;
    while bytes.get(end).is_some_and(|b| {
        if numeric {
            b.is_ascii_hexdigit()
        } else {
            b.is_ascii_alphanumeric() || *b == b'_'
        }
    }) {
        end += 1;
    }
    if end == first_digit || bytes.get(end) != Some(&b';') {
        return None;
    }
    let inner = &body[..end];
    Some((end + 2, decode_entity(inner, &s[..end + 2])))
}

/// The decoded entity, or the matched text when Node keeps it as it is.
fn decode_entity(inner: &str, original: &str) -> String {
    let Some(rest) = inner.strip_prefix('#') else {
        let lower = inner.to_ascii_lowercase();
        return ENTITIES
            .iter()
            .find(|(name, _)| *name == inner)
            .or_else(|| ENTITIES.iter().find(|(name, _)| *name == lower.as_str()))
            .map_or_else(|| original.to_string(), |(_, value)| (*value).to_string());
    };
    let (digits, radix) = match rest.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (rest, 10),
    };
    // A lone surrogate is the one divergence: Node yields an unpaired code unit, which a Rust
    // `char` cannot hold, so the entity text stays.
    match parse_int(digits, radix).and_then(char::from_u32) {
        Some(c) if c != '\0' => c.to_string(),
        _ => original.to_string(),
    }
}

/// `parseInt` over the digits the entity regex already matched, saturating far past the Unicode range.
fn parse_int(digits: &str, radix: u32) -> Option<u32> {
    let mut value: u32 = 0;
    let mut any = false;
    for b in digits.bytes() {
        let Some(digit) = (b as char).to_digit(radix) else {
            break;
        };
        value = value.saturating_mul(radix).saturating_add(digit);
        any = true;
    }
    any.then_some(value)
}

/// `/[ \t]+\n/g` -> `"\n"`.
fn drop_blanks_before_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = String::new();
    for c in text.chars() {
        match c {
            '\n' => {
                blanks.clear();
                out.push('\n');
            }
            ' ' | '\t' => blanks.push(c),
            _ => {
                out.push_str(&blanks);
                blanks.clear();
                out.push(c);
            }
        }
    }
    out.push_str(&blanks);
    out
}

/// `/\n{3,}/g` -> `"\n\n"`.
fn collapse_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
            if run <= 2 {
                out.push('\n');
            }
        } else {
            run = 0;
            out.push(c);
        }
    }
    out
}

/// ASCII case-insensitive `str::strip_prefix`.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    strip_prefix_ci(s, prefix).is_some()
}

/// The extractor's text budget, in UTF-16 units (`MAX_TEXT`, `extract.ts:23`).
const MAX_TEXT: usize = 60_000;

/// `<course>/<stem>` -> repo-relative lesson path; null when the id is not a plausible lesson id.
pub fn source_rel(source: &str) -> Option<String> {
    let mut parts = source.split('/');
    let course = parts.next().unwrap_or("");
    let stem = parts.next().unwrap_or("");
    if course.is_empty() || stem.is_empty() || parts.next().is_some() {
        return None;
    }
    lesson_rel(&format!("courses/{course}/lessons/{stem}.html"))
}

/// The extractor's view of one lesson; its hash is the coverage fingerprint.
pub struct LessonText {
    pub title: String,
    pub course: String,
    pub lesson_number: f64,
    /// Repo-relative.
    pub lesson_path: String,
    pub text: String,
}

/// A lesson as text, or `None` when the path is not a lesson id or the HTML is missing.
///
/// An unreadable data island falls back to the visible prose, like Node's `try { readLesson } catch`.
pub fn read_lesson_text(root: &Path, source: &str) -> Option<LessonText> {
    let rel = source_rel(source)?;
    let raw = read_lesson_html(root, &rel).ok()??;
    let doc = read_lesson(root, &rel)
        .ok()
        .flatten()
        .map(|lesson| lesson.doc);

    let mut parts = source.split('/');
    let course = parts.next().unwrap_or("").to_string();
    let stem = parts.next().unwrap_or("");
    let metadata = doc.as_ref().and_then(|doc| doc.get("metadata"));
    let title = metadata
        .and_then(|metadata| metadata.get("title"))
        .filter(|value| js::truthy(Some(value)))
        .map(|value| js::display(Some(value)))
        .or_else(|| {
            let text = html_to_text(title_tag(&raw).unwrap_or(""));
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_else(|| stem.to_string());
    let lesson_number = metadata
        .and_then(|metadata| metadata.get("lessonNumber"))
        .filter(|value| js::truthy(Some(value)))
        .map(|value| js::to_number(Some(value)))
        .filter(is_js_number)
        .or_else(|| stem_number(stem))
        .filter(is_js_number)
        .unwrap_or(0.0);

    let text = match doc
        .as_ref()
        .and_then(|doc| doc.get("nodes"))
        .and_then(Value::as_array)
        .filter(|nodes| !nodes.is_empty())
    {
        Some(nodes) => nodes.iter().map(node_text).collect::<Vec<_>>().join("\n\n"),
        None => html_to_text(body_text(&raw).unwrap_or(&raw)),
    };
    Some(LessonText {
        title,
        course,
        lesson_number,
        lesson_path: rel,
        text: truncate_utf16(&text, MAX_TEXT),
    })
}

/// JS truthiness of a number: 0 and NaN fall through to the next fallback.
fn is_js_number(value: &f64) -> bool {
    *value != 0.0 && !value.is_nan()
}

/// `Number(/^(\d+)-/.exec(stem)?.[1])` as an option: no match is `None`.
fn stem_number(stem: &str) -> Option<f64> {
    let digits = stem.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || stem.as_bytes().get(digits) != Some(&b'-') {
        return None;
    }
    stem[..digits].parse().ok()
}

static TITLE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<title>(.*?)</title>").expect("title regex"));
static BODY_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<body[^>]*>(.*)</body>").expect("body regex"));

/// `/<title>([\s\S]*?)<\/title>/i` first capture.
fn title_tag(raw: &str) -> Option<&str> {
    TITLE_TAG.captures(raw)?.get(1).map(|found| found.as_str())
}

/// `/<body[^>]*>([\s\S]*)<\/body>/i` first capture; the greedy group ends at the last close tag.
fn body_text(raw: &str) -> Option<&str> {
    BODY_TAG.captures(raw)?.get(1).map(|found| found.as_str())
}

/// One node block of the extractor input. Anchors go by name: the fingerprint must not move when a
/// concept is deleted.
fn node_text(node: &Value) -> String {
    let mut parts = vec![
        format!(
            "## Nodo {}: {} ({})",
            js::display(node.get("nodeId")),
            js::display(node.get("title")),
            js::display(node.get("tier"))
        ),
        html_to_text(node.get("content").and_then(Value::as_str).unwrap_or("")),
    ];
    if let Some(anchors) = node
        .get("anchors")
        .and_then(Value::as_array)
        .filter(|anchors| !anchors.is_empty())
    {
        let names: Vec<String> = anchors
            .iter()
            .map(|anchor| {
                anchor
                    .get("name")
                    .map(|name| js::display(Some(name)))
                    .unwrap_or_default()
            })
            .collect();
        parts.push(format!("Si appoggia su: {}", names.join("; ")));
    }
    if let Some(connection) = node
        .get("connection")
        .and_then(Value::as_str)
        .filter(|connection| !connection.is_empty())
    {
        parts.push(html_to_text(connection));
    }
    if let Some(quiz) = node.get("quiz").filter(|quiz| js::truthy(Some(quiz))) {
        parts.push(format!(
            "Domanda di verifica: {}",
            html_to_text(quiz.get("question").and_then(Value::as_str).unwrap_or(""))
        ));
        parts.push(format!("Risposta corretta: {}", correct_answer(quiz)));
    }
    parts.join("\n\n")
}

/// `q.correctAnswer ?? options[q.correctIndex - 1] ?? ""`, then as visible text.
fn correct_answer(quiz: &Value) -> String {
    let picked = match quiz.get("correctAnswer") {
        Some(correct) if !correct.is_null() => correct.as_str().unwrap_or(""),
        _ => match (
            quiz.get("options")
                .filter(|options| js::truthy(Some(options))),
            quiz.get("correctIndex"),
        ) {
            (Some(options), Some(index)) => option_at(options, js::to_number(Some(index)) - 1.0),
            _ => "",
        },
    };
    html_to_text(picked)
}

/// `options[index] ?? ""`: only an integral, in-range index names an element.
fn option_at(options: &Value, index: f64) -> &str {
    if !(index.is_finite() && index.fract() == 0.0 && index >= 0.0) {
        return "";
    }
    match options {
        Value::Array(items) => items
            .get(index as usize)
            .and_then(Value::as_str)
            .unwrap_or(""),
        _ => "",
    }
}

/// `sha256(title + "\n" + text)` in hex: what marks coverage stale when the lesson changes.
pub fn fingerprint(lesson: &LessonText) -> String {
    let digest = Sha256::digest(format!("{}\n{}", lesson.title, lesson.text).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Lesson node id -> the anchors `write_node` stored on it. Missing/unreadable lesson: empty map.
pub fn anchors_by_node(root: &Path, source: &str) -> IndexMap<String, Vec<(String, String)>> {
    let mut out = IndexMap::new();
    let Some(rel) = source_rel(source) else {
        return out;
    };
    let Ok(Some(lesson)) = read_lesson(root, &rel) else {
        return out;
    };
    let Some(nodes) = lesson.doc.get("nodes").and_then(Value::as_array) else {
        return out;
    };
    for node in nodes {
        let Some(anchors) = node
            .get("anchors")
            .and_then(Value::as_array)
            .filter(|anchors| !anchors.is_empty())
        else {
            continue;
        };
        let node_id = node
            .get("nodeId")
            .map(|id| js::display(Some(id)))
            .unwrap_or_default();
        let list = anchors
            .iter()
            .map(|anchor| {
                (
                    anchor
                        .get("id")
                        .map(|id| js::display(Some(id)))
                        .unwrap_or_default(),
                    anchor
                        .get("name")
                        .map(|name| js::display(Some(name)))
                        .unwrap_or_default(),
                )
            })
            .collect();
        out.insert(node_id, list);
    }
    out
}

/// Turn each live declared anchor into a chain `anchor -> the node's proposition`, so the arrows show
/// the cross-lesson link even when the model forgot it. Dangling anchors are skipped; `f` is untouched.
pub fn with_anchor_chains(
    f: &Value,
    anchors: &IndexMap<String, Vec<(String, String)>>,
    live: &HashSet<String>,
) -> Value {
    let mut out = f.as_object().cloned().unwrap_or_default();
    let mut chains: Vec<Value> = f
        .get("chains")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let propositions = f.get("propositions").and_then(Value::as_array);
    for (node_id, list) in anchors {
        let Some(proposition) = propositions.and_then(|list| {
            list.iter()
                .find(|entry| entry.get("nodeId").and_then(Value::as_str) == Some(node_id.as_str()))
        }) else {
            continue; // the node was not extracted (or the model linked it elsewhere)
        };
        let conclusion = proposition.get("id").cloned().unwrap_or(Value::Null);
        let linked_to = proposition.get("linkTo").and_then(Value::as_str);
        for (id, _name) in list {
            if !live.contains(id) || linked_to == Some(id.as_str()) {
                continue;
            }
            let known = chains.iter().any(|chain| {
                chain.get("conclusion") == Some(&conclusion)
                    && chain
                        .get("premises")
                        .and_then(Value::as_array)
                        .is_some_and(|premises| {
                            premises.iter().any(|p| p.as_str() == Some(id.as_str()))
                        })
            });
            if known {
                continue;
            }
            chains.push(serde_json::json!({ "premises": [id], "conclusion": conclusion }));
        }
    }
    out.insert("chains".to_string(), Value::Array(chains));
    Value::Object(out)
}

/// One line of `graph/coverage.jsonl`: the text the extractor was shown, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub source: String,
    pub fp: String,
    pub at: String,
}

/// The coverage ledger; a corrupt or blank line is skipped and the last entry for a source wins.
pub fn read_coverage(root: &Path) -> IndexMap<String, Coverage> {
    let mut out = IndexMap::new();
    let Ok(raw) = fs::read(root.join("graph").join("coverage.jsonl")) else {
        return out;
    };
    let text = String::from_utf8_lossy(&raw);
    for line in text.split('\n') {
        let Ok(value) = js::parse(line) else {
            continue;
        };
        let Some(entry) = value.as_object() else {
            continue;
        };
        let (Some(source), Some(fp)) = (
            entry.get("source").and_then(Value::as_str),
            entry.get("fp").and_then(Value::as_str),
        ) else {
            continue;
        };
        out.insert(
            source.to_string(),
            Coverage {
                source: source.to_string(),
                fp: fp.to_string(),
                at: entry
                    .get("at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            },
        );
    }
    out
}

pub fn append_coverage(root: &Path, source: &str, fp: &str) -> io::Result<()> {
    let dir = root.join("graph");
    fs::create_dir_all(&dir)?;
    let mut row = Map::new();
    row.insert("source".to_string(), Value::String(source.to_string()));
    row.insert("fp".to_string(), Value::String(fp.to_string()));
    row.insert("at".to_string(), Value::String(events::iso_now()));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("coverage.jsonl"))?;
    file.write_all(format!("{}\n", Value::Object(row)).as_bytes())
}

/// Every lesson under `courses/<course>/lessons/*.html` with covered/stale/isolated.
pub fn list_sources(root: &Path, graph: &GraphStore) -> Vec<Value> {
    let coverage = read_coverage(root);
    let isolated = graph.isolated_by_source();
    let mut out = Vec::new();
    let courses_dir = root.join("courses");
    let Ok(entries) = fs::read_dir(&courses_dir) else {
        return out;
    };
    let mut courses: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    courses.sort();
    for course in courses {
        let Ok(entries) = fs::read_dir(courses_dir.join(&course).join("lessons")) else {
            continue;
        };
        let mut files: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|file| file.ends_with(".html"))
            .collect();
        files.sort();
        for file in files {
            let id = format!("{course}/{}", file.strip_suffix(".html").unwrap_or(&file));
            let Some(lesson) = read_lesson_text(root, &id) else {
                continue;
            };
            let covered = coverage.get(&id);
            let stale = covered.is_some_and(|entry| entry.fp != fingerprint(&lesson));
            let rows = isolated.get(&id).map(Vec::len).unwrap_or(0);
            let mut row = Map::new();
            row.insert("id".to_string(), Value::String(id));
            row.insert("course".to_string(), Value::String(course.clone()));
            row.insert("lessonTitle".to_string(), Value::String(lesson.title));
            row.insert("lessonNumber".to_string(), js::number(lesson.lesson_number));
            row.insert("lessonPath".to_string(), Value::String(lesson.lesson_path));
            row.insert("covered".to_string(), Value::Bool(covered.is_some()));
            row.insert("stale".to_string(), Value::Bool(stale));
            row.insert("isolated".to_string(), Value::from(rows));
            out.push(Value::Object(row));
        }
    }
    out
}

/// Not extracted yet, changed since, or holding ideas no chain uses.
pub fn needs_mapping(source: &Value) -> bool {
    !js::truthy(source.get("covered"))
        || js::truthy(source.get("stale"))
        || js::to_number(source.get("isolated")) > 0.0
}

pub fn map_job_title(lesson_title: &str) -> String {
    format!("Mappa: {lesson_title}")
}

static EXISTING_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(n_[^\s|]+)\s*\|").expect("existing id regex"));

/// The `n_…` ids of a graph digest: the propositions an extractor may link with `linkTo`.
pub fn existing_ids(digest: &str) -> HashSet<String> {
    EXISTING_ID
        .captures_iter(digest)
        .map(|caps| caps[1].to_string())
        .collect()
}

/// One accepted submission per job: a repeated submit_findings (model retry after a lost reply) must not merge twice.
static SUBMITTED: LazyLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<ToolOutcome>>>>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Called when the job settles: drops per-job in-memory state.
pub fn cleanup(job_id: &str) {
    if let Ok(mut map) = SUBMITTED.lock() {
        map.remove(job_id);
    }
}

/// The map role: 6 turns, no built-in grants, extracts knowledge graph findings from a completed lesson.
pub static MAP_ROLE: Role = Role {
    kind: "map",
    grant: &[],
    max_turns: 6,
    system_prompt: map_system_prompt,
    first_message: map_first_message,
    tools: map_tools,
    cleanup,
};

fn map_system_prompt(env: &RoleEnv, _input: &Value) -> Result<String, String> {
    let base = read_prompt(env.code_root, "map.md")?;
    let digest = env.graph.graph_digest();
    let trimmed = digest.trim();
    let graph_section = if trimmed.is_empty() {
        "(vuoto: nessuna proposizione ancora)"
    } else {
        trimmed
    };
    Ok(format!("{base}\n\n---\n\n# Il grafo finora\n\n{graph_section}\n"))
}

fn map_first_message(env: &RoleEnv, input: &mut Value) -> String {
    let source = input.get("source").and_then(Value::as_str).unwrap_or("").to_string();
    let Some(l) = read_lesson_text(env.data_root, &source) else {
        return format!("Lezione non trovata: {source}. Consegna liste vuote.");
    };
    let fp = fingerprint(&l);
    let mut isolated_by_source = env.graph.isolated_by_source();
    let isolated_here = isolated_by_source.swap_remove(&source).unwrap_or_default();
    if !input.is_object() {
        *input = Value::Object(Map::new());
    }
    if let Some(obj) = input.as_object_mut() {
        obj.insert("fp".to_string(), Value::String(fp));
        obj.insert(
            "isolated".to_string(),
            Value::Array(
                isolated_here
                    .iter()
                    .map(|(id, _)| Value::String(id.clone()))
                    .collect(),
            ),
        );
    }
    let live: HashSet<String> = env
        .graph
        .read_node_lines()
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let anchored: Vec<String> = anchors_by_node(env.data_root, &source)
        .into_iter()
        .flat_map(|(nid, list)| {
            list.into_iter()
                .filter(|(id, _)| live.contains(id))
                .map(move |(id, name)| format!("- nodo {nid} \u{2192} {id} | {name}"))
        })
        .collect();
    let anchors_block = if !anchored.is_empty() {
        format!(
            "\n\nAncore dichiarate dal tutor (concetti gi\u{e0} nella mappa su cui il nodo costruisce):\n{}",
            anchored.join("\n")
        )
    } else {
        String::new()
    };
    let repair = if !isolated_here.is_empty() {
        let lines: Vec<String> = isolated_here
            .iter()
            .map(|(id, name)| format!("- {id} | {name}"))
            .collect();
        format!(
            "\n\nIdee di questa lezione gi\u{e0} nella mappa ma isolate (nessuna catena le usa). Collegale ora, ciascuna in almeno una catena, citandole col loro id; se una non \u{e8} davvero portante, mettila in remove:\n{}",
            lines.join("\n")
        )
    } else {
        String::new()
    };
    format!(
        "Lezione: {}\nCorso: {}\nSorgente: {source}\n\n{}{anchors_block}{repair}\n\nEstrai le proposizioni e le catene, poi chiama submit_findings.",
        l.title, l.course, l.text
    )
}

fn map_tools(_input: &Value) -> Vec<&'static ToolDef> {
    vec![&SUBMIT_FINDINGS]
}

/// The `submit_findings` tool: deliver the extraction for this lesson.
pub const SUBMIT_FINDINGS: ToolDef = ToolDef {
    name: "submit_findings",
    description: "Deliver the extraction for this lesson (call once). Returns validation errors to fix and resubmit, or confirms the merge.",
    schema: submit_findings_schema,
    validate: validate_findings_schema,
    run: run_submit_findings,
    writes: false,
};

fn submit_findings_schema() -> Value {
    serde_json::from_str(include_str!("../tools/schemas/submit_findings.json"))
        .expect("submit_findings schema parses")
}

fn validate_findings_schema(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut clean = Map::new();

    // propositions: z.array(...)
    match object.get("propositions") {
        None => issues.push(issue("propositions", &invalid_type("array", None))),
        Some(props) => match props.as_array() {
            None => issues.push(issue("propositions", &invalid_type("array", Some(props)))),
            Some(items) => {
                let mut clean_props = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let path = format!("propositions.{i}");
                    let Some(prop_obj) = item.as_object() else {
                        issues.push(issue(&path, &invalid_type("object", Some(item))));
                        continue;
                    };
                    let mut clean_prop = Map::new();

                    match prop_obj.get("id") {
                        Some(Value::String(s)) => {
                            clean_prop.insert("id".to_string(), Value::String(s.clone()));
                        }
                        Some(other) => issues.push(issue(&format!("{path}.id"), &invalid_type("string", Some(other)))),
                        None => issues.push(issue(&format!("{path}.id"), &invalid_type("string", None))),
                    }

                    match prop_obj.get("name") {
                        Some(Value::String(s)) => {
                            clean_prop.insert("name".to_string(), Value::String(s.clone()));
                        }
                        Some(other) => issues.push(issue(&format!("{path}.name"), &invalid_type("string", Some(other)))),
                        None => issues.push(issue(&format!("{path}.name"), &invalid_type("string", None))),
                    }

                    match prop_obj.get("statement") {
                        Some(Value::String(s)) => {
                            clean_prop.insert("statement".to_string(), Value::String(s.clone()));
                        }
                        Some(other) => issues.push(issue(&format!("{path}.statement"), &invalid_type("string", Some(other)))),
                        None => issues.push(issue(&format!("{path}.statement"), &invalid_type("string", None))),
                    }

                    match prop_obj.get("aliases") {
                        Some(Value::Array(arr)) => {
                            let mut clean_aliases = Vec::new();
                            for (j, alias) in arr.iter().enumerate() {
                                match alias {
                                    Value::String(s) => clean_aliases.push(Value::String(s.clone())),
                                    other => issues.push(issue(&format!("{path}.aliases.{j}"), &invalid_type("string", Some(other)))),
                                }
                            }
                            clean_prop.insert("aliases".to_string(), Value::Array(clean_aliases));
                        }
                        None => {}
                        Some(other) => issues.push(issue(&format!("{path}.aliases"), &invalid_type("array", Some(other)))),
                    }

                    match prop_obj.get("nodeId") {
                        Some(Value::String(s)) => {
                            clean_prop.insert("nodeId".to_string(), Value::String(s.clone()));
                        }
                        None => {}
                        Some(other) => issues.push(issue(&format!("{path}.nodeId"), &invalid_type("string", Some(other)))),
                    }

                    match prop_obj.get("linkTo") {
                        Some(Value::String(s)) => {
                            clean_prop.insert("linkTo".to_string(), Value::String(s.clone()));
                        }
                        None => {}
                        Some(other) => issues.push(issue(&format!("{path}.linkTo"), &invalid_type("string", Some(other)))),
                    }

                    clean_props.push(Value::Object(clean_prop));
                }
                clean.insert("propositions".to_string(), Value::Array(clean_props));
            }
        },
    }

    // chains: z.array(...)
    match object.get("chains") {
        None => issues.push(issue("chains", &invalid_type("array", None))),
        Some(chains_val) => match chains_val.as_array() {
            None => issues.push(issue("chains", &invalid_type("array", Some(chains_val)))),
            Some(items) => {
                let mut clean_chains = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let path = format!("chains.{i}");
                    let Some(chain_obj) = item.as_object() else {
                        issues.push(issue(&path, &invalid_type("object", Some(item))));
                        continue;
                    };
                    let mut clean_chain = Map::new();

                    match chain_obj.get("premises") {
                        None => issues.push(issue(&format!("{path}.premises"), &invalid_type("array", None))),
                        Some(premises_val) => match premises_val.as_array() {
                            None => issues.push(issue(&format!("{path}.premises"), &invalid_type("array", Some(premises_val)))),
                            Some(p_items) => {
                                if p_items.is_empty() {
                                    issues.push(issue(&format!("{path}.premises"), "Too small: expected array to have >=1 items"));
                                }
                                let mut clean_premises = Vec::new();
                                for (j, p_item) in p_items.iter().enumerate() {
                                    match p_item {
                                        Value::String(s) => clean_premises.push(Value::String(s.clone())),
                                        other => issues.push(issue(&format!("{path}.premises.{j}"), &invalid_type("string", Some(other)))),
                                    }
                                }
                                clean_chain.insert("premises".to_string(), Value::Array(clean_premises));
                            }
                        },
                    }

                    match chain_obj.get("conclusion") {
                        Some(Value::String(s)) => {
                            clean_chain.insert("conclusion".to_string(), Value::String(s.clone()));
                        }
                        Some(other) => issues.push(issue(&format!("{path}.conclusion"), &invalid_type("string", Some(other)))),
                        None => issues.push(issue(&format!("{path}.conclusion"), &invalid_type("string", None))),
                    }

                    clean_chains.push(Value::Object(clean_chain));
                }
                clean.insert("chains".to_string(), Value::Array(clean_chains));
            }
        },
    }

    // remove: z.array(z.string()).optional()
    match object.get("remove") {
        Some(Value::Array(arr)) => {
            let mut clean_remove = Vec::new();
            for (i, item) in arr.iter().enumerate() {
                match item {
                    Value::String(s) => clean_remove.push(Value::String(s.clone())),
                    other => issues.push(issue(&format!("remove.{i}"), &invalid_type("string", Some(other)))),
                }
            }
            clean.insert("remove".to_string(), Value::Array(clean_remove));
        }
        None => {}
        Some(other) => issues.push(issue("remove", &invalid_type("array", Some(other)))),
    }

    if issues.is_empty() {
        Ok(Value::Object(clean))
    } else {
        Err(issues)
    }
}

fn run_submit_findings<'a>(f: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(async move {
        let Some(ctx) = host.job() else {
            return ToolOutcome::error("submit_findings called outside a job context");
        };
        let input = ctx.input();
        let source = input.get("source").and_then(Value::as_str).unwrap_or("").to_string();
        let isolated: Vec<String> = input
            .get("isolated")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let live = existing_ids(&ctx.graph().graph_digest());
        let errors = validate_findings(&f, &live, &isolated);
        if !errors.is_empty() {
            return ToolOutcome::error(format!(
                "Findings rejected, fix these and call submit_findings again:\n- {}",
                errors.join("\n- ")
            ));
        }

        let job_id = ctx.job_id().to_string();
        let cell = {
            let mut map = SUBMITTED.lock().unwrap();
            map.entry(job_id)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
                .clone()
        };
        let mut guard = cell.lock().await;
        if let Some(outcome) = guard.as_ref() {
            return outcome.clone();
        }

        let fp = input
            .get("fp")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                read_lesson_text(host.data_root(), &source)
                    .map(|l| fingerprint(&l))
                    .unwrap_or_default()
            });

        let anchors = anchors_by_node(host.data_root(), &source);
        let g = with_anchor_chains(&f, &anchors, &live);

        let graph = Arc::clone(ctx.graph());
        let root = host.data_root().to_path_buf();
        let (source_clone, fp_clone) = (source.clone(), fp.clone());

        let merge_res = tokio::task::spawn_blocking(move || {
            graph.merge_findings(&source_clone, &g, || {
                if !fp_clone.is_empty() {
                    let _ = append_coverage(&root, &source_clone, &fp_clone);
                }
            })
        })
        .await;

        let counts = match merge_res {
            Ok(Ok(counts)) => counts,
            Ok(Err(io_err)) => return ToolOutcome::error(io_err.to_string()),
            Err(join_err) => return ToolOutcome::error(join_err.to_string()),
        };

        ctx.events().emit(&GlobalEvent::Graph {
            revision: ctx.graph().next_revision(),
        });
        ctx.finish(serde_json::json!({
            "kind": "map",
            "source": source,
            "added": counts.added,
            "linked": counts.linked,
            "chains": counts.chains,
        }));
        let outcome = ToolOutcome::ok(format!(
            "Merged: {} new, {} linked, {} chains. Done.",
            counts.added, counts.linked, counts.chains
        ));
        *guard = Some(outcome.clone());
        outcome
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::jobs::roles::FakeJob;
    use crate::tools::fake::{temp_dir, TempDir};
    use crate::tools::JobContext;

    fn node() -> Value {
        json!({
            "nodeId": "ref-scatola",
            "title": "Il ref",
            "tier": "axiom",
            "content": "<p>Un <code>ref</code> &egrave; una scatola &amp; basta.</p>",
            "quiz": {
                "question": "Che cosa è un ref?",
                "options": ["a", "una scatola"],
                "correctIndex": 2,
                "explanation": "e",
            },
        })
    }

    fn node_a() -> Value {
        let mut value = node();
        value["anchors"] =
            json!([{ "id": "n_aaa111", "name": "Ref" }, { "id": "n_gone", "name": "Sparito" }]);
        value
    }

    fn island(nodes: &[Value], title: &str) -> String {
        let doc = json!({
            "metadata": {
                "lessonNumber": 1,
                "topic": "t",
                "title": title,
                "slug": "s",
                "dateCreated": "",
                "dateModified": "",
                "version": "1",
            },
            "nodes": nodes,
        });
        format!(
            r#"<html><head><title>x</title></head><body><script type="application/json" id="lesson-data">{doc}</script><p>chrome</p></body></html>"#
        )
    }

    fn lesson_path(root: &Path, file: &str) -> PathBuf {
        root.join("courses").join("demo").join("lessons").join(file)
    }

    /// The two fixtures of `extract.test.ts`: a node lesson and a prose one.
    fn repo(tag: &str) -> TempDir {
        let root = temp_dir(tag);
        let lessons = root.join("courses").join("demo").join("lessons");
        fs::create_dir_all(&lessons).unwrap();
        fs::write(
            lessons.join("0001-il-ref.html"),
            island(&[node()], "Il ref è una scatola"),
        )
        .unwrap();
        fs::write(
            lessons.join("0002-prosa.html"),
            "<html><head><title>Lezione in prosa</title><style>p{}</style></head><body><h1>Titolo</h1><p>Le props scendono.</p></body></html>",
        )
        .unwrap();
        root
    }

    #[test]
    fn raw_blocks_go_with_their_content_in_any_case() {
        assert_eq!(
            html_to_text("<SCRIPT>a</SCRIPT><Style>b</STYLE><svg>c</svg><HEAD>d</HEAD>e"),
            "e"
        );
        assert_eq!(
            html_to_text("<head><title>t</title></head><p>body</p>"),
            "body"
        );
        assert_eq!(html_to_text("<script>if (a<b) {}</script>c"), "c");
        assert_eq!(html_to_text("<head>a</head>x"), "x");
        assert_eq!(html_to_text("<head</head>x"), "x");
    }

    #[test]
    fn a_dropped_block_leaves_a_space_behind() {
        assert_eq!(
            html_to_text("<p>a</p><script>var x = 1;</script><p>b</p>"),
            "a\n b"
        );
    }

    #[test]
    fn a_block_without_its_closing_tag_is_left_for_the_tag_stripper() {
        assert_eq!(html_to_text("<script>never closed"), "never closed");
        assert_eq!(html_to_text("<head>a</header>b"), "ab");
    }

    #[test]
    fn every_br_spelling_is_one_newline() {
        assert_eq!(
            html_to_text("<p>a<br>b<BR/>c<br />d<br\n/>e<br\t />f</p>"),
            "a\nb\nc\nd\ne\nf"
        );
        assert_eq!(html_to_text("a<br/>b"), "a\nb");
        assert_eq!(html_to_text("<br>"), "");
    }

    #[test]
    fn block_closing_tags_are_newlines() {
        assert_eq!(
            html_to_text(
                "a</div>b</li>c</h1>d</h6>e</tr>f</pre>g</blockquote>h</section>i</article>j</p>"
            ),
            "a\nb\nc\nd\ne\nf\ng\nh\ni\nj"
        );
        assert_eq!(html_to_text("<h1>T</h1><p>x</p>"), "T\nx");
        assert_eq!(html_to_text("<p>a</p><p>b</p>"), "a\nb");
    }

    #[test]
    fn a_spaced_closing_tag_is_no_break() {
        assert_eq!(html_to_text("a</p >b"), "ab");
    }

    #[test]
    fn tags_are_stripped_and_their_text_kept() {
        assert_eq!(html_to_text("<span class=\"x\">t</span>"), "t");
        assert_eq!(html_to_text("x<b>y</b>z"), "xyz");
        assert_eq!(html_to_text("a<!-- comment -->b"), "ab");
    }

    #[test]
    fn numeric_entities_decode_decimal_and_hex() {
        assert_eq!(html_to_text("&#65;&#x41;&#X41;"), "AAA");
        assert_eq!(html_to_text("&#1114111;"), "\u{10FFFF}");
        assert_eq!(html_to_text("&#x10FFFF;"), "\u{10FFFF}");
    }

    #[test]
    fn an_out_of_range_or_zero_code_point_keeps_the_entity() {
        assert_eq!(html_to_text("&#0;"), "&#0;");
        assert_eq!(html_to_text("&#x0;"), "&#x0;");
        assert_eq!(html_to_text("&#1114112;"), "&#1114112;");
        assert_eq!(html_to_text("&#x110000;"), "&#x110000;");
        assert_eq!(
            html_to_text("&#999999999999999999999;"),
            "&#999999999999999999999;"
        );
    }

    #[test]
    fn a_decimal_entity_stops_at_the_first_non_digit() {
        assert_eq!(html_to_text("&#1a;"), "\u{1}");
        assert_eq!(html_to_text("&#a1;"), "&#a1;");
    }

    #[test]
    fn a_lone_surrogate_keeps_the_entity_text() {
        // Node gives "\ud800" here; a Rust `char` cannot hold it (the module's one divergence).
        assert_eq!(html_to_text("&#xD800;"), "&#xD800;");
    }

    #[test]
    fn named_entities_use_the_exact_case_then_the_lowercase_fallback() {
        assert_eq!(html_to_text("&Egrave;&egrave;&EGRAVE;"), "Èèè");
        assert_eq!(html_to_text("&Agrave;&Eacute;&eacute;"), "ÀÉé");
        assert_eq!(html_to_text("&AMP;&Amp;&amp;"), "&&&");
        assert_eq!(html_to_text("&hellip;&mdash;&ndash;"), "…—–");
        assert_eq!(html_to_text("&amp;lt;"), "&lt;");
    }

    #[test]
    fn an_unknown_entity_stays_as_text() {
        assert_eq!(html_to_text("&foo;"), "&foo;");
        assert_eq!(html_to_text("A & B"), "A & B");
    }

    #[test]
    fn whitespace_before_a_newline_goes_and_three_newlines_become_two() {
        assert_eq!(html_to_text("a  \n  b\t\nc"), "a\n  b\nc");
        assert_eq!(html_to_text("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(html_to_text("<p>  a  </p>\n\n\n<p>b</p>"), "a\n\nb");
        assert_eq!(html_to_text("  a  "), "a");
    }

    #[test]
    fn a_carriage_return_is_not_a_newline() {
        assert_eq!(html_to_text("<p>a</p>\r\n<p>b</p>"), "a\n\r\nb");
    }

    #[test]
    fn malformed_html_is_text() {
        assert_eq!(html_to_text("a < b"), "a < b");
        assert_eq!(html_to_text("<p>unclosed"), "unclosed");
        assert_eq!(html_to_text("&#xZZ;"), "&#xZZ;");
        assert_eq!(html_to_text("<<>>"), ">");
        assert_eq!(html_to_text(""), "");
    }

    #[test]
    fn a_tag_name_is_not_a_word_boundary() {
        assert_eq!(html_to_text("<header>x</header>"), "x");
        assert_eq!(html_to_text("<header>x</header><head>y</head>z"), "z");
        assert_eq!(html_to_text("<header>x</head>er>"), "er>");
        assert_eq!(html_to_text("<headline>x</head>y"), "y");
        assert_eq!(html_to_text("<scripts>a</script>b"), "b");
        assert_eq!(html_to_text("<svgfoo>a</svg>b"), "b");
        assert_eq!(html_to_text("a<head>b</header>c</head>d"), "a d");
        assert_eq!(html_to_text("<p>a</p><header>T</header><p>b</p>"), "a\nTb");
    }

    #[test]
    fn source_rel_accepts_exactly_two_non_empty_parts() {
        assert_eq!(
            source_rel("demo/0001-il-ref").as_deref(),
            Some("courses/demo/lessons/0001-il-ref.html")
        );
        assert_eq!(
            source_rel("demo/0001-il-ref.html").as_deref(),
            Some("courses/demo/lessons/0001-il-ref.html.html")
        );
        for bad in ["demo/", "/demo", "demo/a/b", "../etc/passwd", "", "demo//x"] {
            assert_eq!(source_rel(bad), None, "{bad}");
        }
    }

    #[test]
    fn lists_lessons_uncovered_at_first_and_reads_node_and_prose_lessons() {
        let root = repo("sources");
        let store = GraphStore::new(root.to_path_buf());
        let sources = list_sources(&root, &store);
        let brief = |row: &Value| {
            (
                row["id"].clone(),
                row["covered"].clone(),
                row["stale"].clone(),
                row["isolated"].clone(),
            )
        };
        assert_eq!(
            sources.iter().map(brief).collect::<Vec<_>>(),
            vec![
                (
                    json!("demo/0001-il-ref"),
                    json!(false),
                    json!(false),
                    json!(0)
                ),
                (
                    json!("demo/0002-prosa"),
                    json!(false),
                    json!(false),
                    json!(0)
                ),
            ]
        );
        assert_eq!(sources[0]["course"], json!("demo"));
        assert_eq!(sources[0]["lessonTitle"], json!("Il ref è una scatola"));
        assert_eq!(sources[0]["lessonNumber"], json!(1));
        assert_eq!(
            sources[0]["lessonPath"],
            json!("courses/demo/lessons/0001-il-ref.html")
        );
        assert_eq!(sources[1]["lessonTitle"], json!("Lezione in prosa"));
        assert_eq!(sources[1]["lessonNumber"], json!(2));
        assert!(needs_mapping(&sources[0]));
        // Node's GraphSource key order, pinned
        assert_eq!(
            serde_json::to_string(&sources[0]).unwrap(),
            "{\"id\":\"demo/0001-il-ref\",\"course\":\"demo\",\"lessonTitle\":\"Il ref è una scatola\",\"lessonNumber\":1,\"lessonPath\":\"courses/demo/lessons/0001-il-ref.html\",\"covered\":false,\"stale\":false,\"isolated\":0}"
        );

        let text = read_lesson_text(&root, "demo/0001-il-ref").expect("lesson text");
        assert_eq!(text.title, "Il ref è una scatola");
        assert_eq!(text.course, "demo");
        assert_eq!(text.lesson_number, 1.0);
        assert_eq!(text.lesson_path, "courses/demo/lessons/0001-il-ref.html");
        assert_eq!(
            text.text,
            "## Nodo ref-scatola: Il ref (axiom)\n\nUn ref è una scatola & basta.\n\nDomanda di verifica: Che cosa è un ref?\n\nRisposta corretta: una scatola"
        );
        assert!(!text.text.contains("chrome"));
        assert_eq!(
            read_lesson_text(&root, "demo/0002-prosa").unwrap().text,
            "Titolo\nLe props scendono."
        );
        assert!(read_lesson_text(&root, "../etc/passwd").is_none());
    }

    #[test]
    fn fingerprint_hashes_title_and_text_like_node() {
        let root = repo("fingerprint");
        let lesson = read_lesson_text(&root, "demo/0001-il-ref").unwrap();
        // Node: createHash("sha256").update(title + "\n" + text).digest("hex")
        assert_eq!(
            fingerprint(&lesson),
            "eaf4513495495fcdc33921b957f231202dbcfbb708f82611292770e6a771c6e0"
        );
    }

    #[test]
    fn the_lesson_text_is_cut_at_sixty_thousand_utf16_units() {
        let root = temp_dir("max-text");
        let lessons = root.join("courses").join("demo").join("lessons");
        fs::create_dir_all(&lessons).unwrap();
        let body = "a".repeat(60_005);
        fs::write(
            lessons.join("0001-lunga.html"),
            format!("<html><body><p>{body}</p></body></html>"),
        )
        .unwrap();
        assert_eq!(
            read_lesson_text(&root, "demo/0001-lunga").unwrap().text,
            "a".repeat(MAX_TEXT)
        );
    }

    #[test]
    fn coverage_skips_corrupt_lines_reads_stale_and_last_entry_wins() {
        let root = repo("coverage");
        let store = GraphStore::new(root.to_path_buf());
        let lesson = read_lesson_text(&root, "demo/0001-il-ref").unwrap();
        append_coverage(&root, "demo/0001-il-ref", &fingerprint(&lesson)).unwrap();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(root.join("graph").join("coverage.jsonl"))
            .unwrap();
        file.write_all(b"{not json\n").unwrap();
        drop(file);
        assert_eq!(read_coverage(&root).len(), 1);
        let sources = list_sources(&root, &store);
        assert_eq!(sources[0]["covered"], json!(true));
        assert_eq!(sources[0]["stale"], json!(false));
        assert_eq!(sources[1]["covered"], json!(false));

        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(&[json!({ "nodeId": "ref-scatola", "title": "Il ref", "tier": "axiom", "content": "<p>cambiato</p>" })], "Il ref è una scatola"),
        )
        .unwrap();
        let sources = list_sources(&root, &store);
        assert_eq!(sources[0]["covered"], json!(true));
        assert_eq!(sources[0]["stale"], json!(true));

        // re-extraction refreshes the fingerprint
        let lesson = read_lesson_text(&root, "demo/0001-il-ref").unwrap();
        append_coverage(&root, "demo/0001-il-ref", &fingerprint(&lesson)).unwrap();
        assert_eq!(list_sources(&root, &store)[0]["stale"], json!(false));

        // the last entry wins and keeps the first entry's slot
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(root.join("graph").join("coverage.jsonl"))
            .unwrap();
        file.write_all(b"{\"source\":\"demo/0001-il-ref\",\"fp\":\"other\",\"at\":\"t\"}\n")
            .unwrap();
        drop(file);
        let coverage = read_coverage(&root);
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage["demo/0001-il-ref"].fp, "other");
        assert_eq!(list_sources(&root, &store)[0]["stale"], json!(true));
    }

    #[test]
    fn append_coverage_writes_one_compact_line_with_a_newline() {
        let root = temp_dir("coverage-append");
        append_coverage(&root, "demo/0001-x", "fp").unwrap();
        let raw = fs::read_to_string(root.join("graph").join("coverage.jsonl")).unwrap();
        assert_eq!(raw.lines().count(), 1);
        assert!(
            raw.starts_with("{\"source\":\"demo/0001-x\",\"fp\":\"fp\",\"at\":\""),
            "{raw}"
        );
        assert!(raw.ends_with("\"}\n"), "{raw}");
    }

    #[test]
    fn a_crlf_coverage_file_reads_the_last_entry() {
        let root = temp_dir("coverage-crlf");
        fs::create_dir_all(root.join("graph")).unwrap();
        fs::write(
            root.join("graph").join("coverage.jsonl"),
            "{\"source\":\"a/b\",\"fp\":\"x\",\"at\":\"t\"}\r\n{\"source\":\"a/b\",\"fp\":\"y\",\"at\":\"u\"}\r\n",
        )
        .unwrap();
        let coverage = read_coverage(&root);
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage["a/b"].fp, "y");
        assert_eq!(coverage["a/b"].at, "u");
    }

    #[test]
    fn a_covered_lesson_with_an_isolated_idea_is_pending_again() {
        let root = repo("isolated");
        let store = GraphStore::new(root.to_path_buf());
        store
            .merge_findings(
                "demo/0001-il-ref",
                &json!({
                    "propositions": [
                        { "id": "p1", "name": "Sola", "statement": "Sola." },
                        { "id": "p2", "name": "A", "statement": "A." },
                        { "id": "p3", "name": "B", "statement": "B." },
                    ],
                    "chains": [{ "premises": ["p2"], "conclusion": "p3" }],
                }),
                || {},
            )
            .unwrap();
        append_coverage(&root, "demo/0001-il-ref", "fp").unwrap();
        let sources = list_sources(&root, &store);
        assert_eq!(sources[0]["id"], json!("demo/0001-il-ref"));
        assert_eq!(sources[0]["isolated"], json!(1));
        assert_eq!(sources[0]["covered"], json!(true));
        assert!(needs_mapping(&sources[0]));
        assert!(needs_mapping(&sources[1]));
    }

    #[test]
    fn map_job_title_and_existing_ids_read_the_digest() {
        assert_eq!(
            map_job_title("Il ref è una scatola"),
            "Mappa: Il ref è una scatola"
        );
        let digest = "## L1\nn_aaa111 | Ref | Un ref\n\n## L2\n  n_bbb222 | Prop | Le props\nn_ccc | x | y\nxn_ddd | z";
        assert_eq!(
            existing_ids(digest),
            HashSet::from([
                "n_aaa111".to_string(),
                "n_bbb222".to_string(),
                "n_ccc".to_string(),
            ])
        );
        assert!(existing_ids("(vuoto: nessuna proposizione ancora)").is_empty());
    }

    #[test]
    fn read_lesson_text_shows_the_anchor_names() {
        let root = repo("anchor-text");
        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(&[node_a()], "Il ref è una scatola"),
        )
        .unwrap();
        let text = read_lesson_text(&root, "demo/0001-il-ref").unwrap().text;
        assert!(text.contains("Si appoggia su: Ref; Sparito"), "{text}");
    }

    #[test]
    fn anchors_by_node_is_empty_for_a_missing_or_unreadable_lesson() {
        let root = repo("anchors");
        assert!(anchors_by_node(&root, "demo/nope").is_empty());
        assert!(anchors_by_node(&root, "../etc/passwd").is_empty());
        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(&[node_a()], "Il ref è una scatola"),
        )
        .unwrap();
        let anchors = anchors_by_node(&root, "demo/0001-il-ref");
        assert_eq!(anchors.len(), 1);
        assert_eq!(
            anchors["ref-scatola"],
            vec![
                ("n_aaa111".to_string(), "Ref".to_string()),
                ("n_gone".to_string(), "Sparito".to_string()),
            ]
        );
    }

    #[test]
    fn with_anchor_chains_adds_each_live_anchor_once_and_leaves_the_input_alone() {
        let f = json!({
            "propositions": [{ "id": "p1", "name": "N", "statement": "S", "nodeId": "ref-scatola" }],
            "chains": [{ "premises": ["n_bbb222"], "conclusion": "p1" }],
        });
        let anchors = IndexMap::from([
            (
                "ref-scatola".to_string(),
                vec![
                    ("n_aaa111".to_string(), "Ref".to_string()),
                    ("n_gone".to_string(), "Sparito".to_string()),
                    ("n_bbb222".to_string(), "Prop".to_string()),
                ],
            ),
            (
                "nodo-mai-estratto".to_string(),
                vec![("n_aaa111".to_string(), "Ref".to_string())],
            ),
        ]);
        let live = HashSet::from(["n_aaa111".to_string(), "n_bbb222".to_string()]);
        let g = with_anchor_chains(&f, &anchors, &live);
        assert_eq!(
            g["chains"],
            json!([
                { "premises": ["n_bbb222"], "conclusion": "p1" },
                { "premises": ["n_aaa111"], "conclusion": "p1" },
            ])
        );
        assert_eq!(
            f["chains"],
            json!([{ "premises": ["n_bbb222"], "conclusion": "p1" }])
        );
    }

    #[test]
    fn with_anchor_chains_does_not_repeat_the_link_to_the_proposition_already_has() {
        let f = json!({
            "propositions": [{ "id": "p1", "name": "N", "statement": "S", "nodeId": "ref-scatola", "linkTo": "n_aaa111" }],
            "chains": [{ "premises": ["n_bbb222"], "conclusion": "p1" }],
        });
        let anchors = IndexMap::from([(
            "ref-scatola".to_string(),
            vec![("n_aaa111".to_string(), "Ref".to_string())],
        )]);
        let live = HashSet::from(["n_aaa111".to_string()]);
        let g = with_anchor_chains(&f, &anchors, &live);
        assert_eq!(
            g["chains"],
            json!([{ "premises": ["n_bbb222"], "conclusion": "p1" }])
        );
    }

    #[test]
    fn with_anchor_chains_attaches_to_the_nodes_first_proposition_once() {
        let f = json!({
            "propositions": [
                { "id": "p1", "name": "N", "statement": "S", "nodeId": "ref-scatola" },
                { "id": "p2", "name": "N di nuovo", "statement": "S2", "nodeId": "ref-scatola" },
            ],
            "chains": [],
        });
        let anchors = IndexMap::from([(
            "ref-scatola".to_string(),
            vec![("n_aaa111".to_string(), "Ref".to_string())],
        )]);
        let live = HashSet::from(["n_aaa111".to_string()]);
        let g = with_anchor_chains(&f, &anchors, &live);
        assert_eq!(
            g["chains"],
            json!([{ "premises": ["n_aaa111"], "conclusion": "p1" }])
        );
    }

    #[test]
    fn an_unreadable_island_falls_back_to_the_prose() {
        let root = repo("corrupt");
        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            "<html><head><title>T</title></head><body><script type=\"application/json\" id=\"lesson-data\">{nope</script><p>prosa rotta</p></body></html>",
        )
        .unwrap();
        let lesson = read_lesson_text(&root, "demo/0001-il-ref").expect("prose fallback");
        assert_eq!(lesson.title, "T");
        assert_eq!(lesson.lesson_number, 1.0);
        assert_eq!(lesson.text, "prosa rotta");
        assert!(anchors_by_node(&root, "demo/0001-il-ref").is_empty());
    }

    fn code_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[tokio::test]
    async fn map_role_is_configured_and_builds_its_prompts() {
        let root = repo("role-config");
        let store = GraphStore::new(root.to_path_buf());
        assert_eq!(MAP_ROLE.kind, "map");
        assert!(MAP_ROLE.grant.is_empty());
        assert_eq!(MAP_ROLE.max_turns, 6);

        fs::create_dir_all(root.join("graph")).unwrap();
        fs::write(
            root.join("graph").join("nodes.jsonl"),
            json!({
                "id": "n_aaa111",
                "name": "Ref",
                "statement": "Un ref è una scatola.",
                "aliases": [],
                "createdAt": "2026-11-01T00:00:00.000Z",
                "provenance": [{ "chatId": "demo/0000-seed", "messageId": "m1" }],
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let env = RoleEnv {
            code_root: &code_root(),
            data_root: &root,
            graph: &store,
        };
        let sys = (MAP_ROLE.system_prompt)(&env, &json!({ "source": "demo/0001-il-ref" })).unwrap();
        assert!(sys.contains("proposizioni"), "{sys}");
        assert!(sys.contains("n_aaa111 | Ref | Un ref è una scatola"), "{sys}");

        let mut input = json!({ "source": "demo/0001-il-ref" });
        let first = (MAP_ROLE.first_message)(&env, &mut input);
        assert!(first.contains("Il ref è una scatola"), "{first}");
        assert!(first.contains("Nodo ref-scatola"), "{first}");
        assert!(first.contains("Risposta corretta: una scatola"), "{first}");
    }

    #[tokio::test]
    async fn submit_findings_rejects_invalid_findings_then_merges_records_coverage_emits_graph_and_finishes() {
        let root = repo("submit-findings");
        let f = json!({
            "propositions": [
                { "id": "p1", "name": "Ref", "statement": "Un ref è una scatola.", "linkTo": "n_aaa111" },
                { "id": "p2", "name": "Nuovo", "statement": "Altro." },
            ],
            "chains": [{ "premises": ["p2", "n_bbb222"], "conclusion": "p1" }],
        });

        let fake_job = FakeJob::new("j1", json!({ "source": "demo/0001-il-ref" }), &root);
        let mut rx = fake_job.events().subscribe();

        // 1. Rejected with linkTo non esiste
        let r1 = (SUBMIT_FINDINGS.run)(f.clone(), &fake_job).await;
        assert!(r1.is_error);
        assert!(r1.text.contains("linkTo \"n_aaa111\" non esiste"), "{}", r1.text);
        assert_eq!(fake_job.finished(), None);
        assert_eq!(read_coverage(&root).len(), 0);

        // 2. Add n_aaa111 and n_bbb222 to nodes ledger
        fs::create_dir_all(root.join("graph")).unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("graph").join("nodes.jsonl"))
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({
                "id": "n_aaa111",
                "name": "Ref",
                "statement": "Un ref è una scatola.",
                "aliases": [],
                "createdAt": "2026-11-01T00:00:00.000Z",
                "provenance": [{ "chatId": "demo/0000-seed", "messageId": "m1" }],
            })
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            json!({
                "id": "n_bbb222",
                "name": "Base",
                "statement": "Base.",
                "aliases": [],
                "createdAt": "2026-11-01T00:00:00.000Z",
                "provenance": [{ "chatId": "demo/0000-seed", "messageId": "m2" }],
            })
        )
        .unwrap();
        drop(file);

        // 3. Merges successfully
        let r2 = (SUBMIT_FINDINGS.run)(f, &fake_job).await;
        assert!(!r2.is_error, "{}", r2.text);
        assert_eq!(
            fake_job.finished(),
            Some(json!({
                "kind": "map",
                "source": "demo/0001-il-ref",
                "added": 1,
                "linked": 1,
                "chains": 1,
            }))
        );
        let coverage = read_coverage(&root);
        assert_eq!(coverage.get("demo/0001-il-ref").map(|c| c.fp.len()), Some(64));
        let event = rx.try_recv().expect("graph event emitted");
        assert!(event.contains("\"type\":\"graph\""), "{event}");
    }

    #[tokio::test]
    async fn submit_findings_serves_over_mcp_with_schema_enforced() {
        let root = repo("mcp-schema");
        let fake_job = FakeJob::new("j-mcp", json!({ "source": "demo/0001-il-ref" }), &root);
        let res = crate::mcp::handle_as(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "submit_findings",
                    "arguments": { "propositions": "no" },
                },
            }),
            &[&SUBMIT_FINDINGS],
            &fake_job,
            "job",
        )
        .await
        .expect("mcp response");
        assert_eq!(res["result"]["isError"], true);
    }

    #[test]
    fn first_message_lists_the_live_anchors_with_their_ids_never_a_dangling_one() {
        let root = repo("live-anchors");
        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(&[node_a()], "Il ref è una scatola"),
        )
        .unwrap();
        fs::create_dir_all(root.join("graph")).unwrap();
        fs::write(
            root.join("graph").join("nodes.jsonl"),
            json!({
                "id": "n_aaa111",
                "name": "Ref",
                "statement": "Un ref è una scatola.",
                "aliases": [],
                "createdAt": "2026-11-01T00:00:00.000Z",
                "provenance": [{ "chatId": "demo/0000-seed", "messageId": "m1" }],
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let store = GraphStore::new(root.to_path_buf());
        let env = RoleEnv {
            code_root: &code_root(),
            data_root: &root,
            graph: &store,
        };
        let mut input = json!({ "source": "demo/0001-il-ref" });
        let msg = (MAP_ROLE.first_message)(&env, &mut input);
        assert!(msg.contains("nodo ref-scatola \u{2192} n_aaa111 | Ref"), "{msg}");
        assert!(!msg.contains("n_gone"), "{msg}");
    }

    #[test]
    fn first_message_keeps_the_anchors_block_on_the_isolated_repair_path() {
        let root = repo("anchors-repair");
        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(&[node_a()], "Il ref è una scatola"),
        )
        .unwrap();
        fs::create_dir_all(root.join("graph")).unwrap();
        fs::write(
            root.join("graph").join("nodes.jsonl"),
            json!({
                "id": "n_aaa111",
                "name": "Ref",
                "statement": "Un ref è una scatola.",
                "aliases": [],
                "createdAt": "2026-11-01T00:00:00.000Z",
                "provenance": [{ "chatId": "demo/0000-seed", "messageId": "m1" }],
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let store = GraphStore::new(root.to_path_buf());
        store
            .merge_findings(
                "demo/0001-il-ref",
                &json!({
                    "propositions": [
                        { "id": "p1", "name": "Sola", "statement": "Sola." },
                        { "id": "p2", "name": "A", "statement": "A." },
                        { "id": "p3", "name": "B", "statement": "B." },
                    ],
                    "chains": [{ "premises": ["p2"], "conclusion": "p3" }],
                }),
                || {},
            )
            .unwrap();

        let env = RoleEnv {
            code_root: &code_root(),
            data_root: &root,
            graph: &store,
        };
        let mut input = json!({ "source": "demo/0001-il-ref" });
        let msg = (MAP_ROLE.first_message)(&env, &mut input);
        assert!(msg.contains("Ancore dichiarate dal tutor"), "{msg}");
        assert!(msg.contains("nodo ref-scatola \u{2192} n_aaa111 | Ref"), "{msg}");
        assert!(!msg.contains("n_gone"), "{msg}");
        assert!(msg.contains("gi\u{e0} nella mappa ma isolate"), "{msg}");
        assert!(msg.contains("| Sola"), "{msg}");
    }

    #[tokio::test]
    async fn submit_findings_adds_the_anchor_chain_before_merging() {
        let root = repo("anchor-chain-add");
        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(&[node_a()], "Il ref è una scatola"),
        )
        .unwrap();
        fs::create_dir_all(root.join("graph")).unwrap();
        fs::write(
            root.join("graph").join("nodes.jsonl"),
            json!({
                "id": "n_aaa111",
                "name": "Ref",
                "statement": "Un ref è una scatola.",
                "aliases": [],
                "createdAt": "2026-11-01T00:00:00.000Z",
                "provenance": [{ "chatId": "demo/0000-seed", "messageId": "m1" }],
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let fake_job = FakeJob::new("j-anchors", json!({ "source": "demo/0001-il-ref" }), &root);
        let r = (SUBMIT_FINDINGS.run)(
            json!({
                "propositions": [
                    { "id": "p1", "name": "N", "statement": "S", "nodeId": "ref-scatola" },
                    { "id": "p2", "name": "M", "statement": "M." },
                ],
                "chains": [{ "premises": ["p2"], "conclusion": "p1" }],
            }),
            &fake_job,
        )
        .await;

        assert!(!r.is_error, "{}", r.text);
        let chains = fake_job.graph().read_chain_lines();
        assert!(chains.iter().any(|c| {
            c.get("premises")
                .and_then(Value::as_array)
                .is_some_and(|p| p.iter().any(|x| x == "n_aaa111"))
        }));
        assert!(!chains.iter().any(|c| {
            c.get("premises")
                .and_then(Value::as_array)
                .is_some_and(|p| p.iter().any(|x| x == "n_gone"))
        }));
    }

    #[tokio::test]
    async fn ac3_a_declared_anchor_becomes_a_chain_from_the_live_concept_to_the_new_one() {
        let root = repo("ac3");
        let store = GraphStore::new(root.to_path_buf());
        store
            .merge_findings(
                "demo/0000-seed",
                &json!({
                    "propositions": [
                        { "id": "p1", "name": "Ref", "statement": "Un ref è una scatola." },
                        { "id": "p2", "name": "Base", "statement": "B." },
                    ],
                    "chains": [{ "premises": ["p2"], "conclusion": "p1" }],
                }),
                || {},
            )
            .unwrap();

        let seed_id = store
            .read_node_lines()
            .into_iter()
            .find(|n| n.get("name").and_then(Value::as_str) == Some("Ref"))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(
                &[json!({
                    "nodeId": "ref-scatola",
                    "title": "Il ref",
                    "tier": "axiom",
                    "content": "<p>content</p>",
                    "anchors": [{ "id": seed_id, "name": "Ref" }],
                })],
                "Il ref è una scatola",
            ),
        )
        .unwrap();

        let fake_job = FakeJob::new("j-ac3", json!({ "source": "demo/0001-il-ref" }), &root);
        let r = (SUBMIT_FINDINGS.run)(
            json!({
                "propositions": [
                    { "id": "p1", "name": "Nuovo", "statement": "Un nuovo nodo.", "nodeId": "ref-scatola" },
                    { "id": "p2", "name": "M", "statement": "M." },
                ],
                "chains": [{ "premises": ["p2"], "conclusion": "p1" }],
            }),
            &fake_job,
        )
        .await;

        assert!(!r.is_error, "{}", r.text);
        let new_id = store
            .read_node_lines()
            .into_iter()
            .find(|n| n.get("name").and_then(Value::as_str) == Some("Nuovo"))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(store.read_chain_lines().iter().any(|c| {
            c.get("premises")
                .and_then(Value::as_array)
                .is_some_and(|p| p.iter().any(|x| x == seed_id.as_str()))
                && c.get("conclusion").and_then(Value::as_str) == Some(new_id.as_str())
        }));
    }

    #[tokio::test]
    async fn ac4_a_deleted_anchored_concept_degrades_to_text() {
        let root = repo("ac4");
        let store = GraphStore::new(root.to_path_buf());
        store
            .merge_findings(
                "demo/0000-seed",
                &json!({
                    "propositions": [
                        { "id": "p1", "name": "Ref", "statement": "Un ref è una scatola." },
                        { "id": "p2", "name": "Base", "statement": "B." },
                    ],
                    "chains": [{ "premises": ["p2"], "conclusion": "p1" }],
                }),
                || {},
            )
            .unwrap();

        let seed_id = store
            .read_node_lines()
            .into_iter()
            .find(|n| n.get("name").and_then(Value::as_str) == Some("Ref"))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(
                &[json!({
                    "nodeId": "ref-scatola",
                    "title": "Il ref",
                    "tier": "axiom",
                    "content": "<p>content</p>",
                    "anchors": [{ "id": seed_id, "name": "Ref" }],
                })],
                "Il ref è una scatola",
            ),
        )
        .unwrap();

        // First pass links the node to the concept
        let fake_job1 = FakeJob::new("j-first", json!({ "source": "demo/0001-il-ref" }), &root);
        let r1 = (SUBMIT_FINDINGS.run)(
            json!({
                "propositions": [
                    { "id": "p1", "name": "Nuovo", "statement": "Un nuovo nodo.", "nodeId": "ref-scatola" },
                    { "id": "p2", "name": "M", "statement": "M." },
                ],
                "chains": [{ "premises": ["p2"], "conclusion": "p1" }],
            }),
            &fake_job1,
        )
        .await;
        assert!(!r1.is_error, "{}", r1.text);
        assert!(store.read_chain_lines().iter().any(|c| {
            c.get("premises")
                .and_then(Value::as_array)
                .is_some_and(|p| p.iter().any(|x| x == seed_id.as_str()))
        }));

        store.delete_node(&seed_id).unwrap();

        let text = read_lesson_text(&root, "demo/0001-il-ref").unwrap();
        assert!(text.text.contains("Si appoggia su: Ref"), "{}", text.text);

        let env = RoleEnv {
            code_root: &code_root(),
            data_root: &root,
            graph: &store,
        };
        let mut input = json!({ "source": "demo/0001-il-ref" });
        assert!(!(MAP_ROLE.first_message)(&env, &mut input).contains(&seed_id));

        cleanup("j-first");
        let fake_job2 = FakeJob::new("j-second", json!({ "source": "demo/0001-il-ref" }), &root);
        let r2 = (SUBMIT_FINDINGS.run)(
            json!({
                "propositions": [
                    { "id": "p1", "name": "N2", "statement": "S2", "nodeId": "ref-scatola" },
                    { "id": "p2", "name": "M", "statement": "M." },
                ],
                "chains": [{ "premises": ["p2"], "conclusion": "p1" }],
            }),
            &fake_job2,
        )
        .await;
        assert!(!r2.is_error, "{}", r2.text);
        assert!(!store.read_chain_lines().iter().any(|c| {
            c.get("premises")
                .and_then(Value::as_array)
                .is_some_and(|p| p.iter().any(|x| x == seed_id.as_str()))
                || c.get("conclusion").and_then(Value::as_str) == Some(seed_id.as_str())
        }));
    }

    #[tokio::test]
    async fn submit_findings_is_idempotent_per_job() {
        let root = repo("idempotent");
        let fake_job = FakeJob::new("j-idem", json!({ "source": "demo/0001-il-ref" }), &root);
        let f = json!({
            "propositions": [
                { "id": "p1", "name": "A", "statement": "a." },
                { "id": "p2", "name": "B", "statement": "b." },
            ],
            "chains": [{ "premises": ["p1"], "conclusion": "p2" }],
        });
        let a = (SUBMIT_FINDINGS.run)(f.clone(), &fake_job).await;
        assert!(!a.is_error, "{}", a.text);
        let count1 = fake_job.graph().read_node_lines().len();
        let b = (SUBMIT_FINDINGS.run)(f, &fake_job).await;
        let count2 = fake_job.graph().read_node_lines().len();
        assert_eq!(count1, 2);
        assert_eq!(count2, 2);
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn coverage_records_the_fingerprint_seen_at_start_so_mid_job_edit_reads_stale() {
        let root = repo("stale-edit");
        let store = GraphStore::new(root.to_path_buf());
        let env = RoleEnv {
            code_root: &code_root(),
            data_root: &root,
            graph: &store,
        };
        let mut input = json!({ "source": "demo/0001-il-ref" });
        (MAP_ROLE.first_message)(&env, &mut input);
        let fp = input["fp"].as_str().unwrap().to_string();
        assert_eq!(fp.len(), 64);

        fs::write(
            lesson_path(&root, "0001-il-ref.html"),
            island(
                &[json!({
                    "nodeId": "ref-scatola",
                    "title": "Il ref",
                    "tier": "axiom",
                    "content": "<p>cambiato durante il job</p>",
                })],
                "Il ref è una scatola",
            ),
        )
        .unwrap();

        let fake_job = FakeJob::new("j-mid", input, &root);
        (SUBMIT_FINDINGS.run)(json!({ "propositions": [], "chains": [] }), &fake_job).await;
        let sources = list_sources(&root, &store);
        assert_eq!(sources[0]["stale"], json!(true));
    }
}
