//! Snapshot derivation from the raw ledger lines: merged nodes, chains, implication nodes.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

pub type Record = Map<String, Value>;
/// id -> `{"x":..,"y":..}`, already JS-shaped.
pub type Positions = Map<String, Value>;

/// A region needs at least three propositions to be drawn.
const MIN_CLUSTER_SIZE: usize = 3;
const IMP_NAME: &str = "⟹";

fn s<'a>(record: &'a Record, key: &str) -> &'a str {
    record.get(key).and_then(Value::as_str).unwrap_or("")
}

fn list(record: &Record, key: &str) -> Vec<String> {
    match record.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn points(record: &Record) -> &[Value] {
    match record.get("provenance") {
        Some(Value::Array(items)) => items.as_slice(),
        _ => &[],
    }
}

fn array_mut<'a>(record: &'a mut Record, key: &str) -> &'a mut Vec<Value> {
    let slot = record
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !slot.is_array() {
        *slot = Value::Array(Vec::new());
    }
    match slot {
        Value::Array(items) => items,
        _ => unreachable!(),
    }
}

fn same_point(a: &Value, b: &Value) -> bool {
    a.get("chatId") == b.get("chatId") && a.get("messageId") == b.get("messageId")
}

/// A chain's merge key: sorted premises + conclusion.
pub fn chain_key(premises: &[String], conclusion: &str) -> String {
    let mut sorted = premises.to_vec();
    sorted.sort();
    format!("{}>{}", sorted.join("+"), conclusion)
}

/// The merged stored truth: node lines deduped by id, chain lines by their premises + conclusion.
pub struct Graph {
    pub nodes: Vec<Record>,
    pub chains: Vec<Record>,
}

/// First line defines the node; later lines only add phrasings and provenance points.
pub fn get_graph(node_lines: &[Record], chain_lines: &[Record]) -> Graph {
    let mut nodes: Vec<Record> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for line in node_lines {
        let id = s(line, "id").to_string();
        match by_id.get(&id) {
            Some(&index) => merge_node(&mut nodes[index], line),
            None => {
                by_id.insert(id, nodes.len());
                nodes.push(line.clone());
            }
        }
    }

    let mut chains: Vec<Record> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for line in chain_lines {
        let key = chain_key(&list(line, "premises"), s(line, "conclusion"));
        match by_key.get(&key) {
            Some(&index) => merge_provenance(&mut chains[index], line),
            None => {
                by_key.insert(key, chains.len());
                chains.push(line.clone());
            }
        }
    }
    Graph { nodes, chains }
}

fn merge_node(existing: &mut Record, line: &Record) {
    let mut phrasings = Vec::new();
    let statement = s(line, "statement");
    if !statement.is_empty() {
        phrasings.push(statement.to_string());
    }
    phrasings.extend(list(line, "aliases").into_iter().filter(|p| !p.is_empty()));

    let aliases = array_mut(existing, "aliases");
    for phrasing in phrasings {
        if !aliases.iter().any(|v| v.as_str() == Some(phrasing.as_str())) {
            aliases.push(Value::String(phrasing));
        }
    }
    merge_provenance(existing, line);
}

fn merge_provenance(existing: &mut Record, line: &Record) {
    let incoming: Vec<Value> = points(line).to_vec();
    let provenance = array_mut(existing, "provenance");
    for point in incoming {
        if !provenance.iter().any(|q| same_point(q, &point)) {
            provenance.push(point);
        }
    }
}

/// A node's birth session: its first surviving provenance point's chatId.
pub fn birth_session_of(node: &Record) -> Option<String> {
    points(node)
        .first()?
        .get("chatId")?
        .as_str()
        .map(str::to_string)
}

/// A derived implication: the chain id, its premises and conclusion.
pub struct ImpLink {
    pub id: String,
    pub premises: Vec<String>,
    pub conclusion: String,
}

/// The canvas image: propositions, derived implication nodes, drawn and logical edges.
pub struct View {
    pub nodes: Vec<Record>,
    pub edges: Vec<(String, String)>,
    pub logical_edges: Vec<(String, String)>,
    pub imp_links: Vec<ImpLink>,
}

