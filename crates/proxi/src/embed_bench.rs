//! `proxi embed-bench`: recall@3 and median query time of the embedding model.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use crate::concepts::{concept_text, NodeText, RankedHit};
use crate::data_root;
use crate::embedder::Embedder;
use crate::embeddings::{model_dir, model_present, models_dir, record_bench, EmbeddingConfig};
use crate::graph::store::GraphStore;
use crate::model_download::ensure_model;

/// Gate of D5: the model must find the expected concept in the top three almost always.
pub const MIN_RECALL: f64 = 0.95;
pub const MAX_MEDIAN_MS: f64 = 10.0;
pub const TOP_K: usize = 3;

/// `proxi embed-bench` flags.
#[derive(clap::Args, Debug, Default)]
pub struct EmbedBenchArgs {
    /// Query file (default <dataRoot>/bench/embed-bench-queries.json)
    #[arg(long, value_name = "PATH")]
    pub queries: Option<PathBuf>,
    /// Print the result only; leave the model file untouched
    #[arg(long)]
    pub no_write: bool,
    /// Hugging Face base URL for a missing model (tests: a local fake)
    #[arg(long, value_name = "URL")]
    pub huggingface_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BenchQuery {
    pub q: String,
    pub lang: String,
    pub expect: String,
    pub expect_id: String,
}

/// Port of `readQueries` (extensions/embed-bench.ts): names become ids once, ranking compares ids.
pub fn parse_queries(
    text: &str,
    label: &str,
    concept_ids: &HashMap<String, String>,
) -> Result<Vec<BenchQuery>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| format!("{label}: invalid JSON: {e}"))?;
    let Value::Array(items) = value else {
        return Err(format!("{label} must be a non-empty JSON array"));
    };
    if items.is_empty() {
        return Err(format!("{label} must be a non-empty JSON array"));
    }

    let mut queries = Vec::with_capacity(items.len());
    for (i, entry) in items.iter().enumerate() {
        let at = format!("{label}[{i}]");
        let q = match entry.get("q").and_then(Value::as_str) {
            Some(q) if !q.trim().is_empty() => q.to_string(),
            _ => return Err(format!("{at}.q must be a non-empty string")),
        };
        let lang = match entry.get("lang").and_then(Value::as_str) {
            Some("it") => "it",
            Some("en") => "en",
            _ => return Err(format!("{at}.lang must be \"it\" or \"en\"")),
        };
        let expect = entry.get("expect");
        let id = expect
            .and_then(Value::as_str)
            .and_then(|name| concept_ids.get(name));
        let Some(id) = id else {
            return Err(format!(
                "{at}.expect does not match any concept name: {}",
                crate::js::display(expect)
            ));
        };
        queries.push(BenchQuery {
            q,
            lang: lang.to_string(),
            expect: expect
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            expect_id: id.clone(),
        });
    }
    Ok(queries)
}

pub fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// Nearest-rank percentile, like the Node bench: good enough for 40 samples.
pub fn percentile(xs: &[f64], p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let last = sorted.len() - 1;
    let index = (p * last as f64).floor().clamp(0.0, last as f64) as usize;
    sorted[index]
}

/// The floor under the model's own top hits: p10 of the third cosine, rounded down to 0.05.
pub fn min_score_suggestion(third_cosines: &[f64]) -> f64 {
    let p10 = percentile(third_cosines, 0.1);
    round2(((p10 / 0.05).floor() * 0.05).max(0.2))
}

pub fn run_cli(args: EmbedBenchArgs) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("Error: {message}");
            1
        }
    }
}

