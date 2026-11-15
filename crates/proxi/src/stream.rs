//! Parses Claude's stream-json lines into engine events (port of `app/server/stream.ts`).

use serde_json::{Map, Value};

use crate::engine::{EngineEvent, McpStatus, ToolUse};
use crate::events::iso_now;
use crate::js;

/// The id of the message currently streaming (from `stream_event` `message_start`).
#[derive(Default)]
pub struct ParseState {
    pub message_id: String,
}

fn ignored() -> Vec<EngineEvent> {
    vec![EngineEvent::Ignored]
}

/// A finite JSON number, else 0 (Node's `num`).
fn num(value: Option<&Value>) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .unwrap_or(0.0)
}

/// A JSON string, else "" (Node's `str`).
fn str(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// A tool result's content: a string, or the text blocks of an array joined.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| {
                part.get("type").and_then(Value::as_str) == Some("text")
                    && part.get("text").is_some_and(Value::is_string)
            })
            .map(|part| part.get("text").and_then(Value::as_str).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// One stdout line -> the events it carries; never panics on garbage.
pub fn parse_stream_line(line: &str, state: &mut ParseState) -> Vec<EngineEvent> {
    let Ok(m) = serde_json::from_str::<Value>(line) else {
        return ignored();
    };
    if !m.is_object() {
        return ignored();
    }
    parse_object(&m, state)
}

fn parse_object(m: &Value, state: &mut ParseState) -> Vec<EngineEvent> {
    match m.get("type").and_then(Value::as_str) {
        Some("system") => {
            if m.get("subtype").and_then(Value::as_str) != Some("init") {
                return ignored();
            }
            let tools = match m.get("tools") {
                Some(Value::Array(items)) => items.iter().map(|x| js::js_string(Some(x))).collect(),
                _ => Vec::new(),
            };
            let mcp_servers = match m.get("mcp_servers") {
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|s| McpStatus {
                        name: str(s.get("name")),
                        status: str(s.get("status")),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            vec![EngineEvent::Init {
                session_id: str(m.get("session_id")),
                tools,
                mcp_servers,
            }]
        }
        Some("stream_event") => {
            let Some(e) = m.get("event") else {
                return ignored();
            };
            match e.get("type").and_then(Value::as_str) {
                Some("message_start") => {
                    state.message_id = str(e.get("message").and_then(|msg| msg.get("id")));
                    ignored()
                }
                Some("content_block_start") => {
                    let block = e.get("content_block").and_then(|b| b.get("type"));
                    if block.and_then(Value::as_str) != Some("text") {
                        return ignored();
                    }
                    vec![EngineEvent::TextStart {
                        message_id: state.message_id.clone(),
                        index: num(e.get("index")),
                    }]
                }
                Some("content_block_delta") => {
                    let delta = e.get("delta");
                    if delta.and_then(|d| d.get("type")).and_then(Value::as_str)
                        != Some("text_delta")
                    {
                        return ignored();
                    }
                    vec![EngineEvent::TextDelta {
                        message_id: state.message_id.clone(),
                        index: num(e.get("index")),
                        text: str(delta.and_then(|d| d.get("text"))),
                    }]
                }
                _ => ignored(),
            }
        }
        Some("assistant") => {
            let message = m.get("message");
            let content = message
                .and_then(|msg| msg.get("content"))
                .and_then(Value::as_array);
            let mut texts = Vec::new();
            let mut tool_uses = Vec::new();
            for (index, b) in content.into_iter().flatten().enumerate() {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => texts.push((index, str(b.get("text")))),
                    Some("tool_use") => tool_uses.push(ToolUse {
                        index,
                        id: str(b.get("id")),
                        name: str(b.get("name")),
                        input: b.get("input").cloned(),
                    }),
                    _ => {}
                }
            }
            vec![EngineEvent::Assistant {
                message_id: str(message.and_then(|msg| msg.get("id"))),
                texts,
                tool_uses,
            }]
        }
        Some("user") => {
            let Some(content) = m
                .get("message")
                .and_then(|msg| msg.get("content"))
                .and_then(Value::as_array)
            else {
                return ignored();
            };
            let out: Vec<EngineEvent> = content
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                .map(|b| EngineEvent::ToolResult {
                    tool_use_id: str(b.get("tool_use_id")),
                    is_error: b.get("is_error") == Some(&Value::Bool(true)),
                    text: result_text(b.get("content")),
                })
                .collect();
            if out.is_empty() { ignored() } else { out }
        }
        Some("result") => {
            let usage = m.get("usage");
            let tokens = |key: &str| num(usage.and_then(|u| u.get(key)));
            vec![EngineEvent::Result {
                is_error: m.get("is_error") == Some(&Value::Bool(true)),
                subtype: str(m.get("subtype")),
                text: m.get("result").and_then(Value::as_str).map(str::to_owned),
                session_id: str(m.get("session_id")),
                input_tokens: tokens("input_tokens"),
                output_tokens: tokens("output_tokens"),
                cache_read_tokens: tokens("cache_read_input_tokens"),
                cache_creation_tokens: tokens("cache_creation_input_tokens"),
                cost_usd: num(m.get("total_cost_usd")),
            }]
        }
        Some("rate_limit_event") => {
            let info = m.get("rate_limit_info");
            let unified = info.and_then(|i| i.get("unifiedWindows"));
            let rate_type = info
                .and_then(|i| i.get("rateLimitType"))
                .and_then(Value::as_str);
            let five_hour = window(unified.and_then(|u| u.get("five_hour"))).or_else(|| {
                if rate_type == Some("five_hour") {
                    window(info)
                } else {
                    None
                }
            });
            let seven_day = window(unified.and_then(|u| u.get("seven_day"))).or_else(|| {
                if rate_type == Some("seven_day") {
                    window(info)
                } else {
                    None
                }
            });
            if five_hour.is_none() && seven_day.is_none() {
                return ignored();
            }
            let mut limits = Map::new();
            if let Some(win) = five_hour {
                limits.insert("fiveHour".into(), win);
            }
            if let Some(win) = seven_day {
                limits.insert("sevenDay".into(), win);
            }
            limits.insert("at".into(), Value::String(iso_now()));
            vec![EngineEvent::Limits {
                limits: Value::Object(limits),
            }]
        }
        Some("control_response") => {
            let r = m.get("response");
            vec![EngineEvent::ControlResponse {
                request_id: str(r.and_then(|r| r.get("request_id"))),
                ok: r.and_then(|r| r.get("subtype")).and_then(Value::as_str) == Some("success"),
                error: r
                    .and_then(|r| r.get("error"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }]
        }
        _ => ignored(),
    }
}

/// One quota window: only objects with a numeric `utilization` count.
fn window(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    let utilization = js::number(value.get("utilization")?.as_f64()?);
    let mut win = Map::new();
    win.insert("utilization".into(), utilization);
    // `resetsAt: undefined` is dropped by JSON.stringify, so only a number makes the key.
    if let Some(resets) = value.get("resetsAt").and_then(Value::as_f64) {
        win.insert("resetsAt".into(), js::number(resets));
    }
    Some(Value::Object(win))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(value: Value) -> Vec<EngineEvent> {
        parse_stream_line(&value.to_string(), &mut ParseState::default())
    }

    fn parse_text(line: &str) -> Vec<EngineEvent> {
        parse_stream_line(line, &mut ParseState::default())
    }

    #[test]
    fn system_init() {
        let events = parse(json!({
            "type": "system", "subtype": "init", "session_id": "s1",
            "tools": ["Read", "mcp__app__quiz"],
            "mcp_servers": [{ "name": "app", "status": "connected", "source": "dynamic" }],
        }));
        assert_eq!(
            events,
            vec![EngineEvent::Init {
                session_id: "s1".into(),
                tools: vec!["Read".into(), "mcp__app__quiz".into()],
                mcp_servers: vec![McpStatus {
                    name: "app".into(),
                    status: "connected".into(),
                }],
            }]
        );
    }

    #[test]
    fn system_non_init_and_unknown_types_are_ignored() {
        for value in [
            json!({ "type": "system", "subtype": "status" }),
            json!({ "type": "rate_limit_event" }),
            json!({ "type": "system", "subtype": "ui_invalidate" }),
        ] {
            assert_eq!(parse(value), vec![EngineEvent::Ignored]);
        }
    }

    #[test]
    fn stream_event_remembers_message_start_then_text_blocks() {
        let mut state = ParseState::default();
        let mut parse_with = |value: Value| parse_stream_line(&value.to_string(), &mut state);

        assert_eq!(
            parse_with(
                json!({ "type": "stream_event", "event": { "type": "message_start", "message": { "id": "msg_1" } } })
            ),
            vec![EngineEvent::Ignored]
        );
        assert_eq!(
            parse_with(
                json!({ "type": "stream_event", "event": { "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } } })
            ),
            vec![EngineEvent::TextStart {
                message_id: "msg_1".into(),
                index: 1.0,
            }]
        );
        assert_eq!(
            parse_with(
                json!({ "type": "stream_event", "event": { "type": "content_block_start", "index": 0, "content_block": { "type": "thinking" } } })
            ),
            vec![EngineEvent::Ignored]
        );
        assert_eq!(
            parse_with(
                json!({ "type": "stream_event", "event": { "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Ciao" } } })
            ),
            vec![EngineEvent::TextDelta {
                message_id: "msg_1".into(),
                index: 1.0,
                text: "Ciao".into(),
            }]
        );
        assert_eq!(
            parse_with(
                json!({ "type": "stream_event", "event": { "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "x" } } })
            ),
            vec![EngineEvent::Ignored]
        );
        assert_eq!(
            parse_with(
                json!({ "type": "stream_event", "event": { "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{" } } })
            ),
            vec![EngineEvent::Ignored]
        );
        assert_eq!(
            parse_with(json!({ "type": "stream_event", "event": { "type": "message_stop" } })),
            vec![EngineEvent::Ignored]
        );
    }

    #[test]
    fn assistant_blocks_keep_their_content_index() {
        let events = parse(json!({
            "type": "assistant",
            "message": { "id": "msg_2", "content": [
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": "Vediamo." },
                { "type": "tool_use", "id": "toolu_1", "name": "mcp__app__quiz", "input": { "question": "?" }, "caller": { "type": "direct" } },
            ] },
        }));
        assert_eq!(
            events,
            vec![EngineEvent::Assistant {
                message_id: "msg_2".into(),
                texts: vec![(1, "Vediamo.".into())],
                tool_uses: vec![ToolUse {
                    index: 2,
                    id: "toolu_1".into(),
                    name: "mcp__app__quiz".into(),
                    input: Some(json!({ "question": "?" })),
                }],
            }]
        );
    }

    #[test]
    fn user_tool_results_read_string_and_array_content() {
        assert_eq!(
            parse(json!({ "type": "user", "message": { "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": [{ "type": "text", "text": "Learner " }, { "type": "text", "text": "chose: 4" }] },
                { "type": "tool_result", "tool_use_id": "t2", "content": "denied", "is_error": true },
            ] } })),
            vec![
                EngineEvent::ToolResult {
                    tool_use_id: "t1".into(),
                    is_error: false,
                    text: "Learner chose: 4".into(),
                },
                EngineEvent::ToolResult {
                    tool_use_id: "t2".into(),
                    is_error: true,
                    text: "denied".into(),
                },
            ]
        );
        assert_eq!(
            parse(json!({ "type": "user", "message": { "content": "hi" } })),
            vec![EngineEvent::Ignored]
        );
        assert_eq!(
            parse(
                json!({ "type": "user", "message": { "content": [{ "type": "text", "text": "[Request interrupted by user]" }] } })
            ),
            vec![EngineEvent::Ignored]
        );
    }

    #[test]
    fn result_carries_text_usage_and_cost() {
        assert_eq!(
            parse(json!({
                "type": "result", "subtype": "success", "is_error": false, "result": "Sette",
                "session_id": "s", "total_cost_usd": 0.5,
                "usage": { "input_tokens": 2, "output_tokens": 5, "cache_read_input_tokens": 625, "cache_creation_input_tokens": 150 },
            })),
            vec![EngineEvent::Result {
                is_error: false,
                subtype: "success".into(),
                text: Some("Sette".into()),
                session_id: "s".into(),
                input_tokens: 2.0,
                output_tokens: 5.0,
                cache_read_tokens: 625.0,
                cache_creation_tokens: 150.0,
                cost_usd: 0.5,
            }]
        );

        let events = parse(
            json!({ "type": "result", "subtype": "error_during_execution", "is_error": true, "session_id": "s" }),
        );
        let EngineEvent::Result {
            is_error,
            input_tokens,
            text,
            ..
        } = &events[0]
        else {
            panic!("{events:?}");
        };
        assert!(*is_error);
        assert_eq!(*input_tokens, 0.0);
        assert_eq!(*text, None);
    }

    #[test]
    fn control_response_reports_ok_only_on_success() {
        assert_eq!(
            parse(
                json!({ "type": "control_response", "response": { "subtype": "success", "request_id": "int-1", "response": {} } })
            ),
            vec![EngineEvent::ControlResponse {
                request_id: "int-1".into(),
                ok: true,
                error: None,
            }]
        );
        assert_eq!(
            parse(
                json!({ "type": "control_response", "response": { "subtype": "error", "request_id": "int-2", "error": "boom" } })
            ),
            vec![EngineEvent::ControlResponse {
                request_id: "int-2".into(),
                ok: false,
                error: Some("boom".into()),
            }]
        );
    }

    #[test]
    fn garbage_never_throws() {
        for line in [
            "",
            "not json",
            "{",
            "null",
            "42",
            "[]",
            "\"x\"",
            "{\"type\":\"stream_event\"}",
            "{\"type\":\"assistant\",\"message\":null}",
            "{\"type\":\"result\",\"usage\":5}",
            "{\"type\":\"assistant\",\"message\":5}",
            "{\"type\":\"user\",\"message\":{\"content\":[null,5,\"x\"]}}",
            "{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[null,{},\"Read\"],\"mcp_servers\":[null,5]}",
            "{\"type\":\"stream_event\",\"event\":{\"type\":\"message_start\",\"message\":null}}",
            "{\"type\":\"control_response\",\"response\":null}",
        ] {
            let events = parse_text(line);
            assert!(!events.is_empty(), "{line}");
        }
        assert_eq!(parse_text("not json"), vec![EngineEvent::Ignored]);
        assert_eq!(parse_text("null"), vec![EngineEvent::Ignored]);
        assert_eq!(parse_text("[]"), vec![EngineEvent::Ignored]);
        assert_eq!(
            parse_text("{\"type\":\"assistant\",\"message\":5}"),
            vec![EngineEvent::Assistant {
                message_id: String::new(),
                texts: Vec::new(),
                tool_uses: Vec::new(),
            }]
        );

        // Deep nulls: every leaf falls back like Node's `str`/`num`, nothing panics.
        assert_eq!(
            parse(json!({ "type": "user", "message": { "content": [
                { "type": "tool_result", "tool_use_id": null, "is_error": null,
                  "content": [null, { "type": "text", "text": 5 }, { "type": "text", "text": "ok" }] },
            ] } })),
            vec![EngineEvent::ToolResult {
                tool_use_id: String::new(),
                is_error: false,
                text: "ok".into(),
            }]
        );
        assert_eq!(
            parse(
                json!({ "type": "stream_event", "event": { "type": "content_block_start", "index": null, "content_block": { "type": "text" } } })
            ),
            vec![EngineEvent::TextStart {
                message_id: String::new(),
                index: 0.0,
            }]
        );
        assert_eq!(
            parse(
                json!({ "type": "rate_limit_event", "rate_limit_info": { "unifiedWindows": { "five_hour": null } } })
            ),
            vec![EngineEvent::Ignored]
        );
    }

    #[test]
    fn rate_limit_event_reports_both_windows_or_one() {
        let events = parse(json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "allowed_warning",
                "rateLimitType": "seven_day",
                "utilization": 0.94,
                "unifiedWindows": {
                    "five_hour": { "utilization": 0.26, "resetsAt": 1793212200 },
                    "seven_day": { "utilization": 0.94, "resetsAt": 1793401200 },
                },
            },
        }));
        let EngineEvent::Limits { limits } = &events[0] else {
            panic!("{events:?}");
        };
        assert_eq!(
            limits.get("fiveHour"),
            Some(&json!({ "utilization": 0.26, "resetsAt": 1793212200 }))
        );
        assert_eq!(
            limits.get("sevenDay"),
            Some(&json!({ "utilization": 0.94, "resetsAt": 1793401200 }))
        );
        let keys: Vec<&String> = limits.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["fiveHour", "sevenDay", "at"]);
        let at = limits.get("at").and_then(Value::as_str).unwrap();
        assert_eq!(at.len(), 24);
        assert!(at.ends_with('Z'));

        let events = parse(json!({
            "type": "rate_limit_event",
            "rate_limit_info": { "rateLimitType": "five_hour", "utilization": 0.5 },
        }));
        let EngineEvent::Limits { limits } = &events[0] else {
            panic!("{events:?}");
        };
        assert_eq!(limits.get("fiveHour"), Some(&json!({ "utilization": 0.5 })));
        assert_eq!(limits.get("sevenDay"), None);
        let keys: Vec<&String> = limits.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["fiveHour", "at"]);
        let window = limits.get("fiveHour").unwrap().as_object().unwrap();
        let keys: Vec<&String> = window.keys().collect();
        assert_eq!(keys, ["utilization"]);

        assert_eq!(
            parse(
                json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "allowed" } })
            ),
            vec![EngineEvent::Ignored]
        );
        assert_eq!(
            parse(
                json!({ "type": "rate_limit_event", "rate_limit_info": { "rateLimitType": "seven_day", "utilization": "high" } })
            ),
            vec![EngineEvent::Ignored]
        );
    }
}
