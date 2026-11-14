//! Semantic search: cosine of a query against every concept, cached passage vectors.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::concepts::{concept_text, NodeText, RankedHit};
use crate::embedder::Embedder;
use crate::embeddings::{
    cache_file, model_dir, model_present, models_dir, read_cache, sha1_hex, write_cache,
    EmbeddingConfig, VectorCache,
};

/// What `rank` says when the model folder is not there: no download on the search path.
pub const MODEL_MISSING: &str = "model missing";

/// One embedder and one vector cache per instance, both loaded on first use.
pub struct SemanticSearch {
    models_dir: PathBuf,
    cache_file: PathBuf,
    cfg: EmbeddingConfig,
    // A poisoned lock still holds a valid embedder and cache: never fail a query over it.
    state: Mutex<State>,
}

struct State {
    embedder: Option<Embedder>,
    vectors: Option<VectorCache>,
}

impl SemanticSearch {
    pub fn new(models_dir: PathBuf, cache_file: PathBuf, cfg: EmbeddingConfig) -> Self {
        Self {
            models_dir,
            cache_file,
            cfg,
            state: Mutex::new(State {
                embedder: None,
                vectors: None,
            }),
        }
    }

    pub fn for_repo(repo_root: &Path, cfg: EmbeddingConfig) -> Self {
        Self::new(models_dir(repo_root), cache_file(repo_root, &cfg), cfg)
    }

    pub fn config(&self) -> &EmbeddingConfig {
        &self.cfg
    }

    /// Blocking: async callers run it on `spawn_blocking`.
    pub fn rank(&self, query: &str, nodes: &[NodeText]) -> Result<Vec<RankedHit>, String> {
        if !model_present(&self.models_dir, &self.cfg) {
            return Err(MODEL_MISSING.to_string());
        }
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let state = &mut *guard;
        if state.embedder.is_none() {
            // not stored on failure, so the next call retries the load
            let embedder =
                Embedder::load(&model_dir(&self.models_dir, &self.cfg), &self.cfg.dtype)?;
            state.embedder = Some(embedder);
        }
        if state.vectors.is_none() {
            state.vectors = Some(read_cache(&self.cache_file, &self.cfg));
        }

        let State { embedder, vectors } = state;
        let embedder = embedder.as_mut().expect("loaded above");
        let cache = vectors.as_mut().expect("read above");

        let keys: Vec<String> = nodes
            .iter()
            .map(|node| {
                sha1_hex(&format!(
                    "{}{}",
                    self.cfg.passage_prefix,
                    concept_text(node)
                ))
            })
            .collect();

        // one entry per distinct text the cache does not hold yet, in input order
        let mut missing: Vec<(String, String)> = Vec::new();
        for (node, key) in nodes.iter().zip(keys.iter()) {
            if cache.get(key).is_some() || missing.iter().any(|(seen, _)| seen == key) {
                continue;
            }
            missing.push((
                key.clone(),
                format!("{}{}", self.cfg.passage_prefix, concept_text(node)),
            ));
        }

        let mut changed = false;
        for chunk in missing.chunks(32) {
            let texts: Vec<String> = chunk.iter().map(|(_, text)| text.clone()).collect();
            let embedded = embedder.embed(&texts)?;
            if embedded.len() != chunk.len() {
                return Err(format!(
                    "model returned {} vectors for {} texts",
                    embedded.len(),
                    chunk.len()
                ));
            }
            for ((key, _), vector) in chunk.iter().zip(embedded) {
                cache.insert(key.clone(), vector);
            }
            changed = true;
        }
        if changed {
            // best effort: a failed write only costs a re-embed next process
            write_cache(&self.cache_file, &self.cfg, cache);
        }

        let mut query_vectors = embedder.embed(&[format!("{}{}", self.cfg.query_prefix, query)])?;
        let query_vector = query_vectors
            .drain(..)
            .next()
            .ok_or_else(|| "model returned no vector for the query".to_string())?;

        Ok(nodes
            .iter()
            .zip(keys.iter())
            .map(|(node, key)| RankedHit {
                id: node.id.clone(),
                cosine: cache
                    .get(key)
                    .map(|vector| dot(&query_vector, vector))
                    .unwrap_or(0.0),
            })
            .collect())
    }
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    (0..n).map(|i| f64::from(a[i]) * f64::from(b[i])).sum()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("proxi-semantic-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn rank_without_the_model_fails_before_touching_the_cache() {
        let dir = temp_dir("missing");
        let models = dir.join("models");
        fs::create_dir_all(&models).unwrap();
        let cache = dir.join("vectors.json");
        let cfg = EmbeddingConfig::load(&dir).unwrap();
        let search = SemanticSearch::new(models, cache.clone(), cfg);
        let nodes = vec![NodeText {
            id: "n_1".to_string(),
            name: "Props".to_string(),
            statement: "Dati passati dal genitore".to_string(),
            aliases: Vec::new(),
        }];

        assert_eq!(search.rank("props", &nodes).unwrap_err(), MODEL_MISSING);
        assert!(!cache.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
