//! The `quiz` tool and its grading (port of app/server/tools/quiz.ts).

use serde_json::{Map, Value, json};

use crate::js;
use crate::lesson::artifact;
use crate::sessions::{invalid_type, issue};
use crate::tools::util;
use crate::tools::{Cancelled, ToolDef, ToolFuture, ToolHost, ToolOutcome};

/// Node's `quizTool`.
pub const TOOL: ToolDef = ToolDef {
    name: "quiz",
    description: "Ask the learner one graded multiple-choice question as a clickable card; blocks until answered and returns the graded result with his confidence and optional note. Use for probes, predictions and checks.",
    schema,
    validate,
    run,
    writes: false,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("schemas/quiz.json")).expect("the quiz schema parses")
}

const TOO_SMALL_ONE: &str = "Too small: expected string to have >=1 characters";
const MISSING_STRING: &str = "Invalid input: expected string, received undefined";
const BAD_KIND: &str = "Invalid option: expected one of \"probe\"|\"predict\"|\"check\"";

/// Node's `quizSchema`, hand-rolled: the cleaned data or one `"<path>: <message>"` per issue.
fn validate(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    match object.get("kind") {
        Some(Value::String(kind)) if matches!(kind.as_str(), "probe" | "predict" | "check") => {
            data.insert("kind".to_string(), Value::String(kind.clone()));
        }
        _ => issues.push(issue("kind", BAD_KIND)),
    }

    if let Some(node_id) = object.get("nodeId") {
        match node_id {
            Value::String(value) if is_plan_node(value) => {
                data.insert("nodeId".to_string(), node_id.clone());
            }
            Value::String(_) => issues.push(issue(
                "nodeId",
                "Invalid string: must match pattern /^N\\d+$/",
            )),
            other => issues.push(issue("nodeId", &invalid_type("string", Some(other)))),
        }
    }

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

    if let Some(options) = object.get("options") {
        match options.as_array() {
            None => issues.push(issue("options", &invalid_type("array", Some(options)))),
            Some(items) if items.len() > 6 => {
                issues.push(issue("options", "Too big: expected array to have <=6 items"));
            }
            Some(items) => {
                let mut clean = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    let path = format!("options.{index}");
                    match item.as_object() {
                        None => issues.push(issue(&path, &invalid_type("object", Some(item)))),
                        Some(option) => clean.push(read_option(option, &path, &mut issues)),
                    }
                }
                // zod checks the items, then the minimum.
                if items.len() < 2 {
                    issues.push(issue("options", "Too small: expected array to have >=2 items"));
                }
                data.insert("options".to_string(), Value::Array(clean));
            }
        }
    } else {
        issues.push(issue("options", &invalid_type("array", None)));
    }

    if let Some(multi_select) = object.get("multiSelect") {
        match multi_select {
            Value::Bool(_) => {
                data.insert("multiSelect".to_string(), multi_select.clone());
            }
            other => issues.push(issue("multiSelect", &invalid_type("boolean", Some(other)))),
        }
    }

    match object.get("explanation") {
        Some(Value::String(explanation)) if explanation.is_empty() => {
            issues.push(issue("explanation", TOO_SMALL_ONE));
        }
        Some(Value::String(explanation)) => {
            data.insert("explanation".to_string(), Value::String(explanation.clone()));
        }
        Some(other) => issues.push(issue("explanation", &invalid_type("string", Some(other)))),
        None => issues.push(issue("explanation", MISSING_STRING)),
    }

    match object.get("correctAnswer") {
        Some(Value::String(_)) => {
            data.insert("correctAnswer".to_string(), object["correctAnswer"].clone());
        }
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            data.insert("correctAnswer".to_string(), object["correctAnswer"].clone());
        }
        // zod's union reports one "Invalid input" for every failed branch together.
        _ => issues.push(issue("correctAnswer", "Invalid input")),
    }

    if let Some(misconceptions) = object.get("misconceptions") {
        match misconceptions.as_array() {
            None => issues.push(issue(
                "misconceptions",
                &invalid_type("array", Some(misconceptions)),
            )),
            Some(items) => {
                let mut clean = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    let path = format!("misconceptions.{index}");
                    match item.as_object() {
                        None => issues.push(issue(&path, &invalid_type("object", Some(item)))),
                        Some(misconception) => {
                            clean.push(read_misconception(misconception, &path, &mut issues))
                        }
                    }
                }
                data.insert("misconceptions".to_string(), Value::Array(clean));
            }
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
    Value::Object(clean)
}

