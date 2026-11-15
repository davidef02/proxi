//! Antigravity (agy) adapter (port of `app/server/agy.ts`).
//!
//! agy takes its system prompt, tools and MCP servers only from an agent file in
//! `<scratch>/.agents/agents`, so the process runs in the scratch dir and reaches the repo via
//! `--add-dir`. Resume token = the agy conversation id.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

use crate::claude;
use crate::engine::{
    EngineBin, EngineEvent, EngineHandle, EngineLaunch, EngineMsg, Grant, ToolUse,
};
use crate::js;

/// The static list `agy models` falls back to (`agy.ts:10-17`).
pub const AGY_MODELS_FALLBACK: [&str; 6] = [
    "gemini-3.8-flash-high",
    "gemini-3.8-flash-medium",
    "gemini-3.8-flash-low",
    "claude-sonnet-4-6",
    "claude-opus-4-6-thinking",
    "gpt-oss-120b-medium",
];

const AGENT: &str = "ia-learner";

/// Our grants -> agy's tool names. read_url_content (web fetch) is denied by agy without a
/// global rule: omitted on purpose.
fn tools_for(grant: &[Grant]) -> Vec<&'static str> {
    let mut tools = Vec::new();
    for grant in grant {
        match grant {
            Grant::Read => tools.extend(["view_file", "list_dir", "grep_search"]),
            Grant::Web => tools.push("search_web"),
        }
    }
    tools
}

/// A finite JSON number, else 0 (Node's `num`).
fn num(value: Option<&Value>) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .unwrap_or(0.0)
}

/// A JSON string, else "" (Node's `str`).
fn str(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// `mcp_app_x` -> `mcp__app__x`: the rest of the app matches claude-style names.
pub fn map_tool_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("mcp_app_") {
        return format!("mcp__app__{rest}");
    }
    if let Some(rest) = name.strip_prefix("mcp_job_") {
        return format!("mcp__job__{rest}");
    }
    name.to_string()
}

/// Per-process parse state: ids must not repeat after a respawn, and agy reports cumulative usage.
pub struct AgyParseState {
    /// Unique per process: message and tool ids must not repeat after a respawn.
    run: String,
    conversation_id: String,
    /// Tool steps already announced (step -> tool-use id).
    tools: HashMap<String, String>,
    /// agent_response steps: text so far; absent from `done` = still open.
    text: IndexMap<String, String>,
    done: HashSet<String>,
    /// Cumulative usage of the process at the previous result.
    last: (f64, f64, f64),
    /// Diagnostics for the stderr callback (error_message steps).
    pub notes: Vec<String>,
}

impl AgyParseState {
    pub fn new() -> Self {
        static RUNS: AtomicU64 = AtomicU64::new(0);
        Self {
            run: format!(
                "{}_{}",
                std::process::id(),
                RUNS.fetch_add(1, Ordering::Relaxed) + 1
            ),
            conversation_id: String::new(),
            tools: HashMap::new(),
            text: IndexMap::new(),
            done: HashSet::new(),
            last: (0.0, 0.0, 0.0),
            notes: Vec::new(),
        }
    }
}

/// One assistant message per text step, once.
fn finalise(st: &mut AgyParseState, step: &str, out: &mut Vec<EngineEvent>) {
    if !st.done.insert(step.to_string()) {
        return;
    }
    let text = st.text.get(step).cloned().unwrap_or_default();
    if !text.is_empty() {
        out.push(EngineEvent::Assistant {
            message_id: format!("a{}_{}", st.run, step),
            texts: vec![(0, text)],
            tool_uses: Vec::new(),
        });
    }
}

/// One stdout line -> the events it carries; never panics on garbage.
pub fn parse_agy_line(line: &str, st: &mut AgyParseState) -> Vec<EngineEvent> {
    let Ok(m) = js::parse(line) else {
        return Vec::new();
    };
    if !m.is_object() {
        return Vec::new();
    }
    parse(&m, st)
}

