//! A fake `ToolHost` for the tool tests (port of `fakeContext` in `app/test/helpers.ts`).
//!
//! The data root and the repo root are one copied fixture folder, so a test may write lessons and
//! the graph without touching the repo; it is removed on drop.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use crate::data_root::resolve_path;
use crate::embeddings::EmbeddingConfig;
use crate::events::iso_now;
use crate::exercises::{Installs, NpmCommand, RunConfig};
use crate::js;
use crate::lesson::doc;
use crate::planstate;
use crate::semantic::SemanticSearch;
use crate::tools::util;
use crate::tools::{Cancelled, ToolDef, ToolHost, ToolOutcome, invoke};

/// A job a test started through the host, with what the tool asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedJob {
    pub kind: String,
    pub input: Value,
    pub title: String,
}

pub struct FakeHost {
    dir: TempDir,
    config: Value,
    lesson_path: Mutex<Option<String>>,
    plan: Mutex<Value>,
    answers: Mutex<VecDeque<Result<Value, Cancelled>>>,
    requests: Mutex<Vec<Value>>,
    items: Mutex<Vec<Value>>,
    events: Mutex<Vec<Value>>,
    progress: Mutex<Vec<Value>>,
    jobs: Mutex<Vec<RecordedJob>>,
    maps: Mutex<Vec<String>>,
    fail_jobs: Mutex<bool>,
    changed: Mutex<usize>,
    exercises: Mutex<Vec<Value>>,
    problem: Mutex<Option<(String, Value)>>,
    node: Mutex<Option<PathBuf>>,
    installs: Installs,
    anki_url: Mutex<Option<String>>,
    semantic: Mutex<Arc<SemanticSearch>>,
}

impl FakeHost {
    /// A fresh copy of `app/test/fixtures/repo`, used as both repo and data root.
    pub fn new(config: Value) -> FakeHost {
        let dir = temp_dir("host");
        copy_dir(&fixture_repo(), &dir);
        // No model in the temp repo: `rank` answers MODEL_MISSING, the worst case each tool handles.
        let semantic = Arc::new(SemanticSearch::for_repo(
            &dir.0,
            EmbeddingConfig::load(&dir.0).expect("embedding config"),
        ));
        FakeHost {
            dir,
            config,
            lesson_path: Mutex::new(None),
            plan: Mutex::new(Value::Null),
            answers: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            items: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            progress: Mutex::new(Vec::new()),
            jobs: Mutex::new(Vec::new()),
            maps: Mutex::new(Vec::new()),
            fail_jobs: Mutex::new(false),
            changed: Mutex::new(0),
            exercises: Mutex::new(Vec::new()),
            problem: Mutex::new(None),
            node: Mutex::new(None),
            installs: Installs::default(),
            // A closed port: no test ever reaches a real Anki.
            anki_url: Mutex::new(Some("http://127.0.0.1:1".to_string())),
            semantic: Mutex::new(semantic),
        }
    }

    /// A host with an approved plan and its lesson artifact: the state a finished `propose_plan`
    /// leaves (topic `"Demo topic"`, title `"Il primo passo"`, goal `"capire"`).
    pub fn with_lesson(config: Value, dag: &str) -> FakeHost {
        let host = FakeHost::new(config);
        let session = host.config();
        let course = js::js_string(session.get("course"));
        let base = host.dir.0.join("courses").join(&course);
        let lesson = doc::init_lesson(&doc::InitOptions {
            topic: "Demo topic".to_string(),
            title: Some("Il primo passo".to_string()),
            slug: None,
            description: None,
            dag: Some(dag.to_string()),
            target_goal: Some("capire".to_string()),
            base_dir: Some(base.to_string_lossy().into_owned()),
            lessons_dir: None,
            repo_root: repo_root(),
        })
        .expect("init the fake lesson");
        let rel = util::to_rel(&host.dir.0, Path::new(&lesson.file_path));
        *host.lesson_path.lock().expect("fake lock") = Some(rel);
        *host.plan.lock().expect("fake lock") =
            planstate::derive_plan_state("Demo topic", "capire", dag, true, 0, &[]);
        host
    }

    /// Node's `tools/call` path: validate, then run (the tool sees the cleaned data).
    pub async fn call(&self, def: &ToolDef, args: Value) -> ToolOutcome {
        match (def.validate)(&args) {
            Err(issues) => ToolOutcome::error(format!(
                "Invalid input: {}. Fix and call again.",
                issues.join("; ")
            )),
            Ok(data) => invoke(def, data, self).await,
        }
    }

