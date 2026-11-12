//! Learner settings: file, validation and API-key masking.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::protocol::{
    CLAUDE_AUTH_MODES, EFFORT_SETTINGS, LearnerSettings, MODELS, PROVIDERS, RoleModels,
    STT_PROVIDERS, SYNC_PROVIDERS,
};

/// The keys of `DEFAULT_SETTINGS`, in the order the Node schema validates them.
const SETTING_KEYS: &[&str] = &[
    "provider",
    "claudeAuth",
    "anthropicApiKey",
    "agyModels",
    "opencodeModels",
    "teacherModel",
    "subagentModel",
    "mapModel",
    "effort",
    "autoMap",
    "sttProvider",
    "sttApiKey",
    "dictationLanguage",
    "syncProvider",
    "githubClientId",
    "googleClientId",
    "googleClientSecret",
];

pub fn settings_file(repo_root: &Path) -> PathBuf {
    repo_root.join(".learner").join("settings.json")
}

pub fn mask_api_key(key: &str) -> String {
    let t = key.trim();
    if t.is_empty() {
        return String::new();
    }
    let units: Vec<u16> = t.encode_utf16().collect();
    if units.len() <= 10 {
        return "••••••••".to_string();
    }
    let mut out = String::from_utf16_lossy(&units[..7]);
    out.push('…');
    out.push_str(&String::from_utf16_lossy(&units[units.len() - 4..]));
    out
}

#[derive(Debug)]
pub enum SettingsError {
    Invalid(String),
    Io(std::io::Error),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::Invalid(msg) => f.write_str(msg),
            SettingsError::Io(err) => err.fmt(f),
        }
    }
}

fn len16(s: &str) -> usize {
    s.encode_utf16().count()
}

fn invalid(key: &str) -> String {
    format!("{key}: Invalid input")
}

fn enum_value(key: &str, v: &Value, allowed: &[&str]) -> Result<Value, String> {
    let s = v
        .as_str()
        .filter(|s| allowed.contains(s))
        .ok_or_else(|| invalid(key))?;
    Ok(Value::String(s.to_string()))
}

fn string_max(key: &str, v: &Value, max: usize) -> Result<Value, String> {
    let s = v
        .as_str()
        .filter(|s| len16(s) <= max)
        .ok_or_else(|| invalid(key))?;
    Ok(Value::String(s.to_string()))
}

fn role_models(key: &str, v: &Value) -> Result<Value, String> {
    let obj = v.as_object().ok_or_else(|| invalid(key))?;
    let mut out = Map::new();
    for part in ["teacher", "subagent", "map"] {
        let s = obj
            .get(part)
            .and_then(Value::as_str)
            .filter(|s| (1..=100).contains(&len16(s)))
            .ok_or_else(|| invalid(key))?;
        out.insert(part.to_string(), Value::String(s.to_string()));
    }
    Ok(Value::Object(out))
}

fn effort(v: &Value) -> Result<Value, String> {
    let obj = v.as_object().ok_or_else(|| invalid("effort"))?;
    let mut out = Map::new();
    for part in ["teacher", "subagent", "map"] {
        if let Some(value) = obj.get(part) {
            let s = value
                .as_str()
                .filter(|s| EFFORT_SETTINGS.contains(s))
                .ok_or_else(|| invalid("effort"))?;
            out.insert(part.to_string(), Value::String(s.to_string()));
        }
    }
    Ok(Value::Object(out))
}

/// Returns the cleaned value to apply; unknown keys are rejected.
fn validate(key: &str, v: &Value) -> Result<Value, String> {
    match key {
        "provider" => enum_value(key, v, PROVIDERS),
        "claudeAuth" => enum_value(key, v, CLAUDE_AUTH_MODES),
        "anthropicApiKey" | "sttApiKey" | "githubClientId" | "googleClientSecret" => {
            string_max(key, v, 200)
        }
        "agyModels" | "opencodeModels" => role_models(key, v),
        "teacherModel" | "subagentModel" | "mapModel" => enum_value(key, v, MODELS),
        "effort" => effort(v),
        "autoMap" => v
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| invalid(key)),
        "dictationLanguage" => {
            let s = v
                .as_str()
                .filter(|s| (2..=20).contains(&len16(s)))
                .ok_or_else(|| invalid(key))?;
            Ok(Value::String(s.to_string()))
        }
        "sttProvider" => enum_value(key, v, STT_PROVIDERS),
        "syncProvider" => enum_value(key, v, SYNC_PROVIDERS),
        _ => Err(format!(": Unrecognized key: \"{key}\"")),
    }
}

fn text(v: &Value) -> String {
    v.as_str().expect("validated").to_string()
}

