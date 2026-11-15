//! OpenCode adapter (port of app/server/opencode.ts).
//!
//! One process per turn: `opencode run --format json` writes JSON lines; resume token = the opencode
//! session id. The system prompt lives in `.opencode/agents/ia-learner.md` and the MCP servers in
//! `opencode.json`, both in the launch's scratch dir. A normal turn exit is not a session exit: the
//! session ends (`Exit`, exactly once) only after `stop`.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};

use crate::claude;
use crate::engine::{EngineBin, EngineEvent, EngineHandle, EngineLaunch, EngineMsg, ToolUse};
use crate::js;

pub const OPENCODE_MODELS_FALLBACK: [&str; 6] = [
    "omniroute/auto/best-coding",
    "omniroute/auto/best-fast",
    "omniroute/auto/fast",
    "google/gemini-2.5-pro",
    "google/gemini-2.5-flash",
    "opencode/big-pickle",
];

const AGENT: &str = "ia-learner";

fn num(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|value| value.is_finite())
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

/// `value?.key`: a missing or non-object value has no field.
fn field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    value.and_then(|value| value.get(key))
}

/// The parse state of one opencode process: the ids it invents, its accumulated text and usage.
pub struct OpencodeParseState {
    run: String,
    pub session_id: String,
    has_init: bool,
    seen_text: HashSet<String>,
    message_text: HashMap<String, String>,
    tools: HashMap<String, String>,
    tokens: (f64, f64, f64),
    cost_usd: f64,
    pub notes: Vec<String>,
}

static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

impl OpencodeParseState {
    pub fn new() -> Self {
        let run = RUN_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        Self {
            run: format!("{}_{run}", std::process::id()),
            session_id: String::new(),
            has_init: false,
            seen_text: HashSet::new(),
            message_text: HashMap::new(),
            tools: HashMap::new(),
            tokens: (0.0, 0.0, 0.0),
            cost_usd: 0.0,
            notes: Vec::new(),
        }
    }
}

impl Default for OpencodeParseState {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps opencode tool names (e.g. `app.write_node`, `app_write_node`) to `mcp__app__write_node`.
pub fn map_opencode_tool_name(name: &str) -> String {
    if name.starts_with("mcp__") {
        return name.to_string();
    }
    for prefix in ["app", "job"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if let Some(tail) = rest.strip_prefix('.').or_else(|| rest.strip_prefix('_')) {
                return format!("mcp__{prefix}__{tail}");
            }
        }
    }
    name.to_string()
}

fn ensure_init(st: &mut OpencodeParseState, out: &mut Vec<EngineEvent>) {
    if st.has_init {
        return;
    }
    st.has_init = true;
    out.push(EngineEvent::Init {
        session_id: st.session_id.clone(),
        tools: Vec::new(),
        mcp_servers: Vec::new(),
    });
}

