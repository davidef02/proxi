//! Graph ledgers under `<dataRoot>/graph/`: readers, positions, delete and the layout.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};

use super::derive::{self, Positions, Record, Snapshot};
use super::layout;
use crate::curriculum;
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
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
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
}