/// Applies a value returned by `validate`.
fn apply(s: &mut LearnerSettings, key: &str, v: Value) {
    match key {
        "provider" => s.provider = text(&v),
        "claudeAuth" => s.claude_auth = text(&v),
        "anthropicApiKey" => s.anthropic_api_key = text(&v),
        "agyModels" => s.agy_models = serde_json::from_value::<RoleModels>(v).expect("validated"),
        "opencodeModels" => {
            s.opencode_models = serde_json::from_value::<RoleModels>(v).expect("validated")
        }
        "teacherModel" => s.teacher_model = text(&v),
        "subagentModel" => s.subagent_model = text(&v),
        "mapModel" => s.map_model = text(&v),
        "effort" => {
            let obj = v.as_object().expect("validated");
            if let Some(x) = obj.get("teacher").and_then(Value::as_str) {
                s.effort.teacher = x.to_string();
            }
            if let Some(x) = obj.get("subagent").and_then(Value::as_str) {
                s.effort.subagent = x.to_string();
            }
            if let Some(x) = obj.get("map").and_then(Value::as_str) {
                s.effort.map = x.to_string();
            }
        }
        "autoMap" => s.auto_map = v.as_bool().expect("validated"),
        "sttProvider" => s.stt_provider = text(&v),
        "sttApiKey" => s.stt_api_key = text(&v),
        "dictationLanguage" => s.dictation_language = text(&v),
        "syncProvider" => s.sync_provider = text(&v),
        "githubClientId" => s.github_client_id = text(&v),
        "googleClientId" => s.google_client_id = text(&v),
        "googleClientSecret" => s.google_client_secret = text(&v),
        _ => unreachable!("validate returned a known key"),
    }
}

pub fn to_file_json(s: &LearnerSettings) -> String {
    let mut b = serde_json::to_string_pretty(s)
        .expect("LearnerSettings has only strings, bools and nested structs");
    b.push('\n');
    b
}

/// Lenient: a bad or unknown key is dropped on its own, the other settings survive.
pub fn read_settings(repo_root: &Path) -> LearnerSettings {
    let mut s = LearnerSettings::default();
    let Ok(raw) = fs::read_to_string(settings_file(repo_root)) else {
        return s;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return s;
    };
    let Some(obj) = value.as_object() else {
        return s;
    };
    for key in SETTING_KEYS {
        if let Some(v) = obj.get(*key) {
            if let Ok(cleaned) = validate(key, v) {
                apply(&mut s, key, cleaned);
            }
        }
    }
    s
}

