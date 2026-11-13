//! Wire protocol shared with the web UI: settings, limits and events.

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 4317;

pub const PROVIDERS: &[&str] = &["claude", "agy", "opencode"];
pub const CLAUDE_AUTH_MODES: &[&str] = &["subscription", "apiKey"];
pub const MODELS: &[&str] = &["opus", "sonnet", "haiku"];
pub const EFFORT_SETTINGS: &[&str] = &["default", "low", "medium", "high", "xhigh", "max"];
pub const STT_PROVIDERS: &[&str] = &["groq", "deepgram"];
pub const SYNC_PROVIDERS: &[&str] = &["local", "github", "gdrive"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoleModels {
    pub teacher: String,
    pub subagent: String,
    pub map: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoleEffort {
    pub teacher: String,
    pub subagent: String,
    pub map: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LearnerSettings {
    pub provider: String,
    pub claude_auth: String,
    pub anthropic_api_key: String,
    pub agy_models: RoleModels,
    pub opencode_models: RoleModels,
    pub teacher_model: String,
    pub subagent_model: String,
    pub map_model: String,
    pub effort: RoleEffort,
    pub auto_map: bool,
    pub stt_provider: String,
    pub stt_api_key: String,
    pub dictation_language: String,
    pub sync_provider: String,
    pub github_client_id: String,
    pub google_client_id: String,
    pub google_client_secret: String,
}

impl Default for LearnerSettings {
    fn default() -> Self {
        Self {
            provider: "claude".to_string(),
            claude_auth: "subscription".to_string(),
            anthropic_api_key: String::new(),
            agy_models: RoleModels {
                teacher: "gemini-3.8-flash-high".to_string(),
                subagent: "gemini-3.8-flash-medium".to_string(),
                map: "gemini-3.8-flash-medium".to_string(),
            },
            opencode_models: RoleModels {
                teacher: "omniroute/auto/best-coding".to_string(),
                subagent: "omniroute/auto/fast".to_string(),
                map: "omniroute/auto/fast".to_string(),
            },
            teacher_model: "sonnet".to_string(),
            subagent_model: "sonnet".to_string(),
            map_model: "sonnet".to_string(),
            effort: RoleEffort {
                teacher: "default".to_string(),
                subagent: "default".to_string(),
                map: "default".to_string(),
            },
            auto_map: true,
            stt_provider: "groq".to_string(),
            stt_api_key: String::new(),
            dictation_language: "it".to_string(),
            sync_provider: "local".to_string(),
            github_client_id: String::new(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsResponse {
    #[serde(flatten)]
    pub settings: LearnerSettings,
    pub repo_root: String,
    pub data_root: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum GlobalEvent {
    Limits { limits: serde_json::Value },
    Settings { settings: SettingsResponse },
}

/// A number as JS prints it: `JSON.stringify(4)` is `4`, serde prints `4.0` for an f64.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct JsNumber(pub f64);

impl Serialize for JsNumber {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let v = self.0;
        if !v.is_finite() {
            // JSON.stringify(NaN) and (±)Infinity are `null`.
            return serializer.serialize_unit();
        }
        // i64 cannot hold integral doubles beyond 2^53 exactly; those keep the f64 path.
        if v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0 {
            return serializer.serialize_i64(v as i64);
        }
        serializer.serialize_f64(v)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AnkiCourseStats {
    pub due: JsNumber,
    pub learning: JsNumber,
    pub new_cards: JsNumber,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AnkiStatusResponse {
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<JsNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_cards: Option<JsNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learning: Option<JsNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<JsNumber>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deck: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deck_exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_course: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AnkiImportResult {
    pub online: bool,
    pub imported: u64,
    pub skipped: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AnkiImportResult {
    pub fn offline(error: impl Into<String>) -> Self {
        Self {
            online: false,
            imported: 0,
            skipped: 0,
            error: Some(error.into()),
        }
    }
}

/// One `progress.jsonl` line written by the Anki import: key order is Node's insertion order.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AnkiReviewRecord {
    pub at: String,
    pub session_id: String,
    pub course: String,
    pub lesson_slug: String,
    pub node_refs: Vec<String>,
    pub kind: String,
    pub source: String,
    pub review_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_id: Option<serde_json::Value>,
    pub ease: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lapses: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<serde_json::Value>,
    pub correct: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn default_settings_json_matches_node() {
        assert_eq!(
            serde_json::to_string(&LearnerSettings::default()).unwrap(),
            r#"{"provider":"claude","claudeAuth":"subscription","anthropicApiKey":"","agyModels":{"teacher":"gemini-3.8-flash-high","subagent":"gemini-3.8-flash-medium","map":"gemini-3.8-flash-medium"},"opencodeModels":{"teacher":"omniroute/auto/best-coding","subagent":"omniroute/auto/fast","map":"omniroute/auto/fast"},"teacherModel":"sonnet","subagentModel":"sonnet","mapModel":"sonnet","effort":{"teacher":"default","subagent":"default","map":"default"},"autoMap":true,"sttProvider":"groq","sttApiKey":"","dictationLanguage":"it","syncProvider":"local","githubClientId":"","googleClientId":"","googleClientSecret":""}"#
        );
    }

    #[test]
    fn settings_response_puts_roots_last() {
        let response = SettingsResponse {
            settings: LearnerSettings::default(),
            repo_root: "R".to_string(),
            data_root: "D".to_string(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.ends_with(r#","repoRoot":"R","dataRoot":"D"}"#), "{json}");
    }

    #[test]
    fn global_events_serialise_type_first() {
        let limits = GlobalEvent::Limits {
            limits: json!({"at": "x"}),
        };
        assert_eq!(
            serde_json::to_string(&limits).unwrap(),
            r#"{"type":"limits","limits":{"at":"x"}}"#
        );

        let settings = GlobalEvent::Settings {
            settings: SettingsResponse {
                settings: LearnerSettings::default(),
                repo_root: "R".to_string(),
                data_root: "D".to_string(),
            },
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            json.starts_with("{\"type\":\"settings\",\"settings\":{\"provider\":\"claude\","),
            "{json}"
        );
        assert!(json.ends_with(",\"repoRoot\":\"R\",\"dataRoot\":\"D\"}}"), "{json}");
    }

    #[test]
    fn js_number_prints_integers_like_json_stringify() {
        assert_eq!(serde_json::to_string(&JsNumber(4.0)).unwrap(), "4");
        assert_eq!(serde_json::to_string(&JsNumber(-0.0)).unwrap(), "0");
        assert_eq!(serde_json::to_string(&JsNumber(2.5)).unwrap(), "2.5");
        assert_eq!(serde_json::to_string(&JsNumber(f64::NAN)).unwrap(), "null");
    }

    #[test]
    fn anki_status_shapes_keep_node_key_order() {
        assert_eq!(
            serde_json::to_string(&AnkiStatusResponse {
                online: true,
                due: Some(JsNumber(1.0)),
                new_cards: Some(JsNumber(2.0)),
                learning: Some(JsNumber(3.0)),
                total: Some(JsNumber(4.0)),
                deck: Some("Proxi::D".to_string()),
                deck_exists: Some(true),
                ..Default::default()
            })
            .unwrap(),
            r#"{"online":true,"due":1,"newCards":2,"learning":3,"total":4,"deck":"Proxi::D","deckExists":true}"#
        );

        let mut by_course = serde_json::Map::new();
        by_course.insert(
            "d".to_string(),
            serde_json::to_value(AnkiCourseStats {
                due: JsNumber(0.0),
                learning: JsNumber(0.0),
                new_cards: JsNumber(0.0),
            })
            .unwrap(),
        );
        assert_eq!(
            serde_json::to_string(&AnkiStatusResponse {
                online: true,
                due: Some(JsNumber(1.0)),
                new_cards: Some(JsNumber(2.0)),
                learning: Some(JsNumber(3.0)),
                total: Some(JsNumber(4.0)),
                by_course: Some(by_course),
                ..Default::default()
            })
            .unwrap(),
            r#"{"online":true,"due":1,"newCards":2,"learning":3,"total":4,"byCourse":{"d":{"due":0,"learning":0,"newCards":0}}}"#
        );

        assert_eq!(
            serde_json::to_string(&AnkiStatusResponse {
                online: false,
                error: Some("x".to_string()),
                ..Default::default()
            })
            .unwrap(),
            r#"{"online":false,"error":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&AnkiStatusResponse {
                online: false,
                error: Some("x".to_string()),
                deck: Some("Proxi::D".to_string()),
                ..Default::default()
            })
            .unwrap(),
            r#"{"online":false,"error":"x","deck":"Proxi::D"}"#
        );
    }

    #[test]
    fn anki_import_result_keeps_error_last() {
        assert_eq!(
            serde_json::to_string(&AnkiImportResult {
                online: true,
                imported: 1,
                skipped: 0,
                error: None,
            })
            .unwrap(),
            r#"{"online":true,"imported":1,"skipped":0}"#
        );
        assert_eq!(
            serde_json::to_string(&AnkiImportResult::offline("e")).unwrap(),
            r#"{"online":false,"imported":0,"skipped":0,"error":"e"}"#
        );
    }

    #[test]
    fn anki_review_record_matches_the_progress_line() {
        let record = AnkiReviewRecord {
            at: "2023-11-14T22:13:21.000Z".to_string(),
            session_id: "anki".to_string(),
            course: "demo".to_string(),
            lesson_slug: "t".to_string(),
            node_refs: vec!["demo/t#alpha".to_string()],
            kind: "review".to_string(),
            source: "anki".to_string(),
            review_id: 1_700_000_001_000,
            card_id: Some(json!(1001)),
            ease: json!(3),
            lapses: Some(json!(0)),
            interval: Some(json!(10)),
            correct: true,
        };
        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            r#"{"at":"2023-11-14T22:13:21.000Z","sessionId":"anki","course":"demo","lessonSlug":"t","nodeRefs":["demo/t#alpha"],"kind":"review","source":"anki","reviewId":1700000001000,"cardId":1001,"ease":3,"lapses":0,"interval":10,"correct":true}"#
        );

        let without_lapses = AnkiReviewRecord {
            lapses: None,
            ..record
        };
        assert!(
            !serde_json::to_string(&without_lapses).unwrap().contains("lapses"),
            "None must be dropped like undefined"
        );
    }
}
