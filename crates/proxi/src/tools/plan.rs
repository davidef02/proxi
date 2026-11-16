//! The `propose_plan` tool (port of app/server/tools/plan.ts).

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::js;
use crate::lesson::artifact;
use crate::lesson::doc::{self, InitOptions};
use crate::planstate;
use crate::sessions::{invalid_type, issue, utf16_len};
use crate::tools::util;
use crate::tools::{Cancelled, ToolDef, ToolFuture, ToolHost, ToolOutcome};

/// Node's `proposePlanTool`.
pub const TOOL: ToolDef = ToolDef {
    name: "propose_plan",
    description: "Show the learner the lesson approach and dependency DAG and wait for approval (blocks). On approval the lesson artifact is created and nodes can be written with write_node.",
    schema,
    validate,
    run,
    writes: true,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("schemas/propose_plan.json"))
        .expect("the propose_plan schema parses")
}

const TOO_SMALL_ONE: &str = "Too small: expected string to have >=1 characters";
const MISSING_STRING: &str = "Invalid input: expected string, received undefined";

/// Node's `proposePlanSchema`, hand-rolled: the cleaned data or one `"<path>: <message>"` per issue.
fn validate(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    // Schema order, so the card input and the errors come out as Node's `safeParse`.
    for (key, required) in [
        ("topic", true),
        ("title", false),
        ("approach", true),
        ("dag", true),
        ("goal", true),
    ] {
        match object.get(key) {
            Some(Value::String(value)) if utf16_len(value) == 0 => {
                issues.push(issue(key, TOO_SMALL_ONE));
            }
            Some(Value::String(value)) => {
                data.insert(key.to_string(), Value::String(value.clone()));
            }
            Some(other) => issues.push(issue(key, &invalid_type("string", Some(other)))),
            None if required => issues.push(issue(key, MISSING_STRING)),
            None => {}
        }
    }

    if issues.is_empty() {
        Ok(Value::Object(data))
    } else {
        Err(issues)
    }
}

fn run(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(plan(input, host))
}

