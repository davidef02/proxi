#![allow(dead_code)]

//! In-memory `SyncProvider` for the engine tests (port of `memProvider` in `app/test/sync-engine.test.ts`).
//! Writes and removals are staged until `commit()`, like the GitHub provider.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use proxi::sync::types::{ProviderError, RemoteStat, SyncProvider};

pub fn sha1_hex(data: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let digest = Sha1::digest(data);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct MemRemote {
    pub store: IndexMap<String, (Vec<u8>, f64)>,
    pub staged: IndexMap<String, Option<(Vec<u8>, f64)>>,
    pub reads: usize,
    pub writes: usize,
    pub commits: usize,
    pub lists: usize,
    pub removes: usize,
    /// A commit with `moves_left > 0` moves the remote under us and fails once per count.
    pub moves_left: usize,
    pub fail_write: HashSet<String>,
    pub fail_list: Option<String>,
}

#[derive(Clone)]
pub struct MemProvider(pub Arc<Mutex<MemRemote>>);

impl MemProvider {
    pub fn new(files: &[(&str, &str, f64)]) -> Self {
        let mut store = IndexMap::new();
        for (rel, data, mtime_ms) in files {
            store.insert((*rel).to_string(), (data.as_bytes().to_vec(), *mtime_ms));
        }
        Self(Arc::new(Mutex::new(MemRemote {
            store,
            staged: IndexMap::new(),
            reads: 0,
            writes: 0,
            commits: 0,
            lists: 0,
            removes: 0,
            moves_left: 0,
            fail_write: HashSet::new(),
            fail_list: None,
        })))
    }

    pub fn file(&self, rel: &str) -> Option<String> {
        let state = self.0.lock().unwrap();
        state
            .store
            .get(rel)
            .map(|(data, _)| String::from_utf8_lossy(data).into_owned())
    }

    pub fn has(&self, rel: &str) -> bool {
        self.0.lock().unwrap().store.contains_key(rel)
    }
}

impl SyncProvider for MemProvider {
    fn key(&self) -> String {
        "mem:test".to_string()
    }

    fn list(&mut self) -> Result<Vec<(String, RemoteStat)>, ProviderError> {
        let mut state = self.0.lock().unwrap();
        state.lists += 1;
        state.staged.clear();
        if let Some(message) = state.fail_list.clone() {
            return Err(ProviderError::Other(message));
        }
        Ok(state
            .store
            .iter()
            .map(|(rel, (data, mtime_ms))| {
                (
                    rel.clone(),
                    RemoteStat {
                        size: data.len() as u64,
                        mtime_ms: *mtime_ms,
                        token: sha1_hex(data),
                    },
                )
            })
            .collect())
    }

    fn read(&mut self, rel: &str) -> Result<Vec<u8>, ProviderError> {
        let mut state = self.0.lock().unwrap();
        state.reads += 1;
        match state.store.get(rel) {
            Some((data, _)) => Ok(data.clone()),
            None => Err(ProviderError::Other(format!("missing {rel}"))),
        }
    }

    fn write(&mut self, rel: &str, data: &[u8], mtime_ms: f64) -> Result<RemoteStat, ProviderError> {
        let mut state = self.0.lock().unwrap();
        state.writes += 1;
        if state.fail_write.contains(rel) {
            return Err(ProviderError::Other("boom".to_string()));
        }
        state
            .staged
            .insert(rel.to_string(), Some((data.to_vec(), mtime_ms)));
        Ok(RemoteStat {
            size: data.len() as u64,
            mtime_ms,
            token: sha1_hex(data),
        })
    }

    fn remove(&mut self, rel: &str) -> Result<(), ProviderError> {
        let mut state = self.0.lock().unwrap();
        state.removes += 1;
        state.staged.insert(rel.to_string(), None);
        Ok(())
    }

    fn commit(&mut self, _message: &str) -> Result<(), ProviderError> {
        let mut state = self.0.lock().unwrap();
        if state.moves_left > 0 {
            // Another PC pushed meanwhile: the store moves, our staged changes are lost.
            state.moves_left -= 1;
            state.store.insert(
                "curriculum.json".to_string(),
                (b"other pc".to_vec(), now_ms() as f64 + 1_000_000.0),
            );
            return Err(ProviderError::RemoteMoved);
        }
        state.commits += 1;
        for (rel, change) in std::mem::take(&mut state.staged) {
            match change {
                Some((data, mtime_ms)) => {
                    state.store.insert(rel, (data, mtime_ms));
                }
                None => {
                    state.store.shift_remove(&rel);
                }
            }
        }
        Ok(())
    }
}
