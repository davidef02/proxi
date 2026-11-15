//! JSON-RPC 2.0 MCP handler (port of app/server/mcp.ts).
//!
//! Plain JSON-RPC over POST, hand-written: the tool registry is the only thing that changes between
//! sessions, so `handle` takes it as a slice and knows nothing about the tools themselves.

use futures_util::future::join_all;
use serde_json::{Map, Value, json};

use crate::js;
use crate::tools::{ToolDef, ToolHost, invoke};

/// Node's tool-name prefix, kept for parity: nothing here builds engine tool names yet.
pub const MCP_PREFIX: &str = "mcp__app__";

/// The JSON-RPC answer, or `None` for a notification (HTTP 202).
pub async fn handle(body: &Value, tools: &[&ToolDef], host: &dyn ToolHost) -> Option<Value> {
    let Value::Array(items) = body else {
        return one(body, tools, host).await;
    };
    // Node's `Promise.all`: all of them run, the notifications drop out of the answer.
    let answers: Vec<Value> = join_all(items.iter().map(|item| one(item, tools, host)))
        .await
        .into_iter()
        .flatten()
        .collect();
    if answers.is_empty() {
        None
    } else {
        Some(Value::Array(answers))
    }
}

async fn one(req: &Value, tools: &[&ToolDef], host: &dyn ToolHost) -> Option<Value> {
    let object = req.as_object();
    let Some(method) = object
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
    else {
        return Some(fail(error_id(req), -32600, "Invalid Request"));
    };
    let id = object.and_then(|object| object.get("id"));
    let is_notification = matches!(id, None | Some(Value::Null));
    // Node bails before the switch, so `notifications/…` never answers, known method or not.
    if method.starts_with("notifications/") {
        return None;
    }
    match method {
        "initialize" => Some(ok(id, initialized(req.get("params")))),
        "ping" => Some(ok(id, json!({}))),
        "tools/list" => Some(ok(id, json!({ "tools": tool_listing(tools) }))),
        "tools/call" => Some(ok(id, call_tool(req, tools, host).await)),
        _ if is_notification => None,
        _ => Some(fail(id, -32601, &format!("Method not found: {method}"))),
    }
}

/// Node's `initialize` result: the client's protocol version wins, else the one this build speaks.
fn initialized(params: Option<&Value>) -> Value {
    let version = params
        .and_then(Value::as_object)
        .and_then(|params| params.get("protocolVersion"))
        .filter(|version| !version.is_null())
        .cloned()
        .unwrap_or_else(|| json!("2025-06-18"));
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "app", "version": "1"},
    })
}

/// The `tools/list` entries: `{name, description, inputSchema, _meta}`, in registry order.
fn tool_listing(tools: &[&ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|def| {
            json!({
                "name": def.name,
                "description": def.description,
                "inputSchema": (def.schema)(),
                "_meta": {"antigravity/alwaysLoad": true},
            })
        })
        .collect()
}

/// Node's `tools/call`: an unknown tool, invalid input and a tool error all answer `isError` text.
async fn call_tool(req: &Value, tools: &[&ToolDef], host: &dyn ToolHost) -> Value {
    let params = req.get("params").and_then(Value::as_object);
    let name = params.and_then(|params| params.get("name"));
    let Some(def) = name
        .and_then(Value::as_str)
        .and_then(|name| tools.iter().copied().find(|def| def.name == name))
    else {
        let available: Vec<&str> = tools.iter().map(|def| def.name).collect();
        return content(
            format!(
                "Unknown tool \"{}\". Available: {}.",
                js::js_string(name),
                available.join(", ")
            ),
            true,
        );
    };
    // `?? {}` in Node: missing and null arguments are an empty object.
    let arguments = match params.and_then(|params| params.get("arguments")) {
        Some(value) if !value.is_null() => value.clone(),
        _ => json!({}),
    };
    match (def.validate)(&arguments) {
        Err(issues) => content(
            format!(
                "Invalid input: {}. Fix and call again.",
                issues.join("; ")
            ),
            true,
        ),
        Ok(input) => {
            let out = invoke(def, input, host).await;
            content(out.text, out.is_error)
        }
    }
}