/// Single-premise chains collapse to one arrow; multi-premise ones mint an `arg_` node; dangling chains drop.
pub fn derive_view(graph: &Graph) -> View {
    let names: HashMap<String, String> = graph
        .nodes
        .iter()
        .map(|node| (s(node, "id").to_string(), s(node, "name").to_string()))
        .collect();
    let live: HashSet<&str> = graph.nodes.iter().map(|node| s(node, "id")).collect();

    let mut imp_nodes: Vec<Record> = Vec::new();
    let mut edges: Vec<(String, String)> = Vec::new();
    let mut imp_links: Vec<ImpLink> = Vec::new();
    let mut logical_edges: Vec<(String, String)> = Vec::new();
    let mut logical_seen: HashSet<(String, String)> = HashSet::new();

    for chain in &graph.chains {
        let conclusion = s(chain, "conclusion");
        let premises = list(chain, "premises");
        if !live.contains(conclusion) || !premises.iter().all(|p| live.contains(p.as_str())) {
            continue;
        }
        for premise in &premises {
            let edge = (premise.clone(), conclusion.to_string());
            if logical_seen.insert(edge.clone()) {
                logical_edges.push(edge);
            }
        }
        if premises.len() == 1 {
            edges.push((premises[0].clone(), conclusion.to_string()));
            continue;
        }

        let id = s(chain, "id").to_string();
        let mut imp = Record::new();
        imp.insert("id".to_string(), Value::String(id.clone()));
        imp.insert("name".to_string(), Value::String(IMP_NAME.to_string()));
        imp.insert(
            "statement".to_string(),
            Value::String(format_chain_line(chain, &names)),
        );
        imp.insert("aliases".to_string(), Value::Array(Vec::new()));
        imp.insert(
            "createdAt".to_string(),
            Value::String(s(chain, "createdAt").to_string()),
        );
        imp.insert(
            "provenance".to_string(),
            Value::Array(points(chain).to_vec()),
        );
        imp_nodes.push(imp);

        imp_links.push(ImpLink {
            id: id.clone(),
            premises: premises.clone(),
            conclusion: conclusion.to_string(),
        });
        for premise in &premises {
            edges.push((premise.clone(), id.clone()));
        }
        edges.push((id, conclusion.to_string()));
    }

    let mut nodes = graph.nodes.clone();
    nodes.extend(imp_nodes);
    View {
        nodes,
        edges,
        logical_edges,
        imp_links,
    }
}

/// "Premise one, Premise two → Conclusion" by proposition names.
fn format_chain_line(chain: &Record, names: &HashMap<String, String>) -> String {
    let name_of = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_string());
    let mut premises = list(chain, "premises");
    premises.sort();
    let joined = premises
        .iter()
        .map(|premise| name_of(premise))
        .collect::<Vec<_>>()
        .join(", ");
    let conclusion = name_of(s(chain, "conclusion"));
    if joined.is_empty() {
        format!("{conclusion} (nessuna premessa)")
    } else {
        format!("{joined} → {conclusion}")
    }
}

/// One cluster of the map: a birth session (or a course) and its sorted member ids.
pub struct Cluster {
    pub id: String,
    pub label: Value,
    pub members: Vec<String>,
}

/// Groups node lines by birth session, size-desc, members sorted. Nodes without provenance stay unclustered.
pub fn birth_session_partition(nodes: &[Record]) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for node in nodes {
        let id = s(node, "id").to_string();
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(birth) = birth_session_of(node) else {
            continue;
        };
        match index.get(&birth) {
            Some(&group) => groups[group].1.push(id),
            None => {
                index.insert(birth.clone(), groups.len());
                groups.push((birth, vec![id]));
            }
        }
    }
    for (_, members) in &mut groups {
        members.sort();
    }
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    groups
}

