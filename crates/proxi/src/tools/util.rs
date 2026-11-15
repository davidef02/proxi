//! Helpers shared by the tools (port of `app/server/tools/util.ts`).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::course_data::valid_course;
use crate::js;
use crate::knowledge;
use crate::lesson::artifact::{self, Lesson};
use crate::planstate;
use crate::sessions::{trunc, utf16_len};
use crate::tools::{ToolHost, ToolOutcome};

/// Node's message for an interrupted card; the tool answers it as an error result.
pub const CANCELLED: &str = "The learner dismissed the question (session interrupted).";

/// Node's `json(o)`: the result is a JSON string for the engine to read.
pub fn json(v: &Value) -> ToolOutcome {
    ToolOutcome::ok(serde_json::to_string(v).expect("a JSON value serialises"))
}

pub fn fail(text: impl Into<String>) -> ToolOutcome {
    ToolOutcome::error(text)
}

/// `courses/<course>` under the data root; the course name comes from config, so it is validated.
pub fn course_dir(host: &dyn ToolHost) -> Result<PathBuf, String> {
    let config = host.config();
    let course = js::js_string(config.get("course"));
    if !valid_course(&course) {
        return Err(format!("Invalid course directory name \"{course}\"."));
    }
    Ok(host.data_root().join("courses").join(course))
}

/// Node's `path.relative(root, abs)` with `/` separators.
pub fn to_rel(data_root: &Path, abs: &Path) -> String {
    let base = segments(data_root);
    let target = segments(abs);
    let common = base
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        // Another root: Node's path.relative gives up and returns the absolute path.
        return target.join("/");
    }
    let mut parts = vec!["..".to_string(); base.len() - common];
    parts.extend(target[common..].iter().cloned());
    parts.join("/")
}

/// Path segments over `/` and `\` alike, so a Windows path keeps the same shape on any host.
fn segments(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// The session's lesson, read through the artifact module; `None` without a path, a file or a
/// data island.
pub fn current_lesson(host: &dyn ToolHost) -> Option<Lesson> {
    let rel = host.lesson_path()?;
    artifact::read_lesson(host.data_root(), &rel).ok().flatten()
}

/// The progress lines of the session's lesson (its slug), or `[]` without a lesson path.
pub fn lesson_progress(host: &dyn ToolHost) -> Vec<Value> {
    let Some(path) = host.lesson_path() else {
        return Vec::new();
    };
    let slug = artifact::slug_of_lesson(&path);
    let config = host.config();
    let course = js::js_string(config.get("course"));
    knowledge::read_progress(host.data_root(), &course)
        .into_iter()
        .filter(|record| record.get("lessonSlug").and_then(Value::as_str) == Some(slug.as_str()))
        .collect()
}

/// Re-derive the plan from the artifact node count and the progress log; no-op without a plan.
pub fn recompute_plan(host: &dyn ToolHost) {
    let plan = host.plan();
    if plan.is_null() {
        return;
    }
    let topic = js::js_string(plan.get("topic"));
    let goal = js::js_string(plan.get("goal"));
    let dag = js::js_string(plan.get("dag"));
    let approved = plan.get("approved") == Some(&Value::Bool(true));
    let node_count = current_lesson(host)
        .and_then(|lesson| lesson.doc.get("nodes").and_then(Value::as_array).map(Vec::len))
        .unwrap_or(0);
    host.set_plan(planstate::derive_plan_state(
        &topic,
        &goal,
        &dag,
        approved,
        node_count,
        &lesson_progress(host),
    ));
}

/// Short single-line label for a job title, cut at `n` UTF-16 units like `String.slice`.
pub fn label(s: &str, n: usize) -> String {
    let collapsed = js::collapse_ws(s);
    let one = js::js_trim(&collapsed);
    if utf16_len(one) > n {
        trunc(one, n.saturating_sub(1))
    } else {
        one.to_string()
    }
}

/// The ack of the dispatch tools: the result arrives later as a chat message.
pub fn ack(job_id: &str) -> ToolOutcome {
    ToolOutcome::ok(format!(
        "Avviato in background (job {job_id}): il risultato arriverà come messaggio. Continua la lezione."
    ))
}

/// Node's `gradeQuiz`: the answer with `correct` appended (same keys, same order).
pub fn grade_quiz(input: &Value, answer: &Value) -> Value {
    let mut out = match answer.as_object() {
        Some(object) => object.clone(),
        None => Map::new(),
    };
    let correct_set = unique(values(input.get("correctAnswer")));
    let selected = unique(values(answer.get("selected")));
    let correct = !js::truthy(answer.get("dontKnow"))
        && selected.len() == correct_set.len()
        && selected.iter().all(|value| correct_set.contains(value));
    out.insert("correct".to_string(), Value::Bool(correct));
    Value::Object(out)
}

/// Node's `asArray`: one value or a list of them, each as a set member.
fn values(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(|item| js::js_string(Some(item))).collect(),
        Some(value) => vec![js::js_string(Some(value))],
        None => Vec::new(),
    }
}

