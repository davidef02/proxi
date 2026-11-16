//! Background subagents (port of app/server/jobs/index.ts).

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::claude::{ClaudeBin, ClaudeLaunch, McpServer};
use crate::engine::{EngineEvent, EngineHandle, EngineMsg};
use crate::events::{self, EventBus};
use crate::graph::store::GraphStore;
use crate::jobs::render::Renderer;
use crate::jobs::roles::{Role, RoleEnv};
use crate::js;
use crate::limits::LimitsCache;
use crate::protocol::GlobalEvent;
use crate::tools::{Cancelled, JobContext, JobStarter, ToolDef, ToolHost};

pub mod deliver;
pub mod render;
pub mod routes;
pub mod roles;
pub mod sanitize;

const RETENTION_MS: i64 = 30 * 24 * 3600 * 1000;

/// Howard Hinnant's inverse `days_from_civil`: (year, month, day) -> days since 1970-01-01.
fn days_from_civil(mut y: i64, m: i64, d: i64) -> i64 {
    if m <= 2 {
        y -= 1;
    }
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Private ISO-8601 -> ms parser (`YYYY-MM-DDTHH:MM:SS(.mmm)?(Z)?`). Unparsable = None.
fn parse_iso_to_millis(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b't') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&min)
        || !(0..=60).contains(&sec)
    {
        return None;
    }

    let mut millis: i64 = 0;
    if s.len() > 19 && b[19] == b'.' {
        let rest = &s[20..];
        let digit_count = rest.bytes().take_while(|c| c.is_ascii_digit()).count();
        if digit_count > 0 {
            let frac_str = &rest[..digit_count];
            let frac_val: i64 = frac_str.parse().ok()?;
            millis = match digit_count {
                1 => frac_val * 100,
                2 => frac_val * 10,
                3 => frac_val,
                _ => frac_str[..3].parse().ok()?,
            };
        }
    }

    let days = days_from_civil(year, month, day);
    let total_secs = days * 86_400 + hour * 3600 + min * 60 + sec;
    Some(total_secs * 1000 + millis)
}

fn trunc(s: &str, n: usize) -> String {
    if crate::sessions::utf16_len(s) > n {
        crate::vitest::truncate_utf16(s, n - 1) + "\u{2026}"
    } else {
        s.to_string()
    }
}

fn activity_of(name: &str, input: Option<&Value>) -> String {
    if name == "WebSearch" {
        let query = input
            .and_then(|inp| inp.get("query"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return trunc(&format!("cerca: {query}"), 120);
    }
    if name == "WebFetch" {
        let url = input
            .and_then(|inp| inp.get("url"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return trunc(&format!("legge: {url}"), 120);
    }
    let clean = name.strip_prefix("mcp__job__").unwrap_or(name);
    clean.to_string()
}

fn persist(repo_root: &Path, rec: &Map<String, Value>) {
    let Some(id) = rec.get("id").and_then(Value::as_str) else {
        return;
    };
    let dir = repo_root.join(".learner").join("jobs");
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("[jobs] save failed: {e}");
        return;
    }
    match serde_json::to_string(rec) {
        Ok(json_str) => {
            if let Err(e) = fs::write(dir.join(format!("{id}.json")), json_str) {
                eprintln!("[jobs] save failed: {e}");
            }
        }
        Err(e) => eprintln!("[jobs] save failed: {e}"),
    }
}

fn slim(rec: &Map<String, Value>) -> Map<String, Value> {
    let mut out = rec.clone();
    out.shift_remove("result");
    out
}

fn role(kind: &str) -> Option<&'static Role> {
    match kind {
        "research" => Some(&roles::research::ROLE),
        "visual" => Some(&roles::visual::ROLE),
        "demo" => Some(&roles::demo::ROLE),
        "map" => Some(&crate::graph::extract::MAP_ROLE),
        _ => None,
    }
}

#[derive(Clone)]
pub struct JobManagerOptions {
    pub repo_root: PathBuf,
    pub data_root: PathBuf,
    pub code_root: PathBuf,
    pub port: u16,
    pub token: String,
    pub claude: ClaudeBin,
    pub agy: ClaudeBin,
    pub opencode: ClaudeBin,
    pub events: EventBus,
    pub limits: Arc<LimitsCache>,
    pub graph: Arc<GraphStore>,
    pub renderer: Arc<Renderer>,
    pub max_concurrent: usize,
    pub job_timeout: Duration,
}

struct Entry {
    rec: Map<String, Value>,
    input: Value,
    provider: String,
    handle: Option<Box<dyn EngineHandle>>,
    claimed: bool,
    result: Option<Value>,
    settled: bool,
    stderr: VecDeque<String>,
    deadline: Option<tokio::task::JoinHandle<()>>,
}

struct State {
    entries: IndexMap<String, Entry>,
    queue: Vec<String>,
    closed: bool,
    live: HashMap<u64, Option<u32>>,
    inflight: HashMap<String, String>,
}

pub struct JobManager {
    opts: JobManagerOptions,
    state: Mutex<State>,
    callback: Mutex<Option<Arc<dyn Fn(Value) + Send + Sync>>>,
    me: Weak<JobManager>,
}

struct LiveTicketGuard<'a> {
    state: &'a Mutex<State>,
    ticket: u64,
}

impl<'a> Drop for LiveTicketGuard<'a> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.live.remove(&self.ticket);
        }
    }
}