/// One line of `opencode run --format json` into normalised events; malformed input never panics.
pub fn parse_opencode_line(line: &str, st: &mut OpencodeParseState) -> Vec<EngineEvent> {
    let Ok(message) = js::parse(line) else {
        return Vec::new();
    };
    if !message.is_object() {
        return Vec::new();
    }
    let mut out: Vec<EngineEvent> = Vec::new();

    let kind = text(message.get("type"));
    let sid = {
        let primary = text(message.get("sessionID"));
        if primary.is_empty() {
            text(message.get("sessionId"))
        } else {
            primary
        }
    };
    if !sid.is_empty() {
        st.session_id = sid;
    }
    let part = message.get("part").filter(|value| !value.is_null());

    if kind == "step_start" {
        ensure_init(st, &mut out);
        return out;
    }

    if kind == "text" {
        let mid = {
            let given = text(field(part, "messageID"));
            if given.is_empty() {
                format!("msg_{}", st.run)
            } else {
                given
            }
        };
        let delta = text(field(part, "text"));
        ensure_init(st, &mut out);
        if st.seen_text.insert(mid.clone()) {
            out.push(EngineEvent::TextStart {
                message_id: mid.clone(),
                index: 0.0,
            });
        }
        if !delta.is_empty() {
            st.message_text
                .entry(mid.clone())
                .or_default()
                .push_str(&delta);
            let accumulated = st.message_text.get(&mid).cloned().unwrap_or_default();
            out.push(EngineEvent::TextDelta {
                message_id: mid.clone(),
                index: 0.0,
                text: delta,
            });
            out.push(EngineEvent::Assistant {
                message_id: mid,
                texts: vec![(0, accumulated)],
                tool_uses: Vec::new(),
            });
        }
        return out;
    }

    if kind == "tool_use" {
        let tool_call = field(field(field(part, "state"), "metadata"), "metadata")
            .and_then(|metadata| metadata.get("toolCalls"))
            .and_then(Value::as_array)
            .and_then(|calls| calls.first());
        let raw_tool = {
            let from_metadata = text(field(tool_call, "tool"));
            if from_metadata.is_empty() {
                text(field(part, "tool"))
            } else {
                from_metadata
            }
        };
        let tool_name = map_opencode_tool_name(&raw_tool);
        let id = {
            let own = text(field(part, "id"));
            if !own.is_empty() {
                own
            } else {
                let part_id = text(field(part, "partID"));
                if part_id.is_empty() {
                    format!("opencode_{}_call", st.run)
                } else {
                    part_id
                }
            }
        };
        ensure_init(st, &mut out);
        if !st.tools.contains_key(&id) {
            st.tools.insert(id.clone(), tool_name.clone());
            let input = field(field(part, "state"), "input")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or_else(|| json!({}));
            out.push(EngineEvent::Assistant {
                message_id: format!("m{id}"),
                texts: Vec::new(),
                tool_uses: vec![ToolUse {
                    index: 0,
                    id: id.clone(),
                    name: tool_name,
                    input: Some(input),
                }],
            });
        }
        let status = text(field(field(part, "state"), "status"));
        if status == "completed" || status == "error" {
            let result_text = match field(field(part, "state"), "output") {
                Some(Value::String(output)) => output.clone(),
                Some(output) if !output.is_null() => {
                    serde_json::to_string(output).unwrap_or_default()
                }
                _ => serde_json::to_string("").unwrap_or_default(),
            };
            out.push(EngineEvent::ToolResult {
                tool_use_id: id,
                is_error: status == "error",
                text: result_text,
            });
        }
        return out;
    }

    if kind == "step_finish" {
        let tokens = field(part, "tokens");
        st.tokens.0 += num(field(tokens, "input"));
        st.tokens.1 += num(field(tokens, "output")) + num(field(tokens, "reasoning"));
        st.tokens.2 += num(field(field(tokens, "cache"), "read"));
        st.cost_usd += num(field(part, "cost"));

        if text(field(part, "reason")) == "stop" {
            out.push(EngineEvent::Result {
                is_error: false,
                subtype: "success".to_string(),
                text: None,
                session_id: st.session_id.clone(),
                input_tokens: st.tokens.0,
                output_tokens: st.tokens.1,
                cache_read_tokens: st.tokens.2,
                cache_creation_tokens: 0.0,
                cost_usd: st.cost_usd,
            });
        }
        return out;
    }

    if kind == "error" {
        let message_text = {
            let own = text(message.get("error"));
            if own.is_empty() {
                serde_json::to_string(&message).unwrap_or_default()
            } else {
                own
            }
        };
        st.notes.push(format!("opencode error: {message_text}"));
    }

    out
}

/// The agent file opencode reads the system prompt from.
pub fn agent_file(system_prompt: &str) -> String {
    format!("---\ndescription: {AGENT}\nmode: primary\n---\n\n{system_prompt}\n")
}

