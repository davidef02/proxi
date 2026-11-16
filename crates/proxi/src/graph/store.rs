//! Graph ledgers under `<dataRoot>/graph/`: readers, positions, delete and the layout.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use indexmap::IndexMap;
use serde_json::{Map, Value};

use super::derive::{self, Positions, Record, Snapshot};
use super::layout;
use crate::curriculum;
use crate::events;
use crate::js;
use crate::knowledge::{self, Status};
use crate::lesson;

fn is_point(value: &Value) -> bool {
    value.get("chatId").is_some_and(Value::is_string)
        && value.get("messageId").is_some_and(Value::is_string)
}

fn strs(value: Option<&Value>) -> bool {
    value.is_some_and(|v| {
        v.as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    })
}

fn is_node(value: &Value) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    record.get("id").is_some_and(Value::is_string)
        && record.get("name").is_some_and(Value::is_string)
        && record.get("statement").is_some_and(Value::is_string)
        && strs(record.get("aliases"))
        && record.get("createdAt").is_some_and(Value::is_string)
        && record
            .get("provenance")
            .and_then(Value::as_array)
            .is_some_and(|points| points.iter().all(is_point))
}

fn is_chain(value: &Value) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    record.get("id").is_some_and(Value::is_string)
        && strs(record.get("premises"))
        && record
            .get("premises")
            .and_then(Value::as_array)
            .is_some_and(|premises| !premises.is_empty())
        && record.get("conclusion").is_some_and(Value::is_string)
        && record.get("createdAt").is_some_and(Value::is_string)
        && record
            .get("provenance")
            .and_then(Value::as_array)
            .is_some_and(|points| points.iter().all(is_point))
}

fn read_jsonl(path: &Path, guard: fn(&Value) -> bool) -> Vec<Record> {
    let Ok(raw) = fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&raw);
    let mut out = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = js::parse(line) else {
            continue;
        };
        if guard(&value) {
            if let Some(record) = value.as_object() {
                out.push(record.clone());
            }
        }
    }
    out
}

/// Drop only the lines `drop` rejects; lines readers cannot parse stay verbatim, never lost.
fn rewrite_without(
    path: &Path,
    guard: fn(&Value) -> bool,
    drop: impl Fn(&Record) -> bool,
) -> io::Result<()> {
    let Ok(raw) = fs::read(path) else {
        return Ok(());
    };
    let text = String::from_utf8_lossy(&raw);
    let mut out = String::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let keep = match js::parse(line) {
            Err(_) => true,
            Ok(value) => {
                !(guard(&value) && value.as_object().is_some_and(|record| drop(record)))
            }
        };
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    js::write_atomic(path, out.as_bytes())
}

fn s<'a>(record: &'a Record, key: &str) -> &'a str {
    record.get(key).and_then(Value::as_str).unwrap_or("")
}

fn id_of(record: &Record) -> String {
    s(record, "id").to_string()
}

/// `Object.entries` / array index keys.
fn entries(value: &Value) -> Vec<(String, &Value)> {
    match value {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        _ => Vec::new(),
    }
}

fn position(x: f64, y: f64) -> Value {
    let mut pos = Map::new();
    pos.insert("x".to_string(), js::number(x));
    pos.insert("y".to_string(), js::number(y));
    Value::Object(pos)
}

/// The ledgers plus the process-wide write serialisation and the event revision counter.
pub struct GraphStore {
    data_root: PathBuf,
    lock: Mutex<()>,
    revision: AtomicU64,
}

/// What one `merge_findings` write added, linked and asserted.
pub struct MergeCounts {
    pub added: usize,
    pub linked: usize,
    pub chains: usize,
}

