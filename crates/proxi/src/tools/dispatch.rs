//! The `demo`, `visual` and `research` job starters (port of app/server/tools/{demo,visual,research}.ts).
//!
//! Each starts a background job and acks at once: the result arrives later as a message from the
//! app. Without a job manager the call is an error (never an ack whose result would never come).

use serde_json::{Map, Value};

use crate::sessions::{invalid_type, issue};
use crate::tools::util::{ack, fail, label};
use crate::tools::{ToolDef, ToolFuture, ToolHost};

/// One field of a zod object of plain strings: `.min()` counts UTF-16 units, like JS `.length`.
struct Field {
    key: &'static str,
    min: usize,
    required: bool,
}

const DEMO_FIELDS: &[Field] = &[
    Field {
        key: "title",
        min: 1,
        required: true,
    },
    Field {
        key: "brief",
        min: 1,
        required: true,
    },
];

const VISUAL_FIELDS: &[Field] = &[Field {
    key: "brief",
    min: 1,
    required: true,
}];

const RESEARCH_FIELDS: &[Field] = &[
    Field {
        key: "question",
        min: 1,
        required: true,
    },
    Field {
        key: "context",
        min: 1,
        required: false,
    },
];

/// The `demo` tool.
pub const DEMO: ToolDef = ToolDef {
    name: "demo",
    description: "Start a background subagent that builds one interactive single-file HTML demo and tests it headless. Returns at once with a job id; the demo appears in the chat when ready.",
    schema: demo_schema,
    validate: validate_demo,
    run: run_demo,
    writes: false,
};

/// The `visual` tool.
pub const VISUAL: ToolDef = ToolDef {
    name: "visual",
    description: "Start a background subagent that draws one SVG figure and checks it by rendering it. Returns at once with a job id; the figure appears in the chat when ready.",
    schema: visual_schema,
    validate: validate_visual,
    run: run_visual,
    writes: false,
};

/// The `research` tool.
pub const RESEARCH: ToolDef = ToolDef {
    name: "research",
    description: "Start a background web research subagent (WebSearch/WebFetch). Returns at once with a job id; the sourced brief arrives later as a message from the app.",
    schema: research_schema,
    validate: validate_research,
    run: run_research,
    writes: false,
};

fn validate_demo(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, DEMO_FIELDS)
}

fn validate_visual(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, VISUAL_FIELDS)
}

fn validate_research(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, RESEARCH_FIELDS)
}

fn run_demo<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    start("demo", "Demo", "title", input, host)
}

fn run_visual<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    start("visual", "Figura", "brief", input, host)
}

fn run_research<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    start("research", "Ricerca", "question", input, host)
}

fn demo_schema() -> Value {
    serde_json::from_str(include_str!("schemas/demo.json")).expect("the demo schema is valid JSON")
}

fn visual_schema() -> Value {
    serde_json::from_str(include_str!("schemas/visual.json")).expect("the visual schema is valid JSON")
}

fn research_schema() -> Value {
    serde_json::from_str(include_str!("schemas/research.json")).expect("the research schema is valid JSON")
}

/// `ctx.jobs.start(kind, input, { title: "<label>: <short input>" })`, then the ack.
fn start<'a>(
    kind: &'static str,
    prefix: &'static str,
    title_field: &'static str,
    input: Value,
    host: &'a dyn ToolHost,
) -> ToolFuture<'a> {
    Box::pin(async move {
        // Only the validated input reaches `run`, so the title field is a string.
        let text = input.get(title_field).and_then(Value::as_str).unwrap_or("");
        let title = format!("{prefix}: {}", label(text, 60));
        match host.start_job(kind, input, &title) {
            Ok(id) => ack(&id),
            Err(message) => fail(message),
        }
    })
}

/// A zod object of plain string fields: issues in schema key order, the cleaned data rebuilt in the
/// same order with unknown keys dropped (that is what Node stores and what the tool receives).
fn string_object(input: &Value, fields: &[Field]) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut out = Map::new();
    for field in fields {
        match string_value(object.get(field.key), field) {
            Ok(Some(text)) => {
                out.insert(field.key.to_string(), Value::String(text));
            }
            Ok(None) => {}
            Err(issue) => issues.push(issue),
        }
    }
    if issues.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(issues)
    }
}