impl JobManager {
    pub fn new(opts: JobManagerOptions) -> Arc<Self> {
        let dir = opts.repo_root.join(".learner").join("jobs");
        let mut entries = IndexMap::new();

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        if dir.exists() {
            if let Ok(rd) = fs::read_dir(&dir) {
                for item in rd.flatten() {
                    let file_name = item.file_name().to_string_lossy().into_owned();
                    if !file_name.ends_with(".json") {
                        continue;
                    }
                    let path = item.path();
                    let Ok(raw) = fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(Value::Object(mut rec)) = serde_json::from_str(&raw) else {
                        continue;
                    };
                    let Some(id) = rec.get("id").and_then(Value::as_str).map(str::to_string) else {
                        continue;
                    };
                    let status = rec.get("status").and_then(Value::as_str).unwrap_or("");
                    let updated_at = rec.get("updatedAt").and_then(Value::as_str).unwrap_or("");

                    if status != "running" {
                        if let Some(parsed_ms) = parse_iso_to_millis(updated_at) {
                            if now_ms - parsed_ms > RETENTION_MS {
                                let _ = fs::remove_file(&path);
                                let _ = fs::remove_dir_all(dir.join(&id));
                                continue;
                            }
                        }
                    }

                    if status == "running" {
                        rec.insert("status".to_string(), Value::String("error".to_string()));
                        rec.insert(
                            "error".to_string(),
                            Value::String("interrotto alla chiusura dell'app".to_string()),
                        );
                        rec.insert("activity".to_string(), Value::String("".to_string()));
                        persist(&opts.repo_root, &rec);
                    }

                    let result = rec.get("result").cloned();
                    let entry = Entry {
                        rec,
                        input: Value::Null,
                        provider: "claude".to_string(),
                        handle: None,
                        claimed: false,
                        result,
                        settled: true,
                        stderr: VecDeque::new(),
                        deadline: None,
                    };
                    entries.insert(id, entry);
                }
            }
        }

        Arc::new_cyclic(|me| Self {
            opts,
            state: Mutex::new(State {
                entries,
                queue: Vec::new(),
                closed: false,
                live: HashMap::new(),
                inflight: HashMap::new(),
            }),
            callback: Mutex::new(None),
            me: me.clone(),
        })
    }

    pub fn bind_sessions(&self, f: Arc<dyn Fn(Value) + Send + Sync>) {
        *self.callback.lock().expect("callback lock") = Some(f);
    }

    fn changed_locked(&self, entry: &mut Entry, closed: bool) -> (Value, Option<Value>) {
        entry.rec.insert("updatedAt".to_string(), Value::String(events::iso_now()));
        persist(&self.opts.repo_root, &entry.rec);
        let slim_val = Value::Object(slim(&entry.rec));
        let full_clone = if entry.rec.contains_key("sessionId") && !closed {
            Some(Value::Object(entry.rec.clone()))
        } else {
            None
        };
        (slim_val, full_clone)
    }

    fn fire_changed(&self, slim: Value, full: Option<Value>) {
        self.opts.events.emit(&GlobalEvent::Job { job: slim });
        if let Some(full_rec) = full {
            let cb = self.callback.lock().expect("callback lock").clone();
            if let Some(cb) = cb {
                cb(full_rec);
            }
        }
    }

    pub fn start(
        &self,
        kind: &str,
        input: Value,
        title: &str,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        self.start_inner(kind, input, title, session_id, None)
    }

    fn start_inner(
        &self,
        kind: &str,
        input: Value,
        title: &str,
        session_id: Option<&str>,
        map_source: Option<&str>,
    ) -> Result<Value, String> {
        let _role = role(kind).ok_or_else(|| format!("Subagente sconosciuto: {kind}"))?;
        let settings = crate::settings::read_settings(&self.opts.repo_root);
        let role_target = if kind == "map" { "map" } else { "subagent" };
        let model = crate::engine::model_for(&settings, &settings.provider, role_target, None);
        let t = events::iso_now();
        let id = crate::engine::uuid_v4();

        let mut rec = Map::new();
        rec.insert("id".to_string(), Value::String(id.clone()));
        rec.insert("kind".to_string(), Value::String(kind.to_string()));
        rec.insert("title".to_string(), Value::String(title.to_string()));
        if let Some(sess) = session_id {
            rec.insert("sessionId".to_string(), Value::String(sess.to_string()));
        }
        rec.insert("model".to_string(), Value::String(model));
        rec.insert("status".to_string(), Value::String("running".to_string()));
        rec.insert("activity".to_string(), Value::String("in coda".to_string()));
        rec.insert("createdAt".to_string(), Value::String(t.clone()));
        rec.insert("updatedAt".to_string(), Value::String(t));
        rec.insert("costUsd".to_string(), crate::js::number(0.0));

        let entry = Entry {
            rec: rec.clone(),
            input,
            provider: settings.provider,
            handle: None,
            claimed: false,
            result: None,
            settled: false,
            stderr: VecDeque::new(),
            deadline: None,
        };

        let (slim_event, full_clone) = {
            let mut state = self.state.lock().expect("state lock");
            if state.closed {
                return Err("L'app si sta chiudendo".to_string());
            }
            if let Some(source) = map_source {
                if state.inflight.values().any(|s| s == source) {
                    return Err("Map source already in flight".to_string());
                }
                state.inflight.insert(id.clone(), source.to_string());
            }
            state.entries.insert(id.clone(), entry);
            state.queue.push(id.clone());
            let closed = state.closed;
            let e = state.entries.get_mut(&id).unwrap();
            self.changed_locked(e, closed)
        };

        self.fire_changed(slim_event, full_clone);
        self.pump();

        Ok(Value::Object(rec))
    }

    pub fn start_map(
        &self,
        source: &str,
        session_id: Option<&str>,
        force: bool,
    ) -> Result<Option<Value>, String> {
        let sources = crate::graph::extract::list_sources(&self.opts.data_root, &self.opts.graph);
        let s = match sources.iter().find(|x| x.get("id").and_then(Value::as_str) == Some(source)) {
            Some(s) => s,
            None => return Ok(None),
        };
        if !force && !crate::graph::extract::needs_mapping(s) {
            return Ok(None);
        }
        {
            let state = self.state.lock().expect("state lock");
            if state.inflight.values().any(|v| v == source) {
                return Ok(None);
            }
        }
        let lesson_title = s.get("lessonTitle").and_then(Value::as_str).unwrap_or("");
        let title = crate::graph::extract::map_job_title(lesson_title);
        let input = serde_json::json!({ "source": source });

        let full = match self.start_inner("map", input, &title, session_id, Some(source)) {
            Ok(v) => v,
            Err(e) if e == "Map source already in flight" => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(full))
    }

    pub fn list(&self) -> Vec<Value> {
        let state = self.state.lock().expect("state lock");
        let mut rows: Vec<Value> = state
            .entries
            .values()
            .map(|e| Value::Object(e.rec.clone()))
            .collect();
        rows.sort_by(|a, b| {
            let b_created = b.get("createdAt").and_then(Value::as_str).unwrap_or("");
            let a_created = a.get("createdAt").and_then(Value::as_str).unwrap_or("");
            b_created.cmp(a_created)
        });
        rows
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        let state = self.state.lock().expect("state lock");
        state.entries.get(id).map(|e| Value::Object(e.rec.clone()))
    }

    pub fn cancel(&self, id: &str) {
        let to_settle = {
            let mut state = self.state.lock().expect("state lock");
            let Some(e) = state.entries.get(id) else {
                return;
            };
            if e.settled {
                return;
            }
            state.queue.retain(|x| x != id);
            id.to_string()
        };
        self.settle(&to_settle, "cancelled");
    }

