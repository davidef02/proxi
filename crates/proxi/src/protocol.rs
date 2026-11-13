//! Wire protocol shared with the web UI: settings, limits, updates and events.


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

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available,
    Updating,
    Relaunching,
    Error,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCommitInfo {
    pub hash: String,
    pub subject: String,
    pub relative_time: String,
}

/// Field order is Node's runtime order: `error` is spread onto the previous status, so it comes last.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub state: UpdateState,
    pub behind: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits: Option<Vec<UpdateCommitInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for UpdateStatus {
    fn default() -> Self {
        Self {
            state: UpdateState::Idle,
            behind: 0,
            commits: None,
            checked_at: None,
            error: None,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusResponse {
    #[serde(flatten)]
    pub status: UpdateStatus,
    pub active_sessions: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct UpdateApplyResponse {
    pub ok: bool,
    pub relaunching: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum GlobalEvent {
    Limits { limits: serde_json::Value },
    Settings { settings: SettingsResponse },
    Update { status: UpdateStatus },
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
    fn update_status_default_is_idle() {
        assert_eq!(
            serde_json::to_string(&UpdateStatus::default()).unwrap(),
            r#"{"state":"idle","behind":0}"#
        );
    }

    #[test]
    fn update_status_keeps_node_key_order() {
        let status = UpdateStatus {
            state: UpdateState::Error,
            behind: 2,
            commits: Some(vec![UpdateCommitInfo {
                hash: "h".to_string(),
                subject: "s".to_string(),
                relative_time: "r".to_string(),
            }]),
            checked_at: Some("t".to_string()),
            error: Some("e".to_string()),
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"state":"error","behind":2,"commits":[{"hash":"h","subject":"s","relativeTime":"r"}],"checkedAt":"t","error":"e"}"#
        );
    }

    #[test]
    fn update_state_names_match_node() {
        let names: Vec<String> = [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Available,
            UpdateState::Updating,
            UpdateState::Relaunching,
            UpdateState::Error,
        ]
        .iter()
        .map(|state| serde_json::to_string(state).unwrap())
        .collect();
        assert_eq!(
            names,
            [
                r#""idle""#,
                r#""checking""#,
                r#""up_to_date""#,
                r#""available""#,
                r#""updating""#,
                r#""relaunching""#,
                r#""error""#
            ]
        );
    }

    #[test]
    fn update_status_response_puts_active_sessions_last() {
        let response = UpdateStatusResponse {
            status: UpdateStatus {
                state: UpdateState::UpToDate,
                behind: 0,
                commits: None,
                checked_at: Some("t".to_string()),
                error: None,
            },
            active_sessions: 0,
        };
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"state":"up_to_date","behind":0,"checkedAt":"t","activeSessions":0}"#
        );
    }

    #[test]
    fn update_event_serialises_type_first() {
        let event = GlobalEvent::Update {
            status: UpdateStatus::default(),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"update","status":{"state":"idle","behind":0}}"#
        );
    }
}
