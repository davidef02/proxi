//! Claude usage limits read from `.learner/limits.json`, with the server cache and recorder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use crate::events::EventBus;
use crate::protocol::{GlobalEvent, LearnerSettings};

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

/// Mirrors Node's in-memory limits cache: the first successful read wins until a report replaces it.
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

    pub fn set(&self, value: Value) {
        *self.last.lock().expect("limits cache poisoned") = Some(value);
    }
}

/// Merge one `rate_limit_event` report into the cache and `.learner/limits.json`, then tell the tabs.
pub fn record_limits(
    repo_root: &Path,
    cache: &LimitsCache,
    events: &EventBus,
    limits: &Value,
) -> Value {
    let next = match cache.get(repo_root) {
        Some(Value::Object(mut base)) => {
            // A report without unifiedWindows covers one window only: keep the other one's last value.
            if let Some(patch) = limits.as_object() {
                for (key, value) in patch {
                    base.insert(key.clone(), value.clone());
                }
            }
            Value::Object(base)
        }
        _ => limits.clone(),
    };
    cache.set(next.clone());
    // Best effort: the meter still has the in-memory value when the folder is not writable.
    if fs::create_dir_all(repo_root.join(".learner")).is_ok() {
        let _ = fs::write(limits_file(repo_root), next.to_string());
    }
    events.emit(&GlobalEvent::Limits {
        limits: next.clone(),
    });
    next
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

    #[test]
    fn set_replaces_the_cached_value() {
        let repo = temp_dir("set");
        let cache = LimitsCache::new();
        cache.set(json!({"at": "new"}));
        assert_eq!(cache.get(&repo), Some(json!({"at": "new"})));
    }

    #[test]
    fn record_without_a_base_returns_and_writes_the_report() {
        let repo = temp_dir("record-fresh");
        let cache = LimitsCache::new();
        let limits = json!({"fiveHour": {"utilization": 0.5}, "at": "a"});

        let next = record_limits(&repo, &cache, &EventBus::new(), &limits);

        assert_eq!(next, limits);
        assert_eq!(read_limits(&repo), Some(limits));
    }

    #[test]
    fn record_merges_reports_keeping_the_other_window_in_place() {
        let repo = temp_dir("record-merge");
        write_limits(
            &repo,
            r#"{"fiveHour":{"utilization":0.1},"sevenDay":{"utilization":0.2},"at":"a"}"#,
        );
        let cache = LimitsCache::new();

        let next = record_limits(
            &repo,
            &cache,
            &EventBus::new(),
            &json!({"fiveHour": {"utilization": 0.9}, "at": "b"}),
        );

        assert_eq!(
            serde_json::to_string(&next).unwrap(),
            r#"{"fiveHour":{"utilization":0.9},"sevenDay":{"utilization":0.2},"at":"b"}"#
        );
        assert_eq!(cache.get(&repo), Some(next));
    }

    #[test]
    fn record_emits_the_limits_event() {
        let repo = temp_dir("record-bus");
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        record_limits(&repo, &LimitsCache::new(), &bus, &json!({"at": "a"}));

        assert_eq!(
            rx.try_recv().unwrap(),
            r#"{"type":"limits","limits":{"at":"a"}}"#
        );
    }

    #[test]
    fn record_survives_an_unwritable_data_dir() {
        let repo = temp_dir("record-unwritable");
        fs::write(repo.join(".learner"), "not a directory").unwrap();
        let cache = LimitsCache::new();
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        let next = record_limits(&repo, &cache, &bus, &json!({"at": "a"}));

        assert_eq!(next, json!({"at": "a"}));
        assert_eq!(cache.get(&repo), Some(json!({"at": "a"})));
        assert!(rx.try_recv().is_ok());
    }
}
