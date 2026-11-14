//! AnkiConnect version probe (port of `pingAnkiConnect`, `anki-sync.ts:517-563`).

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::lesson::js::{is_js_space, js_to_string, js_trim, js_truthy};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8765";

/// The maximum `setTimeout` delay: Node clamps anything above it to 1ms.
const MAX_TIMER_MS: u128 = 2_147_483_647;

#[derive(Debug, Clone)]
pub struct PingResult {
    pub online: bool,
    pub version: Option<String>,
    pub error: Option<String>,
    pub response_time_ms: u128,
}

/// JS `parseInt(raw || "2000", 10)` followed by `setTimeout` clamping.
pub fn parse_timeout(raw: Option<&str>) -> u64 {
    let raw = match raw {
        Some(text) if !text.is_empty() => text,
        _ => "2000",
    };
    let mut chars = raw.chars().peekable();
    while chars.peek().is_some_and(|c| is_js_space(*c)) {
        chars.next();
    }
    let negative = match chars.peek() {
        Some('+') => {
            chars.next();
            false
        }
        Some('-') => {
            chars.next();
            true
        }
        _ => false,
    };
    let mut value: u128 = 0;
    let mut any = false;
    while let Some(c) = chars.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        any = true;
        value = value
            .saturating_mul(10)
            .saturating_add(u128::from(c as u8 - b'0'));
        chars.next();
    }
    if !any || negative || value == 0 || value > MAX_TIMER_MS {
        1
    } else {
        value as u64
    }
}

/// Node's `res.json()` + AnkiConnect envelope: Ok is the version text, Err the error text.
pub fn interpret_body(bytes: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = js_trim(&text);
    if trimmed.is_empty() {
        return Err("Unexpected end of JSON input".to_string());
    }
    let data: Value = serde_json::from_str(trimmed).map_err(|error| error.to_string())?;
    match &data {
        Value::Null => Err("Cannot read properties of null (reading 'error')".to_string()),
        Value::Object(map) => {
            let error = map.get("error").filter(|value| js_truthy(Some(*value)));
            if let Some(error) = error {
                return Err(js_to_string(error));
            }
            Ok(match map.get("result") {
                Some(result) if !result.is_null() => js_to_string(result),
                _ => "6".to_string(),
            })
        }
        // Numbers, strings and arrays have no `.error`, so Node answers the `?? 6` default.
        _ => Ok("6".to_string()),
    }
}

pub fn ping_anki_connect(url: &str, timeout_ms: u64) -> PingResult {
    let start = Instant::now();
    let offline = |error: String| PingResult {
        online: false,
        version: None,
        error: Some(error),
        response_time_ms: start.elapsed().as_millis(),
    };

    if reqwest::Url::parse(url).is_err() {
        return offline(format!("Failed to parse URL from {url}"));
    }
    let client = match reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(timeout_ms))
        .build()
    {
        Ok(client) => client,
        Err(_) => return offline("fetch failed".to_string()),
    };

    let response = match client
        .post(url)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(r#"{"action":"version","version":6}"#)
        .send()
    {
        Ok(response) => response,
        Err(error) => return offline(transport_error(&error)),
    };
    let status = response.status();
    if !status.is_success() {
        return offline(format!("HTTP error status {}", status.as_u16()));
    }
    let bytes = match response.bytes() {
        Ok(bytes) => bytes,
        Err(error) => return offline(transport_error(&error)),
    };

    match interpret_body(&bytes) {
        Ok(version) => PingResult {
            online: true,
            version: Some(version),
            error: None,
            response_time_ms: start.elapsed().as_millis(),
        },
        Err(error) => offline(error),
    }
}

/// undici says `fetch failed` for anything that never reached the server; an abort has its own text.
fn transport_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "Connection timed out".to_string()
    } else {
        "fetch failed".to_string()
    }
}