fn string_value(value: Option<&Value>, field: &Field) -> Result<Option<String>, String> {
    let text = match value {
        Some(Value::String(text)) => text,
        Some(other) => {
            return Err(issue(field.key, &invalid_type("string", Some(other))));
        }
        None if field.required => {
            return Err(issue(field.key, &invalid_type("string", None)));
        }
        None => return Ok(None),
    };
    let length = text.encode_utf16().count();
    if length < field.min {
        return Err(format!(
            "{}: Too small: expected string to have >={} characters",
            field.key, field.min
        ));
    }
    Ok(Some(text.clone()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::fake::{FakeHost, RecordedJob};
    use crate::tools::{NO_JOBS_MESSAGE, ToolOutcome};

    fn host() -> FakeHost {
        FakeHost::new(json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false}))
    }

    const ACK: &str =
        "Avviato in background (job job-1): il risultato arriverà come messaggio. Continua la lezione.";

    #[tokio::test]
    async fn demo_records_the_job_and_acks() {
        let host = host();
        let input = json!({"title": "La caduta", "brief": "Una palla che cade."});

        let out = host.call(&DEMO, input.clone()).await;

        assert_eq!(out, ToolOutcome::ok(ACK));
        assert_eq!(
            host.jobs(),
            vec![RecordedJob {
                kind: "demo".to_string(),
                input,
                title: "Demo: La caduta".to_string(),
            }]
        );
        assert_eq!(host.courses_changed_count(), 0, "a dispatch tool does not write");
    }

    #[tokio::test]
    async fn visual_and_research_record_their_own_kind_and_title() {
        let host = host();
        let ack = |n: usize| {
            ToolOutcome::ok(format!(
                "Avviato in background (job job-{n}): il risultato arriverà come messaggio. Continua la lezione."
            ))
        };

        assert_eq!(
            host.call(&VISUAL, json!({"brief": "Due rette che si incontrano."}))
                .await,
            ack(1)
        );
        assert_eq!(
            host.call(&RESEARCH, json!({"question": "Chi ha inventato il calcolo?"}))
                .await,
            ack(2)
        );

        assert_eq!(
            host.jobs(),
            vec![
                RecordedJob {
                    kind: "visual".to_string(),
                    input: json!({"brief": "Due rette che si incontrano."}),
                    title: "Figura: Due rette che si incontrano.".to_string(),
                },
                RecordedJob {
                    kind: "research".to_string(),
                    input: json!({"question": "Chi ha inventato il calcolo?"}),
                    title: "Ricerca: Chi ha inventato il calcolo?".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn without_a_job_manager_the_tool_returns_an_error() {
        let host = host();
        host.set_fail_jobs(true);

        let out = host.call(&DEMO, json!({"title": "T", "brief": "B"})).await;

        assert_eq!(out, ToolOutcome::error(NO_JOBS_MESSAGE));
        assert!(host.jobs().is_empty());
    }

    #[tokio::test]
    async fn long_titles_are_labelled_like_node() {
        let host = host();
        let long = "x".repeat(70);

        host.call(&DEMO, json!({"title": long, "brief": "b"})).await;
        host.call(&RESEARCH, json!({"question": long})).await;

        let jobs = host.jobs();
        // `label(s, 60)` cuts at 59 UTF-16 units and appends the ellipsis.
        assert_eq!(jobs[0].title, format!("Demo: {}…", "x".repeat(59)));
        assert_eq!(jobs[1].title, format!("Ricerca: {}…", "x".repeat(59)));
    }

    #[tokio::test]
    async fn invalid_input_is_rejected_before_a_job_starts() {
        let host = host();

        let out = host.call(&DEMO, json!({"title": ""})).await;

        assert_eq!(
            out,
            ToolOutcome::error(
                "Invalid input: title: Too small: expected string to have >=1 characters; brief: Invalid input: expected string, received undefined. Fix and call again."
            )
        );
        assert!(host.jobs().is_empty());

        let out = host.call(&VISUAL, json!([])).await;
        assert_eq!(
            out,
            ToolOutcome::error(
                "Invalid input: (input): Invalid input: expected object, received array. Fix and call again."
            )
        );
    }

    #[tokio::test]
    async fn the_cleaned_input_is_in_schema_key_order_without_unknown_keys() {
        let host = host();

        host.call(&RESEARCH, json!({"extra": 1, "context": "c", "question": "q"}))
            .await;

        let input = &host.jobs()[0].input;
        assert_eq!(
            serde_json::to_string(input).expect("an object serialises"),
            r#"{"question":"q","context":"c"}"#
        );
    }

    #[test]
    fn the_schema_files_are_what_the_tools_serve() {
        let cases = [
            (&DEMO, include_str!("schemas/demo.json")),
            (&VISUAL, include_str!("schemas/visual.json")),
            (&RESEARCH, include_str!("schemas/research.json")),
        ];
        for (def, source) in cases {
            let file: Value = serde_json::from_str(source).expect("the schema file parses");
            assert_eq!(
                serde_json::to_string(&(def.schema)()).expect("a schema serialises"),
                serde_json::to_string(&file).expect("the file serialises")
            );
        }
    }
}