/// An imp node takes the majority birth of its premises; ties go to the chain's own birth.
pub fn imp_cluster_key(
    chain: &Record,
    birth_of: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for premise in list(chain, "premises") {
        if let Some(birth) = birth_of(&premise) {
            *counts.entry(birth).or_insert(0) += 1;
        }
    }
    let own = birth_session_of(chain);
    if counts.is_empty() {
        return own;
    }
    let max = counts.values().copied().max().unwrap_or(0);
    let mut winners: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n == max)
        .map(|(key, _)| key)
        .collect();
    if winners.len() == 1 {
        return winners.pop();
    }
    winners.sort();
    own.or_else(|| winners.into_iter().next())
}

/// Adds every multi-premise fully-live chain id to the community of its cluster key, then sorts members.
pub fn attach_imp_to_communities(
    communities: &mut [(String, Vec<String>)],
    propositions: &[Record],
    chains: &[Record],
) {
    let births: HashMap<String, Option<String>> = propositions
        .iter()
        .map(|node| (s(node, "id").to_string(), birth_session_of(node)))
        .collect();
    let live: HashSet<String> = propositions
        .iter()
        .map(|node| s(node, "id").to_string())
        .collect();

    for chain in chains {
        let premises = list(chain, "premises");
        if premises.len() < 2 {
            continue;
        }
        let conclusion = s(chain, "conclusion");
        if !live.contains(conclusion) || !premises.iter().all(|p| live.contains(p)) {
            continue;
        }
        let birth_of = |id: &str| births.get(id).cloned().flatten();
        let Some(key) = imp_cluster_key(chain, &birth_of) else {
            continue;
        };
        let Some(community) = communities.iter_mut().find(|(id, _)| *id == key) else {
            continue;
        };
        community.1.push(s(chain, "id").to_string());
    }
    for (_, members) in communities.iter_mut() {
        members.sort();
    }
}

/// The `/api/graph` payload over raw ledger lines.
pub struct Snapshot {
    /// Propositions + derived implication nodes.
    pub nodes: Vec<Record>,
    /// Stored propositions only.
    pub propositions: Vec<Record>,
    pub edges: Vec<(String, String)>,
    pub logical_edges: Vec<(String, String)>,
    pub imp_links: Vec<ImpLink>,
    pub chains: Vec<Record>,
    pub sessions: Map<String, Value>,
    pub node_sessions: Map<String, Value>,
    pub clusters: Vec<Cluster>,
    pub courses: Vec<Cluster>,
    pub positions: Positions,
}