/// A JS `Set` of the strings, in first-appearance order.
fn unique(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::fake::FakeHost;

    fn host() -> FakeHost {
        FakeHost::new(json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false}))
    }

    #[test]
    fn label_collapses_whitespace_and_cuts_at_the_limit() {
        assert_eq!(label("  la\n\tprima   cosa ", 60), "la prima cosa");
        // `slice(0, n - 1)` plus the ellipsis: only past the limit.
        assert_eq!(label("abcde", 5), "abcde");
        assert_eq!(label("abcdef", 5), "abcd…");
        assert_eq!(label("", 60), "");
    }

    #[test]
    fn label_counts_utf16_units_not_chars() {
        // Two emoji are 2 chars but 4 UTF-16 units: exactly at the limit, so nothing is cut.
        assert_eq!(label("😀😀", 4), "😀😀");
        // Cutting at 3 units cannot split the second emoji: it is dropped whole.
        assert_eq!(label("😀😀", 3), "😀…");
    }

    #[test]
    fn grade_quiz_needs_the_exact_set() {
        let input = json!({"correctAnswer": ["a", "b"], "multiSelect": true});
        let answer = json!({"selected": ["b", "a", "b"], "dontKnow": false});
        assert_eq!(grade_quiz(&input, &answer)["correct"], json!(true));

        let subset = json!({"selected": ["a"], "dontKnow": false});
        assert_eq!(grade_quiz(&input, &subset)["correct"], json!(false));

        let extra = json!({"selected": ["a", "b", "c"], "dontKnow": false});
        assert_eq!(grade_quiz(&input, &extra)["correct"], json!(false));

        let single = json!({"correctAnswer": "B"});
        assert_eq!(grade_quiz(&single, &json!({"selected": ["B"]}))["correct"], json!(true));
    }

    #[test]
    fn grade_quiz_never_marks_dont_know_correct() {
        let input = json!({"correctAnswer": "B"});
        let answer = json!({"selected": ["B"], "dontKnow": true});
        assert_eq!(grade_quiz(&input, &answer)["correct"], json!(false));
    }

    #[test]
    fn grade_quiz_keeps_the_answer_keys_then_appends_correct() {
        let input = json!({"correctAnswer": "B"});
        let answer = json!({"selected": ["B"], "dontKnow": false, "confidence": 80, "note": "sicuro"});
        assert_eq!(
            serde_json::to_string(&grade_quiz(&input, &answer)).unwrap(),
            r#"{"selected":["B"],"dontKnow":false,"confidence":80,"note":"sicuro","correct":true}"#
        );
    }

    #[test]
    fn to_rel_uses_forward_slashes_on_windows_paths() {
        assert_eq!(
            to_rel(
                Path::new(r"C:\repo\data"),
                Path::new(r"C:\repo\data\courses\demo\lessons\0001-prima.html")
            ),
            "courses/demo/lessons/0001-prima.html"
        );
        assert_eq!(
            to_rel(Path::new("C:/repo/data"), Path::new("C:/repo/data/courses")),
            "courses"
        );
        assert_eq!(
            to_rel(Path::new(r"C:\repo\data"), Path::new(r"C:\repo\records\x.md")),
            "../records/x.md"
        );
        assert_eq!(to_rel(Path::new("C:/repo/data"), Path::new("C:/repo/data")), "");
    }

    #[test]
    fn recompute_plan_is_a_no_op_without_a_plan() {
        let host = host();
        recompute_plan(&host);
        assert!(host.plan().is_null());
        assert!(host.events().is_empty());
    }

    #[test]
    fn recompute_plan_reads_the_artifact_node_count() {
        let host = FakeHost::with_lesson(
            json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false}),
            "graph TD\n  N1[\"Nodo 1: la prima cosa\"] --> N2[\"Nodo 2: la seconda\"]",
        );
        let abs = host.data_root().join(host.lesson_path().expect("a lesson"));
        crate::lesson::doc::append_node(crate::lesson::doc::AppendOptions {
            lesson_path: Some(abs.to_string_lossy().into_owned()),
            slug: None,
            node: json!({"nodeId": "prima", "title": "La prima", "tier": "axiom", "content": "<p>vera</p>"}),
            base_dir: Some(host.data_root().to_string_lossy().into_owned()),
        })
        .expect("append one node");

        recompute_plan(&host);

        assert_eq!(host.plan()["nodeStatus"]["N1"], json!("done"));
        assert_eq!(host.plan()["nodeStatus"]["N2"], json!("current"));
        assert_eq!(host.plan()["topic"], json!("Demo topic"));
    }
}
