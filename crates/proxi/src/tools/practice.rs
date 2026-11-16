//! The `problem` and `conclude` tools (port of app/server/tools/practice.ts).
//!
//! The open problem lives on the session (`ToolHost::open_problem`), not in a per-call context:
//! each `/mcp` POST builds a new host and the server restart forgets it, like Node's `WeakMap`.

use serde_json::{Map, Value, json};

use crate::sessions::{invalid_type, issue};
use crate::tools::util;
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolOutcome};

/// Node's `problemTool`.
pub const PROBLEM: ToolDef = ToolDef {
    name: "problem",
    description: "Show the learner a practice problem card. Non-blocking: coach his attempt in chat, then call conclude. Only one problem may be open.",
    schema: problem_schema,
    validate: validate_problem,
    run: run_problem,
    writes: false,
};

/// Node's `concludeTool`.
pub const CONCLUDE: ToolDef = ToolDef {
    name: "conclude",
    description: "Close the open practice problem with the outcome, highest hint rung, miss class and whether to retest. Free practice: nothing is recorded toward mastery.",
    schema: conclude_schema,
    validate: validate_conclude,
    run: run_conclude,
    writes: false,
};

fn problem_schema() -> Value {
    serde_json::from_str(include_str!("schemas/problem.json")).expect("the problem schema parses")
}

fn conclude_schema() -> Value {
    serde_json::from_str(include_str!("schemas/conclude.json")).expect("the conclude schema parses")
}

const TOO_SMALL_ONE: &str = "Too small: expected string to have >=1 characters";
const MISSING_STRING: &str = "Invalid input: expected string, received undefined";
const TOO_SMALL_ONE_ITEM: &str = "Too small: expected array to have >=1 items";
const BAD_MODE: &str =
    "Invalid option: expected one of \"recall\"|\"discrimination\"|\"composition\"";
const BAD_OUTCOME: &str =
    "Invalid option: expected one of \"solved\"|\"solvedWithHints\"|\"movedOn\"|\"surrendered\"";
const BAD_RUNG: &str = "Invalid option: expected one of \"R0\"|\"R1\"|\"R2\"|\"R3\"|\"R4\"|\"R5\"";
const BAD_MISS_CLASS: &str = "Invalid option: expected one of \"cue\"|\"discrimination\"|\"execution\"|\"careless\"|\"structural\"";

/// Node's `problemSchema`, hand-rolled: the cleaned data or one `"<path>: <message>"` per issue.
fn validate_problem(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    match object.get("statement") {
        Some(Value::String(statement)) if statement.is_empty() => {
            issues.push(issue("statement", TOO_SMALL_ONE));
        }
        Some(Value::String(statement)) => {
            data.insert("statement".to_string(), Value::String(statement.clone()));
        }
        Some(other) => issues.push(issue("statement", &invalid_type("string", Some(other)))),
        None => issues.push(issue("statement", MISSING_STRING)),
    }

    match object.get("mode") {
        Some(Value::String(mode))
            if matches!(mode.as_str(), "recall" | "discrimination" | "composition") =>
        {
            data.insert("mode".to_string(), Value::String(mode.clone()));
        }
        _ => issues.push(issue("mode", BAD_MODE)),
    }

    match object.get("nodeKeys") {
        None => issues.push(issue("nodeKeys", &invalid_type("array", None))),
        Some(keys) => match keys.as_array() {
            None => issues.push(issue("nodeKeys", &invalid_type("array", Some(keys)))),
            Some(items) => {
                let mut clean = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    match item {
                        Value::String(_) => clean.push(item.clone()),
                        other => issues.push(issue(
                            &format!("nodeKeys.{index}"),
                            &invalid_type("string", Some(other)),
                        )),
                    }
                }
                // zod checks the items, then the minimum.
                if items.is_empty() {
                    issues.push(issue("nodeKeys", TOO_SMALL_ONE_ITEM));
                }
                data.insert("nodeKeys".to_string(), Value::Array(clean));
            }
        },
    }

    match object.get("modelSolution") {
        Some(Value::String(solution)) if solution.is_empty() => {
            issues.push(issue("modelSolution", TOO_SMALL_ONE));
        }
        Some(Value::String(solution)) => {
            data.insert("modelSolution".to_string(), Value::String(solution.clone()));
        }
        Some(other) => issues.push(issue("modelSolution", &invalid_type("string", Some(other)))),
        None => issues.push(issue("modelSolution", MISSING_STRING)),
    }

    if issues.is_empty() {
        Ok(Value::Object(data))
    } else {
        Err(issues)
    }
}