/// Derives the snapshot: view, sessions, birth clusters and course frames.
pub fn build_snapshot(
    node_lines: &[Record],
    chain_lines: &[Record],
    sessions: &Map<String, Value>,
    positions: Positions,
    course_labels: &HashMap<String, Value>,
) -> Snapshot {
    let graph = get_graph(node_lines, chain_lines);
    let view = derive_view(&graph);

    let mut node_chats: Vec<(String, Vec<String>)> = Vec::new();
    let mut node_chat_index: HashMap<String, usize> = HashMap::new();
    let mut seen_chats: Vec<HashSet<String>> = Vec::new();
    for node in &view.nodes {
        for point in points(node) {
            let chat = point
                .get("chatId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let id = s(node, "id").to_string();
            let index = match node_chat_index.get(&id) {
                Some(&index) => index,
                None => {
                    let index = node_chats.len();
                    node_chat_index.insert(id.clone(), index);
                    node_chats.push((id, Vec::new()));
                    seen_chats.push(HashSet::new());
                    index
                }
            };
            if seen_chats[index].insert(chat.clone()) {
                node_chats[index].1.push(chat);
            }
        }
    }
    let mut node_sessions = Map::new();
    for (id, chats) in node_chats {
        node_sessions.insert(
            id,
            Value::Array(chats.into_iter().map(Value::String).collect()),
        );
    }

    let mut cluster_groups: Vec<(String, Vec<String>)> = birth_session_partition(&graph.nodes)
        .into_iter()
        .filter(|(_, members)| members.len() >= MIN_CLUSTER_SIZE)
        .collect();
    attach_imp_to_communities(&mut cluster_groups, &graph.nodes, &graph.chains);
    let clusters = cluster_groups
        .into_iter()
        .map(|(id, members)| {
            let label = sessions.get(&id).cloned().unwrap_or(Value::Null);
            Cluster {
                id,
                label,
                members,
            }
        })
        .collect();

    // Every birth lesson, any size, grouped by its course so no node falls outside its frame.
    let mut lesson_groups = birth_session_partition(&graph.nodes);
    attach_imp_to_communities(&mut lesson_groups, &graph.nodes, &graph.chains);
    let mut by_course: HashMap<String, Vec<String>> = HashMap::new();
    for (id, members) in lesson_groups {
        let course = id.split('/').next().unwrap_or("").to_string();
        by_course.entry(course).or_default().extend(members);
    }
    let mut courses: Vec<Cluster> = by_course
        .into_iter()
        .map(|(id, mut members)| {
            members.sort();
            let label = match course_labels.get(&id) {
                Some(label) if !label.is_null() => label.clone(),
                _ => Value::String(id.clone()),
            };
            Cluster {
                id,
                label,
                members,
            }
        })
        .collect();
    courses.sort_by(|a, b| a.id.cmp(&b.id));

    Snapshot {
        nodes: view.nodes,
        propositions: graph.nodes,
        edges: view.edges,
        logical_edges: view.logical_edges,
        imp_links: view.imp_links,
        chains: graph.chains,
        sessions: sessions.clone(),
        node_sessions,
        clusters,
        courses,
        positions,
    }
}

impl Snapshot {
    /// Keys in the order the wire contract expects; `positions` only when non-empty.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("nodes".to_string(), records_json(&self.nodes));
        out.insert("propositions".to_string(), records_json(&self.propositions));
        out.insert("edges".to_string(), edges_json(&self.edges));
        out.insert("logicalEdges".to_string(), edges_json(&self.logical_edges));
        out.insert(
            "impLinks".to_string(),
            Value::Array(self.imp_links.iter().map(ImpLink::to_json).collect()),
        );
        out.insert("chains".to_string(), records_json(&self.chains));
        out.insert("sessions".to_string(), Value::Object(self.sessions.clone()));
        out.insert(
            "nodeSessions".to_string(),
            Value::Object(self.node_sessions.clone()),
        );
        out.insert(
            "clusters".to_string(),
            Value::Array(self.clusters.iter().map(Cluster::to_json).collect()),
        );
        out.insert(
            "courses".to_string(),
            Value::Array(self.courses.iter().map(Cluster::to_json).collect()),
        );
        if !self.positions.is_empty() {
            out.insert(
                "positions".to_string(),
                Value::Object(self.positions.clone()),
            );
        }
        Value::Object(out)
    }
}

impl ImpLink {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".to_string(), Value::String(self.id.clone()));
        map.insert(
            "premises".to_string(),
            Value::Array(self.premises.iter().cloned().map(Value::String).collect()),
        );
        map.insert(
            "conclusion".to_string(),
            Value::String(self.conclusion.clone()),
        );
        Value::Object(map)
    }
}

impl Cluster {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".to_string(), Value::String(self.id.clone()));
        map.insert("label".to_string(), self.label.clone());
        map.insert(
            "members".to_string(),
            Value::Array(self.members.iter().cloned().map(Value::String).collect()),
        );
        Value::Object(map)
    }
}

fn records_json(records: &[Record]) -> Value {
    Value::Array(records.iter().cloned().map(Value::Object).collect())
}

