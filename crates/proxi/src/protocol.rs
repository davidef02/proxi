//! Wire protocol shared with the web UI: settings, limits, updates, graph, sessions and events.

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 4317;

pub const PROVIDERS: &[&str] = &["claude", "agy", "opencode"];
pub const CLAUDE_AUTH_MODES: &[&str] = &["subscription", "apiKey"];
pub const MODELS: &[&str] = &["opus", "sonnet", "haiku"];
pub const EFFORT_SETTINGS: &[&str] = &["default", "low", "medium", "high", "xhigh", "max"];
pub const STT_PROVIDERS: &[&str] = &["groq", "deepgram"];
pub const SYNC_PROVIDERS: &[&str] = &["local", "github", "gdrive"];
/// The exact Phase 1c question (skills/teach), asked as the session's first `ask` card.
pub const ANKI_OPT_IN_QUESTION: &str =
    "Vuoi che vengano create e sincronizzate le flashcard Anki per questa sessione? (Sì / No)";

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

/// The Claude CLI probe's answer; `protocol.ts:566-572` order, absent fields dropped like undefined.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeStatusResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub min_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bin_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    Graph { revision: u64 },
    Sessions,
    Sync { status: SyncStatus },
}

// ───────────────────────────── sessions ─────────────────────────────

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Starting,
    Running,
    Waiting,
    Idle,
    Error,
    Closed,
}

impl SessionStatus {
    /// The teacher is busy: a message would be refused with a 409.
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Waiting)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: JsNumber,
    pub output_tokens: JsNumber,
    pub cache_read_tokens: JsNumber,
    pub cache_creation_tokens: JsNumber,
    pub turns: JsNumber,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub id: String,
    pub config: serde_json::Map<String, serde_json::Value>,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson_path: Option<String>,
    pub usage: Usage,
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    // Node writes the key as null until the first question, so it must never be skipped.
    #[serde(default)]
    pub pending_request_id: Option<String>,
    #[serde(default)]
    pub plan: serde_json::Value,
    #[serde(default)]
    pub exercises: Vec<serde_json::Value>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// A `SessionState` minus the heavy fields, key order preserved (`sessions.ts` list's rest spread).
pub fn session_summary(state: &SessionState) -> serde_json::Value {
    let serde_json::Value::Object(state) =
        serde_json::to_value(state).expect("SessionState serialises to an object")
    else {
        unreachable!("a struct serialises to a JSON object")
    };
    let mut summary = serde_json::Map::new();
    for (key, value) in state {
        if matches!(
            key.as_str(),
            "items" | "pendingRequestId" | "plan" | "exercises" | "lastError"
        ) {
            continue;
        }
        summary.insert(key, value);
    }
    serde_json::Value::Object(summary)
}

// The snapshot carries the whole state by design, so the variant is large.
#[allow(clippy::large_enum_variant)]
#[derive(Serialize, Clone, Debug)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum ServerEvent {
    Snapshot {
        state: SessionState,
    },
    Item {
        item: serde_json::Value,
    },
    Delta {
        item_id: String,
        text: String,
    },
    Status {
        status: SessionStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        retryable: bool,
    },
    Pending {
        request_id: Option<String>,
    },
    Usage {
        usage: Usage,
    },
}

/// A number as JS prints it: `JSON.stringify(4)` is `4`, serde prints `4.0` for an f64.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct JsNumber(pub f64);

impl<'de> Deserialize<'de> for JsNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        f64::deserialize(deserializer).map(Self)
    }
}

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

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PendingDeletions {
    pub count: u64,
    pub total: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GithubPending {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at: String,
}

/// The loose `...last` spread of Node's status: state first, the counters only when a pass set them.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncLast {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pushed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pulled: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_deletions: Option<PendingDeletions>,
}