    pub fn mark_delivered(&self, id: &str) {
        let slim_to_emit = {
            let mut state = self.state.lock().expect("state lock");
            let Some(e) = state.entries.get_mut(id) else {
                return;
            };
            if e.rec.get("delivered").is_some_and(|v| js::truthy(Some(v))) {
                return;
            }
            e.rec.insert("delivered".to_string(), Value::Bool(true));
            e.rec.insert("updatedAt".to_string(), Value::String(events::iso_now()));
            persist(&self.opts.repo_root, &e.rec);
            Value::Object(slim(&e.rec))
        };
        self.opts.events.emit(&GlobalEvent::Job { job: slim_to_emit });
    }

    pub fn live_pids(&self) -> Vec<u32> {
        let state = self.state.lock().expect("state lock");
        let mut pids: Vec<u32> = state.live.values().flatten().copied().collect();
        pids.sort();
        pids
    }

    pub fn renderer(&self) -> &Arc<Renderer> {
        &self.opts.renderer
    }

    pub async fn shutdown(&self) {
        let unsettled_ids: Vec<String> = {
            let mut state = self.state.lock().expect("state lock");
            state.closed = true;
            state.queue.clear();
            state
                .entries
                .iter()
                .filter(|(_, e)| !e.settled)
                .map(|(id, _)| id.clone())
                .collect()
        };

        let mut handles = Vec::new();
        for id in &unsettled_ids {
            let (changed_data, handle, role_kind) = {
                let mut state = self.state.lock().expect("state lock");
                let closed = state.closed;
                let Some(e) = state.entries.get_mut(id) else { continue; };
                if e.settled { continue; }
                e.settled = true;
                e.rec.insert("status".to_string(), Value::String("error".to_string()));
                e.rec.insert(
                    "error".to_string(),
                    Value::String("interrotto alla chiusura dell'app".to_string()),
                );
                e.rec.insert("activity".to_string(), Value::String("".to_string()));
                if let Some(dl) = e.deadline.take() {
                    dl.abort();
                }
                let kind = e.rec.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
                let changed_data = self.changed_locked(e, closed);
                let handle = e.handle.take();
                state.inflight.remove(id);
                (changed_data, handle, kind)
            };
            if let Some(r) = role(&role_kind) {
                (r.cleanup)(id);
            }
            self.fire_changed(changed_data.0, changed_data.1);
            if let Some(h) = handle {
                handles.push(h);
            }
        }

        for mut h in handles {
            h.stop().await;
        }

        self.opts.renderer.close().await;

        if let Ok(mut cb) = self.callback.lock() {
            *cb = None;
        }
    }

    fn pump(&self) {
        let Some(manager) = self.me.upgrade() else { return; };
        let mut to_launch = Vec::new();
        {
            let mut state = manager.state.lock().expect("state lock");
            let max = manager.opts.max_concurrent;
            loop {
                if state.closed {
                    break;
                }
                let map_claimed = state
                    .entries
                    .values()
                    .filter(|e| {
                        !e.settled
                            && e.claimed
                            && e.rec.get("kind").and_then(Value::as_str) == Some("map")
                    })
                    .count();
                let pool = state
                    .entries
                    .values()
                    .filter(|e| {
                        !e.settled
                            && e.claimed
                            && e.rec.get("kind").and_then(Value::as_str) != Some("map")
                    })
                    .count();

                let found_idx = state.queue.iter().position(|id| {
                    let Some(e) = state.entries.get(id) else {
                        return true; // stale
                    };
                    if e.settled {
                        return true; // stale
                    }
                    let is_map = e.rec.get("kind").and_then(Value::as_str) == Some("map");
                    if is_map {
                        map_claimed < 1
                    } else {
                        pool < max
                    }
                });

                let Some(i) = found_idx else {
                    break;
                };

                let id = state.queue.remove(i);
                let Some(e) = state.entries.get_mut(&id) else {
                    continue;
                };
                if e.settled {
                    continue;
                }
                e.claimed = true;

                let mgr = manager.clone();
                let job_id = id.clone();
                let timeout = manager.opts.job_timeout;
                let deadline = tokio::spawn(async move {
                    tokio::time::sleep(timeout).await;
                    mgr.timeout_job(&job_id);
                });
                e.deadline = Some(deadline);

                to_launch.push(id);
            }
        }
        for id in to_launch {
            let mgr = manager.clone();
            tokio::spawn(async move {
                mgr.launch(&id).await;
            });
        }
    }

    fn timeout_job(&self, id: &str) {
        self.set_error_and_settle(id, "tempo scaduto".to_string(), "error");
    }

    fn set_error_and_settle(&self, id: &str, error_msg: String, status: &str) {
        {
            let mut state = self.state.lock().expect("state lock");
            if let Some(e) = state.entries.get_mut(id) {
                if !e.settled {
                    e.rec.insert("error".to_string(), Value::String(error_msg));
                }
            }
        }
        self.settle(id, status);
    }

    fn settle(&self, id: &str, status: &str) {
        let (changed_data, handle, role_kind) = {
            let mut state = self.state.lock().expect("state lock");
            let closed = state.closed;
            let Some(e) = state.entries.get_mut(id) else {
                return;
            };
            if e.settled {
                return;
            }
            e.settled = true;
            e.rec.insert("status".to_string(), Value::String(status.to_string()));
            e.rec.insert("activity".to_string(), Value::String("".to_string()));
            if let Some(dl) = e.deadline.take() {
                dl.abort();
            }
            let kind = e.rec.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
            if status == "done" {
                if let Some(res) = &e.result {
                    e.rec.insert("result".to_string(), res.clone());
                }
            }
            if status == "cancelled" {
                e.rec.shift_remove("error");
            }
            let changed_data = self.changed_locked(e, closed);
            let handle = e.handle.take();
            state.inflight.remove(id);
            (changed_data, handle, kind)
        };

        if let Some(r) = role(&role_kind) {
            (r.cleanup)(id);
        }

        self.fire_changed(changed_data.0, changed_data.1);

        if let Some(mut h) = handle {
            tokio::spawn(async move {
                h.stop().await;
            });
        }

        let renderer = self.opts.renderer.clone();
        let id_clone = id.to_string();
        tokio::spawn(async move {
            renderer.close_owner(&id_clone).await;
        });

        self.pump();
    }

