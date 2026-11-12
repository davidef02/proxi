//! Claude usage limits read from `.learner/limits.json`, with the server cache.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use crate::protocol::LearnerSettings;

pub fn limits_file(repo_root: &Path) -> PathBuf {
    repo_root.join(".learner").join("limits.json")
}

pub fn read_limits(repo_root: &Path) -> Option<Value> {
    let raw = fs::read_to_string(limits_file(repo_root)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn limits_disabled(s: &LearnerSettings) -> bool {
    s.provider == "agy" || s.provider == "opencode" || s.claude_auth == "apiKey"
}

/// Mirrors Node's in-memory limits cache: the first successful read wins until restart (no recorder in Rust yet).
#[derive(Default)]
pub struct LimitsCache {
    last: Mutex<Option<Value>>,
}

impl LimitsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, repo_root: &Path) -> Option<Value> {
        let mut last = self.last.lock().expect("limits cache poisoned");
        if let Some(cached) = last.as_ref() {
            return Some(cached.clone());
        }
        let value = read_limits(repo_root)?;
        *last = Some(value.clone());
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-limits-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_limits(repo: &Path, raw: &str) {
        let file = limits_file(repo);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, raw).unwrap();
    }

    #[test]
    fn reads_none_when_missing_or_invalid() {
        let repo = temp_dir("read");
        assert_eq!(read_limits(&repo), None);
        write_limits(&repo, "{not json");
        assert_eq!(read_limits(&repo), None);
    }

    #[test]
    fn keeps_key_order() {
        let repo = temp_dir("order");
        let raw = r#"{"fiveHour":{"utilization":0.42,"resetsAt":1700000000},"at":"2026-11-12T10:00:00.000Z"}"#;
        write_limits(&repo, raw);
        let value = read_limits(&repo).unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), raw);
    }

    #[test]
    fn limits_are_disabled_outside_claude_subscription() {
        let mut s = LearnerSettings::default();
        assert!(!limits_disabled(&s));
        s.provider = "agy".to_string();
        assert!(limits_disabled(&s));
        s.provider = "opencode".to_string();
        assert!(limits_disabled(&s));
        s.provider = "claude".to_string();
        s.claude_auth = "apiKey".to_string();
        assert!(limits_disabled(&s));
    }

    #[test]
    fn cache_keeps_the_first_successful_read() {
        let repo = temp_dir("cache");
        let cache = LimitsCache::new();
        assert_eq!(cache.get(&repo), None);

        write_limits(&repo, r#"{"at":"first"}"#);
        assert_eq!(cache.get(&repo), Some(json!({"at": "first"})));

        write_limits(&repo, r#"{"at":"second"}"#);
        assert_eq!(cache.get(&repo), Some(json!({"at": "first"})));
    }

    #[test]
    fn cache_does_not_store_a_failed_read() {
        let repo = temp_dir("cache-invalid");
        write_limits(&repo, "not json");
        let cache = LimitsCache::new();
        assert_eq!(cache.get(&repo), None);

        write_limits(&repo, r#"{"at":"ok"}"#);
        assert_eq!(cache.get(&repo), Some(json!({"at": "ok"})));
    }
}
