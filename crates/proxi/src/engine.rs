//! The engine seam: normalised engine events, the handle a turn drives, Claude's auth/resume texts.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::protocol::{LearnerSettings, RoleModels};

// One launch type for all three engines; agy and opencode ignore the claude-only fields.
pub use crate::claude::{ClaudeBin as EngineBin, ClaudeLaunch as EngineLaunch, Grant, McpServer};

/// One MCP server as the engine reports it at init.
#[derive(Debug, Clone, PartialEq)]
pub struct McpStatus {
    pub name: String,
    pub status: String,
}

/// A tool call inside an assistant message; `index` is its position in the message content.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub input: Option<Value>,
}

/// What the parser makes of one engine line; only the shapes the app uses.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Init {
        session_id: String,
        tools: Vec<String>,
        mcp_servers: Vec<McpStatus>,
    },
    TextStart {
        message_id: String,
        index: f64,
    },
    TextDelta {
        message_id: String,
        index: f64,
        text: String,
    },
    Assistant {
        message_id: String,
        texts: Vec<(usize, String)>,
        tool_uses: Vec<ToolUse>,
    },
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        text: String,
    },
    Result {
        is_error: bool,
        subtype: String,
        text: Option<String>,
        session_id: String,
        input_tokens: f64,
        output_tokens: f64,
        cache_read_tokens: f64,
        cache_creation_tokens: f64,
        cost_usd: f64,
    },
    ControlResponse {
        request_id: String,
        ok: bool,
        error: Option<String>,
    },
    ResumeFailed,
    ActionDenied {
        action: String,
    },
    AuthFailed {
        message: Option<String>,
    },
    Limits {
        limits: Value,
    },
    Ignored,
}

/// One message from a running engine. `Exit` is sent exactly once and last.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineMsg {
    Event(EngineEvent),
    Stderr(String),
    Exit {
        code: Option<i32>,
        signal: Option<String>,
    },
}

/// The handle a turn drives: write a user line, stop the process tree, read the pid.
pub trait EngineHandle: Send {
    fn send(&mut self, text: &str);
    /// Kills the tree and resolves on exit or after 5 s; calling it twice is safe.
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
    fn pid(&self) -> Option<u32>;
}

/// Node's readable guidance when the CLI is not logged in (`app/server/claude.ts:20`).
pub const CLAUDE_AUTH_ERROR_MESSAGE: &str = "Claude Code non risulta autenticato. Esegui `claude login` nel terminale oppure configura una API key Anthropic nelle Impostazioni.";

/// The agy note appended to the system prompt: prompts name the claude tools (`engine.ts:10-16`).
const AGY_ENGINE_NOTE: &str = "On this engine the read-only tools are named view_file (Read), list_dir (Glob), grep_search (Grep), search_web (WebSearch); there is no page fetch: cite search results with their URLs and say when a claim rests only on a snippet.";

/// The one model rule: agy/opencode take their own model per role, claude the session's alias,
/// else the role's setting (`engine.ts:19-23`).
pub fn model_for(
    settings: &LearnerSettings,
    provider: &str,
    role: &str,
    alias: Option<&str>,
) -> String {
    if provider == "agy" {
        return role_model(&settings.agy_models, role);
    }
    if provider == "opencode" {
        return role_model(&settings.opencode_models, role);
    }
    if let Some(alias) = alias {
        return alias.to_string();
    }
    match role {
        "subagent" => settings.subagent_model.clone(),
        "map" => settings.map_model.clone(),
        _ => settings.teacher_model.clone(),
    }
}

fn role_model(models: &RoleModels, role: &str) -> String {
    match role {
        "subagent" => models.subagent.clone(),
        "map" => models.map.clone(),
        _ => models.teacher.clone(),
    }
}

/// The one effort rule: claude only, and never for `"default"` or an empty setting (`engine.ts:26-30`).
pub fn effort_for(settings: &LearnerSettings, provider: &str, role: &str) -> Option<String> {
    if provider == "agy" || provider == "opencode" {
        return None;
    }
    let effort = match role {
        "subagent" => &settings.effort.subagent,
        "map" => &settings.effort.map,
        _ => &settings.effort.teacher,
    };
    // Node's `eff && eff !== "default"`: the empty string is falsy too.
    if effort.is_empty() || effort == "default" {
        None
    } else {
        Some(effort.clone())
    }
}

/// The system prompt an engine sees: agy only, and only when it has built-in tools to rename.
pub fn with_engine_note(system_prompt: &str, provider: &str, grant: &[Grant]) -> String {
    if provider != "agy" || grant.is_empty() {
        return system_prompt.to_string();
    }
    format!("{system_prompt}\n\n{AGY_ENGINE_NOTE}")
}

/// Node's spawn-failure text: Node names the missing program, an io::Error does not.
pub fn spawn_error_message(program: &str, err: &std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::NotFound {
        format!("spawn {program} ENOENT")
    } else {
        err.to_string()
    }
}