    async fn launch(&self, id: &str) {
        let dir = self.opts.repo_root.join(".learner").join("jobs").join(id);
        if let Err(e) = fs::create_dir_all(&dir) {
            self.set_error_and_settle(id, format!("avvio non riuscito: {e}"), "error");
            return;
        }

        let (kind, mut input, provider) = {
            let state = self.state.lock().expect("state lock");
            let Some(e) = state.entries.get(id) else { return; };
            if e.settled { return; }
            (
                e.rec.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
                e.input.clone(),
                e.provider.clone(),
            )
        };

        let Some(r) = role(&kind) else {
            self.set_error_and_settle(id, format!("Subagente sconosciuto: {kind}"), "error");
            return;
        };

        let code_root = self.opts.code_root.clone();
        let data_root = self.opts.data_root.clone();
        let graph = self.opts.graph.clone();

        let res = tokio::task::spawn_blocking(move || {
            let env = RoleEnv {
                code_root: &code_root,
                data_root: &data_root,
                graph: &graph,
            };
            let prompt = (r.system_prompt)(&env, &input)?;
            let first_msg = (r.first_message)(&env, &mut input);
            Ok::<_, String>((prompt, first_msg, input))
        }).await;

        let (system_prompt, first_message, mutated_input) = match res {
            Ok(Ok(vals)) => vals,
            Ok(Err(e)) => {
                self.set_error_and_settle(id, format!("avvio non riuscito: {e}"), "error");
                return;
            }
            Err(e) => {
                self.set_error_and_settle(id, format!("avvio non riuscito: {e}"), "error");
                return;
            }
        };

        let (slim_event, full_clone, model) = {
            let mut state = self.state.lock().expect("state lock");
            let closed = state.closed;
            let Some(e) = state.entries.get_mut(id) else { return; };
            if e.settled { return; }
            e.input = mutated_input;
            e.rec.insert("activity".to_string(), Value::String("avvio".to_string()));
            let model = e.rec.get("model").and_then(Value::as_str).unwrap_or("").to_string();
            let changed = self.changed_locked(e, closed);
            (changed.0, changed.1, model)
        };

        self.fire_changed(slim_event, full_clone);

        let settings = crate::settings::read_settings(&self.opts.repo_root);
        let role_target = if kind == "map" { "map" } else { "subagent" };
        let effort = crate::engine::effort_for(&settings, &provider, role_target);
        let system_prompt_with_note = crate::engine::with_engine_note(&system_prompt, &provider, r.grant);
        let bin = match provider.as_str() {
            "agy" => self.opts.agy.clone(),
            "opencode" => self.opts.opencode.clone(),
            _ => self.opts.claude.clone(),
        };
        let on_limits = {
            let repo_root = self.opts.repo_root.clone();
            let limits = self.opts.limits.clone();
            let events = self.opts.events.clone();
            Arc::new(move |val: Value| {
                crate::limits::record_limits(&repo_root, &limits, &events, &val);
            })
        };

        let launch = ClaudeLaunch {
            bin,
            repo_root: self.opts.repo_root.clone(),
            data_root: Some(self.opts.data_root.clone()),
            scratch_dir: dir,
            model,
            effort,
            system_prompt: system_prompt_with_note,
            grant: r.grant.to_vec(),
            mcp: McpServer {
                name: "job".to_string(),
                url: format!("http://127.0.0.1:{}/mcp/job/{}", self.opts.port, id),
                token: self.opts.token.clone(),
            },
            add_dirs: vec![],
            max_turns: Some(r.max_turns),
            resume: None,
            settings,
            on_limits,
        };

        let (mut handle, receiver) = match provider.as_str() {
            "agy" => crate::agy::spawn_agy(launch),
            "opencode" => crate::opencode::spawn_opencode(launch),
            _ => crate::claude::spawn_claude(launch),
        };

        let pid = handle.pid();
        static TICKET_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let ticket = TICKET_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let already_settled = {
            let mut state = self.state.lock().expect("state lock");
            state.live.insert(ticket, pid);
            let Some(e) = state.entries.get_mut(id) else {
                return;
            };
            if e.settled {
                true
            } else {
                handle.send(&first_message);
                e.handle = Some(handle);
                false
            }
        };

        if already_settled {
            let h = {
                let mut state = self.state.lock().expect("state lock");
                state.live.remove(&ticket);
                state.entries.get_mut(id).and_then(|e| e.handle.take())
            };
            if let Some(mut h) = h {
                tokio::spawn(async move { h.stop().await });
            }
            return;
        }

        let mgr = self.me.upgrade().expect("alive");
        let job_id = id.to_string();
        tokio::spawn(async move {
            mgr.consume_engine(&job_id, ticket, receiver).await;
        });
    }