/// Node's `concludeSchema`, hand-rolled: `rung` and `missClass` are nullable but required.
fn validate_conclude(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    match object.get("outcome") {
        Some(Value::String(outcome))
            if matches!(
                outcome.as_str(),
                "solved" | "solvedWithHints" | "movedOn" | "surrendered"
            ) =>
        {
            data.insert("outcome".to_string(), Value::String(outcome.clone()));
        }
        _ => issues.push(issue("outcome", BAD_OUTCOME)),
    }

    match object.get("verdict") {
        Some(Value::String(verdict)) if verdict.is_empty() => {
            issues.push(issue("verdict", TOO_SMALL_ONE));
        }
        Some(Value::String(verdict)) => {
            data.insert("verdict".to_string(), Value::String(verdict.clone()));
        }
        Some(other) => issues.push(issue("verdict", &invalid_type("string", Some(other)))),
        None => issues.push(issue("verdict", MISSING_STRING)),
    }

    match object.get("rung") {
        Some(Value::String(rung))
            if matches!(rung.as_str(), "R0" | "R1" | "R2" | "R3" | "R4" | "R5") =>
        {
            data.insert("rung".to_string(), Value::String(rung.clone()));
        }
        Some(Value::Null) => {
            data.insert("rung".to_string(), Value::Null);
        }
        _ => issues.push(issue("rung", BAD_RUNG)),
    }

    match object.get("missClass") {
        Some(Value::String(miss))
            if matches!(
                miss.as_str(),
                "cue" | "discrimination" | "execution" | "careless" | "structural"
            ) =>
        {
            data.insert("missClass".to_string(), Value::String(miss.clone()));
        }
        Some(Value::Null) => {
            data.insert("missClass".to_string(), Value::Null);
        }
        _ => issues.push(issue("missClass", BAD_MISS_CLASS)),
    }

    match object.get("retest") {
        Some(Value::Bool(_)) => {
            data.insert("retest".to_string(), object["retest"].clone());
        }
        other => issues.push(issue("retest", &invalid_type("boolean", other))),
    }

    if issues.is_empty() {
        Ok(Value::Object(data))
    } else {
        Err(issues)
    }
}

fn run_problem(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(async move {
        // `open_problem` checks and remembers under one session lock.
        match host.open_problem(json!({ "kind": "problem", "input": input })) {
            Some(_) => ToolOutcome::ok(
                "Problem shown. Wait for the learner's attempt in chat; coach with the hint ladder; call conclude when done.",
            ),
            None => util::fail(
                "A problem is already open: coach it to the end and call conclude before opening another.",
            ),
        }
    })
}