fn read_misconception(
    item: &Map<String, Value>,
    path: &str,
    issues: &mut Vec<String>,
) -> Value {
    let mut clean = Map::new();
    for key in ["value", "misconception"] {
        match item.get(key) {
            Some(Value::String(_)) => {
                clean.insert(key.to_string(), item[key].clone());
            }
            Some(other) => issues.push(issue(
                &format!("{path}.{key}"),
                &invalid_type("string", Some(other)),
            )),
            None => issues.push(issue(&format!("{path}.{key}"), MISSING_STRING)),
        }
    }
    Value::Object(clean)
}

/// `^N\d+$`, JS `\d` being ASCII digits.
fn is_plan_node(text: &str) -> bool {
    match text.strip_prefix('N') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        None => false,
    }
}

/// Node's `validate`: the semantic checks the schema cannot express, in the same order.
fn validate_quiz(input: &Value, dag: Option<&str>) -> Vec<String> {
    let mut errs = Vec::new();
    let options = option_list(input);
    let values: Vec<String> = options.iter().map(util::option_value).collect();
    if util::unique_count(&values) != values.len() {
        errs.push("option values must be unique".to_string());
    }
    let correct = as_strings(input.get("correctAnswer"));
    for value in &correct {
        if !values.contains(value) {
            errs.push(format!("correctAnswer \"{value}\" is not an option value"));
        }
    }
    if util::unique_count(&correct) > 1 && !js::truthy(input.get("multiSelect")) {
        errs.push("more than one correct answer requires multiSelect: true".to_string());
    }
    for misconception in input
        .get("misconceptions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let value = js::js_string(misconception.get("value"));
        if !values.contains(&value) {
            errs.push(format!(
                "misconception value \"{value}\" is not an option value"
            ));
        }
    }
    let kind = js::js_string(input.get("kind"));
    let node_id = input.get("nodeId").and_then(Value::as_str);
    if matches!(kind.as_str(), "predict" | "check") && node_id.is_none() {
        errs.push(format!("kind \"{kind}\" requires nodeId"));
    }
    if let (Some(node_id), Some(dag)) = (node_id, dag) {
        let ids = artifact::parse_dag_ids(dag);
        if !ids.iter().any(|id| id == node_id) {
            errs.push(format!(
                "nodeId {node_id} is not in the approved plan ({})",
                ids.join(", ")
            ));
        }
    }
    errs
}

/// Fisher-Yates: `displayOrder` for the card, a permutation of `0..n`.
pub fn shuffled(n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        let j = draw(i + 1);
        order.swap(i, j);
    }
    order
}

/// `Math.floor(Math.random() * bound)`.
fn draw(bound: usize) -> usize {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("system randomness");
    (u32::from_le_bytes(bytes) as usize) % bound
}

fn run(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(quiz(input, host))
}