    /// The next `request` returns this answer.
    pub fn queue_answer(&self, answer: Value) {
        self.answers
            .lock()
            .expect("fake lock")
            .push_back(Ok(answer));
    }

    /// The next `request` answers `Err(Cancelled)`, like Node's `new Error("cancelled")`.
    pub fn queue_cancel(&self) {
        self.answers
            .lock()
            .expect("fake lock")
            .push_back(Err(Cancelled));
    }

    pub fn set_fail_jobs(&self, fail: bool) {
        *self.fail_jobs.lock().expect("fake lock") = fail;
    }

    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().expect("fake lock").clone()
    }

    pub fn items(&self) -> Vec<Value> {
        self.items.lock().expect("fake lock").clone()
    }

    pub fn events(&self) -> Vec<Value> {
        self.events.lock().expect("fake lock").clone()
    }

    pub fn progress(&self) -> Vec<Value> {
        self.progress.lock().expect("fake lock").clone()
    }

    pub fn jobs(&self) -> Vec<RecordedJob> {
        self.jobs.lock().expect("fake lock").clone()
    }

    /// The map jobs started by `start_map`, by target.
    pub fn maps(&self) -> Vec<String> {
        self.maps.lock().expect("fake lock").clone()
    }

    pub fn courses_changed_count(&self) -> usize {
        *self.changed.lock().expect("fake lock")
    }

    /// The node binary `run_config` hands to `run_exercise` (`None` is Node's missing-Node case).
    pub fn set_node(&self, node: Option<PathBuf>) {
        *self.node.lock().expect("fake lock") = node;
    }

    pub fn set_anki_url(&self, url: Option<String>) {
        *self.anki_url.lock().expect("fake lock") = url;
    }

    pub fn set_semantic(&self, semantic: Arc<SemanticSearch>) {
        *self.semantic.lock().expect("fake lock") = semantic;
    }
}

impl ToolHost for FakeHost {
    fn data_root(&self) -> &Path {
        &self.dir.0
    }

    fn repo_root(&self) -> &Path {
        &self.dir.0
    }

    fn config(&self) -> Map<String, Value> {
        self.config.as_object().cloned().unwrap_or_default()
    }

    fn lesson_path(&self) -> Option<String> {
        self.lesson_path.lock().expect("fake lock").clone()
    }

    fn plan(&self) -> Value {
        self.plan.lock().expect("fake lock").clone()
    }

    fn set_plan(&self, plan: Value) {
        *self.plan.lock().expect("fake lock") = plan.clone();
        self.events
            .lock()
            .expect("fake lock")
            .push(json!({ "type": "plan", "plan": plan }));
    }

    fn set_lesson(&self, lesson_path: String, node_id: Option<String>) {
        *self.lesson_path.lock().expect("fake lock") = Some(lesson_path.clone());
        let mut event = Map::new();
        event.insert("type".to_string(), json!("lesson"));
        event.insert("lessonPath".to_string(), json!(lesson_path));
        if let Some(node_id) = node_id {
            event.insert("nodeId".to_string(), json!(node_id));
        }
        self.events
            .lock()
            .expect("fake lock")
            .push(Value::Object(event));
    }

    fn exercises(&self) -> Vec<Value> {
        self.exercises.lock().expect("fake lock").clone()
    }

    fn set_exercises(&self, list: Vec<Value>) {
        *self.exercises.lock().expect("fake lock") = list.clone();
        self.events
            .lock()
            .expect("fake lock")
            .push(json!({ "type": "exercises", "exercises": list }));
    }

    fn upsert_exercise(&self, info: Value) {
        let name = js::js_string(info.get("name"));
        let mut exercises = self.exercises.lock().expect("fake lock");
        let mut list: Vec<Value> = exercises
            .clone()
            .into_iter()
            .filter(|entry| js::js_string(entry.get("name")) != name)
            .collect();
        list.push(info.clone());
        list.sort_by(|left, right| {
            js::locale_compare(
                &js::js_string(left.get("name")),
                &js::js_string(right.get("name")),
            )
        });
        *exercises = list.clone();
        drop(exercises);
        self.events
            .lock()
            .expect("fake lock")
            .push(json!({ "type": "exercises", "exercises": list }));
        self.emit(json!({
            "id": format!("exercise:{name}"),
            "kind": "exercise",
            "exercise": info,
        }));
    }

