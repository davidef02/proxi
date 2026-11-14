//! Concept ranking: lexical score, cosine + lexical bonus, minScore filter and cap.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde_json::Value;

use crate::graph::derive::Record;

/// Same cap as `SEARCH_LIMIT` in app/server/graph/concepts.ts.
pub const SEARCH_LIMIT: usize = 10;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(source.contains("export const LEXICAL_BONUS = 0.15;"));
        assert_eq!(SEARCH_LIMIT, 10);
        assert_eq!(LEXICAL_BONUS, 0.15);
    }
}