/// The single line the `ping` command prints (no trailing newline).
pub fn ping_line(url: &str, result: &PingResult) -> String {
    if result.online {
        format!(
            "✅ AnkiConnect is ONLINE ({url}) — Version {} ({}ms)",
            result.version.as_deref().unwrap_or_default(),
            result.response_time_ms
        )
    } else {
        format!(
            "⚠️ AnkiConnect is OFFLINE ({url}): {} ({}ms)",
            result.error.as_deref().unwrap_or_default(),
            result.response_time_ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_timeout_matches_parse_int() {
        assert_eq!(parse_timeout(None), 2000);
        assert_eq!(parse_timeout(Some("")), 2000);
        assert_eq!(parse_timeout(Some(" 150ms")), 150);
        assert_eq!(parse_timeout(Some("+7")), 7);
        assert_eq!(parse_timeout(Some("abc")), 1);
        assert_eq!(parse_timeout(Some("0")), 1);
        assert_eq!(parse_timeout(Some("-5")), 1);
        assert_eq!(parse_timeout(Some("99999999999")), 1);
    }

    #[test]
    fn interpret_body_cases() {
        assert_eq!(
            interpret_body(b"").unwrap_err(),
            "Unexpected end of JSON input"
        );
        assert_eq!(
            interpret_body(b"  ").unwrap_err(),
            "Unexpected end of JSON input"
        );
        assert!(!interpret_body(b"nope").unwrap_err().is_empty());
        assert_eq!(
            interpret_body(b"null").unwrap_err(),
            "Cannot read properties of null (reading 'error')"
        );
        assert_eq!(
            interpret_body(br#"{"result":6,"error":null}"#).unwrap(),
            "6"
        );
        assert_eq!(
            interpret_body(br#"{"result":null,"error":null}"#).unwrap(),
            "6"
        );
        assert_eq!(interpret_body(b"{}").unwrap(), "6");
        assert_eq!(interpret_body(b"5").unwrap(), "6");
        assert_eq!(interpret_body(br#""x""#).unwrap(), "6");
        assert_eq!(interpret_body(br#"{"result":7}"#).unwrap(), "7");
        assert_eq!(interpret_body(br#"{"error":"boom"}"#).unwrap_err(), "boom");
        assert_eq!(interpret_body(br#"{"error":5}"#).unwrap_err(), "5");
        assert_eq!(interpret_body(br#"{"error":"","result":3}"#).unwrap(), "3");
        assert_eq!(
            interpret_body(br#"{"error":false,"result":3}"#).unwrap(),
            "3"
        );
    }

    #[test]
    fn dead_port_is_fetch_failed() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a port to drop");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        // Long timeout: Windows takes a while to refuse, and that must not read as a timeout.
        let result = ping_anki_connect(&format!("http://127.0.0.1:{port}"), 10_000);

        assert!(!result.online);
        assert_eq!(result.error.as_deref(), Some("fetch failed"));
        assert_eq!(result.version, None);
    }

    #[test]
    fn invalid_url() {
        let result = ping_anki_connect("notaurl", 2000);

        assert!(!result.online);
        assert_eq!(
            result.error.as_deref(),
            Some("Failed to parse URL from notaurl")
        );
    }

    #[test]
    fn ping_line_formats() {
        let online = PingResult {
            online: true,
            version: Some("6".to_string()),
            error: None,
            response_time_ms: 12,
        };
        assert_eq!(
            ping_line("http://127.0.0.1:8765", &online),
            "✅ AnkiConnect is ONLINE (http://127.0.0.1:8765) — Version 6 (12ms)"
        );

        let offline = PingResult {
            online: false,
            version: None,
            error: Some("fetch failed".to_string()),
            response_time_ms: 7,
        };
        assert_eq!(
            ping_line("http://127.0.0.1:9", &offline),
            "⚠️ AnkiConnect is OFFLINE (http://127.0.0.1:9): fetch failed (7ms)"
        );
    }
}
