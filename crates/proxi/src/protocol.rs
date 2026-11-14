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
        assert!(
            json.ends_with(r#","repoRoot":"R","dataRoot":"D"}"#),
            "{json}"
        );
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
        assert!(
            json.ends_with(",\"repoRoot\":\"R\",\"dataRoot\":\"D\"}}"),
            "{json}"
        );
    }
}