async fn quiz(input: Value, host: &dyn ToolHost) -> ToolOutcome {
    let plan = host.plan();
    let dag = if plan.is_null() {
        None
    } else {
        plan.get("dag").and_then(Value::as_str).map(str::to_string)
    };
    let errs = validate_quiz(&input, dag.as_deref());
    if !errs.is_empty() {
        return util::fail(format!("Invalid quiz: {}. Fix and call again.", errs.join("; ")));
    }

    let options = option_list(&input);
    let display_order = Value::Array(shuffled(options.len()).into_iter().map(|i| json!(i)).collect());
    let answer = match host.request(json!({
        "kind": "quiz",
        "input": input.clone(),
        "displayOrder": display_order,
    })) {
        Ok(receiver) => match receiver.await {
            Ok(Ok(answer)) => answer,
            _ => return util::fail(util::CANCELLED),
        },
        Err(Cancelled) => return util::fail(util::CANCELLED),
    };

    let result = util::grade_quiz(&input, &answer);
    if let Err(message) = log_progress(&input, &result, host) {
        return util::fail(message);
    }
    util::recompute_plan(host);

    let mut out = Map::new();
    out.insert("correct".to_string(), result["correct"].clone());
    if let Some(dont_know) = result.get("dontKnow") {
        out.insert("dontKnow".to_string(), dont_know.clone());
    }
    out.insert(
        "selected".to_string(),
        Value::Array(labels(&options, result.get("selected"))),
    );
    if let Some(confidence) = result.get("confidence") {
        out.insert("confidence".to_string(), confidence.clone());
    }
    if js::truthy(result.get("note")) {
        out.insert("note".to_string(), result["note"].clone());
    }
    if js::truthy(answer.get("thinkingWhileReading")) {
        out.insert(
            "thinkingWhileReading".to_string(),
            answer["thinkingWhileReading"].clone(),
        );
    }
    if result.get("correct") != Some(&Value::Bool(true)) {
        let correct = as_strings(input.get("correctAnswer"))
            .iter()
            .map(|value| util::label_of(&options, &Value::String(value.clone())))
            .collect();
        out.insert("correctAnswer".to_string(), Value::Array(correct));
    }
    util::json(&Value::Object(out))
}

/// The progress record Node writes after a grade; absent values are left out, as JSON.stringify does.
/// An append failure rejects the call, like Node's `appendFileSync`.
fn log_progress(input: &Value, result: &Value, host: &dyn ToolHost) -> Result<(), String> {
    let mut record = Map::new();
    if let Some(path) = host.lesson_path() {
        record.insert(
            "lessonSlug".to_string(),
            json!(artifact::slug_of_lesson(&path)),
        );
    }
    let node_refs = match input.get("nodeId") {
        Some(node) => vec![node.clone()],
        None => Vec::new(),
    };
    record.insert("nodeRefs".to_string(), Value::Array(node_refs));
    record.insert("kind".to_string(), input["kind"].clone());
    record.insert("question".to_string(), input["question"].clone());
    record.insert("correct".to_string(), result["correct"].clone());
    if let Some(dont_know) = result.get("dontKnow") {
        record.insert("dontKnow".to_string(), dont_know.clone());
    }
    if let Some(confidence) = result.get("confidence") {
        record.insert("confidence".to_string(), confidence.clone());
    }
    host.log_progress(record)
}

fn option_list(input: &Value) -> Vec<Value> {
    input
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The labels of the selected values; a value with no option comes back unchanged.
fn labels(options: &[Value], selected: Option<&Value>) -> Vec<Value> {
    selected
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|value| util::label_of(options, value))
                .collect()
        })
        .unwrap_or_default()
}