fn parse(m: &Value, st: &mut AgyParseState) -> Vec<EngineEvent> {
    let mut out = Vec::new();
    match m.get("event").and_then(Value::as_str) {
        Some("init") => {
            let mut conversation = str(m.get("conversation_id"));
            if conversation.is_empty() {
                conversation = str(m.get("init").and_then(|init| init.get("conversation_id")));
            }
            if conversation.is_empty() {
                conversation = st.conversation_id.clone();
            }
            st.conversation_id = conversation.clone();
            let tools = match m.get("init").and_then(|init| init.get("tools")) {
                Some(Value::Array(items)) => {
                    items.iter().map(|item| js::js_string(Some(item))).collect()
                }
                _ => Vec::new(),
            };
            out.push(EngineEvent::Init {
                session_id: conversation,
                tools,
                mcp_servers: Vec::new(),
            });
        }
        Some("step_update") => {
            let u = m.get("step_update");
            let step = js::js_number_string(num(u.and_then(|u| u.get("step_index"))));
            let conversation = str(u.and_then(|u| u.get("conversation_id")));
            if !conversation.is_empty() {
                st.conversation_id = conversation;
            }
            match str(u.and_then(|u| u.get("step_type"))).as_str() {
                "agent_response" => {
                    let delta = str(u.and_then(|u| u.get("text_delta")));
                    let mid = format!("a{}_{}", st.run, step);
                    if !delta.is_empty() && !st.text.contains_key(&step) {
                        st.text.insert(step.clone(), String::new());
                        out.push(EngineEvent::TextStart {
                            message_id: mid.clone(),
                            index: 0.0,
                        });
                    }
                    if !delta.is_empty() {
                        st.text.entry(step.clone()).or_default().push_str(&delta);
                        out.push(EngineEvent::TextDelta {
                            message_id: mid,
                            index: 0.0,
                            text: delta,
                        });
                    }
                    let state = str(u.and_then(|u| u.get("state")));
                    if state == "DONE" || state == "ERROR" {
                        finalise(st, &step, &mut out);
                    }
                }
                "tool" => {
                    let info = u.and_then(|u| u.get("tool_info"));
                    let mut id = st.tools.get(&step).cloned();
                    if id.is_none() {
                        let new_id = format!("agy{}_{}", st.run, step);
                        st.tools.insert(step.clone(), new_id.clone());
                        let tool_name = str(u.and_then(|u| u.get("tool_name")));
                        let name = if tool_name.is_empty() {
                            str(info.and_then(|info| info.get("name")))
                        } else {
                            tool_name
                        };
                        let input = match info.and_then(|info| info.get("parameters")) {
                            None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
                            Some(parameters) => parameters.clone(),
                        };
                        out.push(EngineEvent::Assistant {
                            message_id: format!("m{new_id}"),
                            texts: Vec::new(),
                            tool_uses: vec![ToolUse {
                                index: 0,
                                id: new_id.clone(),
                                name: map_tool_name(&name),
                                input: Some(input),
                            }],
                        });
                        id = Some(new_id);
                    }
                    let state = str(u.and_then(|u| u.get("state")));
                    match (id, state.as_str()) {
                        (Some(id), "DONE") => out.push(EngineEvent::ToolResult {
                            tool_use_id: id,
                            is_error: false,
                            text: str(info.and_then(|info| info.get("output"))),
                        }),
                        (Some(id), "ERROR") => {
                            let message = str(info
                                .and_then(|info| info.get("error"))
                                .and_then(|error| error.get("message")));
                            out.push(EngineEvent::ToolResult {
                                tool_use_id: id,
                                is_error: true,
                                text: if message.is_empty() {
                                    "errore".to_string()
                                } else {
                                    message
                                },
                            });
                        }
                        _ => {}
                    }
                }
                "error_message" => {
                    let text = str(u.and_then(|u| u.get("text_delta")));
                    let detail = if text.is_empty() {
                        u.map(Value::to_string).unwrap_or_else(|| "{}".to_string())
                    } else {
                        text
                    };
                    st.notes.push(format!("agy error: {detail}"));
                }
                _ => {}
            }
        }
        Some("result") => {
            let r = m.get("result");
            let open: Vec<String> = st.text.keys().cloned().collect();
            for step in open {
                finalise(st, &step, &mut out);
            }
            let usage = r.and_then(|r| r.get("usage"));
            let cur = (
                num(usage.and_then(|u| u.get("input_tokens"))),
                num(usage.and_then(|u| u.get("output_tokens")))
                    + num(usage.and_then(|u| u.get("thinking_tokens"))),
                num(usage.and_then(|u| u.get("cache_read_tokens"))),
            );
            let diff = |a: f64, b: f64| (a - b).max(0.0);
            let status = {
                let status = str(r.and_then(|r| r.get("status")));
                if status.is_empty() {
                    "SUCCESS".to_string()
                } else {
                    status
                }
            };
            if let Some(Value::Array(actions)) = r.and_then(|r| r.get("denied_actions")) {
                for action in actions {
                    let display = str(action.get("display_name"));
                    let action = if display.is_empty() {
                        str(action.get("action"))
                    } else {
                        display
                    };
                    out.push(EngineEvent::ActionDenied { action });
                }
            }
            let error = str(r.and_then(|r| r.get("error")));
            let text = if !error.is_empty() {
                Some(error)
            } else {
                let response = str(r.and_then(|r| r.get("response")));
                (!response.is_empty()).then_some(response)
            };
            let session = str(r.and_then(|r| r.get("conversation_id")));
            out.push(EngineEvent::Result {
                is_error: status != "SUCCESS" || js::truthy(r.and_then(|r| r.get("error"))),
                subtype: if status == "SUCCESS" {
                    "success".to_string()
                } else {
                    status.to_lowercase()
                },
                text,
                session_id: if session.is_empty() {
                    st.conversation_id.clone()
                } else {
                    session.clone()
                },
                input_tokens: diff(cur.0, st.last.0),
                output_tokens: diff(cur.1, st.last.1),
                cache_read_tokens: diff(cur.2, st.last.2),
                cache_creation_tokens: 0.0,
                cost_usd: 0.0,
            });
            st.last = cur;
            // Next turn: its steps may reuse nothing from this one.
            st.tools.clear();
            st.text.clear();
            st.done.clear();
            if !session.is_empty() {
                st.conversation_id = session;
            }
        }
        _ => {}
    }
    out
}