/// `opencode.json`: the MCP servers, in Node's pretty-printed layout.
pub fn mcp_config(l: &EngineLaunch) -> String {
    let mut headers = Map::new();
    headers.insert(
        "x-learner-token".to_string(),
        Value::String(l.mcp.token.clone()),
    );
    let mut server = Map::new();
    server.insert("type".to_string(), Value::String("remote".to_string()));
    server.insert("url".to_string(), Value::String(l.mcp.url.clone()));
    server.insert("headers".to_string(), Value::Object(headers));
    let mut servers = Map::new();
    servers.insert(l.mcp.name.clone(), Value::Object(server));
    let mut mcp = Map::new();
    mcp.insert("servers".to_string(), Value::Object(servers));
    let mut root = Map::new();
    root.insert(
        "$schema".to_string(),
        Value::String("https://opencode.ai/config.json".to_string()),
    );
    root.insert("mcp".to_string(), Value::Object(mcp));
    serde_json::to_string_pretty(&Value::Object(root)).expect("the MCP config serialises")
}

/// argv for one turn: a fresh run, or `--session` when there is a token to resume.
pub fn build_opencode_args(l: &EngineLaunch, text: &str, session_id: Option<&str>) -> Vec<String> {
    let mut args = l.bin.args.clone();
    args.extend(
        [
            "run", "--format", "json", "--auto", "--agent", AGENT, "--model",
        ]
        .iter()
        .map(|arg| arg.to_string()),
    );
    args.push(l.model.clone());
    if let Some(session) = session_id.filter(|session| !session.is_empty()) {
        args.push("--session".to_string());
        args.push(session.to_string());
    }
    args.push(text.to_string());
    args
}

/// Node's `resolveOpencodeCmd`: `.cmd`/`.bat` shims are programs std can run itself, so they stay as
/// they are; a bare `opencode` resolves to the npm `.exe`, else the first PATHEXT hit on PATH.
pub fn resolve_opencode_cmd(
    cmd: &str,
    windows: bool,
    app_data: Option<&Path>,
    path: Option<&OsStr>,
) -> String {
    if !windows {
        return cmd.to_string();
    }
    let lower = cmd.to_ascii_lowercase();
    if lower.ends_with(".exe") || lower.ends_with(".cmd") || lower.ends_with(".bat") {
        return cmd.to_string();
    }
    if cmd != "opencode" {
        return cmd.to_string();
    }
    if let Some(app_data) = app_data {
        let npm_exe = app_data
            .join("npm")
            .join("node_modules")
            .join("@opencode")
            .join("cli")
            .join("bin")
            .join("opencode.exe");
        if npm_exe.is_file() {
            return npm_exe.display().to_string();
        }
    }
    if let Some(path) = path.and_then(OsStr::to_str) {
        let separator = if windows { ';' } else { ':' };
        for entry in path.split(separator) {
            let entry = js::js_trim(entry);
            if entry.is_empty() {
                continue;
            }
            // cmd's PATHEXT order, one directory at a time.
            for name in ["opencode.exe", "opencode.bat", "opencode.cmd"] {
                let candidate = Path::new(entry).join(name);
                if candidate.is_file() {
                    return candidate.display().to_string();
                }
            }
        }
    }
    cmd.to_string()
}

/// `resolve_opencode_cmd` against this process's environment.
pub fn resolve_from_process(cmd: &str) -> String {
    let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    let path = std::env::var_os("PATH");
    resolve_opencode_cmd(cmd, cfg!(windows), app_data.as_deref(), path.as_deref())
}

struct Turn {
    generation: u64,
    pid: Option<u32>,
    kill: mpsc::UnboundedSender<()>,
    done: watch::Receiver<bool>,
}

/// The session as Node holds it: one live turn at a time, a newer send orphaning the older process.
struct Inner {
    session_id: Option<String>,
    stopped: bool,
    exit_reported: bool,
    in_flight: bool,
    generation: u64,
    current: Option<Turn>,
    exit: watch::Sender<bool>,
}