fn run_conclude(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(async move {
        let Some((id, problem)) = host.take_problem() else {
            return util::fail("No open problem: call problem first.");
        };
        host.emit(json!({ "id": id, "kind": "problem", "input": problem, "conclusion": input }));
        ToolOutcome::ok("Concluded.")
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::tools::fake::FakeHost;

    fn practice() -> FakeHost {
        FakeHost::new(json!({"kind": "practice", "course": "demo", "model": "haiku"}))
    }

    fn problem() -> Value {
        json!({
            "statement": "S",
            "mode": "recall",
            "nodeKeys": ["demo/x#n1"],
            "modelSolution": "M",
        })
    }

    fn conclusion() -> Value {
        json!({
            "outcome": "solvedWithHints",
            "verdict": "ok",
            "rung": "R2",
            "missClass": null,
            "retest": false,
        })
    }

    #[tokio::test]
    async fn one_open_problem_at_a_time_and_conclude_records_nothing() {
        let host = practice();
        let progress_file = host.data_root().join("courses/demo/progress.jsonl");
        fs::create_dir_all(progress_file.parent().expect("a parent")).expect("create the course");
        fs::write(
            &progress_file,
            "{\"kind\":\"check\",\"nodeRefs\":[\"demo/seed\"],\"correct\":true}\n",
        )
        .expect("seed the progress file");
        let initial = fs::read(&progress_file).expect("read the seeded file");

        assert_eq!(
            host.call(&CONCLUDE, conclusion()).await,
            ToolOutcome::error("No open problem: call problem first.")
        );
        assert_eq!(
            host.call(&PROBLEM, problem()).await,
            ToolOutcome::ok(
                "Problem shown. Wait for the learner's attempt in chat; coach with the hint ladder; call conclude when done."
            )
        );
        assert_eq!(
            host.call(&PROBLEM, problem()).await,
            ToolOutcome::error(
                "A problem is already open: coach it to the end and call conclude before opening another."
            )
        );
        assert_eq!(
            host.call(&CONCLUDE, conclusion()).await,
            ToolOutcome::ok("Concluded.")
        );

        let items = host.items();
        assert_eq!(items[1]["id"], json!("item-1"));
        assert_eq!(items[1]["input"], problem());
        assert_eq!(items[1]["conclusion"]["outcome"], json!("solvedWithHints"));

        assert!(host.progress().is_empty());
        assert_eq!(fs::read(&progress_file).expect("read the file"), initial);

        // The concluded problem is forgotten: a new one opens.
        assert!(!host.call(&PROBLEM, problem()).await.is_error);
    }

    #[test]
    fn invalid_problem_uses_zods_text() {
        assert_eq!(
            (PROBLEM.validate)(&json!({"statement": "S", "mode": "guess", "nodeKeys": ["a"], "modelSolution": "M"})),
            Err(vec![
                "mode: Invalid option: expected one of \"recall\"|\"discrimination\"|\"composition\""
                    .to_string(),
            ])
        );
        assert_eq!(
            (PROBLEM.validate)(
                &json!({"statement": "S", "mode": "recall", "nodeKeys": [], "modelSolution": "M"})
            ),
            Err(vec![
                "nodeKeys: Too small: expected array to have >=1 items".to_string()
            ])
        );
        assert_eq!(
            (PROBLEM.validate)(
                &json!({"statement": "S", "mode": "recall", "nodeKeys": [1], "modelSolution": "M"})
            ),
            Err(vec![
                "nodeKeys.0: Invalid input: expected string, received number".to_string()
            ])
        );
        assert_eq!(
            (PROBLEM.validate)(&json!({})),
            Err(vec![
                "statement: Invalid input: expected string, received undefined".to_string(),
                "mode: Invalid option: expected one of \"recall\"|\"discrimination\"|\"composition\""
                    .to_string(),
                "nodeKeys: Invalid input: expected array, received undefined".to_string(),
                "modelSolution: Invalid input: expected string, received undefined".to_string(),
            ])
        );
    }

    #[test]
    fn invalid_conclude_uses_zods_text() {
        let missing_retest =
            json!({"outcome": "solved", "verdict": "ok", "rung": null, "missClass": null});
        assert_eq!(
            (CONCLUDE.validate)(&missing_retest),
            Err(vec![
                "retest: Invalid input: expected boolean, received undefined".to_string(),
            ])
        );
        assert_eq!(
            (CONCLUDE.validate)(&json!({"outcome": "won", "verdict": "ok", "rung": "R9", "missClass": "cue", "retest": false})),
            Err(vec![
                "outcome: Invalid option: expected one of \"solved\"|\"solvedWithHints\"|\"movedOn\"|\"surrendered\"".to_string(),
                "rung: Invalid option: expected one of \"R0\"|\"R1\"|\"R2\"|\"R3\"|\"R4\"|\"R5\"".to_string(),
            ])
        );
    }

    #[test]
    fn the_validators_keep_schema_order_and_drop_unknown_keys() {
        let data = (PROBLEM.validate)(&json!({
            "modelSolution": "M",
            "nodeKeys": ["a"],
            "mode": "composition",
            "statement": "S",
            "extra": 1,
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"statement":"S","mode":"composition","nodeKeys":["a"],"modelSolution":"M"}"#
        );

        let data = (CONCLUDE.validate)(&json!({
            "retest": true,
            "missClass": null,
            "rung": "R4",
            "verdict": "ok",
            "outcome": "movedOn",
            "extra": 1,
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"outcome":"movedOn","verdict":"ok","rung":"R4","missClass":null,"retest":true}"#
        );
    }

    #[test]
    fn the_schema_files_are_the_tool_schemas() {
        for (tool, file) in [
            (&PROBLEM, include_str!("schemas/problem.json")),
            (&CONCLUDE, include_str!("schemas/conclude.json")),
        ] {
            let file: Value = serde_json::from_str(file).expect("the schema file parses");
            assert_eq!(
                serde_json::to_string(&(tool.schema)()).unwrap(),
                serde_json::to_string(&file).unwrap()
            );
        }
        assert_eq!(
            (PROBLEM.schema)()["required"],
            json!(["statement", "mode", "nodeKeys", "modelSolution"])
        );
        assert_eq!(
            (CONCLUDE.schema)()["required"],
            json!(["outcome", "verdict", "rung", "missClass", "retest"])
        );
    }
}