impl Default for SyncLast {
    fn default() -> Self {
        Self {
            state: "idle".to_string(),
            at: None,
            pushed: None,
            pulled: None,
            deleted: None,
            conflicts: None,
            error: None,
            pending_deletions: None,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GithubStatus {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<GithubPending>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GdriveStatus {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    #[serde(flatten)]
    pub last: SyncLast,
    pub provider: String,
    pub data_root: String,
    pub data_root_from_env: bool,
    pub needs_data: bool,
    pub encrypted: bool,
    pub github: GithubStatus,
    pub gdrive: GdriveStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_required: Option<bool>,
    pub conflict_count: u64,
    pub trash_count: u64,
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
    fn claude_status_serialises_in_node_order() {
        assert_eq!(
            serde_json::to_string(&ClaudeStatusResponse {
                status: "meets-minimum".to_string(),
                version: Some("2.1.300".to_string()),
                min_version: "2.1.259".to_string(),
                bin_path: Some("C:\\tools\\claude.exe".to_string()),
                error: None,
            })
            .unwrap(),
            r#"{"status":"meets-minimum","version":"2.1.300","minVersion":"2.1.259","binPath":"C:\\tools\\claude.exe"}"#
        );
        assert_eq!(
            serde_json::to_string(&ClaudeStatusResponse {
                status: "not-found".to_string(),
                version: None,
                min_version: "2.1.259".to_string(),
                bin_path: Some("C:\\tools\\claude.exe".to_string()),
                error: Some("spawn C:\\tools\\claude.exe ENOENT".to_string()),
            })
            .unwrap(),
            r#"{"status":"not-found","minVersion":"2.1.259","binPath":"C:\\tools\\claude.exe","error":"spawn C:\\tools\\claude.exe ENOENT"}"#
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
        assert!(json.ends_with(",\"repoRoot\":\"R\",\"dataRoot\":\"D\"}}"), "{json}");

        assert_eq!(
            serde_json::to_string(&GlobalEvent::Graph { revision: 1 }).unwrap(),
            r#"{"type":"graph","revision":1}"#
        );
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

    #[test]
    fn sync_status_json_matches_node() {
        let status = SyncStatus {
            last: SyncLast::default(),
            provider: "local".to_string(),
            data_root: "D".to_string(),
            data_root_from_env: false,
            needs_data: true,
            encrypted: false,
            github: GithubStatus::default(),
            gdrive: GdriveStatus::default(),
            restart_required: None,
            conflict_count: 0,
            trash_count: 0,
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"state":"idle","provider":"local","dataRoot":"D","dataRootFromEnv":false,"needsData":true,"encrypted":false,"github":{"connected":false},"gdrive":{"connected":false,"pending":false},"conflictCount":0,"trashCount":0}"#
        );
    }

    #[test]
    fn sync_status_last_fields_come_first() {
        let status = SyncStatus {
            last: SyncLast {
                state: "error".to_string(),
                at: Some("A".to_string()),
                error: Some("E".to_string()),
                ..SyncLast::default()
            },
            provider: "github".to_string(),
            data_root: "D".to_string(),
            ..SyncStatus::default()
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(
            json.starts_with(r#"{"state":"error","at":"A","error":"E","provider":"github","#),
            "{json}"
        );
    }

    #[test]
    fn sync_event_serialises_type_first() {
        let event = GlobalEvent::Sync {
            status: SyncStatus {
                provider: "local".to_string(),
                ..SyncStatus::default()
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.starts_with(r#"{"type":"sync","status":{"state":"#),
            "{json}"
        );
    }

    fn fresh_state() -> SessionState {
        SessionState {
            id: "s".to_string(),
            config: serde_json::Map::new(),
            title: "T".to_string(),
            created_at: "C".to_string(),
            updated_at: "U".to_string(),
            status: SessionStatus::Starting,
            lesson_path: None,
            usage: Usage::default(),
            items: Vec::new(),
            pending_request_id: None,
            plan: serde_json::Value::Null,
            exercises: Vec::new(),
            retryable: false,
            last_error: None,
        }
    }

    #[test]
    fn session_state_json_matches_node_order() {
        assert_eq!(
            serde_json::to_string(&fresh_state()).unwrap(),
            r#"{"id":"s","config":{},"title":"T","createdAt":"C","updatedAt":"U","status":"starting","usage":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheCreationTokens":0,"turns":0},"items":[],"pendingRequestId":null,"plan":null,"exercises":[],"retryable":false}"#
        );
    }

    #[test]
    fn update_status_default_is_idle() {
        assert_eq!(
            serde_json::to_string(&UpdateStatus::default()).unwrap(),
            r#"{"state":"idle","behind":0}"#
        );
    }

    #[test]
    fn lesson_path_sits_before_usage_and_last_error_last() {
        let state = SessionState {
            lesson_path: Some("courses/demo/lessons/x.html".to_string()),
            status: SessionStatus::Error,
            retryable: true,
            last_error: Some("E".to_string()),
            ..fresh_state()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(
            json.contains(
                r#""status":"error","lessonPath":"courses/demo/lessons/x.html","usage":{"#
            ),
            "{json}"
        );
        assert!(
            json.ends_with(r#""exercises":[],"retryable":true,"lastError":"E"}"#),
            "{json}"
        );
    }

    #[test]
    fn deserialises_a_node_file_without_newer_fields() {
        let state: SessionState = serde_json::from_value(json!({
            "id": "s",
            "config": {"kind": "teach", "course": "demo"},
            "title": "T",
            "createdAt": "C",
            "updatedAt": "U",
            "status": "idle",
            "usage": {"inputTokens": 1.5, "outputTokens": 0, "cacheReadTokens": 0, "cacheCreationTokens": 0, "turns": 1},
            "items": [],
            "pendingRequestId": null,
            "plan": null
        }))
        .unwrap();
        assert_eq!(state.status, SessionStatus::Idle);
        assert_eq!(state.usage.input_tokens, JsNumber(1.5));
        assert_eq!(state.retryable, false);
        assert_eq!(state.last_error, None);
        assert!(state.exercises.is_empty());
    }

    #[test]
    fn session_summary_drops_heavy_fields_and_keeps_order() {
        let state = SessionState {
            status: SessionStatus::Idle,
            items: vec![json!({"id": "i"})],
            plan: json!({"topic": "t"}),
            exercises: vec![json!({"name": "e"})],
            pending_request_id: Some("r".to_string()),
            last_error: Some("E".to_string()),
            ..fresh_state()
        };
        assert_eq!(
            serde_json::to_string(&session_summary(&state)).unwrap(),
            r#"{"id":"s","config":{},"title":"T","createdAt":"C","updatedAt":"U","status":"idle","usage":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheCreationTokens":0,"turns":0},"retryable":false}"#
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
    fn session_summary_keeps_lesson_path_between_status_and_usage() {
        let state = SessionState {
            status: SessionStatus::Idle,
            lesson_path: Some("p".to_string()),
            ..fresh_state()
        };
        assert_eq!(
            serde_json::to_string(&session_summary(&state)).unwrap(),
            r#"{"id":"s","config":{},"title":"T","createdAt":"C","updatedAt":"U","status":"idle","lessonPath":"p","usage":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheCreationTokens":0,"turns":0},"retryable":false}"#
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
    fn server_events_serialise_with_node_shapes() {
        assert_eq!(
            serde_json::to_string(&ServerEvent::Status {
                status: SessionStatus::Idle,
                error: None,
                retryable: false,
            })
            .unwrap(),
            r#"{"type":"status","status":"idle","retryable":false}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerEvent::Status {
                status: SessionStatus::Error,
                error: Some("E".to_string()),
                retryable: true,
            })
            .unwrap(),
            r#"{"type":"status","status":"error","error":"E","retryable":true}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerEvent::Pending { request_id: None }).unwrap(),
            r#"{"type":"pending","requestId":null}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerEvent::Delta {
                item_id: "t:m:1".to_string(),
                text: "x".to_string(),
            })
            .unwrap(),
            r#"{"type":"delta","itemId":"t:m:1","text":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerEvent::Item {
                item: json!({"id": "i"})
            })
            .unwrap(),
            r#"{"type":"item","item":{"id":"i"}}"#
        );
    }

    #[test]
    fn snapshot_event_wraps_the_state() {
        let event = ServerEvent::Snapshot {
            state: fresh_state(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.starts_with(r#"{"type":"snapshot","state":{"id":"s","config":{},"#),
            "{json}"
        );
    }

    #[test]
    fn sessions_global_event_has_no_payload() {
        assert_eq!(
            serde_json::to_string(&GlobalEvent::Sessions).unwrap(),
            r#"{"type":"sessions"}"#
        );
    }

    #[test]
    fn usage_numbers_print_like_js() {
        let usage = Usage {
            input_tokens: JsNumber(1.5),
            turns: JsNumber(2.0),
            ..Usage::default()
        };
        assert_eq!(
            serde_json::to_string(&usage).unwrap(),
            r#"{"inputTokens":1.5,"outputTokens":0,"cacheReadTokens":0,"cacheCreationTokens":0,"turns":2}"#
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
    fn busy_statuses_are_starting_running_waiting() {
        for status in [
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::Waiting,
        ] {
            assert!(status.is_busy());
        }
        for status in [
            SessionStatus::Idle,
            SessionStatus::Error,
            SessionStatus::Closed,
        ] {
            assert!(!status.is_busy());
        }
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