/// The handle one session drives: every `send` is one `opencode run` process.
pub struct OpencodeHandle {
    inner: Arc<Mutex<Inner>>,
    launch: EngineLaunch,
    tx: mpsc::UnboundedSender<EngineMsg>,
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn report_exit(
    inner: &Mutex<Inner>,
    tx: &mpsc::UnboundedSender<EngineMsg>,
    code: Option<i32>,
    signal: Option<String>,
) {
    let exit = {
        let mut state = lock(inner);
        if state.exit_reported {
            return;
        }
        state.exit_reported = true;
        state.exit.clone()
    };
    let _ = tx.send(EngineMsg::Exit { code, signal });
    let _ = exit.send(true);
}

fn live(inner: &Mutex<Inner>, generation: u64) -> bool {
    lock(inner)
        .current
        .as_ref()
        .is_some_and(|turn| turn.generation == generation)
}

fn finish_turn(
    inner: &Mutex<Inner>,
    tx: &mpsc::UnboundedSender<EngineMsg>,
    generation: u64,
    code: Option<i32>,
    signal: Option<String>,
    resuming: &AtomicBool,
    turn_finished: &AtomicBool,
) {
    let (in_flight, session_id, stopped) = {
        let mut state = lock(inner);
        if !state
            .current
            .as_ref()
            .is_some_and(|turn| turn.generation == generation)
        {
            return;
        }
        state.current = None;
        (
            state.in_flight,
            state.session_id.clone().unwrap_or_default(),
            state.stopped,
        )
    };
    if resuming.load(Ordering::SeqCst) && (code != Some(0) || signal.is_some()) {
        let _ = tx.send(EngineMsg::Event(EngineEvent::ResumeFailed));
        report_exit(inner, tx, code, signal);
        return;
    }
    if in_flight && !turn_finished.load(Ordering::SeqCst) {
        lock(inner).in_flight = false;
        let _ = tx.send(EngineMsg::Event(EngineEvent::Result {
            is_error: code != Some(0),
            subtype: if code == Some(0) { "success" } else { "error" }.to_string(),
            text: None,
            session_id,
            input_tokens: 0.0,
            output_tokens: 0.0,
            cache_read_tokens: 0.0,
            cache_creation_tokens: 0.0,
            cost_usd: 0.0,
        }));
    }
    if stopped {
        report_exit(inner, tx, code, signal);
    }
}

/// Spawns one turn process and pumps it into `EngineMsg`s. The process's job object dies with it.
fn launch_turn(
    launch: &EngineLaunch,
    session: Option<&str>,
    text: &str,
    generation: u64,
    inner: &Arc<Mutex<Inner>>,
    tx: &mpsc::UnboundedSender<EngineMsg>,
) {
    let argv = build_opencode_args(launch, text, session);
    let program = resolve_from_process(&launch.bin.cmd);
    let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<()>();
    let done = lock(inner).exit.subscribe();
    let resuming = Arc::new(AtomicBool::new(session.is_some()));
    let turn_finished = Arc::new(AtomicBool::new(false));

    let mut command = claude::engine_command(
        &program,
        &argv,
        &launch.scratch_dir,
        &claude::base_env(launch.data_root.as_deref()),
        &[],
        Stdio::null(),
    );
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            lock(inner).current = Some(Turn {
                generation,
                pid: None,
                kill: kill_tx,
                done,
            });
            let _ = tx.send(EngineMsg::Stderr(format!(
                "spawn error: {}",
                crate::engine::spawn_error_message(&program, &err)
            )));
            finish_turn(inner, tx, generation, None, None, &resuming, &turn_finished);
            return;
        }
    };
    let pid = child.id();
    let stdout = child.stdout().take();
    let stderr = child.stderr().take();
    lock(inner).current = Some(Turn {
        generation,
        pid,
        kill: kill_tx,
        done,
    });

    let stdout_task = tokio::spawn({
        let inner = inner.clone();
        let tx = tx.clone();
        let resuming = resuming.clone();
        let turn_finished = turn_finished.clone();
        let mut state = OpencodeParseState::new();
        if let Some(session) = session {
            state.session_id = session.to_string();
        }
        async move {
            claude::pump_lines(stdout, |line| {
                if !live(&inner, generation) {
                    return;
                }
                for event in parse_opencode_line(&line, &mut state) {
                    match &event {
                        EngineEvent::Init { session_id, .. } => {
                            resuming.store(false, Ordering::SeqCst);
                            if !session_id.is_empty() {
                                lock(&inner).session_id = Some(session_id.clone());
                            }
                        }
                        EngineEvent::Result { session_id, .. } => {
                            {
                                let mut held = lock(&inner);
                                held.in_flight = false;
                                if !session_id.is_empty() {
                                    held.session_id = Some(session_id.clone());
                                }
                            }
                            turn_finished.store(true, Ordering::SeqCst);
                        }
                        _ => {}
                    }
                    let _ = tx.send(EngineMsg::Event(event));
                }
                for note in std::mem::take(&mut state.notes) {
                    let _ = tx.send(EngineMsg::Stderr(note));
                }
            })
            .await;
        }
    });

    let stderr_task = tokio::spawn({
        let inner = inner.clone();
        let tx = tx.clone();
        async move {
            claude::pump_lines(stderr, |line| {
                if live(&inner, generation) {
                    let _ = tx.send(EngineMsg::Stderr(line));
                }
            })
            .await;
        }
    });

    tokio::spawn({
        let inner = inner.clone();
        let tx = tx.clone();
        async move {
            let (code, signal) = claude::wait_child(&mut child, &mut kill_rx).await;
            // The readers see EOF only once every process holding the pipes is gone.
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            finish_turn(
                &inner,
                &tx,
                generation,
                code,
                signal,
                &resuming,
                &turn_finished,
            );
        }
    });
}