impl GraphStore {
    pub fn new(data_root: PathBuf) -> Self {
        GraphStore {
            data_root,
            lock: Mutex::new(()),
            revision: AtomicU64::new(0),
        }
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Node's counter lives per process; one server per process, and the golden expects 1 on a fresh one.
    pub fn next_revision(&self) -> u64 {
        self.revision.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn graph_dir(&self) -> PathBuf {
        self.data_root.join("graph")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.graph_dir().join(name)
    }

    pub fn read_node_lines(&self) -> Vec<Record> {
        read_jsonl(&self.file("nodes.jsonl"), is_node)
    }

    pub fn read_chain_lines(&self) -> Vec<Record> {
        read_jsonl(&self.file("chains.jsonl"), is_chain)
    }

    /// `layout.json` kept entries only: object entries (or array index keys) with finite `x` and `y`.
    pub fn read_positions(&self) -> Positions {
        let Ok(raw) = fs::read(self.file("layout.json")) else {
            return Positions::new();
        };
        let Ok(parsed) = js::parse(&String::from_utf8_lossy(&raw)) else {
            return Positions::new();
        };
        let mut out = Positions::new();
        for (key, value) in entries(&parsed) {
            let Some(pos) = value.as_object() else {
                continue;
            };
            let (Some(x), Some(y)) = (js::finite(pos.get("x")), js::finite(pos.get("y"))) else {
                continue;
            };
            out.insert(key, position(x, y));
        }
        out
    }

    fn write_positions_file(&self, positions: &Positions) -> io::Result<()> {
        fs::create_dir_all(self.graph_dir())?;
        // pre-v4 free layout: dropped unread
        let _ = fs::remove_file(self.file("positions.json"));
        let text = serde_json::to_string(positions).map_err(io::Error::other)?;
        js::write_atomic(&self.file("layout.json"), text.as_bytes())
    }

    /// Merge dragged positions; ids that are neither a proposition nor a chain are ignored.
    pub fn save_positions(&self, update: &Value) -> io::Result<()> {
        let _guard = self.lock();
        let mut merged = self.read_positions();
        let mut known: HashSet<String> = self.read_node_lines().iter().map(id_of).collect();
        known.extend(self.read_chain_lines().iter().map(id_of));

        for (key, value) in entries(update) {
            if !known.contains(&key) {
                continue;
            }
            let Some(pos) = value.as_object() else {
                continue;
            };
            let (Some(x), Some(y)) = (js::finite(pos.get("x")), js::finite(pos.get("y"))) else {
                continue;
            };
            merged.insert(key, position(x, y));
        }
        self.write_positions_file(&merged)
    }

    /// Delete a proposition (cascading to every chain naming it) or, for an `arg_` id, that one chain.
    pub fn delete_node(&self, id: &str) -> io::Result<(usize, usize)> {
        let _guard = self.lock();
        let chains = self.read_chain_lines();
        let removed_chains: usize;
        let mut removed_nodes = 0;

        if id.starts_with("arg_") {
            removed_chains = chains.iter().filter(|c| s(c, "id") == id).count();
            if removed_chains > 0 {
                rewrite_without(&self.file("chains.jsonl"), is_chain, |c| s(c, "id") == id)?;
            }
        } else {
            removed_nodes = self
                .read_node_lines()
                .iter()
                .filter(|n| s(n, "id") == id)
                .count();
            if removed_nodes > 0 {
                rewrite_without(&self.file("nodes.jsonl"), is_node, |n| s(n, "id") == id)?;
            }
            let hit = |c: &Record| {
                s(c, "conclusion") == id
                    || c.get("premises")
                        .and_then(Value::as_array)
                        .is_some_and(|premises| premises.iter().any(|p| p.as_str() == Some(id)))
            };
            removed_chains = chains.iter().filter(|c| hit(c)).count();
            if removed_chains > 0 {
                rewrite_without(&self.file("chains.jsonl"), is_chain, hit)?;
            }
        }

        let mut positions = self.read_positions();
        if positions.contains_key(id) {
            positions.shift_remove(id);
            self.write_positions_file(&positions)?;
        }
        Ok((removed_nodes, removed_chains))
    }

    /// Layered layout of the whole map written to `layout.json`; deterministic, bottom-up.
    fn settle_layout(&self) -> io::Result<()> {
        let snapshot = derive::build_snapshot(
            &self.read_node_lines(),
            &self.read_chain_lines(),
            &Map::new(),
            Positions::new(),
            &HashMap::new(),
        );
        if snapshot.nodes.is_empty() {
            return Ok(());
        }

        let births: HashMap<String, String> = snapshot
            .propositions
            .iter()
            .map(|n| (id_of(n), derive::birth_session_of(n).unwrap_or_default()))
            .collect();
        let nodes: Vec<String> = snapshot.nodes.iter().map(id_of).collect();
        let imp_links: Vec<(String, Vec<String>, String)> = snapshot
            .imp_links
            .iter()
            .map(|link| {
                (
                    link.id.clone(),
                    link.premises.clone(),
                    link.conclusion.clone(),
                )
            })
            .collect();
        let placed = layout::layered_layout(
            &nodes,
            &snapshot.logical_edges,
            &imp_links,
            &|id| births.get(id).cloned().unwrap_or_default(),
        );

        let mut positions = Positions::new();
        for (id, x, y) in placed {
            positions.insert(id, position(x, y));
        }
        self.write_positions_file(&positions)
    }

    /// Settle when `layout.json` is missing or does not cover every node. Never fails: rebuildable.
    pub fn ensure_layout(&self) {
        let _guard = self.lock();
        let positions = self.read_positions();
        let snapshot = derive::build_snapshot(
            &self.read_node_lines(),
            &self.read_chain_lines(),
            &Map::new(),
            Positions::new(),
            &HashMap::new(),
        );
        if snapshot
            .nodes
            .iter()
            .any(|node| !positions.contains_key(id_of(node).as_str()))
        {
            let _ = self.settle_layout();
        }
    }

    /// Source id (`<course>/<lesson stem>`) -> "Lezione N · titolo" for every lesson on disk.
    pub fn lesson_labels(data_root: &Path) -> HashMap<String, String> {
        let Ok(docs) = lesson::scan_lesson_docs(data_root) else {
            return HashMap::new();
        };
        docs.into_iter()
            .map(|doc| {
                let label = format!(
                    "Lezione {} · {}",
                    js::display(doc.lesson_number()),
                    js::display(doc.title())
                );
                (format!("{}/{}", doc.course, doc.stem()), label)
            })
            .collect()
    }

    /// `course/stem#nodeId` -> status; an unreadable map drops every status, never the snapshot.
    pub fn provenance_status(data_root: &Path) -> HashMap<String, Status> {
        match knowledge::build_knowledge_map(data_root) {
            Ok(map) => map
                .nodes
                .into_iter()
                .map(|node| (node.provenance_key, node.status))
                .collect(),
            Err(_) => HashMap::new(),
        }
    }

    /// A node takes the worst status of its provenance points; no point leaves it unstatused.
    pub fn apply_status(nodes: &mut [Record], lookup: &HashMap<String, Status>) {
        for node in nodes {
            let mut worst: Option<Status> = None;
            if let Some(points) = node.get("provenance").and_then(Value::as_array) {
                for point in points {
                    let message_id = point.get("messageId").and_then(Value::as_str).unwrap_or("");
                    if message_id.is_empty() {
                        continue;
                    }
                    let chat_id = point.get("chatId").and_then(Value::as_str).unwrap_or("");
                    let status = lookup.get(&format!("{chat_id}#{message_id}")).copied();
                    if let Some(status) = status {
                        if worst.is_none_or(|w| status.rank() > w.rank()) {
                            worst = Some(status);
                        }
                    }
                }
            }
            match worst {
                Some(status) => {
                    node.insert("status".to_string(), Value::String(status.as_str().to_string()));
                }
                None => {
                    node.shift_remove("status");
                }
            }
        }
    }

    pub fn read_snapshot(&self) -> Snapshot {
        let nodes = self.read_node_lines();
        let chains = self.read_chain_lines();
        let labels = Self::lesson_labels(&self.data_root);

        let mut used: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for line in nodes.iter().chain(chains.iter()) {
            if let Some(points) = line.get("provenance").and_then(Value::as_array) {
                for point in points {
                    let Some(chat) = point.get("chatId").and_then(Value::as_str) else {
                        continue;
                    };
                    if seen.insert(chat.to_string()) {
                        used.push(chat.to_string());
                    }
                }
            }
        }
        let mut sessions = Map::new();
        for id in used {
            // a source whose lesson file is gone keeps its id as label (honest, never guessed)
            let label = labels.get(&id).cloned().unwrap_or_else(|| id.clone());
            sessions.insert(id, Value::String(label));
        }

        let course_labels = curriculum::course_labels(&self.data_root).unwrap_or_default();
        let mut snapshot = derive::build_snapshot(
            &nodes,
            &chains,
            &sessions,
            self.read_positions(),
            &course_labels,
        );
        let lookup = Self::provenance_status(&self.data_root);
        Self::apply_status(&mut snapshot.propositions, &lookup);
        Self::apply_status(&mut snapshot.nodes, &lookup);
        snapshot
    }

    /// Existing propositions, grouped by the lesson they were born in (oldest lesson first):
    /// `n_id | name | statement`.
    pub fn graph_digest(&self) -> String {
        let graph = derive::get_graph(&self.read_node_lines(), &self.read_chain_lines());
        let labels = Self::lesson_labels(&self.data_root);
        let mut groups: IndexMap<String, Vec<String>> = IndexMap::new();
        for node in &graph.nodes {
            let src = derive::birth_session_of(node).unwrap_or_default();
            groups.entry(src).or_default().push(format!(
                "{} | {} | {}",
                s(node, "id"),
                s(node, "name"),
                s(node, "statement")
            ));
        }
        let mut sources: Vec<&String> = groups.keys().collect();
        sources.sort();
        sources
            .into_iter()
            .map(|src| {
                // a lesson file gone from disk keeps its id; a node born outside any lesson has none
                let label = labels.get(src).cloned().unwrap_or_else(|| {
                    if src.is_empty() {
                        "(senza lezione)".to_string()
                    } else {
                        src.clone()
                    }
                });
                let rows = groups.get(src).map(Vec::as_slice).unwrap_or_default();
                format!("## {label}\n{}", rows.join("\n"))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Propositions no chain uses (neither premise nor conclusion), by birth lesson: the map must never hold them.
    pub fn isolated_by_source(&self) -> IndexMap<String, Vec<(String, String)>> {
        let graph = derive::get_graph(&self.read_node_lines(), &self.read_chain_lines());
        let mut used: HashSet<String> = HashSet::new();
        for chain in &graph.chains {
            used.extend(derive::list(chain, "premises"));
            used.insert(s(chain, "conclusion").to_string());
        }
        let mut out: IndexMap<String, Vec<(String, String)>> = IndexMap::new();
        for node in &graph.nodes {
            let id = s(node, "id");
            if used.contains(id) {
                continue;
            }
            let src = derive::birth_session_of(node).unwrap_or_default();
            out.entry(src)
                .or_default()
                .push((id.to_string(), s(node, "name").to_string()));
        }
        out
    }

    /// Write one submission: new propositions, link-appends, deduped chains, the removals, then simplify.
    /// `after_write` runs inside the lock right after both ledgers are written (coverage).
    pub fn merge_findings(
        &self,
        source_id: &str,
        f: &Value,
        after_write: impl FnOnce(),
    ) -> io::Result<MergeCounts> {
        let _guard = self.lock();
        let propositions = f
            .get("propositions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let chains = f
            .get("chains")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();

        let mut existing: HashSet<String> = self.read_node_lines().iter().map(id_of).collect();
        let now = events::iso_now();
        let mut mapping: HashMap<String, String> = HashMap::new();
        let mut node_rows: Vec<Value> = Vec::new();
        let mut counts = MergeCounts {
            added: 0,
            linked: 0,
            chains: 0,
        };

        for proposition in propositions {
            let key = js::display(proposition.get("id"));
            if mapping.contains_key(&key) {
                continue;
            }
            let message_id = match proposition.get("nodeId") {
                None | Some(Value::Null) => Value::String(String::new()),
                Some(node_id) => node_id.clone(),
            };
            let provenance = Value::Array(vec![provenance_point(source_id, message_id)]);
            match proposition.get("linkTo").and_then(Value::as_str) {
                // link-append: the first line's name/statement stand, phrasings and provenance union at read time
                Some(link) if existing.contains(link) => {
                    node_rows.push(node_row(link.to_string(), proposition, provenance, &now));
                    mapping.insert(key, link.to_string());
                    counts.linked += 1;
                }
                _ => {
                    let id = new_node_id();
                    node_rows.push(node_row(id.clone(), proposition, provenance, &now));
                    existing.insert(id.clone());
                    mapping.insert(key, id);
                    counts.added += 1;
                }
            }
        }
        append_rows(&self.file("nodes.jsonl"), &node_rows)?;

        // chains: dedupe by (sorted premises, conclusion); a known argument re-stated by another source keeps its id
        let lines = self.read_chain_lines();
        let mut id_of_key: HashMap<String, String> = HashMap::new();
        let mut asserted_by_source: HashSet<String> = HashSet::new();
        for line in &lines {
            let key = chain_key_of(line);
            id_of_key.entry(key.clone()).or_insert_with(|| id_of(line));
            if line
                .get("provenance")
                .and_then(Value::as_array)
                .is_some_and(|points| {
                    points
                        .iter()
                        .any(|point| point.get("chatId").and_then(Value::as_str) == Some(source_id))
                })
            {
                asserted_by_source.insert(key);
            }
        }
        let resolve = |entry: &Value| -> Option<String> {
            if let Some(id) = mapping.get(&js::display(Some(entry))) {
                return Some(id.clone());
            }
            let id = entry.as_str()?;
            existing.contains(id).then(|| id.to_string())
        };
        let mut chain_rows: Vec<Value> = Vec::new();
        for chain in chains {
            let Some(conclusion) = chain.get("conclusion").and_then(resolve) else {
                continue;
            };
            let mut premises: Vec<String> = Vec::new();
            for premise in chain.get("premises").and_then(Value::as_array).into_iter().flatten() {
                let Some(id) = resolve(premise) else {
                    continue;
                };
                if id != conclusion && !premises.contains(&id) {
                    premises.push(id);
                }
            }
            if premises.is_empty() {
                continue;
            }
            let key = derive::chain_key(&premises, &conclusion);
            if asserted_by_source.contains(&key) {
                continue;
            }
            let id = id_of_key.get(&key).cloned().unwrap_or_else(new_chain_id);
            let mut row = Map::new();
            row.insert("id".to_string(), Value::String(id.clone()));
            row.insert(
                "premises".to_string(),
                Value::Array(premises.iter().cloned().map(Value::String).collect()),
            );
            row.insert("conclusion".to_string(), Value::String(conclusion));
            row.insert("createdAt".to_string(), Value::String(now.clone()));
            row.insert(
                "provenance".to_string(),
                Value::Array(vec![provenance_point(source_id, Value::String(String::new()))]),
            );
            chain_rows.push(Value::Object(row));
            id_of_key.insert(key.clone(), id);
            asserted_by_source.insert(key);
            counts.chains += 1;
        }
        append_rows(&self.file("chains.jsonl"), &chain_rows)?;

        // Isolated ideas the extractor judged not load-bearing (validated: only ones listed to it, in no chain).
        let removed = f
            .get("remove")
            .and_then(Value::as_array)
            .map(|items| unique(items))
            .unwrap_or_default();
        if !removed.is_empty() {
            rewrite_without(&self.file("nodes.jsonl"), is_node, |node| {
                removed
                    .iter()
                    .any(|id| node.get("id").is_some_and(|value| same_scalar(value, id)))
            })?;
        }

        let mut all: Vec<Record> = lines;
        all.extend(chain_rows.iter().filter_map(Value::as_object).cloned());
        self.simplify_ledger(&all)?;

        after_write();
        // positions are rebuildable: a layout failure must not fail (and so duplicate) a merge that already landed
        let _ = self.settle_layout();
        Ok(counts)
    }

    /// Drop the chains `derive::simplify_chains` finds redundant (by chain key), leaving unparseable lines alone.
    fn simplify_ledger(&self, all: &[Record]) -> io::Result<()> {
        let kept = derive::simplify_chains(all);
        if kept.len() == all.len() {
            return Ok(());
        }
        let kept_keys: HashSet<String> = kept.iter().map(chain_key_of).collect();
        let dropped: HashSet<String> = all
            .iter()
            .map(chain_key_of)
            .filter(|key| !kept_keys.contains(key))
            .collect();
        rewrite_without(&self.file("chains.jsonl"), is_chain, |chain| {
            dropped.contains(&chain_key_of(chain))
        })
    }
}

/// JS `===` on JSON primitives: objects and arrays compare by identity, so two JSON ones never match.
fn same_scalar(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        // JS `===`: objects and arrays compare by identity, so two JSON ones never match
        _ => false,
    }
}

/// The distinct values of `items`, in first-seen order (JS `new Set([...])`).
fn unique(items: &[Value]) -> Vec<&Value> {
    let mut out: Vec<&Value> = Vec::new();
    for item in items {
        if !out.iter().any(|seen| same_scalar(seen, item)) {
            out.push(item);
        }
    }
    out
}

/// Node's `/^p\d+$/`: `p` plus at least one ASCII digit, the whole string.
fn is_p_id(id: &str) -> bool {
    id.strip_prefix('p')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

fn chain_key_of(record: &Record) -> String {
    derive::chain_key(&derive::list(record, "premises"), s(record, "conclusion"))
}

fn hex12() -> String {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn new_node_id() -> String {
    format!("n_{}", hex12())
}

fn new_chain_id() -> String {
    format!("arg_{}", hex12())
}

/// One provenance point in Node's key order.
fn provenance_point(source_id: &str, message_id: Value) -> Value {
    let mut point = Map::new();
    point.insert("chatId".to_string(), Value::String(source_id.to_string()));
    point.insert("messageId".to_string(), message_id);
    Value::Object(point)
}

/// A node ledger row in Node's key order; a missing name or statement leaves the key out, as `JSON.stringify`
/// drops `undefined`.
fn node_row(id: String, proposition: &Value, provenance: Value, created_at: &str) -> Value {
    let mut row = Map::new();
    row.insert("id".to_string(), Value::String(id));
    for key in ["name", "statement"] {
        if let Some(value) = proposition.get(key) {
            row.insert(key.to_string(), value.clone());
        }
    }
    row.insert(
        "aliases".to_string(),
        match proposition.get("aliases") {
            Some(Value::Array(items)) => Value::Array(items.clone()),
            _ => Value::Array(Vec::new()),
        },
    );
    row.insert("createdAt".to_string(), Value::String(created_at.to_string()));
    row.insert("provenance".to_string(), provenance);
    Value::Object(row)
}

/// Node's `append` (`store.ts:91-104`): no-op on empty, mkdir, a leading newline when the file does not end with
/// one, one `JSON` line per row.
fn append_rows(path: &Path, rows: &[Value]) -> io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // a crashed write can leave the last line unterminated: start on a fresh line
    let mut lead = "";
    if let Ok(raw) = fs::read(path) {
        if !raw.is_empty() && !raw.ends_with(b"\n") {
            lead = "\n";
        }
    }
    let mut body = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            body.push('\n');
        }
        body.push_str(&serde_json::to_string(row).map_err(io::Error::other)?);
    }
    let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(format!("{lead}{body}\n").as_bytes())
}

/// Errors the extractor model can act on. `[]` = ok. Every new proposition must sit in a chain of the submission,
/// and so must every `must_connect` id (this lesson's propositions left isolated by an earlier extraction) unless
/// the submission removes it (`remove`: an isolated idea that turned out not to be load-bearing).
pub fn validate_findings(f: &Value, existing: &HashSet<String>, must_connect: &[String]) -> Vec<String> {
    let missing = || vec!["propositions e chains devono essere array (anche vuoti)".to_string()];
    let (Some(propositions), Some(chains)) = (
        f.get("propositions").and_then(Value::as_array),
        f.get("chains").and_then(Value::as_array),
    ) else {
        return missing();
    };

    let mut errors: Vec<String> = Vec::new();
    let mut local: HashSet<String> = HashSet::new();
    for (i, proposition) in propositions.iter().enumerate() {
        let at = format!("propositions[{i}]");
        match proposition.get("id").and_then(Value::as_str) {
            Some(id) if is_p_id(id) => {
                if !local.insert(id.to_string()) {
                    errors.push(format!("{at}.id \"{id}\" duplicato"));
                }
            }
            _ => errors.push(format!("{at}.id deve essere \"p1\", \"p2\", ...")),
        }
        if proposition
            .get("name")
            .and_then(Value::as_str)
            .is_none_or(|name| js::js_trim(name).is_empty())
        {
            errors.push(format!("{at}.name vuoto"));
        }
        if proposition
            .get("statement")
            .and_then(Value::as_str)
            .is_none_or(|statement| js::js_trim(statement).is_empty())
        {
            errors.push(format!("{at}.statement vuoto"));
        }
        if let Some(link) = proposition.get("linkTo") {
            if !link.as_str().is_some_and(|link| existing.contains(link)) {
                errors.push(format!(
                    "{at}.linkTo \"{}\" non esiste nel grafo",
                    js::display(Some(link))
                ));
            }
        }
    }

    let known = |id: &str| local.contains(id) || existing.contains(id);
    for (i, chain) in chains.iter().enumerate() {
        let at = format!("chains[{i}]");
        let Some(premises) = chain.get("premises").and_then(Value::as_array) else {
            errors.push(format!(
                "{at}.premises vuoto: una catena ha almeno una premessa"
            ));
            continue;
        };
        if premises.is_empty() {
            errors.push(format!(
                "{at}.premises vuoto: una catena ha almeno una premessa"
            ));
            continue;
        }
        let conclusion = chain.get("conclusion");
        let mut members: Vec<Option<&Value>> = premises.iter().map(Some).collect();
        members.push(conclusion);
        for id in members {
            if !id.and_then(Value::as_str).is_some_and(known) {
                errors.push(format!(
                    "{at}: \"{}\" non è un id locale pN né un n_ esistente",
                    js::display(id)
                ));
            }
        }
        if conclusion.is_some_and(|conclusion| premises.iter().any(|p| same_scalar(p, conclusion))) {
            errors.push(format!("{at}: la conclusione compare tra le premesse"));
        }
    }

    let mut in_chain: Vec<&Value> = Vec::new();
    for chain in chains {
        if let Some(premises) = chain.get("premises").and_then(Value::as_array) {
            in_chain.extend(premises.iter());
            if let Some(conclusion) = chain.get("conclusion") {
                in_chain.push(conclusion);
            }
        }
    }
    let link_of = |id: &str| {
        propositions
            .iter()
            .rev()
            .find(|p| p.get("linkTo").and_then(Value::as_str) == Some(id))
            .and_then(|p| p.get("id"))
    };
    for (i, proposition) in propositions.iter().enumerate() {
        if !js::truthy(Some(proposition)) {
            continue;
        }
        let Some(id) = proposition.get("id").and_then(Value::as_str) else {
            continue;
        };
        let linked = proposition.get("linkTo").is_some();
        if linked || in_chain.iter().any(|value| value.as_str() == Some(id)) {
            continue;
        }
        errors.push(format!(
            "propositions[{i}] \"{id}\" non è in nessuna catena: ogni idea della mappa regge o deriva da un'altra (mettila in una catena, o toglila se non è portante)"
        ));
    }

    let removed = f
        .get("remove")
        .and_then(Value::as_array)
        .map(|items| unique(items))
        .unwrap_or_default();
    for id in &removed {
        let label = js::display(Some(id));
        if !id.as_str().is_some_and(|id| must_connect.iter().any(|must| must == id)) {
            errors.push(format!(
                "remove: \"{label}\" non è tra le idee isolate elencate (si possono togliere solo quelle)"
            ));
        } else if in_chain.iter().any(|value| same_scalar(value, id)) {
            errors.push(format!(
                "remove: \"{label}\" è anche in una catena: o la colleghi o la togli"
            ));
        }
    }
    for id in must_connect {
        if !existing.contains(id)
            || removed.iter().any(|entry| entry.as_str() == Some(id.as_str()))
            || in_chain.iter().any(|value| value.as_str() == Some(id.as_str()))
        {
            continue;
        }
        let via_link = link_of(id).and_then(Value::as_str).unwrap_or("");
        if in_chain.iter().any(|value| value.as_str() == Some(via_link)) {
            continue;
        }
        errors.push(format!(
            "\"{id}\" è ancora isolata: mettila in una catena come premessa o conclusione, o in remove se non è portante"
        ));
    }
    errors
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;
    use std::thread;

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-store-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn lines(rows: &[String]) -> String {
        let mut out = String::new();
        for row in rows {
            out.push_str(row);
            out.push('\n');
        }
        out
    }

    fn node_line(id: &str, name: &str, chat: &str) -> String {
        json!({
            "id": id,
            "name": name,
            "statement": format!("{name} vale."),
            "aliases": [],
            "createdAt": "2026-01-01T00:00:00Z",
            "provenance": [{ "chatId": chat, "messageId": "m" }],
        })
        .to_string()
    }

    fn chain_line(id: &str, premises: &[&str], conclusion: &str, chat: &str) -> String {
        json!({
            "id": id,
            "premises": premises,
            "conclusion": conclusion,
            "createdAt": "2026-01-01T00:00:00Z",
            "provenance": [{ "chatId": chat, "messageId": "" }],
        })
        .to_string()
    }

    fn write_graph(root: &Path, nodes: &str, chains: &str) {
        let dir = root.join("graph");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("nodes.jsonl"), nodes).unwrap();
        fs::write(dir.join("chains.jsonl"), chains).unwrap();
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

    fn write_lesson(root: &Path, course: &str, filename: &str, metadata: Value, nodes: Value) {
        let dir = root.join("courses").join(course).join("lessons");
        fs::create_dir_all(&dir).unwrap();
        let doc = json!({ "metadata": metadata, "nodes": nodes });
        fs::write(
            dir.join(filename),
            format!("<script type=\"application/json\" id=\"lesson-data\">{doc}</script>"),
        )
        .unwrap();
    }

    #[test]
    fn delete_cascades_chains_and_arg_deletes_one() {
        let root = temp_dir("delete");
        write_graph(
            &root,
            &lines(&[
                node_line("n_a", "A", "demo/0001-t"),
                node_line("n_b", "B", "demo/0001-t"),
                node_line("n_c", "C", "demo/0001-t"),
            ]),
            &lines(&[
                chain_line("arg_1", &["n_a"], "n_b", "demo/0001-t"),
                chain_line("arg_2", &["n_b"], "n_c", "demo/0001-t"),
                chain_line("arg_3", &["n_a"], "n_c", "demo/0001-t"),
            ]),
        );
        let store = GraphStore::new(root.clone());

        assert_eq!(store.delete_node("n_b").unwrap(), (1, 2));
        assert_eq!(
            store.read_node_lines().iter().map(id_of).collect::<Vec<_>>(),
            ["n_a", "n_c"]
        );
        assert_eq!(
            store
                .read_chain_lines()
                .iter()
                .map(id_of)
                .collect::<Vec<_>>(),
            ["arg_3"]
        );

        assert_eq!(store.delete_node("arg_3").unwrap(), (0, 1));
        assert!(store.read_chain_lines().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rewrite_drops_blank_lines_keeps_unparseable_ones_and_rewrites_lf() {
        let root = temp_dir("rewrite");
        let n_a = node_line("n_a", "A", "demo/0001-t");
        let n_b = node_line("n_b", "B", "demo/0001-t");
        let future = "{\"id\":\"n_future\",\"name\":\"x\"}";
        write_graph(
            &root,
            &format!("{n_a}\r\n{n_b}\r\n\r\n{{broken\r\n{future}\r\n"),
            "",
        );
        let store = GraphStore::new(root.clone());

        assert_eq!(store.delete_node("n_b").unwrap(), (1, 0));
        let raw = fs::read_to_string(root.join("graph").join("nodes.jsonl")).unwrap();
        assert_eq!(raw, format!("{n_a}\n{{broken\n{future}\n"));

        // nothing matches: the CRLF bytes stay exactly as they were
        let untouched = format!("{n_a}\r\n{n_b}\r\n");
        let file = root.join("graph").join("nodes.jsonl");
        fs::write(&file, &untouched).unwrap();
        let before = fs::read(&file).unwrap();
        assert_eq!(store.delete_node("n_missing").unwrap(), (0, 0));
        assert_eq!(fs::read(&file).unwrap(), before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn readers_skip_corrupt_guard_failing_and_unterminated_lines() {
        let root = temp_dir("readers");
        write_graph(
            &root,
            &format!(
                "{}\n{{not json\n{{\"id\":\"n_future\",\"name\":\"x\"}}\n{{\"id\":\"n_cut\",\"name\":\"cut",
                node_line("n_a", "A", "demo/0001-t")
            ),
            "",
        );

        let store = GraphStore::new(root.clone());
        let nodes = store.read_node_lines();
        assert_eq!(nodes.len(), 1);
        assert_eq!(id_of(&nodes[0]), "n_a");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn save_positions_writes_known_ids_only_in_compact_js_bytes() {
        let root = temp_dir("save-positions");
        write_graph(
            &root,
            &lines(&[
                node_line("n_a", "A", "demo/0001-t"),
                node_line("n_b", "B", "demo/0001-t"),
            ]),
            "",
        );
        let graph = root.join("graph");
        fs::write(graph.join("layout.json"), "{}").unwrap();
        fs::write(graph.join("positions.json"), "{\"legacy\":true}").unwrap();
        let store = GraphStore::new(root.clone());

        store
            .save_positions(&json!({
                "ghost": { "x": 1, "y": 2 },
                "n_a": { "x": 5, "y": 6 },
            }))
            .unwrap();
        assert_eq!(
            fs::read_to_string(graph.join("layout.json")).unwrap(),
            "{\"n_a\":{\"x\":5,\"y\":6}}"
        );
        assert!(!graph.join("positions.json").exists());

        store
            .save_positions(&json!({ "n_a": { "x": "7", "y": 1 } }))
            .unwrap();
        assert_eq!(
            fs::read_to_string(graph.join("layout.json")).unwrap(),
            "{\"n_a\":{\"x\":5,\"y\":6}}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_layout_covers_every_node_without_rewriting_a_complete_one() {
        let root = temp_dir("ensure");
        write_graph(
            &root,
            &lines(&[
                node_line("n_a", "A", "demo/0001-t"),
                node_line("n_b", "B", "demo/0001-t"),
            ]),
            &lines(&[chain_line("arg_1", &["n_a"], "n_b", "demo/0001-t")]),
        );
        let store = GraphStore::new(root.clone());

        store.ensure_layout();
        let positions = store.read_positions();
        assert!(store
            .read_snapshot()
            .nodes
            .iter()
            .all(|node| positions.contains_key(id_of(node).as_str())));

        // a complete layout is left alone, byte for byte
        let file = root.join("graph").join("layout.json");
        let complete = "{\"n_a\":{\"x\":1,\"y\":2},\"n_b\":{\"x\":3,\"y\":4}}";
        fs::write(&file, complete).unwrap();
        store.ensure_layout();
        assert_eq!(fs::read_to_string(&file).unwrap(), complete);

        // a directory in place of the file must not panic
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        store.ensure_layout();
        assert!(file.is_dir());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshot_applies_the_worst_provenance_status() {
        let root = temp_dir("status");
        write_lesson(
            &root,
            "demo",
            "0001-t.html",
            metadata(1, "t", "T", "T"),
            json!([
                { "nodeId": "alpha", "title": "A", "tier": "axiom", "content": "x" },
                { "nodeId": "beta", "title": "B", "tier": "derivation", "content": "x", "dependencies": ["alpha", "ghost"] },
                { "nodeId": "gamma", "title": "C", "tier": "derivation", "content": "x" },
            ]),
        );
        fs::write(
            root.join("courses").join("demo").join("progress.jsonl"),
            format!(
                "{}\n{}\n",
                json!({"at": "2026-01-01T00:00:00Z", "sessionId": "s", "course": "demo", "lessonSlug": "t", "nodeRefs": ["alpha"], "kind": "check", "correct": true, "confidence": 20}),
                json!({"at": "2026-01-01T00:00:00Z", "sessionId": "s", "course": "demo", "lessonSlug": "t", "nodeRefs": ["beta"], "kind": "check", "correct": false}),
            ),
        )
        .unwrap();
        let point = |message: &str| json!({ "chatId": "demo/0001-t", "messageId": message });
        write_graph(
            &root,
            &lines(&[
                json!({"id": "n_p1", "name": "P1", "statement": "p1", "aliases": [], "createdAt": "2026-01-01T00:00:00Z", "provenance": [point("alpha")]}).to_string(),
                json!({"id": "n_p2", "name": "P2", "statement": "p2", "aliases": [], "createdAt": "2026-01-01T00:00:00Z", "provenance": [point("alpha"), point("beta")]}).to_string(),
                json!({"id": "n_p3", "name": "P3", "statement": "p3", "aliases": [], "createdAt": "2026-01-01T00:00:00Z", "provenance": [point("gamma")]}).to_string(),
            ]),
            &lines(&[
                chain_line("arg_1", &["n_p1"], "n_p2", "demo/0001-t"),
                chain_line("arg_2", &["n_p2"], "n_p3", "demo/0001-t"),
                chain_line("arg_3", &["n_p1", "n_p2"], "n_p3", "demo/0001-t"),
            ]),
        );
        let snapshot = GraphStore::new(root.clone()).read_snapshot();

        let status = |name: &str| {
            snapshot
                .propositions
                .iter()
                .find(|n| s(n, "name") == name)
                .and_then(|n| n.get("status"))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        assert_eq!(status("P1").as_deref(), Some("shaky"));
        assert_eq!(status("P2").as_deref(), Some("missed"));
        assert_eq!(status("P3").as_deref(), Some("taught"));

        for node in snapshot
            .nodes
            .iter()
            .filter(|n| id_of(n).starts_with("arg_"))
        {
            assert!(node.get("status").is_none(), "{}", id_of(node));
        }
        for proposition in &snapshot.propositions {
            let copy = snapshot
                .nodes
                .iter()
                .find(|n| id_of(n) == id_of(proposition))
                .unwrap();
            assert_eq!(copy.get("status"), proposition.get("status"));
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_courses_folder_drops_statuses_but_not_the_map() {
        let root = temp_dir("broken-courses");
        write_graph(
            &root,
            &lines(&[node_line("n_a", "A", "demo/0001-t")]),
            "",
        );
        // A `courses` file instead of a directory makes the lesson scan fail (ENOTDIR).
        fs::write(root.join("courses"), "not a directory").unwrap();

        let snapshot = GraphStore::new(root.clone()).read_snapshot();
        assert_eq!(snapshot.propositions.len(), 1);
        assert!(snapshot
            .propositions
            .iter()
            .all(|n| n.get("status").is_none()));
        assert!(snapshot.nodes.iter().all(|n| n.get("status").is_none()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_null_lesson_node_drops_every_lesson_label() {
        let root = temp_dir("null-node-labels");
        write_lesson(
            &root,
            "demo",
            "0001-t.html",
            metadata(1, "t", "T", "T"),
            json!([{ "nodeId": "a", "tier": "axiom" }, null]),
        );
        // The scan fails as a whole, like Node's summary outside its per-file try.
        assert!(GraphStore::lesson_labels(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sessions_and_courses_use_disk_labels_and_fall_back_to_raw_ids() {
        let root = temp_dir("labels");
        write_lesson(
            &root,
            "demo",
            "0001-t.html",
            metadata(1, "t", "T", "T"),
            json!([]),
        );
        fs::write(
            root.join("curriculum.json"),
            r#"{"chapters":[{"title":"Corso Demo","slug":"demo","lessons":[]}]}"#,
        )
        .unwrap();
        write_graph(
            &root,
            &lines(&[
                node_line("n_a", "A", "demo/0001-t"),
                node_line("n_b", "B", "gone/0001-x"),
            ]),
            "",
        );
        let store = GraphStore::new(root.clone());

        let snapshot = store.read_snapshot();
        assert_eq!(
            snapshot.sessions.get("demo/0001-t"),
            Some(&json!("Lezione 1 · T"))
        );
        assert_eq!(
            snapshot.sessions.get("gone/0001-x"),
            Some(&json!("gone/0001-x"))
        );
        assert_eq!(
            snapshot
                .courses
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["demo", "gone"]
        );
        assert_eq!(snapshot.courses[0].label, json!("Corso Demo"));
        assert_eq!(snapshot.courses[1].label, json!("gone"));

        fs::write(root.join("curriculum.json"), "{not json").unwrap();
        let snapshot = store.read_snapshot();
        assert_eq!(snapshot.courses[0].label, json!("demo"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_position_writes_are_serialised() {
        let root = temp_dir("threads");
        let rows: Vec<String> = (0..8)
            .map(|i| node_line(&format!("n_{i}"), "N", "demo/0001-t"))
            .collect();
        write_graph(&root, &lines(&rows), "");
        let store = std::sync::Arc::new(GraphStore::new(root.clone()));

        let mut handles = Vec::new();
        for i in 0..8 {
            let store = store.clone();
            handles.push(thread::spawn(move || {
                let mut update = Map::new();
                update.insert(format!("n_{i}"), json!({ "x": i, "y": i }));
                store.save_positions(&Value::Object(update)).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let positions = store.read_positions();
        assert_eq!(positions.len(), 8);
        for i in 0..8 {
            assert_eq!(
                positions.get(&format!("n_{i}")),
                Some(&json!({ "x": i, "y": i }))
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn revisions_only_grow() {
        let root = temp_dir("revision");
        let store = GraphStore::new(root.clone());
        assert_eq!(store.next_revision(), 1);
        let first = store.next_revision();
        assert_eq!(store.next_revision(), first + 1);
        let _ = fs::remove_dir_all(&root);
    }

    fn proposition(id: &str, name: &str) -> Value {
        json!({ "id": id, "name": name, "statement": format!("{name} vale.") })
    }

    fn finding_chain(premises: &[&str], conclusion: &str) -> Value {
        json!({ "premises": premises, "conclusion": conclusion })
    }

    fn findings(propositions: Vec<Value>, chains: Vec<Value>) -> Value {
        json!({ "propositions": propositions, "chains": chains })
    }

    #[test]
    fn validate_findings_reports_every_problem() {
        let errs = validate_findings(
            &json!({
                "propositions": [
                    { "id": "x", "name": "", "statement": "s", "linkTo": "n_nope" },
                    { "id": "p1", "name": "a", "statement": "b" },
                ],
                "chains": [
                    { "premises": [], "conclusion": "p1" },
                    { "premises": ["p1", "p9"], "conclusion": "p1" },
                ],
            }),
            &HashSet::new(),
            &[],
        );
        assert_eq!(
            errs,
            [
                "propositions[0].id deve essere \"p1\", \"p2\", ...",
                "propositions[0].name vuoto",
                "propositions[0].linkTo \"n_nope\" non esiste nel grafo",
                "chains[0].premises vuoto: una catena ha almeno una premessa",
                "chains[1]: \"p9\" non è un id locale pN né un n_ esistente",
                "chains[1]: la conclusione compare tra le premesse",
            ]
        );
        assert_eq!(
            validate_findings(
                &findings(
                    vec![proposition("p1", "a"), proposition("p2", "b")],
                    vec![finding_chain(&["p1"], "p2")],
                ),
                &HashSet::new(),
                &[],
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn validate_findings_wants_every_new_idea_in_a_chain() {
        let lone = validate_findings(
            &findings(
                vec![
                    proposition("p1", "a"),
                    proposition("p2", "b"),
                    proposition("p3", "c"),
                ],
                vec![finding_chain(&["p1"], "p2")],
            ),
            &HashSet::new(),
            &[],
        );
        assert_eq!(lone.len(), 1, "{lone:?}");
        assert!(lone[0].contains("\"p3\" non è in nessuna catena"), "{lone:?}");

        let ids: HashSet<String> = ["n_old", "n_gone"].iter().map(|id| id.to_string()).collect();
        let still = validate_findings(
            &findings(
                vec![proposition("p1", "a"), proposition("p2", "b")],
                vec![finding_chain(&["p1"], "p2")],
            ),
            &ids,
            &["n_old".to_string()],
        );
        assert_eq!(still.len(), 1, "{still:?}");
        assert!(still[0].contains("\"n_old\" è ancora isolata"), "{still:?}");

        assert_eq!(
            validate_findings(
                &findings(
                    vec![proposition("p1", "a")],
                    vec![finding_chain(&["n_old"], "p1")],
                ),
                &ids,
                &["n_old".to_string(), "n_deleted".to_string()],
            ),
            Vec::<String>::new()
        );

        let via_link = json!({
            "propositions": [
                { "id": "p1", "name": "a", "statement": "a", "linkTo": "n_old" },
                { "id": "p2", "name": "b", "statement": "b" },
            ],
            "chains": [{ "premises": ["p1"], "conclusion": "p2" }],
        });
        assert_eq!(
            validate_findings(&via_link, &ids, &["n_old".to_string()]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn merge_creates_links_dedupes_chains_and_simplifies() {
        let root = temp_dir("merge");
        let store = GraphStore::new(root.clone());
        let first = store
            .merge_findings(
                "c/0001-a",
                &findings(
                    vec![
                        proposition("p1", "A"),
                        proposition("p2", "B"),
                        proposition("p3", "C"),
                    ],
                    vec![
                        finding_chain(&["p1"], "p3"),
                        finding_chain(&["p1", "p2"], "p3"),
                    ],
                ),
                || {},
            )
            .unwrap();
        assert_eq!(first.added, 3);
        assert_eq!(first.chains, 2);
        // [p1,p2]=>p3 is redundant given [p1]=>p3, but it is B's only chain: kept (no isolated idea)
        assert_eq!(store.read_snapshot().chains.len(), 2);

        let snapshot = store.read_snapshot();
        let id_of_name = |name: &str| {
            snapshot
                .propositions
                .iter()
                .find(|node| s(node, "name") == name)
                .map(id_of)
                .unwrap()
        };
        let (a, c) = (id_of_name("A"), id_of_name("C"));
        let second = store
            .merge_findings(
                "c/0002-b",
                &json!({
                    "propositions": [
                        { "id": "p1", "name": "A bis", "statement": "A riformulato", "linkTo": a.clone(), "nodeId": "n7" },
                        { "id": "p2", "name": "D", "statement": "d" },
                    ],
                    "chains": [
                        { "premises": [a.clone()], "conclusion": c.clone() },
                        { "premises": ["p1"], "conclusion": "p2" },
                    ],
                }),
                || {},
            )
            .unwrap();
        assert_eq!((second.added, second.linked, second.chains), (1, 1, 2));

        let snapshot = store.read_snapshot();
        assert_eq!(snapshot.propositions.len(), 4);
        let linked = snapshot
            .propositions
            .iter()
            .find(|node| id_of(node) == a)
            .unwrap();
        let chats: Vec<&str> = linked
            .get("provenance")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|point| point.get("chatId").and_then(Value::as_str))
            .collect();
        assert_eq!(chats, ["c/0001-a", "c/0002-b"]);
        assert!(linked
            .get("aliases")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|alias| alias == "A riformulato"));
        assert_eq!(s(linked, "name"), "A");
        let same = snapshot
            .chains
            .iter()
            .find(|chain| s(chain, "conclusion") == c)
            .unwrap();
        assert_eq!(
            same.get("provenance").and_then(Value::as_array).unwrap().len(),
            2
        );
        assert!(store.graph_digest().contains(&format!("{a} | A | ")));
        assert!(store.read_positions().len() >= 4);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_appends_after_corrupt_and_unterminated_ledger_lines() {
        let root = temp_dir("merge-corrupt");
        let store = GraphStore::new(root.clone());
        store
            .merge_findings("c/1", &findings(vec![proposition("p1", "A")], vec![]), || {})
            .unwrap();
        let file = root.join("graph").join("nodes.jsonl");
        let mut raw = fs::read_to_string(&file).unwrap();
        raw.push_str("{not json\n{\"id\":3}\n{\"id\":\"n_x\",\"name\":\"cut");
        fs::write(&file, raw).unwrap();

        store
            .merge_findings("c/2", &findings(vec![proposition("p1", "B")], vec![]), || {})
            .unwrap();
        let mut names: Vec<String> = store
            .read_snapshot()
            .propositions
            .iter()
            .map(|node| s(node, "name").to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["A", "B"]);
        // the appended row starts on its own line
        assert!(fs::read_to_string(&file).unwrap().contains("cut\n"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_merges_are_serialised_and_lose_no_lines() {
        let root = temp_dir("merge-threads");
        let store = Arc::new(GraphStore::new(root.clone()));
        let mut handles = Vec::new();
        for i in 0..5 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                store
                    .merge_findings(
                        &format!("c/{i}"),
                        &findings(vec![proposition("p1", &format!("N{i}"))], vec![]),
                        || {},
                    )
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.read_snapshot().propositions.len(), 5);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_layout_failure_does_not_fail_the_merge() {
        let root = temp_dir("merge-layout");
        // a directory in place of layout.json makes every write to it throw
        fs::create_dir_all(root.join("graph").join("layout.json")).unwrap();
        let store = GraphStore::new(root.clone());
        let counts = store
            .merge_findings(
                "c/1",
                &findings(
                    vec![proposition("p1", "A"), proposition("p2", "B")],
                    vec![finding_chain(&["p1"], "p2")],
                ),
                || {},
            )
            .unwrap();
        assert_eq!((counts.added, counts.chains), (2, 1));

        fs::remove_dir_all(root.join("graph").join("layout.json")).unwrap();
        store.ensure_layout();
        let snapshot = store.read_snapshot();
        assert!(snapshot
            .nodes
            .iter()
            .all(|node| snapshot.positions.contains_key(id_of(node).as_str())));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn graph_digest_groups_by_the_birth_lesson() {
        let root = temp_dir("digest");
        let store = GraphStore::new(root.clone());
        store
            .merge_findings("c/0001-a", &findings(vec![proposition("p1", "Uno")], vec![]), || {})
            .unwrap();
        store
            .merge_findings("c/0002-b", &findings(vec![proposition("p1", "Due")], vec![]), || {})
            .unwrap();

        let digest = store.graph_digest();
        let groups: Vec<&str> = digest.split("\n\n").collect();
        assert_eq!(groups.len(), 2, "{digest}");
        assert_eq!(groups[0].lines().next().unwrap(), "## c/0001-a");
        assert_eq!(groups[1].lines().next().unwrap(), "## c/0002-b");
        let row = groups[0].lines().nth(1).unwrap();
        assert!(row.starts_with("n_") && row.ends_with(" | Uno | Uno vale."), "{row}");
        assert!(groups[1].contains(" | Due | Due vale."), "{digest}");

        // a lesson on disk labels its group instead of the raw source id
        write_lesson(
            &root,
            "c",
            "0001-a.html",
            metadata(1, "a", "Lezione A", "T"),
            json!([]),
        );
        assert_eq!(
            store.graph_digest().split('\n').next().unwrap(),
            "## Lezione 1 · Lezione A"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn graph_digest_names_a_source_without_a_lesson() {
        let root = temp_dir("digest-empty-source");
        write_graph(
            &root,
            &lines(&[json!({
                "id": "n_a",
                "name": "A",
                "statement": "A vale.",
                "aliases": [],
                "createdAt": "2026-01-01T00:00:00Z",
                "provenance": [],
            })
            .to_string()]),
            "",
        );
        assert_eq!(
            GraphStore::new(root.clone()).graph_digest(),
            "## (senza lezione)\nn_a | A | A vale."
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn isolated_by_source_groups_by_birth_lesson() {
        let root = temp_dir("isolated");
        write_graph(
            &root,
            &lines(&[
                node_line("n_a", "A", "a/0001-x"),
                node_line("n_b", "B", "a/0001-x"),
                node_line("n_c", "C", "b/0002-y"),
                node_line("n_d", "D", "b/0002-y"),
                json!({
                    "id": "n_e",
                    "name": "E",
                    "statement": "E vale.",
                    "aliases": [],
                    "createdAt": "2026-01-01T00:00:00Z",
                    "provenance": [],
                })
                .to_string(),
            ]),
            &lines(&[chain_line("arg_1", &["n_a"], "n_b", "a/0001-x")]),
        );
        let isolated = GraphStore::new(root.clone()).isolated_by_source();

        let keys: Vec<&str> = isolated.keys().map(String::as_str).collect();
        assert_eq!(keys, ["b/0002-y", ""]);
        assert_eq!(
            isolated["b/0002-y"],
            [
                ("n_c".to_string(), "C".to_string()),
                ("n_d".to_string(), "D".to_string()),
            ]
        );
        assert_eq!(isolated[""], [("n_e".to_string(), "E".to_string())]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn repair_removes_a_listed_isolated_idea() {
        let root = temp_dir("repair");
        let store = GraphStore::new(root.clone());
        store
            .merge_findings(
                "a/0001-x",
                &findings(
                    vec![
                        proposition("p1", "Sola"),
                        proposition("p2", "A"),
                        proposition("p3", "B"),
                    ],
                    vec![finding_chain(&["p2"], "p3")],
                ),
                || {},
            )
            .unwrap();
        let lone = store
            .read_snapshot()
            .propositions
            .iter()
            .find(|node| s(node, "name") == "Sola")
            .map(id_of)
            .unwrap();
        let ids: HashSet<String> = [lone.clone(), "n_other".to_string()].into_iter().collect();
        let must = vec![lone.clone()];
        assert_eq!(
            validate_findings(
                &json!({ "propositions": [], "chains": [], "remove": [lone.clone()] }),
                &ids,
                &must,
            ),
            Vec::<String>::new()
        );
        assert!(validate_findings(
            &json!({ "propositions": [], "chains": [], "remove": ["n_other"] }),
            &ids,
            &must,
        )
        .join("|")
        .contains("non è tra le idee isolate"));

        let counts = store
            .merge_findings(
                "a/0001-x",
                &json!({ "propositions": [], "chains": [], "remove": [lone.clone()] }),
                || {},
            )
            .unwrap();
        assert_eq!((counts.added, counts.linked, counts.chains), (0, 0, 0));
        assert!(!store
            .read_snapshot()
            .propositions
            .iter()
            .any(|node| id_of(node) == lone));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn after_write_runs_after_both_ledgers_are_written() {
        let root = temp_dir("after-write");
        let store = GraphStore::new(root.clone());
        let nodes = root.join("graph").join("nodes.jsonl");
        let chains = root.join("graph").join("chains.jsonl");
        let mut seen: Option<(String, String)> = None;
        store
            .merge_findings(
                "c/1",
                &findings(
                    vec![proposition("p1", "A"), proposition("p2", "B")],
                    vec![finding_chain(&["p1"], "p2")],
                ),
                || {
                    seen = Some((
                        fs::read_to_string(&nodes).unwrap_or_default(),
                        fs::read_to_string(&chains).unwrap_or_default(),
                    ));
                },
            )
            .unwrap();

        let (nodes_text, chains_text) = seen.expect("after_write ran");
        assert!(nodes_text.contains("\"name\":\"A\""), "{nodes_text}");
        assert!(nodes_text.contains("\"name\":\"B\""), "{nodes_text}");
        assert!(chains_text.contains("\"conclusion\""), "{chains_text}");
        assert_eq!(store.read_positions().len(), 2);
        let _ = fs::remove_dir_all(&root);
    }
}
