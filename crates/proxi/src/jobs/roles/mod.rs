//! Job roles (port of app/server/jobs/roles/).
//!
//! A role is data, like a tool: the job manager builds the engine launch from it, and its tools are
//! ordinary `ToolDef`s served by the job MCP endpoint (port of `JobRole`, `app/server/types.ts:198-212`).

use std::fmt::Display;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::engine::Grant;
use crate::graph::store::GraphStore;
use crate::js;
use crate::tools::ToolDef;

pub mod demo;
pub mod research;
pub mod visual;

/// What a role may read when it writes its prompt and first message: the code (the prompt files) and
/// the learner's data (lessons, graph).
pub struct RoleEnv<'a> {
    pub code_root: &'a Path,
    pub data_root: &'a Path,
    pub graph: &'a GraphStore,
}

/// One subagent kind. research/visual/demo live in `roles/`, map in `graph::extract`.
pub struct Role {
    pub kind: &'static str,
    /// Built-in capabilities; `[]` = only the MCP tools. The model comes from `engine::model_for`.
    pub grant: &'static [Grant],
    /// Tool steps before the job's turn is cut.
    pub max_turns: u32,
    pub system_prompt: fn(&RoleEnv, &Value) -> Result<String, String>,
    /// The single user turn the job starts with.
    pub first_message: fn(&RoleEnv, &mut Value) -> String,
    pub tools: fn(&Value) -> Vec<&'static ToolDef>,
    /// Called once when the job settles in any state: drops per-job in-memory state (drafts).
    pub cleanup: fn(&str),
}

