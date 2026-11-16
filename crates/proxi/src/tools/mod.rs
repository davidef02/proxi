//! The app tools served over MCP: one module per Node tool, plus the seams they run on.
//!
//! Tools are data, not trait objects: each tool is a `pub const TOOL: ToolDef`, the registry is a
//! `Vec<&'static ToolDef>` (`tools_for`), and `mcp::handle` only ever sees `&[&ToolDef]`.
//!
//! # Schemas
//!
//! One JSON file per tool in `src/tools/schemas/`, generated from Node's zod schema (exact values,
//! key order, `$schema` dropped, `type` first) and loaded with `include_str!`. Run from the repo root:
//!
//! ```text
//! node --import tsx --input-type=module -e "import {toJsonSchema} from './app/server/mcp.ts'; import {toolsFor} from './app/server/tools/index.ts'; const t = toolsFor({kind:'teach',course:'demo',model:'haiku',ankiOptIn:false}, process.cwd()).find((x) => x.name === process.argv[1]); process.stdout.write(JSON.stringify(toJsonSchema(t.schema), null, 2) + '\n')" <tool-name> > crates/proxi/src/tools/schemas/<tool-name>.json
//! ```
//!
//! The validator's messages are zod 4's own: get one with the same imports, `t.schema.safeParse(<input>).error.issues`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::course_data;
use crate::js;

pub mod anki;
pub mod ask;
pub mod board;
pub mod concepts;
pub mod curriculum;
pub mod dispatch;
pub mod exercise;
pub mod finish;
pub mod lessontext;
pub mod node;
pub mod plan;
pub mod practice;
pub mod quiz;
pub mod util;

#[cfg(test)]
pub(crate) mod fake;

/// What a tool answers: MCP text content plus the `isError` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// The learner interrupted the session (or the card was superseded) while a tool waited: Node's
/// rejected `Error("cancelled")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// Why a tool future is boxed: `ToolDef` is a plain data table, not an object-safe trait.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;

/// One tool as data: its MCP listing, its zod-equivalent validator and its effect.
pub struct ToolDef {
    pub name: &'static str,
    /// Node's English text, verbatim.
    pub description: &'static str,
    /// Node's `toJsonSchema(..., {io: "input"})` minus `$schema`, `type` first.
    pub schema: fn() -> Value,
    /// Node's `safeParse`: the cleaned data (schema key order, unknown keys dropped), or one
    /// `"<path>: <message>"` issue per failure.
    pub validate: fn(&Value) -> Result<Value, Vec<String>>,
    pub run: for<'a> fn(Value, &'a dyn ToolHost) -> ToolFuture<'a>,
    /// Node's `writes`: a non-error run raises `courses_changed`.
    pub writes: bool,
}

/// Everything a tool may touch: Node's `ToolContext`, sync methods only so the trait stays
/// object-safe without `async_trait`. Blocking waits return a receiver the tool awaits outside any
/// session lock.
pub trait ToolHost: Send + Sync {
    fn data_root(&self) -> &Path;
    fn repo_root(&self) -> &Path;
    /// A snapshot of the session config.
    fn config(&self) -> Map<String, Value>;
    fn lesson_path(&self) -> Option<String>;
    /// `Null` = no plan.
    fn plan(&self) -> Value;
    /// Stores the plan and notifies `{"type":"plan"}`.
    fn set_plan(&self, plan: Value);
    /// Stores the lesson path and notifies `{"type":"lesson","lessonPath",…,"nodeId"?}`.
    fn set_lesson(&self, lesson_path: String, node_id: Option<String>);
    /// Node's `getExercises`: the session's exercise list.
    fn exercises(&self) -> Vec<Value>;
    /// Node's `setExercises` + `emit(exercise:<name>)` under one lock: replaces or inserts `info`
    /// in the session's exercise list, sorted by name, and notifies `exercises` event.
    fn upsert_exercise(&self, info: Value);
    /// Node's `open` map: `None` while a problem is already open; else shows `item` and remembers
    /// it (`id`, its input) for `conclude`.
    fn open_problem(&self, item: Value) -> Option<String>;
    /// The open problem, if any: `(id, input)`.
    fn take_problem(&self) -> Option<(String, Value)>;
    /// Pushes the interactive card and waits for the learner's answer.
    fn request(&self, item: Value) -> Result<oneshot::Receiver<Result<Value, Cancelled>>, Cancelled>;
    /// Pushes or upserts a non-blocking item; returns its id.
    fn emit(&self, item: Value) -> String;
    /// Appends one progress record to the course ledger; an append error fails the calling tool,
    /// like Node's `logProgress` (`appendFileSync` inside the awaited call).
    fn log_progress(&self, record: Map<String, Value>) -> Result<(), String>;
    /// Starts a background job (`demo`, `visual`, `research`); returns its id.
    fn start_job(&self, kind: &str, input: Value, title: &str) -> Result<String, String>;
    /// Starts the map job for a lesson (`<course>/<file stem>`).
    fn start_map(&self, target: &str) -> Result<(), String>;
    /// The route's runner for `run_exercise` (the same node and npm the UI uses).
    fn run_config(&self) -> crate::exercises::RunConfig;
    /// The installs registry shared with the routes: one `npm install` per exercises dir.
    fn installs(&self) -> crate::exercises::Installs;
    /// The server's resolved AnkiConnect URL (flag, then env), never the process env here: tests
    /// point at a fake without racing.
    fn anki_url(&self) -> Option<String>;
    /// The server's one `SemanticSearch`.
    fn semantic(&self) -> Arc<crate::semantic::SemanticSearch>;
    fn courses_changed(&self);
}