fn edges_json(edges: &[(String, String)]) -> Value {
    Value::Array(
        edges
            .iter()
            .map(|(from, to)| {
                let mut map = Map::new();
                map.insert("from".to_string(), Value::String(from.clone()));
                map.insert("to".to_string(), Value::String(to.clone()));
                Value::Object(map)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(value: Value) -> Record {
        value.as_object().cloned().unwrap_or_default()
    }

    fn points_list(pairs: &[(&str, &str)]) -> Value {
        Value::Array(
            pairs
                .iter()
                .map(|(chat, message)| json!({ "chatId": chat, "messageId": message }))
                .collect(),
        )
    }

    fn node_line(id: &str, name: &str, provenance: Value) -> Record {
        record(json!({
            "id": id,
            "name": name,
            "statement": format!("{name} vale."),
            "aliases": [],
            "createdAt": "2026-01-01T00:00:00Z",
            "provenance": provenance,
        }))
    }

    fn chain_line(
        id: &str,
        premises: &[&str],
        conclusion: &str,
        provenance: Value,
    ) -> Record {
        record(json!({
            "id": id,
            "premises": premises,
            "conclusion": conclusion,
            "provenance": provenance,
            "createdAt": "2026-01-01T00:00:00Z",
        }))
    }

    #[test]
    fn chain_key_sorts_the_premises() {
        let premises = vec!["n_b".to_string(), "n_a".to_string()];
        assert_eq!(chain_key(&premises, "n_c"), "n_a+n_b>n_c");
    }

    #[test]
    fn merging_node_lines_keeps_the_first_identity() {
        let first = record(json!({
            "id": "n_a",
            "name": "A",
            "statement": "A vale.",
            "aliases": [],
            "createdAt": "2026-01-01T00:00:00Z",
            "provenance": [{ "chatId": "demo/0001-t", "messageId": "alpha" }],
            "note": "kept",
        }));
        let later = record(json!({
            "id": "n_a",
            "name": "A bis",
            "statement": "A riformulato",
            "aliases": ["A riformulato", "", "A2"],
            "createdAt": "2026-02-01T00:00:00Z",
            "provenance": [
                { "chatId": "demo/0001-t", "messageId": "alpha" },
                { "chatId": "demo/0002-u", "messageId": "delta" },
            ],
        }));

        let graph = get_graph(&[first, later], &[]);
        assert_eq!(graph.nodes.len(), 1);
        let text = serde_json::to_string(&Value::Object(graph.nodes[0].clone())).unwrap();
        assert_eq!(
            text,
            "{\"id\":\"n_a\",\"name\":\"A\",\"statement\":\"A vale.\",\"aliases\":[\"A riformulato\",\"A2\"],\"createdAt\":\"2026-01-01T00:00:00Z\",\"provenance\":[{\"chatId\":\"demo/0001-t\",\"messageId\":\"alpha\"},{\"chatId\":\"demo/0002-u\",\"messageId\":\"delta\"}],\"note\":\"kept\"}"
        );
    }

    #[test]
    fn a_repeated_chain_key_merges_into_the_first_line() {
        let chains = vec![
            chain_line(
                "arg_1",
                &["n_a", "n_b"],
                "n_c",
                points_list(&[("demo/0001-t", "")]),
            ),
            chain_line(
                "arg_9",
                &["n_b", "n_a"],
                "n_c",
                points_list(&[("demo/0002-u", "")]),
            ),
        ];

        let graph = get_graph(&[], &chains);
        assert_eq!(graph.chains.len(), 1);
        assert_eq!(s(&graph.chains[0], "id"), "arg_1");
        assert_eq!(list(&graph.chains[0], "premises"), ["n_a", "n_b"]);
        assert_eq!(points(&graph.chains[0]).len(), 2);
    }

    #[test]
    fn dangling_chains_never_reach_the_view() {
        let nodes = vec![
            node_line("n_a", "A", points_list(&[("demo/0001-t", "a")])),
            node_line("n_b", "B", points_list(&[("demo/0001-t", "b")])),
            node_line("n_c", "C", points_list(&[("demo/0001-t", "c")])),
        ];
        let chains = vec![
            chain_line(
                "arg_1",
                &["n_a"],
                "n_b",
                points_list(&[("demo/0001-t", "")]),
            ),
            chain_line(
                "arg_2",
                &["n_missing"],
                "n_c",
                points_list(&[("demo/0001-t", "")]),
            ),
        ];

        let view = derive_view(&get_graph(&nodes, &chains));
        assert_eq!(view.edges, [("n_a".to_string(), "n_b".to_string())]);
        assert!(view.imp_links.is_empty());
        assert_eq!(view.nodes.len(), 3);
    }

    #[test]
    fn an_empty_snapshot_has_no_positions_key() {
        let snapshot = build_snapshot(
            &[],
            &[],
            &Map::new(),
            Positions::new(),
            &HashMap::new(),
        );

        assert!(snapshot.nodes.is_empty());
        let value = snapshot.to_json();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "nodes",
                "propositions",
                "edges",
                "logicalEdges",
                "impLinks",
                "chains",
                "sessions",
                "nodeSessions",
                "clusters",
                "courses"
            ]
        );
        assert!(value.get("positions").is_none());
    }

    #[test]
    fn a_snapshot_mints_the_imp_node_clusters_and_course_frames() {
        let nodes = vec![
            node_line("n_a", "A", points_list(&[("demo/0001-t", "alpha")])),
            node_line("n_b", "B", points_list(&[("demo/0001-t", "beta")])),
            node_line("n_c", "C", points_list(&[("demo/0001-t", "gamma")])),
            node_line("n_d", "D", points_list(&[("zeta/0001-x", "delta")])),
        ];
        let chains = vec![chain_line(
            "arg_1",
            &["n_a", "n_b"],
            "n_c",
            points_list(&[("demo/0001-t", "")]),
        )];
        let mut sessions = Map::new();
        sessions.insert("demo/0001-t".to_string(), json!("Lezione 1 · T"));
        let mut labels = HashMap::new();
        labels.insert("demo".to_string(), json!("Corso Demo"));
        let mut positions = Positions::new();
        positions.insert("n_a".to_string(), json!({"x": 1, "y": 2}));

        let snapshot = build_snapshot(&nodes, &chains, &sessions, positions, &labels);

        assert_eq!(snapshot.nodes.len(), snapshot.propositions.len() + 1);
        assert_eq!(snapshot.imp_links.len(), 1);
        let imp = serde_json::to_string(&Value::Object(snapshot.nodes.last().unwrap().clone()))
            .unwrap();
        assert_eq!(
            imp,
            "{\"id\":\"arg_1\",\"name\":\"⟹\",\"statement\":\"A, B → C\",\"aliases\":[],\"createdAt\":\"2026-01-01T00:00:00Z\",\"provenance\":[{\"chatId\":\"demo/0001-t\",\"messageId\":\"\"}]}"
        );

        assert_eq!(snapshot.clusters.len(), 1);
        assert_eq!(snapshot.clusters[0].id, "demo/0001-t");
        assert_eq!(snapshot.clusters[0].label, json!("Lezione 1 · T"));
        assert_eq!(
            snapshot.clusters[0].members,
            ["arg_1", "n_a", "n_b", "n_c"]
        );

        let course_ids: Vec<&str> = snapshot.courses.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(course_ids, ["demo", "zeta"]);
        assert_eq!(snapshot.courses[0].label, json!("Corso Demo"));
        assert_eq!(snapshot.courses[1].label, json!("zeta"));

        let value = snapshot.to_json();
        let text = serde_json::to_string(&value).unwrap();
        assert!(text.starts_with("{\"nodes\":"));
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "nodes",
                "propositions",
                "edges",
                "logicalEdges",
                "impLinks",
                "chains",
                "sessions",
                "nodeSessions",
                "clusters",
                "courses",
                "positions"
            ]
        );
    }

    #[test]
    fn an_imp_takes_the_majority_birth_and_its_own_birth_on_a_tie() {
        let birth_of = |id: &str| match id {
            "n_a" | "n_c" => Some("s1".to_string()),
            "n_b" => Some("s2".to_string()),
            _ => None,
        };

        let tie = chain_line(
            "arg_1",
            &["n_a", "n_b"],
            "n_c",
            points_list(&[("s3", "")]),
        );
        assert_eq!(imp_cluster_key(&tie, &birth_of), Some("s3".to_string()));

        let no_own_birth = chain_line(
            "arg_1",
            &["n_a", "n_b"],
            "n_c",
            Value::Array(Vec::new()),
        );
        assert_eq!(
            imp_cluster_key(&no_own_birth, &birth_of),
            Some("s1".to_string())
        );

        let majority = chain_line(
            "arg_1",
            &["n_a", "n_c"],
            "n_c",
            points_list(&[("s3", "")]),
        );
        assert_eq!(
            imp_cluster_key(&majority, &birth_of),
            Some("s1".to_string())
        );
    }
}
