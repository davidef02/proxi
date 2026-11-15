//! The `ask` tool (port of app/server/tools/ask.ts).

use serde_json::{Map, Value, json};

use crate::js;
use crate::sessions::{invalid_type, issue};
use crate::tools::util;
use crate::tools::{Cancelled, ToolDef, ToolFuture, ToolHost, ToolOutcome};

/// Node's `askTool`.
pub const TOOL: ToolDef = ToolDef {
    name: "ask",
    description: "Ask the learner an ungraded question (goal, preference, fork) as a card; blocks until answered. Use quiz for anything with a right answer.",
    schema,
    validate,
    run,
    writes: false,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("schemas/ask.json")).expect("the ask schema parses")
}

const TOO_SMALL_ONE: &str = "Too small: expected string to have >=1 characters";
const MISSING_STRING: &str = "Invalid input: expected string, received undefined";

/// Node's `askSchema`, hand-rolled: the cleaned data or one `"<path>: <message>"` per issue.
fn validate(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    match object.get("question") {
        Some(Value::String(question)) if question.is_empty() => {
            issues.push(issue("question", TOO_SMALL_ONE));
        }
        Some(Value::String(question)) => {
            data.insert("question".to_string(), Value::String(question.clone()));
        }
        Some(other) => issues.push(issue("question", &invalid_type("string", Some(other)))),
        None => issues.push(issue("question", MISSING_STRING)),
    }

    if let Some(details) = object.get("details") {
        match details {
            Value::String(_) => {
                data.insert("details".to_string(), details.clone());
            }
            other => issues.push(issue("details", &invalid_type("string", Some(other)))),
        }
    }

    if let Some(options) = object.get("options") {
        match options.as_array() {
            None => issues.push(issue("options", &invalid_type("array", Some(options)))),
            Some(items) => {
                let mut clean = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    let path = format!("options.{index}");
                    match item.as_object() {
                        None => issues.push(issue(&path, &invalid_type("object", Some(item)))),
                        Some(option) => clean.push(read_option(option, &path, &mut issues)),
                    }
                }
                data.insert("options".to_string(), Value::Array(clean));
            }
        }
    }

    if let Some(multi_select) = object.get("multiSelect") {
        match multi_select {
            Value::Bool(_) => {
                data.insert("multiSelect".to_string(), multi_select.clone());
            }
            other => issues.push(issue("multiSelect", &invalid_type("boolean", Some(other)))),
        }
    }

    if issues.is_empty() {
        Ok(Value::Object(data))
    } else {
        Err(issues)
    }
}

fn read_option(option: &Map<String, Value>, path: &str, issues: &mut Vec<String>) -> Value {
    let mut clean = Map::new();
    match option.get("label") {
        Some(Value::String(label)) if label.is_empty() => {
            issues.push(issue(&format!("{path}.label"), TOO_SMALL_ONE));
        }
        Some(Value::String(label)) => {
            clean.insert("label".to_string(), Value::String(label.clone()));
        }
        Some(other) => issues.push(issue(
            &format!("{path}.label"),
            &invalid_type("string", Some(other)),
        )),
        None => issues.push(issue(&format!("{path}.label"), MISSING_STRING)),
    }
    match option.get("value") {
        None => {}
        Some(Value::String(value)) if value.is_empty() => {
            issues.push(issue(&format!("{path}.value"), TOO_SMALL_ONE));
        }
        Some(Value::String(value)) => {
            clean.insert("value".to_string(), Value::String(value.clone()));
        }
        Some(other) => issues.push(issue(
            &format!("{path}.value"),
            &invalid_type("string", Some(other)),
        )),
    }
    if let Some(description) = option.get("description") {
        match description {
            Value::String(_) => {
                clean.insert("description".to_string(), description.clone());
            }
            other => issues.push(issue(
                &format!("{path}.description"),
                &invalid_type("string", Some(other)),
            )),
        }
    }
    Value::Object(clean)
}

fn run(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(ask(input, host))
}