fn run(args: EmbedBenchArgs) -> Result<i32, String> {
    let repo_root = data_root::repo_root();
    let data_root = data_root::resolve_data_root(
        &repo_root,
        std::env::var(data_root::DATA_ROOT_ENV).ok().as_deref(),
        &data_root::home_dir(),
    )
    .data_root;
    let cfg = EmbeddingConfig::load(&repo_root)?;

    // Everything below the model is read first: a bad query file costs no download.
    let concepts: Vec<NodeText> = GraphStore::new(data_root.clone())
        .read_snapshot()
        .propositions
        .iter()
        .map(NodeText::from_record)
        .collect();
    if concepts.is_empty() {
        return Err(format!(
            "no concepts in {}",
            data_root.join("graph").join("nodes.jsonl").display()
        ));
    }

    let file = queries_file(&data_root, args.queries.as_deref());
    if !file.is_file() {
        return Err(format!(
            "queries not found: {}\nwrite bench/embed-bench-queries.json in the data folder",
            file.display()
        ));
    }
    let text =
        std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let by_name: HashMap<String, String> = concepts
        .iter()
        .map(|node| (node.name.clone(), node.id.clone()))
        .collect();
    let queries = parse_queries(&text, &file.display().to_string(), &by_name)?;

    let it = queries.iter().filter(|q| q.lang == "it").count();
    println!(
        "{} concepts, {} queries ({} it, {} en) from {}",
        concepts.len(),
        queries.len(),
        it,
        queries.len() - it,
        file.display()
    );

    let models = models_dir(&repo_root);
    if !model_present(&models, &cfg) {
        println!("Downloading {} ...", cfg.model);
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let downloaded = runtime.block_on(ensure_model(
            &models,
            &cfg,
            args.huggingface_url.as_deref(),
        ));
        if !downloaded {
            return Err(format!(
                "embedding model not available: download of {} failed",
                cfg.model
            ));
        }
    }

    let mut embedder = Embedder::load(&model_dir(&models, &cfg), &cfg.dtype)?;
    let texts: Vec<String> = concepts
        .iter()
        .map(|node| format!("{}{}", cfg.passage_prefix, concept_text(node)))
        .collect();
    let mut passages: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(32) {
        passages.extend(embedder.embed(chunk)?);
    }

    // warm-up: the first inference pays for the session build
    rank(
        &mut embedder,
        &concepts,
        &passages,
        &cfg.query_prefix,
        &queries[0].q,
    )?;

    let mut times: Vec<f64> = Vec::with_capacity(queries.len());
    let mut third_cosines: Vec<f64> = Vec::new();
    let mut missed: Vec<&BenchQuery> = Vec::new();
    let mut found = 0_usize;
    for query in &queries {
        // one search = embed the query + score every concept + sort (corpus vectors excluded)
        let started = Instant::now();
        let hits = rank(
            &mut embedder,
            &concepts,
            &passages,
            &cfg.query_prefix,
            &query.q,
        )?;
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        if hits.iter().any(|hit| hit.id == query.expect_id) {
            found += 1;
            if concepts.len() >= TOP_K {
                third_cosines.push(hits[TOP_K - 1].cosine);
            }
        } else {
            missed.push(query);
        }
    }

    let recall = found as f64 / queries.len() as f64;
    let median_ms = median(&times);
    let passes = recall >= MIN_RECALL && median_ms <= MAX_MEDIAN_MS;

    println!("model: {} ({})", cfg.model, cfg.dtype);
    println!(
        "recall@3: {:.1}% ({}/{}) [min {:.1}%]",
        recall * 100.0,
        found,
        queries.len(),
        MIN_RECALL * 100.0
    );
    println!(
        "median: {:.1} ms [max {:.1} ms]",
        median_ms, MAX_MEDIAN_MS
    );
    if !missed.is_empty() {
        println!("Missed:");
        for query in &missed {
            println!("  [{}] {} -> {}", query.lang, query.q, query.expect);
        }
    }
    println!(
        "minScore suggestion: {:.2} (current {:.2})",
        min_score_suggestion(&third_cosines),
        cfg.min_score
    );
    if passes {
        println!("PASS");
    } else {
        println!(
            "FAIL: try another model: edit model, dtype, queryPrefix, passagePrefix in \
             app/server/graph/embedding-model.json and rerun proxi embed-bench"
        );
    }

    if args.no_write {
        println!("--no-write: model file not updated");
    } else {
        let measured_at = crate::events::iso_now();
        let record = json!({
            "model": cfg.model,
            "dtype": cfg.dtype,
            "recall": round4(recall),
            "medianMs": round1(median_ms),
            "queries": queries.len(),
            "concepts": concepts.len(),
            "passes": passes,
            "measuredAt": &measured_at[..10],
        });
        record_bench(&repo_root, record)?;
        println!("Recorded in app/server/graph/embedding-model.json");
    }

    Ok(if passes { 0 } else { 1 })
}