/// Equivalent of Node's `CLAUDE_AUTH_ERROR_RE`, hand-rolled to avoid a regex crate.
pub fn is_auth_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const PHRASES: &[&str] = &[
        "not logged in",
        "authentication required",
        // `please (?:run )?(?:claude )?login` expands to these four.
        "please login",
        "please run login",
        "please claude login",
        "please run claude login",
        "claude login",
        "authentication_failed",
        "oauth_org_not_allowed",
    ];
    if PHRASES.iter().any(|phrase| lower.contains(phrase)) {
        return true;
    }
    // `invalid[ _]api[ _]key`: the four separator combinations.
    [
        "invalid api key",
        "invalid_api_key",
        "invalid api_key",
        "invalid_api key",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

/// Equivalent of Node's `/no conversation found|session.*not found/i`.
pub fn is_resume_refused(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("no conversation found") {
        return true;
    }
    // `session.*not found`: some `session` occurrence followed later by `not found`.
    let mut from = 0;
    while let Some(at) = lower[from..].find("session") {
        let after = from + at + "session".len();
        if lower[after..].contains("not found") {
            return true;
        }
        from = after;
    }
    false
}

/// A random v4 UUID, formatted like `crypto.randomUUID()`.
pub fn uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_errors_match_the_node_regex() {
        for text in [
            "Not logged in · Please run /login",
            "Error: authentication required",
            "Please run claude login to continue",
            "execute `claude login`",
            "API error: authentication_failed",
            "oauth_org_not_allowed: forbidden",
            "Invalid API key · Fix external API key",
        ] {
            assert!(is_auth_error(text), "{text}");
        }
        for text in [
            "ENOENT: file not found",
            "SyntaxError: Unexpected token",
            "Rate limit exceeded",
        ] {
            assert!(!is_auth_error(text), "{text}");
        }
    }

    #[test]
    fn auth_errors_cover_the_optional_words_and_separators() {
        assert!(is_auth_error("please claude login"));
        assert!(is_auth_error("please run login"));
        assert!(is_auth_error("invalid_api_key"));
        assert!(is_auth_error("INVALID_API_KEY"));
        assert!(!is_auth_error("invalid api"));
        assert!(!is_auth_error("invalid-api-key"));
    }

    #[test]
    fn resume_refusals_match_the_node_regex() {
        assert!(is_resume_refused("No conversation found with session ID x"));
        assert!(is_resume_refused("Error: session abc not found"));
        assert!(!is_resume_refused("found session"));
        assert!(!is_resume_refused("ENOENT"));
    }

    #[test]
    fn uuid_v4_has_the_v4_shape() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        for at in [8, 13, 18, 23] {
            assert_eq!(&id[at..at + 1], "-", "{id}");
        }
        assert_eq!(&id[14..15], "4", "{id}");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        assert_ne!(uuid_v4(), id);
    }

    fn settings() -> LearnerSettings {
        LearnerSettings {
            agy_models: RoleModels {
                teacher: "agy-teacher".to_string(),
                subagent: "agy-subagent".to_string(),
                map: "agy-map".to_string(),
            },
            opencode_models: RoleModels {
                teacher: "oc-teacher".to_string(),
                subagent: "oc-subagent".to_string(),
                map: "oc-map".to_string(),
            },
            teacher_model: "claude-teacher".to_string(),
            subagent_model: "claude-subagent".to_string(),
            map_model: "claude-map".to_string(),
            ..LearnerSettings::default()
        }
    }

    #[test]
    fn model_for_follows_the_engine_rule() {
        let s = settings();
        assert_eq!(model_for(&s, "agy", "teacher", Some("alias")), "agy-teacher");
        assert_eq!(model_for(&s, "agy", "subagent", None), "agy-subagent");
        assert_eq!(model_for(&s, "agy", "map", None), "agy-map");
        assert_eq!(model_for(&s, "opencode", "teacher", None), "oc-teacher");
        assert_eq!(model_for(&s, "opencode", "subagent", Some("alias")), "oc-subagent");
        assert_eq!(model_for(&s, "opencode", "map", None), "oc-map");
        assert_eq!(model_for(&s, "claude", "teacher", Some("alias")), "alias");
        assert_eq!(model_for(&s, "claude", "teacher", None), "claude-teacher");
        assert_eq!(model_for(&s, "claude", "subagent", None), "claude-subagent");
        assert_eq!(model_for(&s, "claude", "map", None), "claude-map");
        // An unknown role behaves as the teacher's.
        assert_eq!(model_for(&s, "claude", "other", None), "claude-teacher");
        assert_eq!(model_for(&s, "agy", "other", None), "agy-teacher");
    }

    #[test]
    fn effort_for_is_none_for_agy_opencode_and_default() {
        let mut s = settings();
        s.effort = crate::protocol::RoleEffort {
            teacher: "high".to_string(),
            subagent: "default".to_string(),
            map: String::new(),
        };
        assert_eq!(effort_for(&s, "claude", "teacher").as_deref(), Some("high"));
        assert_eq!(effort_for(&s, "claude", "subagent"), None);
        assert_eq!(effort_for(&s, "claude", "map"), None);
        assert_eq!(effort_for(&s, "agy", "teacher"), None);
        assert_eq!(effort_for(&s, "opencode", "teacher"), None);
    }

    #[test]
    fn engine_note_only_for_agy_with_grants() {
        let note = "On this engine the read-only tools are named view_file (Read), list_dir (Glob), grep_search (Grep), search_web (WebSearch); there is no page fetch: cite search results with their URLs and say when a claim rests only on a snippet.";
        assert_eq!(
            with_engine_note("Sei il docente.", "agy", &[Grant::Read]),
            format!("Sei il docente.\n\n{note}")
        );
        assert_eq!(
            with_engine_note("Sei il docente.", "agy", &[Grant::Read, Grant::Web]),
            format!("Sei il docente.\n\n{note}")
        );
        assert_eq!(with_engine_note("Sei il docente.", "agy", &[]), "Sei il docente.");
        assert_eq!(
            with_engine_note("Sei il docente.", "claude", &[Grant::Read]),
            "Sei il docente."
        );
        assert_eq!(
            with_engine_note("Sei il docente.", "opencode", &[Grant::Read]),
            "Sei il docente."
        );
    }

    #[test]
    fn spawn_error_message_maps_not_found_to_enoent() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(
            spawn_error_message("C:\\tools\\agy.exe", &missing),
            "spawn C:\\tools\\agy.exe ENOENT"
        );
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            spawn_error_message("claude", &denied),
            denied.to_string()
        );
    }
}
