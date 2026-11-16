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
//! The job tools come from their role's own registry, same recipe:
//!
//! ```text
//! node --import tsx --input-type=module -e "import {toJsonSchema} from './app/server/mcp.ts'; import {researchRole} from './app/server/jobs/roles/research.ts'; import {visualRole} from './app/server/jobs/roles/visual.ts'; import {demoRole} from './app/server/jobs/roles/demo.ts'; import {makeMapRole} from './app/server/graph/extract.ts'; const all = [...researchRole.tools({}), ...visualRole.tools({}), ...demoRole.tools({title:'t'}), ...makeMapRole(process.cwd()).tools({source:'c/l'})]; const t = all.find((x) => x.name === process.argv[1]); process.stdout.write(JSON.stringify(toJsonSchema(t.schema), null, 2) + '\n')" <tool-name> > crates/proxi/src/tools/schemas/<tool-name>.json
//! ```
//!
//! The validator's messages are zod 4's own: get one with the same imports, `t.schema.safeParse(<input>).error.issues`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::events::EventBus;
use crate::graph::store::GraphStore;

pub mod ask;
pub mod board;
pub mod dispatch;
pub mod finish;
pub mod lessontext;
pub mod node;
pub mod quiz;
pub mod util;

#[cfg(test)]
pub(crate) mod fake;

/// One image a tool answers with, after its text (Node's `ToolOutcome["images"]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolImage {
    pub data: String,
    pub mime_type: String,
}

/// What a tool answers: MCP text content, the `isError` flag, and any images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
    pub images: Vec<ToolImage>,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
            images: Vec::new(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            images: Vec::new(),
        }
    }

    /// Node's `images`: MCP appends one `image` content entry per image, after the text.
    pub fn with_images(mut self, images: Vec<ToolImage>) -> Self {
        self.images = images;
        self
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
    fn courses_changed(&self);
    /// The job the calling tool runs in; `None` for a session host (Node's `ToolContext` vs
    /// `JobContext` union).
    fn job(&self) -> Option<&dyn JobContext> {
        None
    }
}

/// What a job tool may touch: Node's `JobContext` (`types.ts:178-186`), implemented by the job
/// manager. Sync methods only, like `ToolHost`, so the trait stays object-safe.
pub trait JobContext: Send + Sync {
    fn job_id(&self) -> &str;
    /// The job's input, from its record.
    fn input(&self) -> Value;
    /// Updates `JobRecord.activity`, shown live in the activity dock.
    fn set_activity(&self, text: &str);
    /// Delivers the result: the job becomes `done`; its process is stopped after the turn.
    fn finish(&self, result: Value);
    /// Settles the job as `error` with that text.
    fn fail(&self, error: &str);
    /// The `Arc` itself, so a tool can clone it into `spawn_blocking`.
    fn graph(&self) -> &Arc<GraphStore>;
    fn events(&self) -> &EventBus;
    /// The server's headless browser, for the visual and demo roles; `None` where rendering is
    /// not wired (tests, a build without jobs).
    fn renderer(&self) -> Option<&crate::jobs::render::Renderer> {
        None
    }
}

/// The background job seam: implemented by the session (it owns the job's session id).
pub trait JobStarter: Send + Sync {
    fn start(&self, kind: &str, input: Value, title: &str, session_id: &str) -> Result<String, String>;
    fn start_map(&self, target: &str, session_id: &str) -> Result<(), String>;
    /// The record of that job, `None` when unknown.
    fn get(&self, _id: &str) -> Option<Value> {
        None
    }
    /// The session's records, newest first.
    fn list_for(&self, _session_id: &str) -> Vec<Value> {
        Vec::new()
    }
    /// The finished result was handed to the owning session's teacher.
    fn mark_delivered(&self, _id: &str) {}
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

/// The tools a session of this kind may call, in Node's order (`toolsFor` in `tools/index.ts`),
/// minus the ones this build does not serve (D11: they are not listed at all).
///
/// `data_root` is where the exercise tools would look for their package: no such tool is served
/// yet, so it is only passed through for the day they land.
pub fn tools_for(config: &Map<String, Value>, _data_root: &Path) -> Vec<&'static ToolDef> {
    let lesson: Vec<&'static ToolDef> = vec![
        &quiz::TOOL,
        &ask::TOOL,
        &node::TOOL,
        &dispatch::RESEARCH,
        &dispatch::VISUAL,
        &dispatch::DEMO,
        &finish::TOOL,
        &lessontext::TOOL,
    ];
    match config.get("kind").and_then(Value::as_str) {
        // `[quiz, ask, propose_curriculum, research]` plus the shared `read_lesson`.
        Some("plan") => vec![
            &quiz::TOOL,
            &ask::TOOL,
            &dispatch::RESEARCH,
            &lessontext::TOOL,
        ],
        // `[quiz, ask, problem, conclude, research, visual, …]` plus the shared `read_lesson`.
        Some("practice") => vec![
            &quiz::TOOL,
            &ask::TOOL,
            &dispatch::RESEARCH,
            &dispatch::VISUAL,
            &lessontext::TOOL,
        ],
        // Node's `default`: the lesson tools, whatever `kind` is missing or unknown.
        _ => lesson,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn names(config: Value) -> Vec<&'static str> {
        let config = config.as_object().expect("a session config").clone();
        tools_for(&config, Path::new("/data"))
            .into_iter()
            .map(|def| def.name)
            .collect()
    }

    #[test]
    fn the_registry_is_nodes_list_without_the_out_of_scope_tools() {
        // The lesson tools of `toolsOf`, in Node's order, then the shared `read_lesson`.
        assert_eq!(
            names(json!({"kind": "teach", "course": "demo"})),
            [
                "quiz",
                "ask",
                "write_node",
                "research",
                "visual",
                "demo",
                "finish_lesson",
                "read_lesson"
            ]
        );
        // A practice session has no write_node, demo or finish_lesson in Node either.
        assert_eq!(
            names(json!({"kind": "practice", "course": "demo"})),
            ["quiz", "ask", "research", "visual", "read_lesson"]
        );
        // The plan tools: propose_curriculum is out of scope, read_lesson is shared.
        assert_eq!(
            names(json!({"kind": "plan", "course": "demo"})),
            ["quiz", "ask", "research", "read_lesson"]
        );
        // A missing kind falls into Node's `default` branch.
        assert_eq!(names(json!({})), names(json!({"kind": "teach"})));
        assert_eq!(names(json!({"kind": "nope"})), names(json!({"kind": "teach"})));
    }
}
