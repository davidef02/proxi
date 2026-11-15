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

use serde_json::{Map, Value};
use tokio::sync::oneshot;

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
    /// Pushes the interactive card and waits for the learner's answer.
    fn request(&self, item: Value) -> Result<oneshot::Receiver<Result<Value, Cancelled>>, Cancelled>;
    /// Pushes or upserts a non-blocking item; returns its id.
    fn emit(&self, item: Value) -> String;
    fn log_progress(&self, record: Map<String, Value>);
    /// Starts a background job (`demo`, `visual`, `research`); returns its id.
    fn start_job(&self, kind: &str, input: Value, title: &str) -> Result<String, String>;
    /// Starts the map job for a lesson (`<course>/<file stem>`).
    fn start_map(&self, target: &str) -> Result<(), String>;
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