fn json_string(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// The agent file agy reads from `<scratch>/.agents/agents/ia-learner.md` (`agy.ts:151-171`).
pub fn agent_file(l: &EngineLaunch) -> String {
    let tools = serde_json::to_string(&tools_for(&l.grant)).unwrap_or_else(|_| "[]".to_string());
    let mut headers = serde_json::Map::new();
    headers.insert(
        "x-learner-token".to_string(),
        Value::String(l.mcp.token.clone()),
    );
    [
        "---".to_string(),
        format!("name: {AGENT}"),
        format!("description: {AGENT}"),
        "mainAgent: true".to_string(),
        "subagent: false".to_string(),
        "inheritMcp: false".to_string(),
        "inheritCustomizations: false".to_string(),
        "excludeDefaultComponents: false".to_string(),
        format!("tools: {tools}"),
        "mcpServers:".to_string(),
        format!("  - name: {}", json_string(&l.mcp.name)),
        format!("    url: {}", json_string(&l.mcp.url)),
        format!("    headers: {}", Value::Object(headers)),
        "---".to_string(),
        l.system_prompt.clone(),
        String::new(),
    ]
    .join("\n")
}

/// Pure: argv for `l`; `conversation_id` is the resumed conversation, if any.
pub fn build_agy_args(l: &EngineLaunch, conversation_id: Option<&str>) -> Vec<String> {
    let mut args = l.bin.args.clone();
    args.extend(
        [
            "--agent",
            AGENT,
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--model",
            &l.model,
            "--disable-slash-commands",
        ]
        .iter()
        .map(|arg| arg.to_string()),
    );
    let mut dirs: Vec<&Path> = vec![&l.repo_root];
    let extra_data_root = l.data_root.as_ref().filter(|data_root| {
        crate::data_root::resolve_path(data_root) != crate::data_root::resolve_path(&l.repo_root)
    });
    if let Some(data_root) = extra_data_root {
        dirs.push(data_root);
    }
    dirs.extend(l.add_dirs.iter().map(|dir| dir.as_path()));
    for dir in dirs {
        args.push("--add-dir".to_string());
        args.push(dir.display().to_string());
    }
    if let Some(conversation) = conversation_id.filter(|token| !token.is_empty()) {
        args.push("--conversation".to_string());
        args.push(conversation.to_string());
    }
    args
}

fn agy_user_line(text: &str) -> String {
    serde_json::json!({
        "event": "user",
        "message": { "content": text },
    })
    .to_string()
}

/// Writes the agent file agy reads; the scratch dir is also the child's cwd.
fn prepare(l: &EngineLaunch) -> std::io::Result<()> {
    let dir = l.scratch_dir.join(".agents").join("agents");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(format!("{AGENT}.md")), agent_file(l))
}