/// Node's `content(text, isError)`; no in-scope tool returns images.
fn content(text: String, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    })
}

/// Node's `ok`: JS drops an `undefined` id, so a request without the key gets no `id` back; an
/// explicit `null` keeps it.
fn ok(id: Option<&Value>, result: Value) -> Value {
    let mut envelope = Map::new();
    envelope.insert("jsonrpc".to_string(), json!("2.0"));
    if let Some(id) = id {
        envelope.insert("id".to_string(), id.clone());
    }
    envelope.insert("result".to_string(), result);
    Value::Object(envelope)
}

/// Node's `fail`: an error envelope always carries an id, `null` when JS had `undefined`.
fn fail(id: Option<&Value>, code: i64, message: &str) -> Value {
    let mut error = Map::new();
    error.insert("code".to_string(), json!(code));
    error.insert("message".to_string(), json!(message));
    let mut envelope = Map::new();
    envelope.insert("jsonrpc".to_string(), json!("2.0"));
    envelope.insert("id".to_string(), id.cloned().unwrap_or(Value::Null));
    envelope.insert("error".to_string(), Value::Object(error));
    Value::Object(envelope)
}

/// Node's `req?.id`: only an object can carry one.
fn error_id(req: &Value) -> Option<&Value> {
    req.as_object().and_then(|object| object.get("id"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::fake::FakeHost;
    use crate::tools::{ToolFuture, ToolOutcome};

    fn config() -> Value {
        json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false})
    }

    fn host() -> FakeHost {
        FakeHost::new(config())
    }

    /// `mcp.test.ts`'s echo tool, with Node's `toJsonSchema` output.
    fn echo_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string", "const": "x"},
                "msg": {"type": "string"},
                "n": {"type": "number"},
            },
            "required": ["kind", "msg"],
        })
    }

    /// zod's issues for that schema, wording included (the Node test only matches their shape).
    fn echo_validate(input: &Value) -> Result<Value, Vec<String>> {
        let Some(object) = input.as_object() else {
            return Err(vec![format!(
                "(input): Invalid input: expected object, received {}",
                js_type(input)
            )]);
        };
        let mut issues = Vec::new();
        if object.get("kind") != Some(&json!("x")) {
            issues.push("kind: Invalid input: expected \"x\"".to_string());
        }
        match object.get("msg") {
            Some(Value::String(_)) => {}
            Some(other) => issues.push(format!(
                "msg: Invalid input: expected string, received {}",
                js_type(other)
            )),
            None => issues.push("msg: Invalid input: expected string, received undefined".to_string()),
        }
        match object.get("n") {
            None | Some(Value::Number(_)) => {}
            Some(other) => issues.push(format!(
                "n: Invalid input: expected number, received {}",
                js_type(other)
            )),
        }
        if !issues.is_empty() {
            return Err(issues);
        }
        let mut data = Map::new();
        data.insert("kind".to_string(), json!("x"));
        data.insert("msg".to_string(), object["msg"].clone());
        if let Some(n) = object.get("n") {
            data.insert("n".to_string(), n.clone());
        }
        Ok(Value::Object(data))
    }

    /// zod's `received <type>` word.
    fn js_type(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    fn echo_run(input: Value, _host: &dyn ToolHost) -> ToolFuture<'_> {
        Box::pin(async move {
            // Node throws on "boom"; a Rust tool has no throw, so its error outcome carries the text.
            match input.get("msg").and_then(Value::as_str).unwrap_or_default() {
                "boom" => ToolOutcome::error("kaboom"),
                "bad" => ToolOutcome::error("nope"),
                msg => ToolOutcome::ok(format!("got {msg}")),
            }
        })
    }

    fn echo_tool() -> ToolDef {
        ToolDef {
            name: "quiz",
            description: "echo",
            schema: echo_schema,
            validate: echo_validate,
            run: echo_run,
            writes: false,
        }
    }

    fn no_input_schema() -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn accept(input: &Value) -> Result<Value, Vec<String>> {
        Ok(input.clone())
    }

    fn ok_run(_input: Value, _host: &dyn ToolHost) -> ToolFuture<'_> {
        Box::pin(async { ToolOutcome::ok("ok") })
    }

    fn error_run(_input: Value, _host: &dyn ToolHost) -> ToolFuture<'_> {
        Box::pin(async { ToolOutcome::error("nope") })
    }

    /// The `{jsonrpc, id, method, params}` body Node's `rpc` helper builds; `None` omits a key.
    fn request(method: &str, params: Option<Value>, id: Option<Value>) -> Value {
        let mut body = Map::new();
        body.insert("jsonrpc".to_string(), json!("2.0"));
        if let Some(id) = id {
            body.insert("id".to_string(), id);
        }
        body.insert("method".to_string(), json!(method));
        if let Some(params) = params {
            body.insert("params".to_string(), params);
        }
        Value::Object(body)
    }

    async fn rpc(host: &FakeHost, def: &ToolDef, method: &str, params: Option<Value>) -> Value {
        let body = request(method, params, Some(json!(1)));
        handle(&body, &[def], host).await.expect("a response")
    }

    #[test]
    fn exports_the_prefix() {
        assert_eq!(MCP_PREFIX, "mcp__app__");
    }

    #[tokio::test]
    async fn initialize_echoes_the_protocol_version() {
        let host = host();
        let fake = echo_tool();
        let r = rpc(
            &host,
            &fake,
            "initialize",
            Some(json!({"protocolVersion": "2025-03-26"})),
        )
        .await;
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"app","version":"1"}}}"#
        );
    }

    #[tokio::test]
    async fn initialize_defaults_the_protocol_version() {
        let host = host();
        let fake = echo_tool();
        let r = rpc(&host, &fake, "initialize", None).await;
        assert_eq!(r["result"]["protocolVersion"], json!("2025-06-18"));
    }

    #[tokio::test]
    async fn initialize_defaults_the_protocol_version_when_it_is_null() {
        // Node's `params?.protocolVersion ?? "2025-06-18"`: an explicit null is not a version.
        let host = host();
        let fake = echo_tool();
        let r = rpc(
            &host,
            &fake,
            "initialize",
            Some(json!({"protocolVersion": null})),
        )
        .await;
        assert_eq!(r["result"]["protocolVersion"], json!("2025-06-18"));
    }

    #[tokio::test]
    async fn a_notification_has_no_response_and_ping_answers_an_empty_object() {
        let host = host();
        let fake = echo_tool();
        let body = request("notifications/initialized", None, None);
        assert_eq!(handle(&body, &[&fake], &host).await, None);
        assert_eq!(
            serde_json::to_string(&rpc(&host, &fake, "ping", None).await).unwrap(),
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#
        );
    }

    #[tokio::test]
    async fn tools_list_gives_an_object_schema_without_schema_and_discriminator_first() {
        let host = host();
        let fake = echo_tool();
        let r = rpc(&host, &fake, "tools/list", None).await;
        let tool = &r["result"]["tools"][0];
        assert_eq!(tool["name"], json!("quiz"));
        assert_eq!(tool["inputSchema"]["type"], json!("object"));
        assert!(tool["inputSchema"].get("$schema").is_none());
        let properties: Vec<&String> = tool["inputSchema"]["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect();
        assert_eq!(properties, ["kind", "msg", "n"]);
        assert_eq!(tool["inputSchema"]["required"], json!(["kind", "msg"]));
        assert_eq!(tool["_meta"], json!({"antigravity/alwaysLoad": true}));
        let keys: Vec<&String> = tool.as_object().expect("a tool entry").keys().collect();
        assert_eq!(keys, ["name", "description", "inputSchema", "_meta"]);
    }

    #[tokio::test]
    async fn tools_call_runs_the_tool() {
        let host = host();
        let fake = echo_tool();
        let r = rpc(
            &host,
            &fake,
            "tools/call",
            Some(json!({"name": "quiz", "arguments": {"kind": "x", "msg": "hi"}})),
        )
        .await;
        assert_eq!(
            serde_json::to_string(&r["result"]).unwrap(),
            r#"{"content":[{"type":"text","text":"got hi"}],"isError":false}"#
        );
    }

    #[tokio::test]
    async fn missing_or_null_arguments_reach_the_validator_as_an_empty_object() {
        // Node's `params?.arguments ?? {}`: a missing key and an explicit null are the same `{}`.
        let host = host();
        let fake = echo_tool();
        let empty = rpc(
            &host,
            &fake,
            "tools/call",
            Some(json!({"name": "quiz", "arguments": {}})),
        )
        .await;
        assert_eq!(empty["result"]["isError"], json!(true));
        for params in [
            json!({"name": "quiz", "arguments": null}),
            json!({"name": "quiz"}),
        ] {
            let r = rpc(&host, &fake, "tools/call", Some(params)).await;
            assert_eq!(r["result"], empty["result"]);
        }
    }

    #[tokio::test]
    async fn invalid_input_is_an_error_result_naming_every_path() {
        let host = host();
        let fake = echo_tool();
        let r = rpc(
            &host,
            &fake,
            "tools/call",
            Some(json!({"name": "quiz", "arguments": {"kind": "y", "n": "a"}})),
        )
        .await;
        assert_eq!(r["result"]["isError"], json!(true));
        assert_eq!(
            r["result"]["content"][0]["text"],
            json!(r#"Invalid input: kind: Invalid input: expected "x"; msg: Invalid input: expected string, received undefined; n: Invalid input: expected number, received string. Fix and call again."#)
        );
    }

    #[tokio::test]
    async fn a_tool_error_stays_inside_the_result() {
        let host = host();
        let fake = echo_tool();
        let boom = rpc(
            &host,
            &fake,
            "tools/call",
            Some(json!({"name": "quiz", "arguments": {"kind": "x", "msg": "boom"}})),
        )
        .await;
        assert_eq!(boom["result"]["isError"], json!(true));
        assert_eq!(boom["result"]["content"][0]["text"], json!("kaboom"));

        let bad = rpc(
            &host,
            &fake,
            "tools/call",
            Some(json!({"name": "quiz", "arguments": {"kind": "x", "msg": "bad"}})),
        )
        .await;
        assert_eq!(
            serde_json::to_string(&bad["result"]).unwrap(),
            r#"{"content":[{"type":"text","text":"nope"}],"isError":true}"#
        );
    }

    #[tokio::test]
    async fn an_unknown_tool_is_an_error_result_listing_the_available_ones() {
        let host = host();
        let fake = echo_tool();
        let r = rpc(
            &host,
            &fake,
            "tools/call",
            Some(json!({"name": "nope", "arguments": {}})),
        )
        .await;
        assert_eq!(r["result"]["isError"], json!(true));
        assert_eq!(
            r["result"]["content"][0]["text"],
            json!(r#"Unknown tool "nope". Available: quiz."#)
        );

        // A missing name is JS's `undefined` in the message.
        let missing = rpc(&host, &fake, "tools/call", Some(json!({}))).await;
        assert_eq!(
            missing["result"]["content"][0]["text"],
            json!(r#"Unknown tool "undefined". Available: quiz."#)
        );
    }

    #[tokio::test]
    async fn unknown_methods_with_an_id_are_method_not_found() {
        let host = host();
        let fake = echo_tool();
        for method in ["server/discover", "resources/list"] {
            let r = rpc(&host, &fake, method, None).await;
            assert_eq!(
                serde_json::to_string(&r).unwrap(),
                format!(
                    r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":-32601,"message":"Method not found: {method}"}}}}"#
                )
            );
        }
        // Without an id the same method is a notification: no answer at all.
        let body = request("foo/bar", None, None);
        assert_eq!(handle(&body, &[&fake], &host).await, None);
    }

    #[tokio::test]
    async fn a_garbage_body_is_an_invalid_request() {
        let host = host();
        let fake = echo_tool();
        let body = json!("nonsense");
        assert_eq!(
            serde_json::to_string(&handle(&body, &[&fake], &host).await.expect("a response")).unwrap(),
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}}"#
        );
        // An object without a method keeps its id.
        let body = json!({"jsonrpc": "2.0", "id": 5});
        let r = handle(&body, &[&fake], &host).await.expect("a response");
        assert_eq!(r["id"], json!(5));
        assert_eq!(r["error"]["code"], json!(-32600));
    }

    #[tokio::test]
    async fn an_id_less_initialize_has_no_id_key() {
        let host = host();
        let fake = echo_tool();
        let body = request("initialize", None, None);
        let r = handle(&body, &[&fake], &host).await.expect("a response");
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.starts_with(r#"{"jsonrpc":"2.0","result":{"protocolVersion":"2025-06-18""#),
            "{json}"
        );
        assert!(!json.contains(r#""id""#), "{json}");

        // An explicit null id is not `undefined`: the key stays.
        let body = request("ping", None, Some(Value::Null));
        let r = handle(&body, &[&fake], &host).await.expect("a response");
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"jsonrpc":"2.0","id":null,"result":{}}"#
        );
    }

    #[tokio::test]
    async fn a_batch_drops_notifications_and_keeps_the_order() {
        let host = host();
        let fake = echo_tool();
        let batch = json!([
            {"jsonrpc": "2.0", "id": 6, "method": "ping"},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 7, "method": "foo/bar"},
        ]);
        let answers = handle(&batch, &[&fake], &host)
            .await
            .expect("a response");
        let answers = answers.as_array().expect("an array");
        assert_eq!(answers.len(), 2);
        assert_eq!(
            serde_json::to_string(&answers[0]).unwrap(),
            r#"{"jsonrpc":"2.0","id":6,"result":{}}"#
        );
        assert_eq!(
            serde_json::to_string(&answers[1]).unwrap(),
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Method not found: foo/bar"}}"#
        );
    }

    #[tokio::test]
    async fn an_empty_batch_or_one_of_notifications_has_no_response() {
        let host = host();
        let fake = echo_tool();
        let empty = json!([]);
        assert_eq!(handle(&empty, &[&fake], &host).await, None);
        let batch = json!([{"jsonrpc": "2.0", "method": "notifications/initialized"}]);
        assert_eq!(handle(&batch, &[&fake], &host).await, None);
    }

    #[tokio::test]
    async fn a_writes_tool_raises_courses_changed_only_when_it_succeeds() {
        let host = host();
        let ok_writer = ToolDef {
            name: "writer",
            description: "writes",
            schema: no_input_schema,
            validate: accept,
            run: ok_run,
            writes: true,
        };
        let bad_writer = ToolDef {
            name: "bad_writer",
            description: "writes and fails",
            schema: no_input_schema,
            validate: accept,
            run: error_run,
            writes: true,
        };

        let body = request("tools/call", Some(json!({"name": "writer"})), Some(json!(1)));
        let r = handle(&body, &[&ok_writer], &host).await.expect("a response");
        assert_eq!(r["result"]["isError"], json!(false));
        assert_eq!(host.courses_changed_count(), 1);

        let body = request(
            "tools/call",
            Some(json!({"name": "bad_writer"})),
            Some(json!(2)),
        );
        let r = handle(&body, &[&bad_writer], &host)
            .await
            .expect("a response");
        assert_eq!(r["result"]["isError"], json!(true));
        assert_eq!(
            host.courses_changed_count(),
            1,
            "an error run must not raise courses_changed"
        );
    }
}