    fn open_problem(&self, item: Value) -> Option<String> {
        let mut problem = self.problem.lock().expect("fake lock");
        if problem.is_some() {
            return None;
        }
        let input = item.get("input").cloned().unwrap_or(Value::Null);
        let id = self.emit(item);
        *problem = Some((id.clone(), input));
        Some(id)
    }

    fn take_problem(&self) -> Option<(String, Value)> {
        self.problem.lock().expect("fake lock").take()
    }

    fn request(&self, item: Value) -> Result<oneshot::Receiver<Result<Value, Cancelled>>, Cancelled> {
        self.requests.lock().expect("fake lock").push(item);
        let answer = self
            .answers
            .lock()
            .expect("fake lock")
            .pop_front()
            .expect("fakeContext: no answer queued");
        if answer.is_err() {
            return Err(Cancelled);
        }
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(answer);
        Ok(rx)
    }

    fn emit(&self, item: Value) -> String {
        let mut items = self.items.lock().expect("fake lock");
        let id = match item.get("id") {
            Some(id) if !id.is_null() => js::js_string(Some(id)),
            _ => format!("item-{}", items.len() + 1),
        };
        items.push(item);
        id
    }

    fn log_progress(&self, record: Map<String, Value>) -> Result<(), String> {
        self.progress
            .lock()
            .expect("fake lock")
            .push(Value::Object(record.clone()));
        let config = self.config();
        let course = js::js_string(config.get("course"));
        let mut full = record;
        full.insert("at".to_string(), json!(iso_now()));
        full.insert("sessionId".to_string(), json!("test-session"));
        full.insert("course".to_string(), json!(course));
        let dir = self.dir.0.join("courses").join(&course);
        let mut line = serde_json::to_string(&Value::Object(full)).expect("a record serialises");
        line.push('\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("progress.jsonl"))
            .map_err(|error| error.to_string())?;
        file.write_all(line.as_bytes()).map_err(|error| error.to_string())
    }

    fn start_job(&self, kind: &str, input: Value, title: &str) -> Result<String, String> {
        if *self.fail_jobs.lock().expect("fake lock") {
            return Err(crate::tools::NO_JOBS_MESSAGE.to_string());
        }
        let mut jobs = self.jobs.lock().expect("fake lock");
        jobs.push(RecordedJob {
            kind: kind.to_string(),
            input,
            title: title.to_string(),
        });
        Ok(format!("job-{}", jobs.len()))
    }

    fn start_map(&self, target: &str) -> Result<(), String> {
        if *self.fail_jobs.lock().expect("fake lock") {
            return Err(crate::tools::NO_JOBS_MESSAGE.to_string());
        }
        self.maps
            .lock()
            .expect("fake lock")
            .push(target.to_string());
        Ok(())
    }

    fn run_config(&self) -> RunConfig {
        let node = self.node.lock().expect("fake lock").clone();
        RunConfig::new(node, NpmCommand::Default)
    }

    fn installs(&self) -> Installs {
        self.installs.clone()
    }

    fn anki_url(&self) -> Option<String> {
        self.anki_url.lock().expect("fake lock").clone()
    }

    fn semantic(&self) -> Arc<SemanticSearch> {
        self.semantic.lock().expect("fake lock").clone()
    }

    fn courses_changed(&self) {
        *self.changed.lock().expect("fake lock") += 1;
    }
}

/// A temp directory that removes itself on drop (the crate's test convention).
pub struct TempDir(pub PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

pub fn temp_dir(tag: &str) -> TempDir {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "proxi-tool-{tag}-{}-{nanos}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create temp dir");
    TempDir(path)
}

fn repo_root() -> PathBuf {
    resolve_path(&Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."))
}

fn fixture_repo() -> PathBuf {
    repo_root().join("app").join("test").join("fixtures").join("repo")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create the copy dir");
    for entry in fs::read_dir(from).expect("read the fixture repo") {
        let entry = entry.expect("a fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a fixture file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAG: &str = "graph TD\n  N1[\"Nodo 1: la prima cosa\"] --> N2[\"Nodo 2: la seconda\"]";

    #[test]
    fn with_lesson_seeds_the_artifact_and_the_approved_plan() {
        let host = FakeHost::with_lesson(
            json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false}),
            DAG,
        );
        assert_eq!(
            host.lesson_path().as_deref(),
            Some("courses/demo/lessons/0001-il-primo-passo.html")
        );
        let lesson = util::current_lesson(&host).expect("the lesson reads back");
        assert_eq!(lesson.summary.slug_str(), "il-primo-passo");
        assert_eq!(host.plan()["approved"], json!(true));
        assert_eq!(host.plan()["nodeStatus"]["N1"], json!("current"));
    }

    #[tokio::test]
    async fn the_fake_records_answers_progress_events_and_jobs() {
        let host = FakeHost::new(
            json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false}),
        );

        host.queue_answer(json!({"selected": ["a"]}));
        let answer = host
            .request(json!({"kind": "ask"}))
            .expect("a queued answer")
            .await
            .expect("the sender is alive");
        assert_eq!(answer, Ok(json!({"selected": ["a"]})));
        assert_eq!(host.requests(), vec![json!({"kind": "ask"})]);

        host.queue_cancel();
        assert_eq!(host.request(json!({"kind": "ask"})).err(), Some(Cancelled));

        host.set_plan(json!({"topic": "t"}));
        host.set_lesson("courses/demo/lessons/0001-x.html".to_string(), Some("n1".to_string()));
        assert_eq!(
            host.events(),
            vec![
                json!({"type": "plan", "plan": {"topic": "t"}}),
                json!({"type": "lesson", "lessonPath": "courses/demo/lessons/0001-x.html", "nodeId": "n1"}),
            ]
        );

        host.log_progress(Map::from_iter([
            ("kind".to_string(), json!("probe")),
            ("question".to_string(), json!("Q?")),
        ]))
        .expect("append the progress line");
        let raw = fs::read_to_string(host.data_root().join("courses/demo/progress.jsonl"))
            .expect("the progress file");
        let record: Value = serde_json::from_str(raw.trim()).expect("one JSON line");
        let keys: Vec<&String> = record.as_object().expect("an object").keys().collect();
        assert_eq!(keys, ["kind", "question", "at", "sessionId", "course"]);
        assert_eq!(record["sessionId"], json!("test-session"));
        assert_eq!(record["course"], json!("demo"));
        assert!(host.progress()[0].get("at").is_none(), "the recorded copy has no at");

        assert_eq!(host.start_job("demo", json!({"title": "x"}), "Demo: x"), Ok("job-1".to_string()));
        host.set_fail_jobs(true);
        assert_eq!(
            host.start_job("demo", json!({}), "Demo: x"),
            Err(crate::tools::NO_JOBS_MESSAGE.to_string())
        );
        assert_eq!(
            host.start_map("demo/0001-x"),
            Err(crate::tools::NO_JOBS_MESSAGE.to_string())
        );

        host.courses_changed();
        assert_eq!(host.courses_changed_count(), 1);
    }

    #[test]
    fn one_open_problem_at_a_time_and_take_returns_it() {
        let host = FakeHost::new(json!({"kind": "practice", "course": "demo"}));
        let item = json!({"kind": "problem", "input": {"statement": "S", "mode": "recall"}});
        assert_eq!(host.open_problem(item.clone()).as_deref(), Some("item-1"));
        assert_eq!(host.open_problem(item), None);
        assert_eq!(
            host.take_problem(),
            Some(("item-1".to_string(), json!({"statement": "S", "mode": "recall"})))
        );
        assert_eq!(host.take_problem(), None);
        assert_eq!(host.items().len(), 1);
    }

    #[test]
    fn set_exercises_stores_the_list_and_notifies() {
        let host = FakeHost::new(json!({"kind": "practice", "course": "demo"}));
        let list = vec![json!({"name": "01-demo"})];
        host.set_exercises(list.clone());
        assert_eq!(host.exercises(), list);
        assert_eq!(
            host.events(),
            vec![json!({"type": "exercises", "exercises": list})]
        );
    }

    #[test]
    fn the_fake_runs_on_a_closed_anki_port_and_no_node() {
        let host = FakeHost::new(json!({"kind": "teach", "course": "demo"}));
        assert_eq!(host.anki_url().as_deref(), Some("http://127.0.0.1:1"));
        assert!(host.run_config().node.is_none());
        assert_eq!(
            host.semantic().rank("x", &[]).unwrap_err(),
            crate::semantic::MODEL_MISSING
        );
    }
}