impl EngineHandle for OpencodeHandle {
    fn send(&mut self, text: &str) {
        let (generation, session) = {
            let mut state = lock(&self.inner);
            if state.stopped || state.exit_reported {
                return;
            }
            state.in_flight = true;
            state.generation += 1;
            (
                state.generation,
                state.session_id.clone().filter(|id| !id.is_empty()),
            )
        };
        launch_turn(
            &self.launch,
            session.as_deref(),
            text,
            generation,
            &self.inner,
            &self.tx,
        );
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        let inner = self.inner.clone();
        let tx = self.tx.clone();
        Box::pin(async move {
            let current = {
                let mut state = lock(&inner);
                state.stopped = true;
                if state.exit_reported {
                    return;
                }
                state
                    .current
                    .as_ref()
                    .map(|turn| (turn.kill.clone(), turn.done.clone()))
            };
            match current {
                Some((kill, mut done)) => {
                    let _ = kill.send(());
                    let _ =
                        tokio::time::timeout(Duration::from_secs(5), done.wait_for(|done| *done))
                            .await;
                }
                None => report_exit(&inner, &tx, None, None),
            }
        })
    }

    fn pid(&self) -> Option<u32> {
        lock(&self.inner).current.as_ref().and_then(|turn| turn.pid)
    }
}

/// Spawns the opencode adapter for `l`; the session itself starts with the first `send`.
pub fn spawn_opencode(
    l: EngineLaunch,
) -> (Box<dyn EngineHandle>, mpsc::UnboundedReceiver<EngineMsg>) {
    let (tx, rx) = mpsc::unbounded_channel();

    let agents = l.scratch_dir.join(".opencode").join("agents");
    let prepared = fs::create_dir_all(&agents)
        .and_then(|_| {
            fs::write(
                agents.join(format!("{AGENT}.md")),
                agent_file(&l.system_prompt),
            )
        })
        .and_then(|_| fs::write(l.scratch_dir.join("opencode.json"), mcp_config(&l)));
    if let Err(err) = prepared {
        return claude::dead(tx, rx, Some(format!("spawn error: {err}")), None);
    }

    let (exit_tx, _) = watch::channel(false);
    let handle = OpencodeHandle {
        inner: Arc::new(Mutex::new(Inner {
            session_id: l.resume.clone().filter(|token| !token.is_empty()),
            stopped: false,
            exit_reported: false,
            in_flight: false,
            generation: 0,
            current: None,
            exit: exit_tx,
        })),
        launch: l,
        tx,
    };
    (Box::new(handle), rx)
}

