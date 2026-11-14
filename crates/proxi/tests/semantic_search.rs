mod common;

use std::fs;
use std::path::PathBuf;

use proxi::concepts::{rank_concepts, NodeText};
use proxi::embeddings::EmbeddingConfig;
use proxi::semantic::SemanticSearch;

fn node(id: &str, name: &str, statement: &str) -> NodeText {
    NodeText {
        id: id.to_string(),
        name: name.to_string(),
        statement: statement.to_string(),
        aliases: Vec::new(),
    }
}

fn sample_nodes() -> Vec<NodeText> {
    common::SAMPLE_CONCEPTS
        .iter()
        .map(|(id, name, statement)| node(id, name, statement))
        .collect()
}

/// Config from the real model file plus a models folder holding the model. `None` skips the test:
/// the model comes from the postinstall download, absent in a fresh worktree.
fn load_model() -> Option<(EmbeddingConfig, PathBuf)> {
    let cfg = EmbeddingConfig::load(&common::repo_root()).expect("embedding-model.json");
    match common::model_root(&cfg.model) {
        Some(root) => Some((cfg, root)),
        None => {
            eprintln!("skipped: embedding model not downloaded");
            None
        }
    }
}

/// The names `search` returns for `query`, best first, after the real ranking and the minScore filter.
fn ranked_names(
    search: &SemanticSearch,
    query: &str,
    nodes: &[NodeText],
    cfg: &EmbeddingConfig,
) -> Vec<String> {
    let hits = search.rank(query, nodes).expect("rank");
    rank_concepts(query, nodes, Some(&hits), cfg.min_score, 10)
        .into_iter()
        .map(|scored| nodes[scored.index].name.clone())
        .collect()
}

#[test]
fn works_offline_with_one_cosine_per_node() {
    let Some((cfg, model_root)) = load_model() else {
        return;
    };
    let temp = common::temp_dir("semantic-offline");
    let nodes = sample_nodes();
    // no URL and no HTTP client anywhere on this path: only the local model folder
    let search = SemanticSearch::new(model_root, temp.0.join("vectors.json"), cfg);

    let hits = search.rank("computed", &nodes).expect("rank");
    assert_eq!(
        hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
        nodes.iter().map(|node| node.id.as_str()).collect::<Vec<_>>()
    );
    for hit in &hits {
        assert!(
            hit.cosine.is_finite() && hit.cosine.abs() <= 1.001,
            "{}: {}",
            hit.id,
            hit.cosine
        );
    }
}

#[test]
fn cache_is_written_and_reused() {
    let Some((cfg, model_root)) = load_model() else {
        return;
    };
    let temp = common::temp_dir("semantic-cache");
    let file = temp.0.join("vectors.json");
    let nodes = sample_nodes();

    let first = SemanticSearch::new(model_root.clone(), file.clone(), cfg.clone());
    let hits = first.rank("computed", &nodes).expect("first rank");
    assert!(file.is_file(), "the first rank writes the cache");

    let cache: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).expect("read cache")).expect("cache json");
    assert_eq!(
        cache
            .as_object()
            .expect("cache object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["model", "dtype", "queryPrefix", "passagePrefix", "vectors"]
    );
    assert_eq!(cache["vectors"].as_object().expect("vectors").len(), 3);

    let second = SemanticSearch::new(model_root.clone(), file.clone(), cfg.clone());
    let again = second.rank("computed", &nodes).expect("second rank");
    for (a, b) in hits.iter().zip(&again) {
        assert_eq!(a.id, b.id);
        assert!(
            (a.cosine - b.cosine).abs() < 1e-6,
            "{}: {} vs {}",
            a.id,
            a.cosine,
            b.cosine
        );
    }

    // a stale metadata field empties the cache; the next batch rewrites it with the current config
    let mut other = cache.clone();
    other["dtype"] = serde_json::json!("other");
    fs::write(&file, other.to_string()).expect("write stale cache");
    let third = SemanticSearch::new(model_root, file.clone(), cfg);
    third
        .rank("computed", &nodes)
        .expect("rank after a stale cache");
    let rewritten: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).expect("read cache")).expect("cache json");
    assert_eq!(rewritten["dtype"], serde_json::json!("q8"));
}

#[test]
fn computed_finds_proprieta_calcolate() {
    let Some((cfg, model_root)) = load_model() else {
        return;
    };
    let temp = common::temp_dir("semantic-computed");
    let nodes = vec![
        node(
            "n_calc",
            "Proprietà calcolate",
            // no "computed" anywhere: the hit must be semantic, not lexical
            "Proprietà calcolata in Vue: un valore calcolato dalle dipendenze reattive e messo in cache, ricalcolato solo al loro cambiare.",
        ),
        node("n_for", "Ciclo for", ""),
        node("n_promessa", "Promessa", ""),
        node("n_template", "Template Vue", ""),
        node("n_props", "Props", ""),
        node("n_click", "Evento click", ""),
        node("n_array", "Array", ""),
    ];
    let search = SemanticSearch::new(model_root, temp.0.join("vectors.json"), cfg.clone());

    let hits = search.rank("computed", &nodes).expect("rank");
    let scored = rank_concepts("computed", &nodes, Some(&hits), cfg.min_score, 10);
    let top: Vec<&str> = scored
        .iter()
        .take(3)
        .map(|scored| nodes[scored.index].id.as_str())
        .collect();
    assert!(top.contains(&"n_calc"), "{top:?}");
}

#[test]
fn added_concept_found_deleted_gone() {
    let Some((cfg, model_root)) = load_model() else {
        return;
    };
    let temp = common::temp_dir("semantic-ac7");
    let search = SemanticSearch::new(model_root, temp.0.join("vectors.json"), cfg.clone());
    let mut nodes = vec![
        node("n_base", "Concetto base", ""),
        node("n_altro", "Altro argomento", ""),
    ];

    let absent = ranked_names(&search, "concetto aggiunto dopo", &nodes, &cfg);
    assert!(
        !absent.iter().any(|name| name == "Concetto aggiunto dopo"),
        "{absent:?}"
    );

    nodes.push(node("n_nuovo", "Concetto aggiunto dopo", ""));
    let found = ranked_names(&search, "concetto aggiunto dopo", &nodes, &cfg);
    assert!(
        found
            .iter()
            .take(3)
            .any(|name| name == "Concetto aggiunto dopo"),
        "{found:?}"
    );

    nodes.pop();
    let gone = ranked_names(&search, "concetto aggiunto dopo", &nodes, &cfg);
    assert!(
        !gone.iter().any(|name| name == "Concetto aggiunto dopo"),
        "{gone:?}"
    );
}