    async fn consume_engine(
        &self,
        id: &str,
        ticket: u64,
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<EngineMsg>,
    ) {
        let _guard = LiveTicketGuard {
            state: &self.state,
            ticket,
        };
        while let Some(msg) = receiver.recv().await {
            match msg {
                EngineMsg::Event(EngineEvent::AuthFailed { message }) => {
                    let err = message.unwrap_or_else(|| crate::engine::CLAUDE_AUTH_ERROR_MESSAGE.to_string());
                    self.set_error_and_settle(id, err, "error");
                }
                EngineMsg::Event(EngineEvent::Assistant { tool_uses, .. }) => {
                    if let Some(use_item) = tool_uses.last() {
                        let act = activity_of(&use_item.name, use_item.input.as_ref());
                        let changed_data = {
                            let mut state = self.state.lock().expect("state lock");
                            let closed = state.closed;
                            let Some(e) = state.entries.get_mut(id) else { continue; };
                            if e.settled { continue; }
                            e.rec.insert("activity".to_string(), Value::String(act));
                            self.changed_locked(e, closed)
                        };
                        self.fire_changed(changed_data.0, changed_data.1);
                    }
                }
                EngineMsg::Event(EngineEvent::Result { cost_usd, is_error, subtype, text, .. }) => {
                    let (has_result, err_to_settle) = {
                        let mut state = self.state.lock().expect("state lock");
                        let Some(e) = state.entries.get_mut(id) else { continue; };
                        if e.settled { continue; }
                        let prev = js::to_number(e.rec.get("costUsd"));
                        e.rec.insert("costUsd".to_string(), js::number(prev + cost_usd));
                        if e.result.is_some() {
                            (true, None)
                        } else {
                            let err = if is_error {
                                let suffix = match text {
                                    Some(t) if !t.is_empty() => format!(": {}", trunc(&t, 300)),
                                    _ => String::new(),
                                };
                                format!("il subagente si \u{e8} fermato ({subtype}){suffix}")
                            } else {
                                "il subagente ha terminato senza consegnare".to_string()
                            };
                            e.rec.insert("error".to_string(), Value::String(err));
                            (false, Some("error"))
                        }
                    };
                    if has_result {
                        self.settle(id, "done");
                    } else if let Some(st) = err_to_settle {
                        self.settle(id, st);
                    }
                }
                EngineMsg::Stderr(line) => {
                    let mut state = self.state.lock().expect("state lock");
                    if let Some(e) = state.entries.get_mut(id) {
                        e.stderr.push_back(line);
                        if e.stderr.len() > 20 {
                            e.stderr.pop_front();
                        }
                    }
                }
                EngineMsg::Exit { code, signal } => {
                    let (has_result, err_msg) = {
                        let mut state = self.state.lock().expect("state lock");
                        state.live.remove(&ticket);
                        let Some(e) = state.entries.get_mut(id) else { return; };
                        if e.settled { return; }
                        if e.result.is_some() {
                            (true, None)
                        } else {
                            let joined = e.stderr.iter().cloned().collect::<Vec<_>>().join(" ");
                            if crate::engine::is_auth_error(&joined) {
                                (false, Some(crate::engine::CLAUDE_AUTH_ERROR_MESSAGE.to_string()))
                            } else {
                                let tail_slice: Vec<&str> = e.stderr.iter().map(String::as_str).rev().take(3).collect();
                                let tail = tail_slice.into_iter().rev().collect::<Vec<_>>().join(" ");
                                let sig_code = if let Some(s) = signal {
                                    s
                                } else if let Some(c) = code {
                                    c.to_string()
                                } else {
                                    "null".to_string()
                                };
                                let msg = format!(
                                    "il subagente \u{e8} terminato inaspettatamente ({sig_code}){}",
                                    if tail.is_empty() { String::new() } else { format!(": {tail}") }
                                );
                                (false, Some(msg))
                            }
                        }
                    };
                    if has_result {
                        self.settle(id, "done");
                    } else if let Some(err) = err_msg {
                        self.set_error_and_settle(id, err, "error");
                    }
                    return;
                }
                _ => {}
            }
        }
    }

    pub fn mcp_host(&self, id: &str) -> Result<(JobHost, Vec<&'static ToolDef>), McpRefusal> {
        let (kind, input) = {
            let state = self.state.lock().expect("state lock");
            let entry = state.entries.get(id).ok_or(McpRefusal::NotFound)?;
            if entry.rec.get("status").and_then(Value::as_str) != Some("running") {
                return Err(McpRefusal::NotRunning);
            }
            let kind = entry.rec.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
            (kind, entry.input.clone())
        };
        let r = role(&kind).ok_or(McpRefusal::NotFound)?;
        let tools = (r.tools)(&input);
        let manager = self.me.upgrade().expect("job manager alive");
        let host = JobHost {
            manager,
            job_id: id.to_string(),
        };
        Ok((host, tools))
    }
}

impl JobStarter for JobManager {
    fn start(&self, kind: &str, input: Value, title: &str, session_id: &str) -> Result<String, String> {
        let rec = self.start(kind, input, title, Some(session_id))?;
        rec.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "Missing job id".to_string())
    }

    fn start_map(&self, target: &str, session_id: &str) -> Result<(), String> {
        self.start_map(target, Some(session_id), false).map(|_| ())
    }

    fn get(&self, id: &str) -> Option<Value> {
        self.get(id)
    }

    fn list_for(&self, session_id: &str) -> Vec<Value> {
        self.list()
            .into_iter()
            .filter(|rec| rec.get("sessionId").and_then(Value::as_str) == Some(session_id))
            .collect()
    }

    fn mark_delivered(&self, id: &str) {
        self.mark_delivered(id);
    }
}

pub struct JobHost {
    pub(crate) manager: Arc<JobManager>,
    pub(crate) job_id: String,
}

impl ToolHost for JobHost {
    fn data_root(&self) -> &Path {
        &self.manager.opts.data_root
    }

    fn repo_root(&self) -> &Path {
        &self.manager.opts.repo_root
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

    fn request(&self, _item: Value) -> Result<oneshot::Receiver<Result<Value, Cancelled>>, Cancelled> {
        Err(Cancelled)
    }

    fn emit(&self, _item: Value) -> String {
        String::new()
    }

    fn log_progress(&self, _record: Map<String, Value>) -> Result<(), String> {
        Ok(())
    }

    fn start_job(&self, _kind: &str, _input: Value, _title: &str) -> Result<String, String> {
        Err("not available to a subagent".to_string())
    }

    fn start_map(&self, _target: &str) -> Result<(), String> {
        Err("not available to a subagent".to_string())
    }

    fn exercises(&self) -> Vec<Value> {
        Vec::new()
    }

    fn upsert_exercise(&self, _info: Value) {}

    fn open_problem(&self, _item: Value) -> Option<String> {
        None
    }

    fn take_problem(&self) -> Option<(String, Value)> {
        None
    }

    fn run_config(&self) -> crate::exercises::RunConfig {
        crate::exercises::default_run_config(None)
    }

    fn installs(&self) -> crate::exercises::Installs {
        crate::exercises::Installs::default()
    }

    fn anki_url(&self) -> Option<String> {
        None
    }

    fn semantic(&self) -> Arc<crate::semantic::SemanticSearch> {
        panic!("semantic is not available to a subagent")
    }

    fn courses_changed(&self) {}

    fn job(&self) -> Option<&dyn JobContext> {
        Some(self)
    }
}

impl JobContext for JobHost {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn input(&self) -> Value {
        let state = self.manager.state.lock().expect("state lock");
        state.entries.get(&self.job_id).map(|e| e.input.clone()).unwrap_or(Value::Null)
    }

    fn set_activity(&self, text: &str) {
        let changed = {
            let mut state = self.manager.state.lock().expect("state lock");
            let closed = state.closed;
            let Some(e) = state.entries.get_mut(&self.job_id) else { return; };
            if e.settled { return; }
            e.rec.insert("activity".to_string(), Value::String(trunc(text, 120)));
            self.manager.changed_locked(e, closed)
        };
        self.manager.fire_changed(changed.0, changed.1);
    }