fn fallback_models() -> Vec<String> {
    OPENCODE_MODELS_FALLBACK
        .iter()
        .map(|model| model.to_string())
        .collect()
}

/// The model ids of `opencode models`: trimmed lines, diagnostics skipped, fallback when empty.
pub fn parse_opencode_models(stdout: &str) -> Vec<String> {
    let ids: Vec<String> = stdout
        .split('\n')
        .map(js::js_trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("error")
                && !line.starts_with("warn")
                && !line.starts_with("info")
        })
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        fallback_models()
    } else {
        ids
    }
}

/// `opencode models`, killed after 10 s; any failure gives the static list.
pub async fn list_opencode_models(bin: &EngineBin) -> Vec<String> {
    let program = resolve_from_process(&bin.cmd);
    let mut command = tokio::process::Command::new(&program);
    command.args(&bin.args);
    command.arg("models");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return fallback_models(),
    };
    let mut buf = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        // A timeout keeps the bytes read so far: the engine is already dead, its list may still do.
        if tokio::time::timeout(Duration::from_secs(10), stdout.read_to_end(&mut buf))
            .await
            .is_err()
        {
            let _ = child.start_kill();
        }
    }
    parse_opencode_models(&String::from_utf8_lossy(&buf))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::engine::Grant;
    use crate::protocol::LearnerSettings;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("proxi-opencode-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn launch_for_args() -> EngineLaunch {
        EngineLaunch {
            bin: EngineBin {
                cmd: "opencode".to_string(),
                args: vec!["--extra-flag".to_string()],
            },
            repo_root: PathBuf::from("/repo"),
            data_root: None,
            scratch_dir: PathBuf::from("/scratch"),
            model: "omniroute/auto/best-coding".to_string(),
            effort: None,
            system_prompt: "sys".to_string(),
            grant: vec![Grant::Read],
            mcp: crate::engine::McpServer {
                name: "app".to_string(),
                url: "http://127.0.0.1:4000/mcp".to_string(),
                token: "tok".to_string(),
            },
            add_dirs: Vec::new(),
            max_turns: None,
            resume: None,
            settings: LearnerSettings::default(),
            on_limits: Arc::new(|_| {}),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn map_opencode_tool_name_handles_prefixes() {
        assert_eq!(
            map_opencode_tool_name("app.write_node"),
            "mcp__app__write_node"
        );
        assert_eq!(
            map_opencode_tool_name("app_write_node"),
            "mcp__app__write_node"
        );
        assert_eq!(
            map_opencode_tool_name("job.submit_findings"),
            "mcp__job__submit_findings"
        );
        assert_eq!(
            map_opencode_tool_name("job_submit_findings"),
            "mcp__job__submit_findings"
        );
        assert_eq!(map_opencode_tool_name("mcp__app__quiz"), "mcp__app__quiz");
        assert_eq!(map_opencode_tool_name("read"), "read");
    }

    #[test]
    fn build_args_fresh_and_resume() {
        let launch = launch_for_args();
        assert_eq!(
            build_opencode_args(&launch, "hello", None),
            strings(&[
                "--extra-flag",
                "run",
                "--format",
                "json",
                "--auto",
                "--agent",
                "ia-learner",
                "--model",
                "omniroute/auto/best-coding",
                "hello",
            ])
        );
        assert_eq!(
            build_opencode_args(&launch, "hello again", Some("ses_12345")),
            strings(&[
                "--extra-flag",
                "run",
                "--format",
                "json",
                "--auto",
                "--agent",
                "ia-learner",
                "--model",
                "omniroute/auto/best-coding",
                "--session",
                "ses_12345",
                "hello again",
            ])
        );
    }

    #[test]
    fn resolve_cmd_rules() {
        // Off Windows the command is the program, as Node leaves it.
        assert_eq!(
            resolve_opencode_cmd("opencode", false, None, None),
            "opencode"
        );
        assert_eq!(
            resolve_opencode_cmd("C:\\path\\to\\opencode.exe", true, None, None),
            "C:\\path\\to\\opencode.exe"
        );
        for shim in ["custom.cmd", "X.BAT"] {
            assert_eq!(resolve_opencode_cmd(shim, true, None, None), shim);
        }

        // The npm install wins over PATH.
        let app_data = temp_dir("appdata");
        let npm = app_data
            .join("npm")
            .join("node_modules")
            .join("@opencode")
            .join("cli")
            .join("bin");
        fs::create_dir_all(&npm).unwrap();
        let npm_exe = npm.join("opencode.exe");
        fs::write(&npm_exe, "").unwrap();
        let empty = temp_dir("no-appdata");
        assert_eq!(
            resolve_opencode_cmd("opencode", true, Some(&app_data), None),
            npm_exe.display().to_string()
        );

        // No npm install: the first PATH directory with a hit, exe before bat before cmd.
        let bin = temp_dir("path");
        fs::write(bin.join("opencode.exe"), "").unwrap();
        fs::write(bin.join("opencode.bat"), "").unwrap();
        fs::write(bin.join("opencode.cmd"), "").unwrap();
        let path = bin.display().to_string();
        assert_eq!(
            resolve_opencode_cmd("opencode", true, Some(&empty), Some(OsStr::new(&path))),
            bin.join("opencode.exe").display().to_string()
        );
        fs::remove_file(bin.join("opencode.exe")).unwrap();
        assert_eq!(
            resolve_opencode_cmd("opencode", true, Some(&empty), Some(OsStr::new(&path))),
            bin.join("opencode.bat").display().to_string()
        );
        fs::remove_file(bin.join("opencode.bat")).unwrap();
        assert_eq!(
            resolve_opencode_cmd("opencode", true, Some(&empty), Some(OsStr::new(&path))),
            bin.join("opencode.cmd").display().to_string()
        );

        // Nothing at all: Node's shell would go on searching; std spawn then reports ENOENT.
        assert_eq!(
            resolve_opencode_cmd("opencode", true, Some(&empty), Some(OsStr::new(""))),
            "opencode"
        );
    }

    #[test]
    fn parse_step_start_text_tool_use_step_finish() {
        let mut st = OpencodeParseState::new();

        let step_start = parse_opencode_line(
            r#"{"type":"step_start","sessionID":"ses_abc123","part":{"id":"p1","messageID":"m1","type":"step-start"}}"#,
            &mut st,
        );
        assert_eq!(
            step_start,
            vec![EngineEvent::Init {
                session_id: "ses_abc123".to_string(),
                tools: Vec::new(),
                mcp_servers: Vec::new(),
            }]
        );

        let text_events = parse_opencode_line(
            r#"{"type":"text","sessionID":"ses_abc123","part":{"id":"p2","messageID":"m1","type":"text","text":"Ciao studente"}}"#,
            &mut st,
        );
        assert_eq!(
            text_events,
            vec![
                EngineEvent::TextStart {
                    message_id: "m1".to_string(),
                    index: 0.0,
                },
                EngineEvent::TextDelta {
                    message_id: "m1".to_string(),
                    index: 0.0,
                    text: "Ciao studente".to_string(),
                },
                EngineEvent::Assistant {
                    message_id: "m1".to_string(),
                    texts: vec![(0, "Ciao studente".to_string())],
                    tool_uses: Vec::new(),
                },
            ]
        );

        let tool_events = parse_opencode_line(
            r#"{"type":"tool_use","sessionID":"ses_abc123","part":{"id":"call_99","tool":"execute","state":{"status":"completed","input":{"query":"test"},"output":"risultato","metadata":{"metadata":{"toolCalls":[{"tool":"app.ask","status":"completed"}]}}}}}"#,
            &mut st,
        );
        assert_eq!(
            tool_events,
            vec![
                EngineEvent::Assistant {
                    message_id: "mcall_99".to_string(),
                    texts: Vec::new(),
                    tool_uses: vec![ToolUse {
                        index: 0,
                        id: "call_99".to_string(),
                        name: "mcp__app__ask".to_string(),
                        input: Some(json!({"query": "test"})),
                    }],
                },
                EngineEvent::ToolResult {
                    tool_use_id: "call_99".to_string(),
                    is_error: false,
                    text: "risultato".to_string(),
                },
            ]
        );

        let finish = parse_opencode_line(
            r#"{"type":"step_finish","sessionID":"ses_abc123","part":{"id":"p4","type":"step-finish","reason":"stop","cost":0.005,"tokens":{"input":150,"output":50,"reasoning":10,"cache":{"read":20,"write":0}}}}"#,
            &mut st,
        );
        assert_eq!(
            finish,
            vec![EngineEvent::Result {
                is_error: false,
                subtype: "success".to_string(),
                text: None,
                session_id: "ses_abc123".to_string(),
                input_tokens: 150.0,
                output_tokens: 60.0,
                cache_read_tokens: 20.0,
                cache_creation_tokens: 0.0,
                cost_usd: 0.005,
            }]
        );
    }

    #[test]
    fn malformed_lines_give_no_events() {
        let mut st = OpencodeParseState::new();
        assert!(parse_opencode_line("not json", &mut st).is_empty());
        assert!(parse_opencode_line("null", &mut st).is_empty());
        assert!(parse_opencode_line("[1]", &mut st).is_empty());
        assert!(!st.has_init);

        // Null part: the defaults still announce the message, it just has no text.
        let events = parse_opencode_line(r#"{"type":"text","part":null}"#, &mut st);
        assert_eq!(
            events,
            vec![
                EngineEvent::Init {
                    session_id: String::new(),
                    tools: Vec::new(),
                    mcp_servers: Vec::new(),
                },
                EngineEvent::TextStart {
                    message_id: format!("msg_{}", st.run),
                    index: 0.0,
                },
            ]
        );
    }

    #[test]
    fn error_lines_become_notes() {
        let mut st = OpencodeParseState::new();
        assert!(parse_opencode_line(r#"{"type":"error","error":"boom"}"#, &mut st).is_empty());
        assert!(parse_opencode_line(r#"{"type":"error"}"#, &mut st).is_empty());
        assert_eq!(
            st.notes,
            vec![
                "opencode error: boom".to_string(),
                r#"opencode error: {"type":"error"}"#.to_string(),
            ]
        );
    }

    #[test]
    fn agent_file_and_mcp_config_match_node() {
        assert_eq!(
            agent_file("sys"),
            "---\ndescription: ia-learner\nmode: primary\n---\n\nsys\n"
        );
        assert_eq!(
            mcp_config(&launch_for_args()),
            concat!(
                "{\n",
                "  \"$schema\": \"https://opencode.ai/config.json\",\n",
                "  \"mcp\": {\n",
                "    \"servers\": {\n",
                "      \"app\": {\n",
                "        \"type\": \"remote\",\n",
                "        \"url\": \"http://127.0.0.1:4000/mcp\",\n",
                "        \"headers\": {\n",
                "          \"x-learner-token\": \"tok\"\n",
                "        }\n",
                "      }\n",
                "    }\n",
                "  }\n",
                "}"
            )
        );
    }

    #[test]
    fn opencode_models_filter_and_fallback() {
        assert_eq!(
            parse_opencode_models("model-a\nwarn: x\nerror: y\ninfo: z\n\n model-b \n"),
            strings(&["model-a", "model-b"])
        );
        assert_eq!(parse_opencode_models("  \n"), fallback_models());
        assert_eq!(
            fallback_models(),
            strings(&[
                "omniroute/auto/best-coding",
                "omniroute/auto/best-fast",
                "omniroute/auto/fast",
                "google/gemini-2.5-pro",
                "google/gemini-2.5-flash",
                "opencode/big-pickle",
            ])
        );
    }
}