pub fn write_settings(repo_root: &Path, patch: &Value) -> Result<LearnerSettings, SettingsError> {
    let obj = patch
        .as_object()
        .ok_or_else(|| SettingsError::Invalid(": Expected object".to_string()))?;
    let mut cleaned = Vec::with_capacity(obj.len());
    for (key, value) in obj {
        cleaned.push((key.clone(), validate(key, value).map_err(SettingsError::Invalid)?));
    }
    // The UI sends the masked key back with every save; keep the stored one.
    cleaned.retain(|(key, value)| {
        !(key == "anthropicApiKey"
            && value
                .as_str()
                .is_some_and(|s| s.contains('…') || s.contains('•')))
    });

    let mut next = read_settings(repo_root);
    for (key, value) in cleaned {
        apply(&mut next, &key, value);
    }

    let file = settings_file(repo_root);
    fs::create_dir_all(file.parent().expect("settings file has a parent"))
        .map_err(SettingsError::Io)?;
    let body = to_file_json(&next);
    let mut tmp = file.clone().into_os_string();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    if fs::write(&tmp, &body)
        .and_then(|_| fs::rename(&tmp, &file))
        .is_err()
    {
        // Windows: rename over a file another process holds open can fail with EPERM/EBUSY,
        // so fall back to writing in place like Node does.
        let _ = fs::remove_file(&tmp);
        fs::write(&file, &body).map_err(SettingsError::Io)?;
    }
    Ok(next)
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
            "proxi-settings-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn masks_api_keys_like_node() {
        assert_eq!(mask_api_key(""), "");
        assert_eq!(mask_api_key("   "), "");
        assert_eq!(mask_api_key("short"), "••••••••");
        assert_eq!(mask_api_key("0123456789"), "••••••••");
        assert_eq!(mask_api_key("sk-ant-api03-abcdWXYZ"), "sk-ant-…WXYZ");
        assert_eq!(mask_api_key("  sk-ant-api03-abcdWXYZ  "), "sk-ant-…WXYZ");
    }

    #[test]
    fn read_falls_back_to_defaults() {
        let repo = temp_dir("read-missing");
        assert_eq!(read_settings(&repo), LearnerSettings::default());

        let file = settings_file(&repo);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{not json").unwrap();
        assert_eq!(read_settings(&repo), LearnerSettings::default());
        fs::write(&file, "[]").unwrap();
        assert_eq!(read_settings(&repo), LearnerSettings::default());
    }

    #[test]
    fn read_is_lenient_per_key() {
        let repo = temp_dir("read-lenient");
        let file = settings_file(&repo);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            r#"{"provider":"bogus","teacherModel":"opus","effort":{"map":"low","x":1},"agyModels":{"teacher":"a"},"extra":1}"#,
        )
        .unwrap();

        let mut want = LearnerSettings::default();
        want.teacher_model = "opus".to_string();
        want.effort.map = "low".to_string();
        assert_eq!(read_settings(&repo), want);
    }

    #[test]
    fn write_rejects_bad_patches() {
        let repo = temp_dir("write-errors");
        for patch in [
            json!({"nope": 1}),
            json!([]),
            json!({"autoMap": null}),
            json!({"agyModels": {"teacher": "a", "subagent": "b"}}),
            json!({"dictationLanguage": "i"}),
            json!({"anthropicApiKey": "x".repeat(201)}),
        ] {
            assert!(
                matches!(
                    write_settings(&repo, &patch),
                    Err(SettingsError::Invalid(_))
                ),
                "{patch}"
            );
        }
        assert!(matches!(
            write_settings(&repo, &json!({"nope": 1})),
            Err(SettingsError::Invalid(msg)) if msg.contains("Unrecognized key")
        ));
        assert!(!settings_file(&repo).exists());
    }

    #[test]
    fn write_accepts_boundary_lengths() {
        let repo = temp_dir("write-lengths");
        let s = write_settings(&repo, &json!({"anthropicApiKey": "x".repeat(200)})).unwrap();
        assert_eq!(s.anthropic_api_key.len(), 200);

        let emoji = "😀".repeat(100); // 200 UTF-16 units
        let s = write_settings(&repo, &json!({"sttApiKey": emoji})).unwrap();
        assert_eq!(s.stt_api_key, emoji);
        let too_long = "😀".repeat(101);
        assert!(write_settings(&repo, &json!({"sttApiKey": too_long})).is_err());
    }

    #[test]
    fn write_ignores_the_masked_key() {
        let repo = temp_dir("write-masked");
        let first =
            write_settings(&repo, &json!({"anthropicApiKey": "sk-ant-real-key-1234"})).unwrap();
        assert_eq!(first.anthropic_api_key, "sk-ant-real-key-1234");

        let after = write_settings(&repo, &json!({"anthropicApiKey": "sk-ant-…abcd"})).unwrap();
        assert_eq!(after.anthropic_api_key, "sk-ant-real-key-1234");

        let after = write_settings(
            &repo,
            &json!({"anthropicApiKey": "••••••••", "teacherModel": "opus"}),
        )
        .unwrap();
        assert_eq!(after.anthropic_api_key, "sk-ant-real-key-1234");
        assert_eq!(after.teacher_model, "opus");
    }

    #[test]
    fn write_merges_effort_and_writes_the_file() {
        let repo = temp_dir("write-effort");
        let s = write_settings(&repo, &json!({"effort": {"teacher": "high"}})).unwrap();
        assert_eq!(s.effort.teacher, "high");
        assert_eq!(s.effort.subagent, "default");
        assert_eq!(s.effort.map, "default");

        let s = write_settings(&repo, &json!({"teacherModel": "haiku"})).unwrap();
        let raw = fs::read_to_string(settings_file(&repo)).unwrap();
        assert_eq!(raw, to_file_json(&s));
        assert!(raw.starts_with("{\n  \"provider\": \"claude\","));
        assert!(raw.ends_with("}\n"));
    }

    #[test]
    fn write_creates_the_learner_folder() {
        let repo = temp_dir("write-mkdir");
        assert!(!repo.join(".learner").exists());
        write_settings(&repo, &json!({})).unwrap();
        assert!(settings_file(&repo).exists());
    }

    #[test]
    fn validate_strips_unknown_keys() {
        let models = validate(
            "agyModels",
            &json!({"teacher": "a", "subagent": "b", "map": "c", "x": 1}),
        )
        .unwrap();
        assert_eq!(models, json!({"teacher": "a", "subagent": "b", "map": "c"}));
        assert_eq!(
            serde_json::to_string(&models).unwrap(),
            r#"{"teacher":"a","subagent":"b","map":"c"}"#
        );

        let effort = validate("effort", &json!({"map": "low", "x": 1})).unwrap();
        assert_eq!(effort, json!({"map": "low"}));
    }

    #[test]
    fn validate_errors() {
        assert_eq!(
            validate("effort", &json!({"teacher": "bogus"})).unwrap_err(),
            "effort: Invalid input"
        );
        let err = validate("nope", &json!(1)).unwrap_err();
        assert!(err.contains("Unrecognized key"), "{err}");
    }

    #[test]
    fn file_json_is_pretty_with_trailing_newline() {
        let json = to_file_json(&LearnerSettings::default());
        assert!(json.starts_with("{\n  \"provider\": \"claude\","));
        assert!(json.ends_with("}\n"));
    }
}