async fn plan(input: Value, host: &dyn ToolHost) -> ToolOutcome {
    let config = host.config();
    let target = match config.get("target") {
        Some(target) if target.is_object() => target.clone(),
        _ => json!({"mode": "free"}),
    };
    let mode = js::js_string(target.get("mode"));
    let topic = js::js_string(input.get("topic"));
    let dag = js::js_string(input.get("dag"));
    let goal = js::js_string(input.get("goal"));

    let errs = planstate::validate_dag_labels(&dag);
    if !errs.is_empty() {
        return util::fail(format!(
            "Invalid DAG:\n{}\nFix and call again.",
            errs.join("\n")
        ));
    }
    if mode == "free" && !js::truthy(input.get("title")) {
        return util::fail(
            "This is a free lesson: propose_plan needs a title (the insight the lesson teaches).",
        );
    }

    let decision = match host.request(json!({"kind": "plan", "input": input.clone()})) {
        Ok(receiver) => match receiver.await {
            Ok(Ok(answer)) => answer,
            _ => return util::fail(util::CANCELLED),
        },
        Err(Cancelled) => return util::fail(util::CANCELLED),
    };
    if !js::truthy(decision.get("approved")) {
        let mut out = Map::new();
        out.insert("approved".to_string(), Value::Bool(false));
        let feedback = match decision.get("feedback") {
            None | Some(Value::Null) => Value::String(String::new()),
            Some(feedback) => feedback.clone(),
        };
        out.insert("feedback".to_string(), feedback);
        return util::json(&Value::Object(out));
    }

    if host.lesson_path().is_none() {
        if mode == "existing" {
            host.set_lesson(js::js_string(target.get("lessonPath")), None);
        } else {
            // The planned title goes in verbatim: the board joins plan entry and artifact by its slug.
            let title = if mode == "planned" {
                Some(js::js_string(target.get("title")))
            } else {
                input
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            let lessons_dir = match util::course_dir(host) {
                Ok(dir) => dir.join("lessons"),
                Err(message) => return util::fail(message),
            };
            let created = doc::init_lesson(&InitOptions {
                topic: topic.clone(),
                title,
                slug: None,
                description: None,
                dag: Some(dag.clone()),
                target_goal: Some(goal.clone()),
                base_dir: Some(host.data_root().to_string_lossy().into_owned()),
                lessons_dir: Some(lessons_dir.to_string_lossy().into_owned()),
                repo_root: host.repo_root().to_path_buf(),
            });
            let created = match created {
                Ok(created) => created,
                Err(message) => return util::fail(message),
            };
            host.set_lesson(
                util::to_rel(host.data_root(), Path::new(&created.file_path)),
                None,
            );
        }
    }

    let lesson_path = host.lesson_path().unwrap_or_default();
    // Approved revision of an existing lesson: the artifact's DAG and goal follow the new plan.
    if let Err(message) = revise_plan(host, &lesson_path, &dag, &goal) {
        return util::fail(message);
    }
    let node_count = util::current_lesson(host)
        .and_then(|lesson| {
            lesson
                .doc
                .get("nodes")
                .and_then(Value::as_array)
                .map(Vec::len)
        })
        .unwrap_or(0);
    host.set_plan(planstate::derive_plan_state(
        &topic,
        &goal,
        &dag,
        true,
        node_count,
        &util::lesson_progress(host),
    ));

    let mut out = Map::new();
    out.insert("approved".to_string(), Value::Bool(true));
    out.insert("lessonPath".to_string(), Value::String(lesson_path));
    util::json(&Value::Object(out))
}

/// Port of `revisePlan` (`app/server/lesson.ts:179-183`): patch the artifact's DAG and goal when the
/// approved plan differs from what it was created with.
fn revise_plan(host: &dyn ToolHost, rel: &str, dag: &str, goal: &str) -> Result<(), String> {
    let Ok(Some(lesson)) = artifact::read_lesson(host.data_root(), rel) else {
        return Ok(());
    };
    let metadata = lesson.doc.get("metadata");
    let unchanged = metadata.and_then(|m| m.get("initialDag"))
        == Some(&Value::String(dag.to_owned()))
        && metadata.and_then(|m| m.get("targetGoal")) == Some(&Value::String(goal.to_owned()));
    if unchanged {
        return Ok(());
    }
    let mut patch = Map::new();
    patch.insert("initialDag".to_string(), Value::String(dag.to_owned()));
    patch.insert("targetGoal".to_string(), Value::String(goal.to_owned()));
    doc::update_lesson_metadata(
        &lesson.abs.to_string_lossy(),
        &host.data_root().to_string_lossy(),
        host.repo_root(),
        &patch,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::tools::fake::FakeHost;

    const DAG: &str =
        "graph TD\n  N1[\"Nodo 1: la prima cosa è vera\"] --> N2[\"Nodo 2: la seconda segue\"]";

    /// The fake's data root has no `assets/`: drop the real template in, as a data folder does.
    fn host(config: Value) -> FakeHost {
        let host = FakeHost::new(config);
        let assets = host.data_root().join("assets");
        fs::create_dir_all(&assets).expect("assets dir");
        fs::copy(
            repo_root().join(doc::TEMPLATE_REL),
            assets.join("lesson-template.html"),
        )
        .expect("copy the lesson template");
        host
    }

    fn repo_root() -> PathBuf {
        crate::data_root::resolve_path(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
    }

    fn teach() -> Value {
        json!({
            "kind": "teach",
            "course": "demo",
            "model": "haiku",
            "target": {"mode": "planned", "title": "Il primo passo"},
            "ankiOptIn": false,
        })
    }

    fn plan_in(extra: Value) -> Value {
        let mut input =
            json!({"topic": "Demo topic", "approach": "prosa", "dag": DAG, "goal": "capire"});
        for (key, value) in extra.as_object().expect("an object of overrides") {
            input[key] = value.clone();
        }
        input
    }

    fn lesson_html(host: &FakeHost) -> Vec<String> {
        fs::read_dir(host.data_root().join("courses/demo/lessons"))
            .expect("the lessons dir")
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.ends_with(".html"))
            .collect()
    }

    #[tokio::test]
    async fn rejects_a_dag_with_bad_labels_without_asking() {
        let host = host(teach());
        let out = host
            .call(&TOOL, plan_in(json!({"dag": "graph TD\nN1[\"Primo\"]"})))
            .await;
        assert!(out.is_error);
        assert!(out.text.contains("line 2"), "{}", out.text);
        assert!(host.requests().is_empty());
    }

    #[tokio::test]
    async fn free_lessons_need_a_title() {
        let host = host(json!({
            "kind": "teach", "course": "demo", "model": "haiku",
            "target": {"mode": "free"}, "ankiOptIn": false,
        }));
        let out = host.call(&TOOL, plan_in(json!({}))).await;
        assert!(out.is_error);
        assert!(out.text.contains("needs a title"), "{}", out.text);
    }

    #[tokio::test]
    async fn approval_creates_the_lesson_with_the_planned_title_verbatim() {
        let host = host(teach());
        host.queue_answer(json!({"approved": true}));
        let out = host
            .call(&TOOL, plan_in(json!({"title": "Ignorato"})))
            .await;
        assert!(!out.is_error, "{}", out.text);

        let value: Value = serde_json::from_str(&out.text).expect("a JSON result");
        assert_eq!(value["approved"], json!(true));
        assert_eq!(
            value["lessonPath"],
            json!("courses/demo/lessons/0001-il-primo-passo.html")
        );
        assert!(
            host.data_root()
                .join("courses/demo/lessons/0001-il-primo-passo.html")
                .exists()
        );
        assert_eq!(
            host.lesson_path().as_deref(),
            Some("courses/demo/lessons/0001-il-primo-passo.html")
        );
        assert_eq!(
            host.events()[0],
            json!({"type": "lesson", "lessonPath": "courses/demo/lessons/0001-il-primo-passo.html"})
        );
        assert_eq!(host.plan()["approved"], json!(true));
        assert_eq!(
            serde_json::to_string(&host.plan()["nodeStatus"]).unwrap(),
            r#"{"N1":"current","N2":"todo"}"#
        );
    }

    #[tokio::test]
    async fn a_free_lesson_uses_the_proposed_title() {
        let host = host(json!({
            "kind": "teach", "course": "demo", "model": "haiku",
            "target": {"mode": "free"}, "ankiOptIn": false,
        }));
        host.queue_answer(json!({"approved": true}));
        let out = host
            .call(&TOOL, plan_in(json!({"title": "Un idea libera"})))
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            host.lesson_path().as_deref(),
            Some("courses/demo/lessons/0001-un-idea-libera.html")
        );
    }

    #[tokio::test]
    async fn rejection_returns_feedback_and_creates_nothing() {
        let host = host(teach());
        host.queue_answer(json!({"approved": false, "feedback": "più corto"}));
        let out = host.call(&TOOL, plan_in(json!({}))).await;
        assert_eq!(
            out,
            ToolOutcome::ok(r#"{"approved":false,"feedback":"più corto"}"#)
        );
        assert_eq!(host.lesson_path(), None);
        assert!(lesson_html(&host).is_empty());
    }

    #[tokio::test]
    async fn existing_mode_attaches_without_creating() {
        let rel = "courses/demo/lessons/0001-il-primo-passo.html";
        let host = host(json!({
            "kind": "teach", "course": "demo", "model": "haiku",
            "target": {"mode": "existing", "lessonPath": rel}, "ankiOptIn": false,
        }));
        // A lesson an earlier session left on disk, with this plan's DAG and goal.
        doc::init_lesson(&InitOptions {
            topic: "Demo topic".to_string(),
            title: Some("Il primo passo".to_string()),
            slug: None,
            description: None,
            dag: Some(DAG.to_string()),
            target_goal: Some("capire".to_string()),
            base_dir: Some(host.data_root().to_string_lossy().into_owned()),
            lessons_dir: Some(
                host.data_root()
                    .join("courses/demo/lessons")
                    .to_string_lossy()
                    .into_owned(),
            ),
            repo_root: host.repo_root().to_path_buf(),
        })
        .expect("the existing lesson");

        host.queue_answer(json!({"approved": true}));
        let out = host.call(&TOOL, plan_in(json!({}))).await;
        assert!(!out.is_error, "{}", out.text);
        let value: Value = serde_json::from_str(&out.text).expect("a JSON result");
        assert_eq!(value["lessonPath"], json!(rel));
        assert_eq!(host.lesson_path().as_deref(), Some(rel));
        assert_eq!(lesson_html(&host), vec!["0001-il-primo-passo.html"]);
    }

    #[tokio::test]
    async fn an_approved_revised_dag_rewrites_the_island_and_the_artifact() {
        let host = host(teach());
        host.queue_answer(json!({"approved": true}));
        let created = host.call(&TOOL, plan_in(json!({}))).await;
        assert!(!created.is_error, "{}", created.text);
        let rel = host.lesson_path().expect("a created lesson");
        let abs = host.data_root().join(&rel);
        doc::append_node(doc::AppendOptions {
            lesson_path: Some(abs.to_string_lossy().into_owned()),
            slug: None,
            node: json!({
                "nodeId": "prima-cosa",
                "title": "La prima cosa",
                "tier": "unconditional-truth",
                "content": "<p>vero</p>",
            }),
            base_dir: Some(host.data_root().to_string_lossy().into_owned()),
        })
        .expect("append the node");

        let dag3 = format!("{DAG}\n  N2 --> N3[\"Nodo 3: la terza completa\"]");
        host.queue_answer(json!({"approved": true}));
        let out = host
            .call(&TOOL, plan_in(json!({"dag": dag3, "goal": "nuovo goal"})))
            .await;
        assert!(!out.is_error, "{}", out.text);

        let html = fs::read_to_string(&abs).expect("the lesson html");
        assert!(html.contains("Nodo 3: la terza completa"));
        assert!(html.contains("\"targetGoal\": \"nuovo goal\""));
        assert!(html.contains("id=\"node-prima-cosa\""));
        let status = host.plan()["nodeStatus"]
            .as_object()
            .expect("nodeStatus")
            .clone();
        assert_eq!(status.keys().collect::<Vec<_>>(), ["N1", "N2", "N3"]);
        assert_eq!(status["N3"], json!("todo"));
    }

    #[tokio::test]
    async fn a_cancelled_card_is_an_error_result() {
        let host = host(teach());
        host.queue_cancel();
        let out = host.call(&TOOL, plan_in(json!({}))).await;
        assert_eq!(out, ToolOutcome::error(util::CANCELLED));
    }

    #[test]
    fn invalid_input_uses_zods_text() {
        assert_eq!(
            (TOOL.validate)(&json!({})),
            Err(vec![
                "topic: Invalid input: expected string, received undefined".to_string(),
                "approach: Invalid input: expected string, received undefined".to_string(),
                "dag: Invalid input: expected string, received undefined".to_string(),
                "goal: Invalid input: expected string, received undefined".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&json!({"topic": "", "approach": "a", "dag": "d", "goal": "g"})),
            Err(vec![
                "topic: Too small: expected string to have >=1 characters".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(
                &json!({"topic": "t", "title": 3, "approach": "a", "dag": "d", "goal": "g"})
            ),
            Err(vec![
                "title: Invalid input: expected string, received number".to_string(),
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
            "goal": "g",
            "extra": 1,
            "dag": "d",
            "approach": "a",
            "title": "t",
            "topic": "x",
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"topic":"x","title":"t","approach":"a","dag":"d","goal":"g"}"#
        );
        let without_title = (TOOL.validate)(&json!({
            "topic": "x", "approach": "a", "dag": "d", "goal": "g",
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&without_title).unwrap(),
            r#"{"topic":"x","approach":"a","dag":"d","goal":"g"}"#
        );
    }
}