/// Node's `asArray`: one answer or several, always a list.
fn as_strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(|item| js::js_string(Some(item))).collect(),
        Some(Value::String(text)) => vec![text.clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::fake::FakeHost;

    const DAG: &str = "graph TD\n  N1[\"Nodo 1: la prima cosa è vera\"] --> N2[\"Nodo 2: la seconda segue\"]";

    fn config() -> Value {
        json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false})
    }

    fn quiz_in() -> Value {
        json!({
            "kind": "probe",
            "question": "Q?",
            "options": [{"label": "A"}, {"label": "B"}, {"label": "C", "value": "c"}],
            "explanation": "because",
            "correctAnswer": "B",
        })
    }

    /// `{...base, ...patch}`, keeping each key where the base put it.
    fn with(mut base: Value, patch: Value) -> Value {
        let object = base.as_object_mut().expect("a quiz object");
        for (key, value) in patch.as_object().expect("a patch object") {
            object.insert(key.clone(), value.clone());
        }
        base
    }

    #[test]
    fn grade_quiz_marks_only_the_exact_set() {
        let quiz = json!({
            "kind": "check",
            "question": "",
            "options": [],
            "explanation": "",
            "correctAnswer": ["a", "b"],
            "multiSelect": true,
        });
        assert_eq!(
            util::grade_quiz(&quiz, &json!({"selected": ["b", "a"], "dontKnow": false}))["correct"],
            json!(true)
        );
        assert_eq!(
            util::grade_quiz(&quiz, &json!({"selected": ["a"], "dontKnow": false}))["correct"],
            json!(false)
        );
        assert_eq!(
            util::grade_quiz(&quiz, &json!({"selected": ["a", "b", "c"], "dontKnow": false}))["correct"],
            json!(false)
        );
        assert_eq!(
            util::grade_quiz(&quiz, &json!({"selected": [], "dontKnow": true}))["correct"],
            json!(false)
        );
    }

    #[tokio::test]
    async fn grades_logs_progress_and_returns_labels() {
        let host = FakeHost::new(config());
        host.queue_answer(json!({"selected": ["c"], "dontKnow": false, "confidence": 80, "note": "boh"}));
        let out = host.call(&TOOL, quiz_in()).await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            out.text,
            r#"{"correct":false,"dontKnow":false,"selected":["C"],"confidence":80,"note":"boh","correctAnswer":["B"]}"#
        );

        let requests = host.requests();
        assert!(
            serde_json::to_string(&requests[0]).unwrap().starts_with(
                r#"{"kind":"quiz","input":{"kind":"probe","question":"Q?","options":[{"label":"A"},{"label":"B"},{"label":"C","value":"c"}],"explanation":"because","correctAnswer":"B"},"displayOrder":["#
            ),
            "{}",
            serde_json::to_string(&requests[0]).unwrap()
        );
        let mut order: Vec<i64> = requests[0]["displayOrder"]
            .as_array()
            .expect("a display order")
            .iter()
            .map(|value| value.as_i64().expect("an index"))
            .collect();
        order.sort();
        assert_eq!(order, vec![0, 1, 2]);

        let progress = host.progress();
        assert_eq!(
            serde_json::to_string(&progress[0]).unwrap(),
            r#"{"nodeRefs":[],"kind":"probe","question":"Q?","correct":false,"dontKnow":false,"confidence":80}"#
        );
    }

    #[tokio::test]
    async fn a_failed_progress_append_fails_the_call() {
        let host = FakeHost::new(config());
        // Node's `appendFileSync` creates the file but not the folder: with the course gone the
        // awaited `logProgress` rejects, so the quiz call is an error, not a silent success.
        std::fs::remove_dir_all(host.data_root().join("courses").join("demo"))
            .expect("remove the course folder");
        host.queue_answer(json!({"selected": ["B"], "dontKnow": false}));

        let out = host.call(&TOOL, quiz_in()).await;

        assert!(out.is_error, "{}", out.text);
        assert!(out.text.contains("os error"), "{}", out.text);
    }

    #[tokio::test]
    async fn a_correct_answer_omits_correct_answer() {
        let host = FakeHost::new(config());
        host.queue_answer(json!({"selected": ["B"], "dontKnow": false, "confidence": 90}));
        let out = host.call(&TOOL, quiz_in()).await;
        let value: Value = serde_json::from_str(&out.text).expect("JSON");
        assert_eq!(value["correct"], json!(true));
        assert!(!value.as_object().expect("an object").contains_key("correctAnswer"));
        assert_eq!(value["confidence"], json!(90));
    }

    #[tokio::test]
    async fn passes_thinking_while_reading_through() {
        let host = FakeHost::new(config());
        host.queue_answer(json!({
            "selected": ["B"],
            "dontKnow": false,
            "thinkingWhileReading": "Pensavo fosse A ma poi B",
        }));
        let out = host.call(&TOOL, quiz_in()).await;
        let value: Value = serde_json::from_str(&out.text).expect("JSON");
        assert_eq!(value["thinkingWhileReading"], json!("Pensavo fosse A ma poi B"));
    }

    #[tokio::test]
    async fn dont_know_is_reported_and_not_correct() {
        let host = FakeHost::new(config());
        host.queue_answer(json!({"selected": [], "dontKnow": true}));
        let out = host.call(&TOOL, quiz_in()).await;
        let value: Value = serde_json::from_str(&out.text).expect("JSON");
        assert_eq!(
            [&value["correct"], &value["dontKnow"], &value["selected"]],
            [&json!(false), &json!(true), &json!([])]
        );
    }

    #[tokio::test]
    async fn rejects_bad_quizzes_before_asking_the_learner() {
        let host = FakeHost::new(config());
        for bad in [
            with(quiz_in(), json!({"correctAnswer": "zzz"})),
            with(quiz_in(), json!({"options": [{"label": "A"}, {"label": "A"}]})),
            with(quiz_in(), json!({"correctAnswer": ["A", "B"]})),
            with(quiz_in(), json!({"kind": "check"})),
            with(quiz_in(), json!({"options": [{"label": "A"}]})),
            with(quiz_in(), json!({"misconceptions": [{"value": "zz", "misconception": "m"}]})),
        ] {
            let out = host.call(&TOOL, bad.clone()).await;
            assert!(out.is_error, "{}", serde_json::to_string(&bad).unwrap());
        }
        assert!(host.requests().is_empty());
    }

    #[tokio::test]
    async fn node_id_must_be_in_the_approved_plan() {
        let host = FakeHost::with_lesson(config(), DAG);
        assert_eq!(host.plan()["nodeStatus"]["N1"], json!("current"));

        let bad = host
            .call(&TOOL, with(quiz_in(), json!({"kind": "check", "nodeId": "N7"})))
            .await;
        assert!(bad.is_error, "{}", bad.text);
        assert!(bad.text.contains("not in the approved plan"), "{}", bad.text);

        host.queue_answer(json!({"selected": ["A"], "dontKnow": false}));
        let ok = host
            .call(&TOOL, with(quiz_in(), json!({"kind": "check", "nodeId": "N1"})))
            .await;
        assert!(!ok.is_error, "{}", ok.text);
        assert_eq!(host.progress()[0]["nodeRefs"], json!(["N1"]));
        assert_eq!(host.plan()["nodeStatus"]["N1"], json!("missed"));
    }

    #[tokio::test]
    async fn a_cancelled_card_is_an_error_result() {
        let host = FakeHost::new(config());
        host.queue_cancel();
        let out = host.call(&TOOL, quiz_in()).await;
        assert_eq!(out, ToolOutcome::error(util::CANCELLED));
    }

    #[test]
    fn shuffled_is_a_permutation() {
        for n in 2..=6 {
            let mut order = shuffled(n);
            assert_eq!(order.len(), n);
            order.sort();
            assert_eq!(order, (0..n).collect::<Vec<usize>>());
        }
        assert!(shuffled(0).is_empty());
        assert_eq!(shuffled(1), vec![0]);
    }

    #[test]
    fn invalid_input_uses_zods_text() {
        assert_eq!(
            (TOOL.validate)(&json!({})),
            Err(vec![
                r#"kind: Invalid option: expected one of "probe"|"predict"|"check""#.to_string(),
                "question: Invalid input: expected string, received undefined".to_string(),
                "options: Invalid input: expected array, received undefined".to_string(),
                "explanation: Invalid input: expected string, received undefined".to_string(),
                "correctAnswer: Invalid input".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&with(quiz_in(), json!({"nodeId": "n1"}))),
            Err(vec![
                r#"nodeId: Invalid string: must match pattern /^N\d+$/"#.to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&with(quiz_in(), json!({"options": [{"label": "A"}]}))),
            Err(vec![
                "options: Too small: expected array to have >=2 items".to_string(),
            ])
        );
    }

    #[test]
    fn the_validator_drops_unknown_keys_and_keeps_schema_order() {
        let data = (TOOL.validate)(&json!({
            "correctAnswer": "B",
            "extra": 1,
            "explanation": "e",
            "options": [{"value": "a", "label": "A"}, {"label": "B"}],
            "question": "Q?",
            "kind": "probe",
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"kind":"probe","question":"Q?","options":[{"label":"A","value":"a"},{"label":"B"}],"explanation":"e","correctAnswer":"B"}"#
        );
    }

    #[test]
    fn the_schema_file_is_the_tool_schema() {
        let file: Value = serde_json::from_str(include_str!("schemas/quiz.json")).expect("parses");
        assert_eq!(
            serde_json::to_string(&(TOOL.schema)()).unwrap(),
            serde_json::to_string(&file).unwrap()
        );
        assert_eq!(
            file["required"],
            json!(["kind", "question", "options", "explanation", "correctAnswer"])
        );
    }
}