    fn finish(&self, result: Value) {
        let mut state = self.manager.state.lock().expect("state lock");
        if let Some(e) = state.entries.get_mut(&self.job_id) {
            if !e.settled {
                e.result = Some(result);
            }
        }
    }

    fn fail(&self, error: &str) {
        self.manager.set_error_and_settle(&self.job_id, error.to_string(), "error");
    }

    fn graph(&self) -> &Arc<GraphStore> {
        &self.manager.opts.graph
    }

    fn events(&self) -> &EventBus {
        &self.manager.opts.events
    }

    fn renderer(&self) -> Option<&Renderer> {
        Some(&self.manager.opts.renderer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRefusal {
    NotFound,
    NotRunning,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use serde_json::json;

    use super::*;
    use crate::tools::fake::{temp_dir, TempDir};

    fn repo_root() -> PathBuf {
        let raw = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let mut out = PathBuf::new();
        for component in raw.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }

    struct TestJobEnv {
        repo: TempDir,
        data: TempDir,
        events: EventBus,
        manager: Arc<JobManager>,
    }

    fn build_test_manager(tag: &str) -> TestJobEnv {
        build_test_manager_with(tag, 2, Duration::from_secs(600))
    }

    fn build_test_manager_with(tag: &str, max_concurrent: usize, timeout: Duration) -> TestJobEnv {
        let repo = temp_dir(&format!("repo-{tag}"));
        let data = temp_dir(&format!("data-{tag}"));
        let events = EventBus::new();
        let r_root = repo_root();
        let fake_bin = ClaudeBin {
            cmd: "node".to_string(),
            args: vec![r_root.join("app").join("test").join("fake-claude.mjs").display().to_string()],
        };
        let graph = Arc::new(GraphStore::new(data.0.clone()));
        let renderer = Arc::new(Renderer::new(None));
        let limits = Arc::new(LimitsCache::new());
        let opts = JobManagerOptions {
            repo_root: repo.0.clone(),
            data_root: data.0.clone(),
            code_root: r_root,
            port: 9,
            token: "test-token".to_string(),
            claude: fake_bin.clone(),
            agy: fake_bin.clone(),
            opencode: fake_bin,
            events: events.clone(),
            limits,
            graph,
            renderer,
            max_concurrent,
            job_timeout: timeout,
        };
        let manager = JobManager::new(opts);
        TestJobEnv {
            repo,
            data,
            events,
            manager,
        }
    }

    fn seed_demo_course(root: &Path) {
        let lessons = root.join("courses").join("demo").join("lessons");
        fs::create_dir_all(&lessons).unwrap();
        let doc = json!({
            "metadata": {
                "lessonNumber": 1,
                "topic": "t",
                "title": "Il ref è una scatola",
                "slug": "0001-il-ref",
                "dateCreated": "",
                "dateModified": "",
                "version": "1",
            },
            "nodes": [{
                "nodeId": "ref-scatola",
                "title": "Il ref",
                "tier": "axiom",
                "content": "<p>Un ref è una scatola.</p>",
            }],
        });
        let html = format!(
            r#"<html><head><title>Il ref è una scatola</title></head><body><script type="application/json" id="lesson-data">{doc}</script><p>text</p></body></html>"#
        );
        fs::write(lessons.join("0001-il-ref.html"), html).unwrap();
    }

    #[test]
    fn trunc_unit_cases() {
        assert_eq!(trunc("abcdef", 3), "ab\u{2026}");
        assert_eq!(trunc("abc", 3), "abc");
    }

    #[tokio::test]
    async fn unknown_kind_returns_error() {
        let env = build_test_manager("unknown");
        let err = env.manager.start("unknown", json!({}), "Title", None).unwrap_err();
        assert_eq!(err, "Subagente sconosciuto: unknown");
        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn load_turns_running_into_error_and_prunes_records() {
        let repo = temp_dir("repo-load");
        let data = temp_dir("data-load");
        let jobs_dir = repo.0.join(".learner").join("jobs");
        fs::create_dir_all(&jobs_dir).unwrap();

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let old_time = events::iso_from_millis(now_ms - 31 * 86_400 * 1000);
        let recent_time = events::iso_from_millis(now_ms - 1 * 86_400 * 1000);
        let run_time = events::iso_from_millis(now_ms);

        // 1. Old settled job (> 30 days) + dir
        let old_rec = json!({
            "id": "old_job",
            "kind": "research",
            "title": "Old",
            "model": "claude-3-5-haiku",
            "status": "done",
            "activity": "",
            "createdAt": old_time,
            "updatedAt": old_time,
            "costUsd": 0,
        });
        fs::write(jobs_dir.join("old_job.json"), serde_json::to_string(&old_rec).unwrap()).unwrap();
        fs::create_dir_all(jobs_dir.join("old_job")).unwrap();

        // 2. Recent settled job
        let recent_rec = json!({
            "id": "recent_job",
            "kind": "research",
            "title": "Recent",
            "model": "claude-3-5-haiku",
            "status": "done",
            "activity": "",
            "createdAt": recent_time,
            "updatedAt": recent_time,
            "costUsd": 0,
        });
        fs::write(jobs_dir.join("recent_job.json"), serde_json::to_string(&recent_rec).unwrap()).unwrap();

        // 3. Running job
        let running_rec = json!({
            "id": "running_job",
            "kind": "research",
            "title": "Running",
            "model": "claude-3-5-haiku",
            "status": "running",
            "activity": "in coda",
            "createdAt": run_time,
            "updatedAt": run_time,
            "costUsd": 0,
        });
        fs::write(jobs_dir.join("running_job.json"), serde_json::to_string(&running_rec).unwrap()).unwrap();

        // 4. Corrupt JSON
        fs::write(jobs_dir.join("corrupt.json"), "{ invalid").unwrap();

        let r_root = repo_root();
        let fake_bin = ClaudeBin {
            cmd: "node".to_string(),
            args: vec![r_root.join("app").join("test").join("fake-claude.mjs").display().to_string()],
        };
        let opts = JobManagerOptions {
            repo_root: repo.0.clone(),
            data_root: data.0.clone(),
            code_root: r_root,
            port: 9,
            token: "test-token".to_string(),
            claude: fake_bin.clone(),
            agy: fake_bin.clone(),
            opencode: fake_bin,
            events: EventBus::new(),
            limits: Arc::new(LimitsCache::new()),
            graph: Arc::new(GraphStore::new(data.0.clone())),
            renderer: Arc::new(Renderer::new(None)),
            max_concurrent: 2,
            job_timeout: Duration::from_secs(600),
        };
        let manager = JobManager::new(opts);

        // Check old is pruned
        assert!(!jobs_dir.join("old_job.json").exists());
        assert!(!jobs_dir.join("old_job").exists());
        assert!(manager.get("old_job").is_none());

        // Check recent is kept
        assert!(jobs_dir.join("recent_job.json").exists());
        assert!(manager.get("recent_job").is_some());

        // Check running was turned into error
        let running = manager.get("running_job").expect("running job exists");
        assert_eq!(running["status"], "error");
        assert_eq!(running["error"], "interrotto alla chiusura dell'app");
        assert_eq!(running["activity"], "");

        // Check persisted on disk
        let on_disk: Value = serde_json::from_str(&fs::read_to_string(jobs_dir.join("running_job.json")).unwrap()).unwrap();
        assert_eq!(on_disk["status"], "error");
        assert_eq!(on_disk["error"], "interrotto alla chiusura dell'app");

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn cancel_of_a_queued_job_removes_it_from_the_queue() {
        let env = build_test_manager_with("cancel-queue", 0, Duration::from_secs(600));
        let rec = env.manager.start(
            "research",
            json!({ "question": "FAKE:slow:5000" }),
            "Test Queue",
            None,
        ).unwrap();
        let id = rec["id"].as_str().unwrap();
        assert_eq!(rec["status"], "running");
        assert_eq!(rec["activity"], "in coda");

        env.manager.cancel(id);

        let cur = env.manager.get(id).unwrap();
        assert_eq!(cur["status"], "cancelled");
        assert!(cur.get("error").is_none());

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn at_most_two_run_and_third_stays_in_coda() {
        let env = build_test_manager_with("concurrency", 2, Duration::from_secs(600));
        let j1 = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "J1", None).unwrap();
        let j2 = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "J2", None).unwrap();
        let j3 = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "J3", None).unwrap();

        let id1 = j1["id"].as_str().unwrap();
        let id2 = j2["id"].as_str().unwrap();
        let id3 = j3["id"].as_str().unwrap();

        // Wait until J1 and J2 show avvio via get()
        let mut launched = false;
        for _ in 0..50 {
            let cur1 = env.manager.get(id1).unwrap();
            let cur2 = env.manager.get(id2).unwrap();
            let act1 = cur1["activity"].as_str().unwrap_or("");
            let act2 = cur2["activity"].as_str().unwrap_or("");
            if (act1 == "avvio" || (!act1.is_empty() && act1 != "in coda"))
                && (act2 == "avvio" || (!act2.is_empty() && act2 != "in coda"))
            {
                launched = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(launched, "J1 and J2 must launch and leave 'in coda'");

        // Assert J3 is still in coda via get() after a further delay
        tokio::time::sleep(Duration::from_millis(200)).await;
        let cur3 = env.manager.get(id3).unwrap();
        assert_eq!(cur3["status"], "running");
        assert_eq!(cur3["activity"], "in coda");

        // Cancel job 1; job 3 should leave queue and start running
        env.manager.cancel(id1);

        // Give it a brief moment to pump
        let mut left_coda = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let cur3 = env.manager.get(id3).unwrap();
            if cur3["activity"] != "in coda" {
                left_coda = true;
                break;
            }
        }
        assert!(left_coda, "J3 must leave coda after J1 cancellation");

        let cur3 = env.manager.get(id3).unwrap();
        assert_ne!(cur3["activity"], "in coda");

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn map_jobs_run_one_at_a_time_beside_the_pool() {
        let env = build_test_manager_with("map-pool", 1, Duration::from_secs(600));
        // Create lessons with FAKE:slow:5000 so the map jobs stay running during the concurrency test
        let lessons = env.data.0.join("courses").join("demo").join("lessons");
        fs::create_dir_all(&lessons).unwrap();
        fs::write(
            lessons.join("0001-il-ref.html"),
            "<html><head><title>First</title></head><body><p>FAKE:slow:5000</p></body></html>",
        )
        .unwrap();
        fs::write(
            lessons.join("0002-second.html"),
            "<html><head><title>Second</title></head><body><p>FAKE:slow:5000</p></body></html>",
        )
        .unwrap();

        // Non-map job takes the 1 slot
        let _j1 = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "J1", None).unwrap();

        // Map job 1 can run beside the pool!
        let m1 = env.manager.start_map("demo/0001-il-ref", None, true).unwrap().expect("starts map 1");
        let id_m1 = m1["id"].as_str().unwrap();

        // Wait until M1 shows avvio via get()
        let mut m1_launched = false;
        for _ in 0..50 {
            let cur_m1 = env.manager.get(id_m1).unwrap();
            let act_m1 = cur_m1["activity"].as_str().unwrap_or("");
            if act_m1 == "avvio" || (!act_m1.is_empty() && act_m1 != "in coda") {
                m1_launched = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(m1_launched, "M1 must launch and leave 'in coda'");

        // Map job 2 should stay in coda because mapRunning == 1
        let m2 = env.manager.start_map("demo/0002-second", None, true).unwrap().expect("starts map 2");
        let id_m2 = m2["id"].as_str().unwrap();

        // Assert M2 is still in coda via get() after a further delay
        tokio::time::sleep(Duration::from_millis(200)).await;
        let cur_m2 = env.manager.get(id_m2).unwrap();
        assert_eq!(cur_m2["status"], "running");
        assert_eq!(cur_m2["activity"], "in coda");

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn hung_job_settles_tempo_scaduto() {
        let env = build_test_manager_with("timeout", 2, Duration::from_millis(300));
        let j = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "Hang", None).unwrap();
        let id = j["id"].as_str().unwrap();

        tokio::time::sleep(Duration::from_millis(600)).await;

        let cur = env.manager.get(id).unwrap();
        assert_eq!(cur["status"], "error");
        assert_eq!(cur["error"], "tempo scaduto");

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn fake_hello_finishes_without_delivering() {
        let env = build_test_manager("hello");
        let j = env.manager.start("research", json!({ "question": "FAKE:hello" }), "Hello", None).unwrap();
        let id = j["id"].as_str().unwrap().to_string();

        let mut settled = false;
        let mut last_rec = env.manager.get(&id).unwrap();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            last_rec = env.manager.get(&id).unwrap();
            if last_rec["status"] != "running" {
                assert_eq!(last_rec["status"], "error");
                assert_eq!(last_rec["error"], "il subagente ha terminato senza consegnare");
                settled = true;
                break;
            }
        }
        if !settled {
            panic!("job did not settle in time, last record: {last_rec:?}");
        }

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn fake_hello_settles_and_live_pids_becomes_empty() {
        let env = build_test_manager("live-pids");
        let j = env.manager.start("research", json!({ "question": "FAKE:hello" }), "Live", None).unwrap();
        let id = j["id"].as_str().unwrap().to_string();

        let mut settled = false;
        let mut last_rec = env.manager.get(&id).unwrap();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            last_rec = env.manager.get(&id).unwrap();
            if last_rec["status"] != "running" {
                settled = true;
                break;
            }
        }
        if !settled {
            panic!("job did not settle in time, last record: {last_rec:?}");
        }

        let mut cleared = false;
        for _ in 0..50 {
            if env.manager.live_pids().is_empty() {
                cleared = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(cleared, "live_pids() did not become empty after settle: {:?}", env.manager.live_pids());

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn global_event_job_carries_no_result() {
        let env = build_test_manager("slim-event");
        let mut rx = env.events.subscribe();

        let j = env.manager.start("research", json!({ "question": "FAKE:hello" }), "Slim", None).unwrap();
        let id = j["id"].as_str().unwrap();

        while let Ok(msg) = rx.recv().await {
            let val: Value = serde_json::from_str(&msg).unwrap();
            if val.get("type").and_then(Value::as_str) == Some("job") {
                let job = val.get("job").unwrap();
                if job.get("id").and_then(Value::as_str) == Some(id) {
                    assert!(job.get("result").is_none());
                    break;
                }
            }
        }

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn record_key_order_and_cost_usd_integer() {
        let env = build_test_manager("key-order");
        let j = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "KeyOrder", None).unwrap();
        let id = j["id"].as_str().unwrap();

        let path = env.repo.0.join(".learner").join("jobs").join(format!("{id}.json"));
        let raw = fs::read_to_string(&path).unwrap();

        // costUsd: 0 formatting
        assert!(raw.contains(r#""costUsd":0"#), "raw was: {raw}");

        // Keys order
        let parsed: Map<String, Value> = serde_json::from_str(&raw).unwrap();
        let keys: Vec<&str> = parsed.keys().map(String::as_str).collect();
        assert_eq!(keys, ["id", "kind", "title", "model", "status", "activity", "createdAt", "updatedAt", "costUsd"]);

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn start_map_skips_covered_and_inflight_and_titles() {
        let env = build_test_manager("start-map");
        seed_demo_course(&env.data.0);

        // 1. First map starts
        let m1 = env.manager.start_map("demo/0001-il-ref", None, false).unwrap();
        assert!(m1.is_some());
        let rec = m1.unwrap();
        assert_eq!(rec["title"], "Mappa: Il ref è una scatola");
        let id = rec["id"].as_str().unwrap().to_string();

        // 2. Second map while in-flight skips
        let m2 = env.manager.start_map("demo/0001-il-ref", None, false).unwrap();
        assert!(m2.is_none());

        env.manager.cancel(&id);

        // 3. Mark covered
        let lesson = crate::graph::extract::read_lesson_text(&env.data.0, "demo/0001-il-ref").unwrap();
        let fp = crate::graph::extract::fingerprint(&lesson);
        crate::graph::extract::append_coverage(&env.data.0, "demo/0001-il-ref", &fp).unwrap();

        // Not forced: skipped
        let m3 = env.manager.start_map("demo/0001-il-ref", None, false).unwrap();
        assert!(m3.is_none());

        // Forced: starts
        let m4 = env.manager.start_map("demo/0001-il-ref", None, true).unwrap();
        assert!(m4.is_some());

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn bind_sessions_callback_no_deadlock() {
        let env = build_test_manager("callback");
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();
        let mgr = env.manager.clone();

        env.manager.bind_sessions(Arc::new(move |rec| {
            // Calling list() requires state lock; should not deadlock!
            let _items = mgr.list();
            if rec.get("sessionId").and_then(Value::as_str) == Some("sess_1") {
                called_clone.store(true, Ordering::SeqCst);
            }
        }));

        let _j = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "Callback", Some("sess_1")).unwrap();

        assert!(called.load(Ordering::SeqCst));

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn mark_delivered_is_idempotent() {
        let env = build_test_manager("mark-delivered");
        let j = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "Delivered", None).unwrap();
        let id = j["id"].as_str().unwrap().to_string();
        env.manager.cancel(&id);

        let mut rx = env.events.subscribe();

        // First call: sets delivered:true, writes to disk, emits event
        env.manager.mark_delivered(&id);

        let mut event_received = false;
        while let Ok(msg) = rx.try_recv() {
            let val: Value = serde_json::from_str(&msg).unwrap();
            if val.get("type").and_then(Value::as_str) == Some("job") {
                if val["job"]["id"] == id && val["job"]["delivered"] == true {
                    event_received = true;
                }
            }
        }
        assert!(event_received);

        let path = env.repo.0.join(".learner").join("jobs").join(format!("{id}.json"));
        let disk_before = fs::read_to_string(&path).unwrap();

        // Second call: no-op, no event, no disk write
        env.manager.mark_delivered(&id);
        assert!(rx.try_recv().is_err());
        let disk_after = fs::read_to_string(&path).unwrap();
        assert_eq!(disk_before, disk_after);

        env.manager.shutdown().await;
    }

    #[tokio::test]
    async fn mcp_host_maps_unknown_and_settled_jobs() {
        let env = build_test_manager("mcp-host");

        // 1. Unknown
        match env.manager.mcp_host("nope") {
            Err(e) => assert_eq!(e, McpRefusal::NotFound),
            Ok(_) => panic!("expected NotFound"),
        }

        // 2. Settled (cancelled)
        let j = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "Settled", None).unwrap();
        let id = j["id"].as_str().unwrap().to_string();
        env.manager.cancel(&id);
        match env.manager.mcp_host(&id) {
            Err(e) => assert_eq!(e, McpRefusal::NotRunning),
            Ok(_) => panic!("expected NotRunning"),
        }

        // 3. Running research job
        let j2 = env.manager.start("research", json!({ "question": "FAKE:slow:5000" }), "Running", None).unwrap();
        let id2 = j2["id"].as_str().unwrap();
        let (host, tools) = env.manager.mcp_host(id2).unwrap();
        assert_eq!(host.job_id(), id2);
        assert_eq!(tools.iter().map(|t| t.name).collect::<Vec<_>>(), ["submit_research"]);

        env.manager.shutdown().await;
    }
}