/// Spawns agy and pumps its output into `EngineMsg`s. Never panics on a bad launch: a dead handle
/// plus the error messages keeps the session's turn machinery uniform.
pub fn spawn_agy(l: EngineLaunch) -> (Box<dyn EngineHandle>, mpsc::UnboundedReceiver<EngineMsg>) {
    let (tx, rx) = mpsc::unbounded_channel();

    if let Err(err) = prepare(&l) {
        return claude::dead(tx, rx, Some(format!("spawn error: {err}")), None);
    }

    let argv = build_agy_args(&l, l.resume.as_deref());
    // No ANTHROPIC_API_KEY stripping: agy has its own auth.
    let mut command = claude::engine_command(
        &l.bin.cmd,
        &argv,
        &l.scratch_dir,
        &claude::base_env(l.data_root.as_deref()),
        &[],
        Stdio::piped(),
    );

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return claude::dead(
                tx,
                rx,
                Some(format!(
                    "spawn error: {}",
                    crate::engine::spawn_error_message(&l.bin.cmd, &err)
                )),
                None,
            );
        }
    };
    let pid = child.id();
    let stdin = child.stdin().take();
    let stdout = child.stdout().take();
    let stderr = child.stderr().take();

    let got_init = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<String>();
    let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<()>();
    let (exit_tx, exit_rx) = watch::channel(false);

    if let Some(mut stdin) = stdin {
        tokio::spawn(async move {
            while let Some(line) = writer_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    break;
                }
            }
            // Dropping stdin closes the pipe: the engine sees EOF.
        });
    }

    let stdout_task = tokio::spawn({
        let tx = tx.clone();
        let got_init = got_init.clone();
        async move {
            let mut state = AgyParseState::new();
            claude::pump_lines(stdout, |line| {
                for event in parse_agy_line(&line, &mut state) {
                    if matches!(event, EngineEvent::Init { .. }) {
                        got_init.store(true, Ordering::SeqCst);
                    }
                    let _ = tx.send(EngineMsg::Event(event));
                }
                for note in state.notes.drain(..) {
                    let _ = tx.send(EngineMsg::Stderr(note));
                }
            })
            .await;
        }
    });

    let stderr_task = tokio::spawn({
        let tx = tx.clone();
        async move {
            claude::pump_lines(stderr, |line| {
                let _ = tx.send(EngineMsg::Stderr(line));
            })
            .await;
        }
    });

    let resume = l.resume.clone();
    let waiter_stopped = stopped.clone();
    tokio::spawn(async move {
        let (code, signal) = claude::wait_child(&mut child, &mut kill_rx).await;
        // The readers see EOF only once every process holding the pipes is gone.
        let _ = stdout_task.await;
        let _ = stderr_task.await;
        // agy's refusal text is unknown: dying before init with --conversation counts as refused.
        let resuming = resume
            .as_deref()
            .filter(|token| !token.is_empty())
            .is_some();
        if resuming && !got_init.load(Ordering::SeqCst) && !waiter_stopped.load(Ordering::SeqCst) {
            let _ = tx.send(EngineMsg::Event(EngineEvent::ResumeFailed));
        }
        let _ = tx.send(EngineMsg::Exit { code, signal });
        let _ = exit_tx.send(true);
    });

    (
        Box::new(claude::LiveHandle::new(
            Some(writer_tx),
            Some(kill_tx),
            exit_rx,
            pid,
            agy_user_line,
            stopped,
        )),
        rx,
    )
}

/// `agy models`: one `<id>\t<label>` line per model; anything else is dropped.
pub fn parse_agy_models(stdout: &str) -> Vec<String> {
    let ids: Vec<String> = stdout
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| line.contains('\t'))
        .map(|line| js::js_trim(line.split('\t').next().unwrap_or_default()).to_string())
        .collect();
    if ids.is_empty() {
        fallback_models()
    } else {
        ids
    }
}

