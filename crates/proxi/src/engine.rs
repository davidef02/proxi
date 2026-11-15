//! The engine seam: normalised engine events, the handle a turn drives, Claude's auth/resume texts.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

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
    /// Aborts the running turn: Claude asks in-band (control request); a kill-based adapter kills the tree and keeps the handle.
    fn interrupt(&mut self);
    /// Kills the tree and resolves on exit or after 5 s; calling it twice is safe.
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
    fn pid(&self) -> Option<u32>;
}

/// Node's readable guidance when the CLI is not logged in (`app/server/claude.ts:20`).
pub const CLAUDE_AUTH_ERROR_MESSAGE: &str = "Claude Code non risulta autenticato. Esegui `claude login` nel terminale oppure configura una API key Anthropic nelle Impostazioni.";

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
}
