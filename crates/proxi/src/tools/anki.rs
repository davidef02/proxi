//! The `sync_anki` tool (port of app/server/tools/anki.ts).

use serde_json::{Map, Value, json};

use crate::js;
use crate::sessions::{invalid_type, issue};
use crate::tools::finish;
use crate::tools::util::{self, fail};
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolOutcome};

pub const TOOL: ToolDef = ToolDef {
    name: "sync_anki",
    description: "Sync the lesson's Anki cards (only exists when the learner opted in). Safe to call repeatedly: duplicates are skipped.",
    schema,
    validate,
    run,
    writes: false,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("schemas/sync_anki.json")).expect("the sync_anki schema")
}

/// Node's `z.object({})`: any object parses, its keys are stripped; only a non-object is an issue.
fn validate(input: &Value) -> Result<Value, Vec<String>> {
    match input.as_object() {
        Some(_) => Ok(Value::Object(Map::new())),
        None => Err(vec![issue(
            "(input)",
            &invalid_type("object", Some(input)),
        )]),
    }
}

fn run(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(run_tool(input, host))
}

async fn run_tool(_input: Value, host: &dyn ToolHost) -> ToolOutcome {
    if !js::truthy(host.config().get("ankiOptIn")) {
        return fail("The learner did not opt in to Anki: no cards.");
    }
    if host.lesson_path().is_none() {
        return fail("No lesson artifact yet: nothing to sync.");
    }
    let summary = finish::sync_lesson(host).await;
    host.emit(json!({"kind": "notice", "level": "info", "text": summary}));
    util::json(&json!({"summary": summary}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson::doc;
    use crate::tools::fake::FakeHost;

    const DAG: &str =
        "graph TD\n  N1[\"Nodo 1: la prima cosa è vera\"] --> N2[\"Nodo 2: la seconda segue\"]";

    fn teach(anki_opt_in: bool) -> Value {
        json!({
            "kind": "teach",
            "course": "demo",
            "model": "haiku",
            "ankiOptIn": anki_opt_in,
            "target": {"mode": "planned", "title": "Il primo passo"},
        })
    }

    /// Node's `approvedLesson()` + a written node carrying the Anki params of the test.
    fn approved_lesson(config: Value) -> FakeHost {
        let host = FakeHost::with_lesson(config, DAG);
        let abs = host.data_root().join(host.lesson_path().expect("a lesson"));
        doc::append_node(doc::AppendOptions {
            lesson_path: Some(abs.to_string_lossy().into_owned()),
            slug: None,
            node: json!({
                "nodeId": "prima-cosa",
                "title": "La prima cosa",
                "tier": "unconditional-truth",
                "content": "<p>vero</p>",
                "ankiParams": {"targetQuestion": "Q?", "targetFact": "F"},
            }),
            base_dir: Some(host.data_root().to_string_lossy().into_owned()),
        })
        .expect("append one node");
        host
    }

    #[test]
    fn the_schema_file_is_the_node_capture() {
        assert_eq!(
            serde_json::to_string(&(TOOL.schema)()).unwrap(),
            r#"{"type":"object","properties":{}}"#
        );
    }

    #[test]
    fn any_object_is_a_valid_input_and_anything_else_is_not() {
        assert_eq!(
            serde_json::to_string(&validate(&json!({"extra": 1})).expect("valid")).unwrap(),
            "{}"
        );
        assert_eq!(
            validate(&json!("x")).expect_err("not an object"),
            vec!["(input): Invalid input: expected object, received string"]
        );
    }

    #[tokio::test]
    async fn refused_without_the_learners_opt_in() {
        let host = approved_lesson(teach(false));
        let out = host.call(&TOOL, json!({})).await;
        assert!(out.is_error);
        assert_eq!(out.text, "The learner did not opt in to Anki: no cards.");
        assert!(host.items().is_empty(), "a refusal emits no notice");
    }

    #[tokio::test]
    async fn needs_a_lesson_artifact() {
        let host = FakeHost::new(teach(true));
        let out = host.call(&TOOL, json!({})).await;
        assert!(out.is_error);
        assert_eq!(out.text, "No lesson artifact yet: nothing to sync.");
    }

    #[tokio::test]
    async fn offline_falls_back_to_the_tsv_and_emits_the_summary_as_a_notice() {
        // The fake host's default URL is a closed port: the sync keeps the deck TSV.
        let host = approved_lesson(teach(true));
        let out = host.call(&TOOL, json!({})).await;
        assert!(!out.is_error, "{}", out.text);
        let result: Value = serde_json::from_str(&out.text).expect("a JSON result");
        let summary = js::js_string(result.get("summary"));
        let lower = summary.to_lowercase();
        assert!(
            lower.contains("offline") || lower.contains("incomplete"),
            "{summary}"
        );
        assert!(summary.contains("decks/"), "{summary}");
        assert!(summary.contains(".tsv"), "{summary}");
        assert_eq!(
            host.items().last().expect("one emitted item"),
            &json!({"kind": "notice", "level": "info", "text": summary})
        );
    }
}