fn fallback_models() -> Vec<String> {
    AGY_MODELS_FALLBACK
        .iter()
        .map(|model| model.to_string())
        .collect()
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The live model list, cached by the caller per server.
pub async fn list_agy_models(bin: &EngineBin) -> Vec<String> {
    let mut command = tokio::process::Command::new(&bin.cmd);
    command
        .args(&bin.args)
        .arg("models")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return fallback_models(),
    };
    let Some(mut stdout) = child.stdout.take() else {
        return fallback_models();
    };
    let mut buffer = Vec::new();
    if tokio::time::timeout(Duration::from_secs(10), stdout.read_to_end(&mut buffer))
        .await
        .is_err()
    {
        // Keep what arrived: Node parses the partial buffer after the kill too.
        let _ = child.start_kill();
    }
    parse_agy_models(&String::from_utf8_lossy(&buffer))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::engine::McpServer;
    use crate::protocol::LearnerSettings;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn kind(event: &EngineEvent) -> &'static str {
        match event {
            EngineEvent::Init { .. } => "init",
            EngineEvent::TextStart { .. } => "text-start",
            EngineEvent::TextDelta { .. } => "text-delta",
            EngineEvent::Assistant { .. } => "assistant",
            EngineEvent::ToolResult { .. } => "tool-result",
            EngineEvent::Result { .. } => "result",
            EngineEvent::ActionDenied { .. } => "action-denied",
            EngineEvent::ResumeFailed => "resume-failed",
            _ => "other",
        }
    }

    fn types(events: &[EngineEvent]) -> Vec<&'static str> {
        events.iter().map(kind).collect()
    }

    /// Node's `su(o)`: a step_update of conversation c1 plus the given fields, in that order.
    fn su(fields: Value) -> Value {
        let mut u = serde_json::Map::new();
        u.insert("conversation_id".to_string(), json!("c1"));
        if let Value::Object(fields) = fields {
            u.extend(fields);
        }
        json!({ "event": "step_update", "step_update": Value::Object(u) })
    }

    fn run(lines: &[Value]) -> (AgyParseState, Vec<EngineEvent>) {
        let mut st = AgyParseState::new();
        let mut events = Vec::new();
        for line in lines {
            events.extend(parse_agy_line(&line.to_string(), &mut st));
        }
        (st, events)
    }

    fn assistant_text(event: &EngineEvent) -> String {
        match event {
            EngineEvent::Assistant { texts, .. } => texts
                .first()
                .map(|(_, text)| text.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn launch_for_args() -> EngineLaunch {
        EngineLaunch {
            bin: EngineBin {
                cmd: "agy".to_string(),
                args: vec!["script.mjs".to_string()],
            },
            repo_root: PathBuf::from("/repo"),
            data_root: None,
            scratch_dir: PathBuf::from("/scratch"),
            model: "m".to_string(),
            effort: None,
            system_prompt: "Sei il docente.".to_string(),
            grant: vec![Grant::Read],
            mcp: McpServer {
                name: "app".to_string(),
                url: "http://127.0.0.1:1/mcp".to_string(),
                token: "tok".to_string(),
            },
            add_dirs: Vec::new(),
            max_turns: None,
            resume: None,
            settings: LearnerSettings::default(),
            on_limits: Arc::new(|_| {}),
        }
    }

    #[test]
    fn maps_tool_names_back_to_the_claude_style() {
        assert_eq!(map_tool_name("mcp_app_quiz"), "mcp__app__quiz");
        assert_eq!(
            map_tool_name("mcp_job_submit_findings"),
            "mcp__job__submit_findings"
        );
        assert_eq!(map_tool_name("view_file"), "view_file");
    }

    #[test]
    fn init_text_deltas_and_one_assistant_per_step() {
        let (_st, events) = run(&[
            json!({ "event": "init", "conversation_id": "c1", "init": { "tools": ["view_file"] } }),
            su(
                json!({ "step_index": 1, "state": "ACTIVE", "step_type": "agent_response", "text_delta": "Ci" }),
            ),
            su(
                json!({ "step_index": 1, "state": "DONE", "step_type": "agent_response", "text_delta": "ao" }),
            ),
            su(
                json!({ "step_index": 2, "state": "DONE", "step_type": "agent_response", "text_delta": "Solo in DONE" }),
            ),
        ]);
        assert_eq!(
            types(&events),
            [
                "init",
                "text-start",
                "text-delta",
                "text-delta",
                "assistant",
                "text-start",
                "text-delta",
                "assistant",
            ]
        );
        let EngineEvent::Init {
            session_id, tools, ..
        } = &events[0]
        else {
            panic!("an init");
        };
        assert_eq!(session_id, "c1");
        assert_eq!(tools, &strings(&["view_file"]));
        assert_eq!(assistant_text(&events[4]), "Ciao");
        assert_eq!(assistant_text(&events[7]), "Solo in DONE");
    }

    #[test]
    fn tool_steps_announce_once_and_result_with_the_same_id() {
        let (_st, events) = run(&[
            su(json!({
                "step_index": 3, "state": "ACTIVE", "step_type": "tool", "tool_name": "mcp_app_echo",
                "tool_info": { "name": "mcp_app_echo", "parameters": { "a": 1 } },
            })),
            su(json!({
                "step_index": 3, "state": "DONE", "step_type": "tool", "tool_name": "mcp_app_echo",
                "tool_info": { "output": "ok" },
            })),
            su(json!({
                "step_index": 4, "state": "ACTIVE", "step_type": "tool", "tool_name": "mcp_job_x",
                "tool_info": { "name": "mcp_job_x", "parameters": {} },
            })),
            su(json!({
                "step_index": 4, "state": "ERROR", "step_type": "tool", "tool_name": "mcp_job_x",
                "tool_info": { "error": { "message": "boom" } },
            })),
        ]);
        assert_eq!(
            types(&events),
            ["assistant", "tool-result", "assistant", "tool-result"]
        );
        let uses: Vec<&ToolUse> = events
            .iter()
            .filter_map(|event| match event {
                EngineEvent::Assistant { tool_uses, .. } => tool_uses.first(),
                _ => None,
            })
            .collect();
        assert_eq!(uses.len(), 2);
        assert_eq!(uses[0].name, "mcp__app__echo");
        assert_eq!(uses[0].input, Some(json!({ "a": 1 })));
        assert_eq!(uses[1].name, "mcp__job__x");
        assert_eq!(uses[1].input, Some(json!({})));
        let EngineEvent::Assistant { message_id, .. } = &events[0] else {
            panic!("an assistant");
        };
        assert_eq!(message_id, &format!("m{}", uses[0].id));

        let results: Vec<(&str, bool, &str)> = events
            .iter()
            .filter_map(|event| match event {
                EngineEvent::ToolResult {
                    tool_use_id,
                    is_error,
                    text,
                } => Some((tool_use_id.as_str(), *is_error, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            results,
            [
                (uses[0].id.as_str(), false, "ok"),
                (uses[1].id.as_str(), true, "boom"),
            ]
        );
    }

    #[test]
    fn result_usage_is_the_per_turn_diff_with_denied_actions() {
        let result = |n: i64| {
            json!({ "event": "result", "result": {
                "conversation_id": "c1",
                "status": "SUCCESS",
                "response": "x",
                "usage": {
                    "input_tokens": 10 * n,
                    "output_tokens": 5 * n,
                    "thinking_tokens": n,
                    "cache_read_tokens": 2 * n,
                },
            } })
        };
        let (_st, events) = run(&[
            result(1),
            json!({ "event": "result", "result": {
                "conversation_id": "c1",
                "status": "ERROR",
                "error": "no quota",
                "denied_actions": [{ "action": "read_url", "display_name": "ReadUrlContent" }],
                "usage": {
                    "input_tokens": 30,
                    "output_tokens": 15,
                    "thinking_tokens": 3,
                    "cache_read_tokens": 6,
                },
            } }),
        ]);
        assert_eq!(types(&events), ["result", "action-denied", "result"]);
        let EngineEvent::Result {
            is_error,
            subtype,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cost_usd,
            ..
        } = &events[0]
        else {
            panic!("a result");
        };
        assert_eq!(
            (
                *input_tokens,
                *output_tokens,
                *cache_read_tokens,
                *cost_usd,
                *is_error,
                subtype.as_str()
            ),
            (10.0, 6.0, 2.0, 0.0, false, "success")
        );
        assert_eq!(
            events[1],
            EngineEvent::ActionDenied {
                action: "ReadUrlContent".to_string()
            }
        );
        let EngineEvent::Result {
            is_error,
            subtype,
            text,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            ..
        } = &events[2]
        else {
            panic!("a second result");
        };
        assert_eq!(
            (
                *input_tokens,
                *output_tokens,
                *cache_read_tokens,
                *is_error,
                subtype.as_str(),
                text.as_deref()
            ),
            (20.0, 12.0, 4.0, true, "error", Some("no quota"))
        );
    }

    #[test]
    fn malformed_lines_give_no_events() {
        let mut st = AgyParseState::new();
        for line in [
            "not json",
            "null",
            "[1]",
            "{\"event\":\"step_update\",\"step_update\":\"x\"}",
            "{\"event\":\"step_update\",\"step_update\":{\"step_type\":\"agent_response\",\"state\":\"DONE\"}}",
        ] {
            assert!(parse_agy_line(line, &mut st).is_empty(), "{line}");
        }
        assert_eq!(
            parse_agy_line("{\"event\":\"result\",\"result\":null}", &mut st),
            vec![EngineEvent::Result {
                is_error: false,
                subtype: "success".to_string(),
                text: None,
                session_id: String::new(),
                input_tokens: 0.0,
                output_tokens: 0.0,
                cache_read_tokens: 0.0,
                cache_creation_tokens: 0.0,
                cost_usd: 0.0,
            }]
        );
    }

    #[test]
    fn error_message_steps_become_notes() {
        let (st, events) = run(&[
            su(json!({ "step_index": 5, "step_type": "error_message", "text_delta": "quota" })),
            su(json!({ "step_index": 6, "step_type": "error_message" })),
        ]);
        assert!(events.is_empty());
        assert_eq!(
            st.notes,
            [
                "agy error: quota".to_string(),
                "agy error: {\"conversation_id\":\"c1\",\"step_index\":6,\"step_type\":\"error_message\"}".to_string(),
            ]
        );
    }

    #[test]
    fn args_have_no_p_and_conversation_on_resume() {
        let mut l = launch_for_args();
        l.add_dirs = vec![PathBuf::from("/x")];
        let want = strings(&[
            "script.mjs",
            "--agent",
            "ia-learner",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--model",
            "m",
            "--disable-slash-commands",
            "--add-dir",
            "/repo",
            "--add-dir",
            "/x",
            "--conversation",
            "c9",
        ]);
        assert_eq!(build_agy_args(&l, Some("c9")), want);
        let fresh = build_agy_args(&l, None);
        assert!(!fresh.iter().any(|arg| arg == "-p"));
        assert!(!fresh.iter().any(|arg| arg == "--conversation"));

        // The data root is added only when it resolves elsewhere; no dedupe against add_dirs.
        l.data_root = Some(PathBuf::from("/repo/"));
        let args = build_agy_args(&l, None);
        assert_eq!(args.iter().filter(|arg| *arg == "--add-dir").count(), 2);
        l.data_root = Some(PathBuf::from("/d"));
        let args = build_agy_args(&l, None);
        let at = args.iter().position(|arg| arg == "--add-dir").unwrap();
        assert_eq!(
            &args[at..at + 4],
            strings(&["--add-dir", "/repo", "--add-dir", "/d"]).as_slice()
        );
    }

    #[test]
    fn agent_file_matches_the_node_layout() {
        let want = r#"---
name: ia-learner
description: ia-learner
mainAgent: true
subagent: false
inheritMcp: false
inheritCustomizations: false
excludeDefaultComponents: false
tools: ["view_file","list_dir","grep_search"]
mcpServers:
  - name: "app"
    url: "http://127.0.0.1:1/mcp"
    headers: {"x-learner-token":"tok"}
---
Sei il docente.
"#;
        assert_eq!(agent_file(&launch_for_args()), want);
    }

    #[test]
    fn agy_models_keep_tab_lines_first_field_else_fallback() {
        assert_eq!(
            AGY_MODELS_FALLBACK,
            [
                "gemini-3.8-flash-high",
                "gemini-3.8-flash-medium",
                "gemini-3.8-flash-low",
                "claude-sonnet-4-6",
                "claude-opus-4-6-thinking",
                "gpt-oss-120b-medium",
            ]
        );
        assert_eq!(
            parse_agy_models("model-a\tA\nModelli disponibili\n model-b \tB\r\n"),
            strings(&["model-a", "model-b"])
        );
        assert_eq!(parse_agy_models("no tabs here"), fallback_models());
        assert_eq!(parse_agy_models(""), fallback_models());
    }
}
