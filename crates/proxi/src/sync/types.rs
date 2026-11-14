//! Sync types shared by the engine and the providers (port of `app/server/sync/types.ts`).

use std::fmt;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize, Serializer};

/// A local file right after the last sync: size and mtime.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalStat {
    pub size: u64,
    #[serde(serialize_with = "js_f64")]
    pub mtime_ms: f64,
}

/// Remote view of one file. `token` changes iff the content changes (GitHub blob sha, Drive md5Checksum).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStat {
    pub size: u64,
    #[serde(serialize_with = "js_f64")]
    pub mtime_ms: f64,
    pub token: String,
}

/// Per file: both sides right after the last sync that touched it. An entry with a side missing = a deletion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub l: LocalStat,
    pub r: RemoteStat,
}

/// `dataRoot` binds the manifest to the local folder it describes: another folder starts a first-meeting pass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub v: u32,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_root: Option<String>,
    pub files: IndexMap<String, ManifestEntry>,
}

/// `JSON.stringify` writes an integral double as an integer: Node's manifests have `1700000000000`, not
/// `1700000000000.0`.
pub fn js_f64<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.fract() == 0.0 && v.abs() < 9007199254740992.0 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

/// A provider failure: `RemoteMoved` means the remote head changed under a commit, everything else is a message.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderError {
    RemoteMoved,
    Other(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::RemoteMoved => f.write_str("ref moved"),
            ProviderError::Other(message) => f.write_str(message),
        }
    }
}

impl From<String> for ProviderError {
    fn from(message: String) -> Self {
        ProviderError::Other(message)
    }
}

/// One remote store (GitHub, Drive): synchronous because the engine runs on a blocking thread.
pub trait SyncProvider: Send {
    /// Manifest key: provider + target ("github:<owner>/<repo>", "gdrive:<folderId>").
    fn key(&self) -> String;
    /// Every remote file (forward-slash rel paths). Called first in each pass; resets any staged change.
    fn list(&mut self) -> Result<Vec<(String, RemoteStat)>, ProviderError>;
    fn read(&mut self, rel: &str) -> Result<Vec<u8>, ProviderError>;
    /// Uploads or stages one file; returns its new remote stat.
    fn write(&mut self, rel: &str, data: &[u8], mtime_ms: f64) -> Result<RemoteStat, ProviderError>;
    /// Deletes or stages the deletion of one remote file.
    fn remove(&mut self, rel: &str) -> Result<(), ProviderError>;
    /// Flushes staged changes (GitHub: one commit; Drive: no-op). Fails with `RemoteMoved` if the remote moved.
    fn commit(&mut self, message: &str) -> Result<(), ProviderError>;
    /// Optional cheap equality (hash of `data` vs `r.token`); the engine falls back to read() + byte compare.
    fn content_equals(&self, _data: &[u8], _r: &RemoteStat) -> Option<bool> {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncResult {
    pub pushed: u64,
    pub pulled: u64,
    /// Remote removals + local files moved to the trash.
    pub deleted: u64,
    pub conflicts: u64,
    pub errors: Vec<String>,
    /// Set when the pass stopped before changing anything: deletions > 30% of the manifest entries.
    pub pending_deletions: Option<crate::protocol::PendingDeletions>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(mtime_ms: f64) -> ManifestEntry {
        ManifestEntry {
            l: LocalStat {
                size: 1,
                mtime_ms,
            },
            r: RemoteStat {
                size: 1,
                mtime_ms,
                token: "t".to_string(),
            },
        }
    }

    #[test]
    fn js_f64_writes_integral_values_like_js() {
        let mut manifest = Manifest {
            v: 2,
            key: "github:tester/proxi-data".to_string(),
            data_root: Some("D".to_string()),
            files: IndexMap::new(),
        };
        manifest.files.insert("a.txt".to_string(), stat(1_700_000_000_000.0));
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains(r#""mtimeMs":1700000000000,"#), "{json}");
        assert!(!json.contains(".0"), "{json}");

        manifest
            .files
            .insert("b.txt".to_string(), stat(1_700_000_000_123.25));
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains(r#""mtimeMs":1700000000123.25,"#), "{json}");

        // A manifest Node wrote (integral mtimes) parses and serialises back unchanged, order kept.
        let raw = concat!(
            r#"{"v":2,"key":"github:tester/proxi-data","dataRoot":"D","files":{"#,
            r#""courses/a.md":{"l":{"size":3,"mtimeMs":1700000000000},"#,
            r#""r":{"size":3,"mtimeMs":1700000000000,"token":"abc"}},"#,
            r#""graph/nodes.jsonl":{"l":{"size":5,"mtimeMs":1700000000000},"#,
            r#""r":{"size":5,"mtimeMs":1700000000000,"token":"def"}}}}"#
        );
        let manifest: Manifest = serde_json::from_str(raw).unwrap();
        assert_eq!(
            manifest
                .files
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["courses/a.md", "graph/nodes.jsonl"]
        );
        assert_eq!(serde_json::to_string(&manifest).unwrap(), raw);
    }
}