async fn ask(input: Value, host: &dyn ToolHost) -> ToolOutcome {
    let options = input
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let values: Vec<String> = options.iter().map(util::option_value).collect();
    if util::unique_count(&values) != values.len() {
        return util::fail("Invalid ask: option values must be unique.");
    }

    let answer = match host.request(json!({ "kind": "ask", "input": input })) {
        Ok(receiver) => match receiver.await {
            Ok(Ok(answer)) => answer,
            _ => return util::fail(util::CANCELLED),
        },
        Err(Cancelled) => return util::fail(util::CANCELLED),
    };

    let selected = answer
        .get("selected")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|value| util::label_of(&options, value))
                .collect::<Vec<Value>>()
        })
        .unwrap_or_default();
    let mut out = Map::new();
    out.insert("selected".to_string(), Value::Array(selected));
    if js::truthy(answer.get("other")) {
        out.insert("other".to_string(), answer["other"].clone());
    }
    util::json(&Value::Object(out))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::fake::FakeHost;

    fn host() -> FakeHost {
        FakeHost::new(json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false}))
    }

    #[tokio::test]
    async fn maps_values_to_labels_and_passes_other() {
        let host = host();
        host.queue_answer(json!({"selected": ["v"], "other": "altro"}));
        let out = host
            .call(
                &TOOL,
                json!({"question": "?", "options": [{"label": "Uno", "value": "v"}, {"label": "Due"}]}),
            )
            .await;
        assert_eq!(out, ToolOutcome::ok(r#"{"selected":["Uno"],"other":"altro"}"#));
        assert_eq!(
            serde_json::to_string(&host.requests()[0]).unwrap(),
            r#"{"kind":"ask","input":{"question":"?","options":[{"label":"Uno","value":"v"},{"label":"Due"}]}}"#
        );

        // A value no option has comes back unchanged.
        host.queue_answer(json!({"selected": ["zzz"]}));
        let out = host
            .call(&TOOL, json!({"question": "?", "options": [{"label": "Uno", "value": "v"}]}))
            .await;
        assert_eq!(out, ToolOutcome::ok(r#"{"selected":["zzz"]}"#));
    }

    #[tokio::test]
    async fn duplicate_option_values_are_rejected_before_the_card() {
        let host = host();
        let out = host
            .call(&TOOL, json!({"question": "?", "options": [{"label": "Uno"}, {"label": "Uno"}]}))
            .await;
        assert_eq!(out, ToolOutcome::error("Invalid ask: option values must be unique."));
        assert!(host.requests().is_empty());
    }

    #[tokio::test]
    async fn a_cancelled_card_is_an_error_result() {
        let host = host();
        host.queue_cancel();
        let out = host.call(&TOOL, json!({"question": "?"})).await;
        assert_eq!(out, ToolOutcome::error(util::CANCELLED));
    }

    #[test]
    fn invalid_input_uses_zods_text() {
        assert_eq!(
            (TOOL.validate)(&json!({})),
            Err(vec![
                "question: Invalid input: expected string, received undefined".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&json!({"question": ""})),
            Err(vec![
                "question: Too small: expected string to have >=1 characters".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&json!({"question": "x", "options": [{"label": "a", "value": ""}]})),
            Err(vec![
                "options.0.value: Too small: expected string to have >=1 characters".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&json!({"question": "x", "multiSelect": "yes"})),
            Err(vec![
                "multiSelect: Invalid input: expected boolean, received string".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&Value::Null),
            Err(vec![
                "(input): Invalid input: expected object, received null".to_string(),
            ])
        );
    }

    #[test]
    fn the_validator_drops_unknown_keys_and_keeps_schema_order() {
        let data = (TOOL.validate)(&json!({
            "multiSelect": true,
            "options": [{"description": "d", "value": "v", "label": "Uno"}],
            "details": "d",
            "question": "x",
            "extra": 1,
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"question":"x","details":"d","options":[{"label":"Uno","value":"v","description":"d"}],"multiSelect":true}"#
        );
    }

    #[test]
    fn the_schema_file_is_the_tool_schema() {
        let file: Value = serde_json::from_str(include_str!("schemas/ask.json")).expect("parses");
        assert_eq!(
            serde_json::to_string(&(TOOL.schema)()).unwrap(),
            serde_json::to_string(&file).unwrap()
        );
        assert_eq!(file["required"], json!(["question"]));
    }
}