/// The background job seam: implemented by the session (it owns the job's session id).
pub trait JobStarter: Send + Sync {
    fn start(&self, kind: &str, input: Value, title: &str, session_id: &str) -> Result<String, String>;
    fn start_map(&self, target: &str, session_id: &str) -> Result<(), String>;
}

/// Node's message when no job manager exists in this build (#22): never a fake ack whose result
/// would never arrive.
pub const NO_JOBS_MESSAGE: &str = "Background jobs are not available in this build yet.";

/// The production `JobStarter` until the job manager lands.
pub struct NoJobs;

impl JobStarter for NoJobs {
    fn start(&self, _kind: &str, _input: Value, _title: &str, _session_id: &str) -> Result<String, String> {
        Err(NO_JOBS_MESSAGE.to_string())
    }

    fn start_map(&self, _target: &str, _session_id: &str) -> Result<(), String> {
        Err(NO_JOBS_MESSAGE.to_string())
    }
}

/// The write seam: runs the tool and raises `courses_changed` when a `writes` tool succeeded
/// (Node's `raising` in `tools/index.ts`).
pub async fn invoke(def: &ToolDef, input: Value, host: &dyn ToolHost) -> ToolOutcome {
    let out = (def.run)(input, host).await;
    if def.writes && !out.is_error {
        host.courses_changed();
    }
    out
}