/// Node `path.resolve(dataRoot, queries)`, then the default file in the data folder.
fn queries_file(data_root: &Path, queries: Option<&Path>) -> PathBuf {
    match queries {
        Some(path) if path.has_root() => data_root::resolve_path(path),
        Some(path) => data_root::resolve_path(&data_root.join(path)),
        None => data_root.join("bench").join("embed-bench-queries.json"),
    }
}

/// One search: embed the query once, cosine every passage, stable sort, top `TOP_K`.
fn rank(
    embedder: &mut Embedder,
    concepts: &[NodeText],
    passages: &[Vec<f32>],
    query_prefix: &str,
    query: &str,
) -> Result<Vec<RankedHit>, String> {
    let mut vectors = embedder.embed(&[format!("{query_prefix}{query}")])?;
    let query_vector = vectors
        .drain(..)
        .next()
        .ok_or_else(|| "model returned no vector for the query".to_string())?;
    let mut hits: Vec<RankedHit> = concepts
        .iter()
        .zip(passages)
        .map(|(node, passage)| RankedHit {
            id: node.id.clone(),
            cosine: dot(&query_vector, passage),
        })
        .collect();
    // stable: equal cosines keep ledger order
    hits.sort_by(|a, b| b.cosine.partial_cmp(&a.cosine).unwrap_or(Ordering::Equal));
    hits.truncate(TOP_K);
    Ok(hits)
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    (0..n).map(|i| f64::from(a[i]) * f64::from(b[i])).sum()
}

/// JS `Math.round(x * 10^d) / 10^d`; only non-negative values reach this.
fn round_to(x: f64, decimals: i32) -> f64 {
    let factor = 10_f64.powi(decimals);
    (x * factor).round() / factor
}

fn round1(x: f64) -> f64 {
    round_to(x, 1)
}

fn round2(x: f64) -> f64 {
    round_to(x, 2)
}

fn round4(x: f64) -> f64 {
    round_to(x, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concept_ids() -> HashMap<String, String> {
        HashMap::from([
            ("A".to_string(), "n_a".to_string()),
            ("B".to_string(), "n_b".to_string()),
        ])
    }

    #[test]
    fn parse_queries_maps_names_to_ids() {
        let ids = concept_ids();
        let queries = parse_queries(
            r#"[{"q":"props","lang":"it","expect":"A"},{"q":"for loop","lang":"en","expect":"B"}]"#,
            "q.json",
            &ids,
        )
        .unwrap();

        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0].q, "props");
        assert_eq!(queries[0].lang, "it");
        assert_eq!(queries[0].expect, "A");
        assert_eq!(queries[0].expect_id, "n_a");
        assert_eq!(queries[1].lang, "en");
        assert_eq!(queries[1].expect_id, "n_b");
    }

    #[test]
    fn parse_queries_reports_the_bad_entry() {
        let ids = concept_ids();
        let error = |text: &str| parse_queries(text, "q.json", &ids).unwrap_err();

        assert!(error("{").starts_with("q.json: invalid JSON:"));
        assert_eq!(error("{}"), "q.json must be a non-empty JSON array");
        assert_eq!(error("[]"), "q.json must be a non-empty JSON array");
        assert_eq!(
            error(r#"[{"q":"  ","lang":"it","expect":"A"}]"#),
            "q.json[0].q must be a non-empty string"
        );
        assert_eq!(
            error(r#"[{"q":"x","lang":"fr","expect":"A"}]"#),
            "q.json[0].lang must be \"it\" or \"en\""
        );
        assert_eq!(
            error(r#"[{"q":"x","lang":"it","expect":"Nope"}]"#),
            "q.json[0].expect does not match any concept name: Nope"
        );
        assert_eq!(
            error(r#"[{"q":"x","lang":"it"}]"#),
            "q.json[0].expect does not match any concept name: undefined"
        );
    }

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn percentile_is_nearest_rank() {
        let xs: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(percentile(&xs, 0.1), 1.0);
        assert_eq!(percentile(&[], 0.1), 0.0);
    }

    #[test]
    fn min_score_suggestion_rounds_down_to_five_cents() {
        assert_eq!(min_score_suggestion(&[0.42, 0.5, 0.6]), 0.4);
        assert_eq!(min_score_suggestion(&[0.1]), 0.2);
    }
}