/// Node's `readPrompt`: `<code_root>/app/prompts/subagents/<name>`, trimmed.
pub fn read_prompt(code_root: &Path, name: &str) -> Result<String, String> {
    let path = code_root
        .join("app")
        .join("prompts")
        .join("subagents")
        .join(name);
    fs::read_to_string(&path)
        .map(|text| js::js_trim(&text).to_string())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// A role with no per-job state to drop.
pub fn no_cleanup(_: &str) {}

/// Node's `errorText`: an `Error`'s message, else `String(e)`.
pub fn error_text(e: impl Display) -> String {
    e.to_string()
}

/// A `ToolHost` that is also the job: what every role test needs and no session has.
#[cfg(test)]
pub(crate) mod fake {
    #![allow(dead_code)]

    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use serde_json::{Map, Value};
    use tokio::sync::oneshot;

    use crate::events::EventBus;
    use crate::graph::store::GraphStore;
    use crate::jobs::render::Renderer;
    use crate::tools::{Cancelled, JobContext, NO_JOBS_MESSAGE, ToolHost};

    /// The job side of a tool call, with what the tool did to it.
    pub(crate) struct FakeJob {
        job_id: String,
        input: Value,
        graph: Arc<GraphStore>,
        events: EventBus,
        activities: Mutex<Vec<String>>,
        finished: Mutex<Option<Value>>,
        failed: Mutex<Option<String>>,
        renderer: Option<Arc<Renderer>>,
    }

    impl FakeJob {
        /// A job over `data_root`; the tool sees `input` as its job input.
        pub fn new(job_id: &str, input: Value, data_root: &Path) -> Self {
            FakeJob {
                job_id: job_id.to_string(),
                input,
                graph: Arc::new(GraphStore::new(data_root.to_path_buf())),
                events: EventBus::new(),
                activities: Mutex::new(Vec::new()),
                finished: Mutex::new(None),
                failed: Mutex::new(None),
                renderer: None,
            }
        }

        /// The same job with a browser to render with (the role browser tests).
        pub fn with_renderer(mut self, renderer: Arc<Renderer>) -> Self {
            self.renderer = Some(renderer);
            self
        }

        pub fn activities(&self) -> Vec<String> {
            self.activities.lock().expect("fake job lock").clone()
        }

        pub fn finished(&self) -> Option<Value> {
            self.finished.lock().expect("fake job lock").clone()
        }

        pub fn failed(&self) -> Option<String> {
            self.failed.lock().expect("fake job lock").clone()
        }
    }

    impl ToolHost for FakeJob {
        fn data_root(&self) -> &Path {
            self.graph.data_root()
        }

        fn repo_root(&self) -> &Path {
            self.graph.data_root()
        }

        fn config(&self) -> Map<String, Value> {
            Map::new()
        }

        fn lesson_path(&self) -> Option<String> {
            None
        }

        fn plan(&self) -> Value {
            Value::Null
        }

        fn set_plan(&self, _plan: Value) {}

        fn set_lesson(&self, _lesson_path: String, _node_id: Option<String>) {}

        fn request(
            &self,
            _item: Value,
        ) -> Result<oneshot::Receiver<Result<Value, Cancelled>>, Cancelled> {
            Err(Cancelled)
        }

        fn emit(&self, _item: Value) -> String {
            format!("job-item-{}", self.job_id)
        }

        fn log_progress(&self, _record: Map<String, Value>) -> Result<(), String> {
            Ok(())
        }

        fn start_job(&self, _kind: &str, _input: Value, _title: &str) -> Result<String, String> {
            Err(NO_JOBS_MESSAGE.to_string())
        }

        fn start_map(&self, _target: &str) -> Result<(), String> {
            Err(NO_JOBS_MESSAGE.to_string())
        }

        fn courses_changed(&self) {}

        fn job(&self) -> Option<&dyn JobContext> {
            Some(self)
        }
    }

    impl JobContext for FakeJob {
        fn job_id(&self) -> &str {
            &self.job_id
        }

        fn input(&self) -> Value {
            self.input.clone()
        }

        fn set_activity(&self, text: &str) {
            self.activities
                .lock()
                .expect("fake job lock")
                .push(text.to_string());
        }

        fn finish(&self, result: Value) {
            *self.finished.lock().expect("fake job lock") = Some(result);
        }

        fn fail(&self, error: &str) {
            *self.failed.lock().expect("fake job lock") = Some(error.to_string());
        }

        fn graph(&self) -> &Arc<GraphStore> {
            &self.graph
        }

        fn events(&self) -> &EventBus {
            &self.events
        }

        fn renderer(&self) -> Option<&Renderer> {
            self.renderer.as_deref()
        }
    }
}

#[cfg(test)]
pub(crate) use fake::FakeJob;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::tools::JobContext;

    use super::*;

    fn code_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[test]
    fn read_prompt_reads_a_subagent_prompt_trimmed() {
        let raw = fs::read_to_string(code_root().join("app/prompts/subagents/research.md"))
            .expect("the prompt file exists");
        let prompt = read_prompt(&code_root(), "research.md").expect("read the prompt");

        assert_eq!(prompt, raw.trim());
        assert!(prompt.starts_with("# Role: researcher"));
    }

    #[test]
    fn fake_job_records_what_the_tool_did() {
        let dir = crate::tools::fake::temp_dir("fake-job");
        let job = FakeJob::new("j1", serde_json::json!({"source": "c/l"}), &dir);

        assert_eq!(job.job_id(), "j1");
        assert_eq!(job.input()["source"], serde_json::json!("c/l"));

        job.set_activity("consegna la ricerca");
        job.finish(serde_json::json!({"kind": "research"}));
        assert_eq!(job.activities(), ["consegna la ricerca"]);
        assert_eq!(job.finished(), Some(serde_json::json!({"kind": "research"})));
        assert_eq!(job.failed(), None);

        job.fail("boom");
        assert_eq!(job.failed(), Some("boom".to_string()));
    }

    #[test]
    fn read_prompt_errors_on_a_missing_name() {
        let error = read_prompt(&code_root(), "nope.md").expect_err("a missing prompt is an error");

        assert!(error.contains("nope.md"), "{error}");
    }
}