/// The tools a session of this kind may call, in Node's order (`toolsFor` in `tools/index.ts`).
///
/// Exercise tools join only when the course has an `exercises/package.json`; the concept tools are
/// shared by every kind. Node's `raising` wrapper is `invoke` here.
pub fn tools_for(config: &Map<String, Value>, data_root: &Path) -> Vec<&'static ToolDef> {
    let course = js::js_string(config.get("course"));
    let has_exercises = course_data::valid_course(&course)
        && data_root
            .join("courses")
            .join(&course)
            .join("exercises")
            .join("package.json")
            .exists();
    let exercise: Vec<&'static ToolDef> = if has_exercises {
        vec![&exercise::WRITE, &exercise::RUN]
    } else {
        Vec::new()
    };
    let mut tools: Vec<&'static ToolDef> = match config.get("kind").and_then(Value::as_str) {
        Some("plan") => vec![
            &quiz::TOOL,
            &ask::TOOL,
            &curriculum::TOOL,
            &dispatch::RESEARCH,
        ],
        Some("practice") => {
            let mut list = vec![
                &quiz::TOOL,
                &ask::TOOL,
                &practice::PROBLEM,
                &practice::CONCLUDE,
                &dispatch::RESEARCH,
                &dispatch::VISUAL,
            ];
            list.extend(exercise);
            list
        }
        // Node's `default`: the lesson tools, whatever `kind` is missing or unknown.
        _ => {
            let mut list = vec![
                &quiz::TOOL,
                &ask::TOOL,
                &plan::TOOL,
                &node::TOOL,
                &dispatch::RESEARCH,
                &dispatch::VISUAL,
                &dispatch::DEMO,
            ];
            if js::truthy(config.get("ankiOptIn")) {
                list.push(&anki::TOOL);
            }
            list.extend(exercise);
            list.push(&finish::TOOL);
            list
        }
    };
    tools.extend([
        &lessontext::TOOL,
        &concepts::SEARCH,
        &concepts::OPEN,
        &concepts::BROWSE,
    ]);
    tools
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::tools::fake;

    /// A temp data root; `package` writes `courses/demo/exercises/package.json` like Node's fixture
    /// repo has it.
    fn data_root(package: bool) -> fake::TempDir {
        let dir = fake::temp_dir("registry");
        if package {
            let exercises = dir.join("courses").join("demo").join("exercises");
            fs::create_dir_all(&exercises).expect("create the exercises dir");
            fs::write(exercises.join("package.json"), "{\n  \"name\": \"demo\"\n}\n")
                .expect("write the exercises package");
        }
        dir
    }

    fn names(config: Value, data: &Path) -> Vec<&'static str> {
        let config = config.as_object().expect("a session config").clone();
        tools_for(&config, data)
            .into_iter()
            .map(|def| def.name)
            .collect()
    }

    #[test]
    fn selects_by_kind_anki_opt_in_and_exercises_package() {
        let with_package = data_root(true);
        assert_eq!(
            names(
                json!({"kind": "teach", "course": "demo", "ankiOptIn": false}),
                &with_package
            ),
            [
                "quiz",
                "ask",
                "propose_plan",
                "write_node",
                "research",
                "visual",
                "demo",
                "write_exercise",
                "run_exercise",
                "finish_lesson",
                "read_lesson",
                "search_concepts",
                "open_concept",
                "browse_concepts"
            ]
        );

        // The Anki tool sits right after `demo`, before the exercise pair.
        let with_anki = names(json!({"kind": "teach", "course": "demo", "ankiOptIn": true}), &with_package);
        assert_eq!(with_anki.len(), 15, "{with_anki:?}");
        let demo = with_anki
            .iter()
            .position(|name| *name == "demo")
            .expect("demo is listed");
        assert_eq!(with_anki.get(demo + 1), Some(&"sync_anki"), "{with_anki:?}");

        assert_eq!(
            names(json!({"kind": "plan", "course": "demo"}), &with_package),
            [
                "quiz",
                "ask",
                "propose_curriculum",
                "research",
                "read_lesson",
                "search_concepts",
                "open_concept",
                "browse_concepts"
            ]
        );
        assert_eq!(
            names(json!({"kind": "practice", "course": "demo"}), &with_package),
            [
                "quiz",
                "ask",
                "problem",
                "conclude",
                "research",
                "visual",
                "write_exercise",
                "run_exercise",
                "read_lesson",
                "search_concepts",
                "open_concept",
                "browse_concepts"
            ]
        );

        // No package: the exercise pair drops out, the rest keeps Node's order.
        let without = data_root(false);
        let teach = names(json!({"kind": "teach", "course": "demo"}), &without);
        assert!(!teach.contains(&"write_exercise"), "{teach:?}");
        assert!(!teach.contains(&"run_exercise"), "{teach:?}");
        assert_eq!(
            names(json!({"kind": "practice", "course": "demo"}), &without),
            [
                "quiz",
                "ask",
                "problem",
                "conclude",
                "research",
                "visual",
                "read_lesson",
                "search_concepts",
                "open_concept",
                "browse_concepts"
            ]
        );
    }

    #[test]
    fn missing_and_unknown_kinds_fall_into_the_lesson_tools() {
        let data = data_root(true);
        let teach = names(json!({"kind": "teach", "course": "demo"}), &data);
        assert_eq!(names(json!({"course": "demo"}), &data), teach);
        assert_eq!(names(json!({"kind": "nope", "course": "demo"}), &data), teach);
    }

    #[test]
    fn the_concept_tools_are_listed_for_every_session_kind() {
        let data = data_root(true);
        for config in [
            json!({"kind": "teach", "course": "demo"}),
            json!({"kind": "teach", "course": "lezioni-singole", "target": {"mode": "free"}}),
            json!({"kind": "plan", "course": "demo", "chapterTitle": "Demo"}),
            json!({"kind": "practice", "course": "demo"}),
        ] {
            let listed = names(config.clone(), &data);
            for name in ["search_concepts", "open_concept", "browse_concepts"] {
                assert!(listed.contains(&name), "{name} missing for {config}");
            }
        }
    }
}
