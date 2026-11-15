//! Claude CLI adapter: locate, argv, env, spawn, kill (port of `app/server/claude.ts`).
//!
//! The engine is the user's own `claude` binary in headless stream-json mode; resume token = the
//! claude session uuid. The process tree dies with the handle (Job Object on Windows, process group
//! on Unix).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

use crate::engine::{
    CLAUDE_AUTH_ERROR_MESSAGE, EngineEvent, EngineHandle, EngineMsg, is_auth_error,
    is_resume_refused, uuid_v4,
};
use crate::js;
use crate::protocol::LearnerSettings;
use crate::stream::{ParseState, parse_stream_line};

/// Executable of an engine plus the args that must precede Claude's own (tests: a fake script).
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeBin {
    pub cmd: String,
    pub args: Vec<String>,
}

/// Built-in capabilities the session may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    Read,
    Web,
}

impl Grant {
    fn tools(self) -> &'static [&'static str] {
        match self {
            Grant::Read => &["Read", "Glob", "Grep"],
            Grant::Web => &["WebSearch", "WebFetch"],
        }
    }
}

/// The app's MCP endpoint as Claude sees it.
pub struct McpServer {
    pub name: String,
    pub url: String,
    pub token: String,
}

/// What a session asks of the adapter (intent, not flags).
pub struct ClaudeLaunch {
    pub bin: ClaudeBin,
    pub repo_root: PathBuf,
    pub data_root: Option<PathBuf>,
    pub scratch_dir: PathBuf,
    pub model: String,
    pub effort: Option<String>,
    pub system_prompt: String,
    pub grant: Vec<Grant>,
    pub mcp: McpServer,
    pub add_dirs: Vec<PathBuf>,
    pub max_turns: Option<u32>,
    pub resume: Option<String>,
    pub settings: LearnerSettings,
    /// App-wide, not the turn's business (Claude's rate-limit reports).
    pub on_limits: Arc<dyn Fn(Value) + Send + Sync>,
}

/// Pure: argv for `l`; `fresh_id` is the session uuid used when there is nothing to resume.
pub fn build_claude_args(l: &ClaudeLaunch, fresh_id: &str) -> Vec<String> {
    let tools: Vec<&str> = l.grant.iter().flat_map(|g| g.tools()).copied().collect();
    let tools_csv = tools.join(",");
    let mut allowed = tools.clone();
    let mcp_tool = format!("mcp__{}", l.mcp.name);
    allowed.push(&mcp_tool);
    let allowed_csv = allowed.join(",");

    let mut args: Vec<String> = l.bin.args.clone();
    args.push("-p".to_string());
    args.push("--model".to_string());
    args.push(l.model.clone());
    if let Some(effort) = l.effort.as_deref().filter(|e| !e.is_empty()) {
        args.push("--effort".into());
        args.push(effort.into());
    }
    args.extend(
        [
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--setting-sources",
            "",
            "--system-prompt-file",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(l.scratch_dir.join("system.md").display().to_string());
    args.push("--tools".into());
    args.push(tools_csv);
    args.extend(
        [
            "--disable-slash-commands",
            "--strict-mcp-config",
            "--mcp-config",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(l.scratch_dir.join("mcp.json").display().to_string());
    args.push("--allowedTools".into());
    args.push(allowed_csv);
    args.extend(
        ["--permission-prompts", "none"]
            .iter()
            .map(|s| s.to_string()),
    );
    for dir in &l.add_dirs {
        args.push("--add-dir".into());
        args.push(dir.display().to_string());
    }
    if let Some(data_root) = &l.data_root {
        let differs = crate::data_root::resolve_path(data_root)
            != crate::data_root::resolve_path(&l.repo_root);
        if differs && !l.add_dirs.iter().any(|dir| dir == data_root) {
            args.push("--add-dir".into());
            args.push(data_root.display().to_string());
        }
    }
    if let Some(max_turns) = l.max_turns.filter(|n| *n > 0) {
        args.push("--max-turns".into());
        args.push(max_turns.to_string());
    }
    match l.resume.as_deref().filter(|token| !token.is_empty()) {
        Some(resume) => {
            args.push("--resume".into());
            args.push(resume.to_string());
        }
        None => {
            args.push("--session-id".into());
            args.push(fresh_id.to_string());
        }
    }
    args
}

fn strip_one_quote_pair(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Resolves the Claude CLI: `LEARNER_CLAUDE_BIN` override, PATH, then the standard install dirs.
/// On Windows only `claude.exe` counts, never a `.cmd` / extensionless shim.
pub fn locate_claude_binary(
    env: &HashMap<String, String>,
    windows: bool,
    os_home: Option<PathBuf>,
) -> Option<String> {
    if let Some(over) = env
        .get("LEARNER_CLAUDE_BIN")
        .map(|value| js::js_trim(value))
        .filter(|value| !value.is_empty())
    {
        return Some(over.to_string());
    }

    let bin_name = if windows { "claude.exe" } else { "claude" };
    let mut search_dirs: Vec<PathBuf> = Vec::new();

    // `??` only falls through on a missing key: an empty PATH is still a PATH.
    let path_val = env
        .get("PATH")
        .or_else(|| {
            if windows {
                env.get("Path").or_else(|| env.get("path"))
            } else {
                None
            }
        })
        .map(String::as_str)
        .unwrap_or("");
    for raw in path_val.split(if windows { ';' } else { ':' }) {
        let unquoted = strip_one_quote_pair(js::js_trim(raw));
        let trimmed = js::js_trim(unquoted);
        if !trimmed.is_empty() {
            search_dirs.push(PathBuf::from(trimmed));
        }
    }

    let home = if windows {
        env.get("USERPROFILE")
    } else {
        env.get("HOME")
    }
    .filter(|value| !value.is_empty())
    .map(PathBuf::from)
    .or(os_home);
    if let Some(home) = &home {
        search_dirs.push(home.join(".local").join("bin"));
        search_dirs.push(home.join(".claude").join("local"));
        search_dirs.push(home.join(".claude").join("local").join("bin"));
    }

    if windows {
        let app_data = env
            .get("APPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                home.as_ref()
                    .map(|home| home.join("AppData").join("Roaming"))
            });
        if let Some(app_data) = app_data {
            search_dirs.push(app_data.join("npm"));
        }
        let local_app_data = env
            .get("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join("AppData").join("Local")));
        if let Some(local_app_data) = local_app_data {
            search_dirs.push(local_app_data.join("Programs").join("claude"));
        }
    } else {
        search_dirs.push(PathBuf::from("/usr/local/bin"));
        search_dirs.push(PathBuf::from("/opt/homebrew/bin"));
        if let Some(home) = &home {
            search_dirs.push(home.join(".npm-global").join("bin"));
        }
    }

    let mut seen = HashSet::new();
    for dir in search_dirs {
        if !seen.insert(crate::data_root::resolve_path(&dir)) {
            continue;
        }
        let candidate = dir.join(bin_name);
        if !candidate.is_file() {
            continue;
        }
        if windows
            && !candidate
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".exe")
        {
            continue;
        }
        return Some(
            crate::data_root::resolve_path(&candidate)
                .display()
                .to_string(),
        );
    }
    None
}

/// `locate_claude_binary` against this process's environment and home.
pub fn locate_from_process() -> Option<String> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let home = crate::data_root::home_dir();
    let os_home = if home.as_os_str().is_empty() {
        None
    } else {
        Some(home)
    };
    locate_claude_binary(&env, cfg!(windows), os_home)
}

/// The environment the child must get on top of the inherited one.
pub enum EnvPlan {
    Ready {
        set: Vec<(String, String)>,
        remove: Vec<String>,
    },
    MissingApiKey,
}

/// Every engine's child env; claude adds the API key decision on top (`child_env`).
pub(crate) fn base_env(data_root: Option<&Path>) -> Vec<(String, String)> {
    let mut set = vec![("MCP_TOOL_TIMEOUT".to_string(), "86400000".to_string())];
    if let Some(root) = data_root {
        set.push(("LEARNER_DATA_ROOT".to_string(), root.display().to_string()));
    }
    set
}

pub fn child_env(settings: &LearnerSettings, data_root: Option<&Path>) -> EnvPlan {
    let mut set = base_env(data_root);
    if settings.claude_auth == "apiKey" {
        let key = js::js_trim(&settings.anthropic_api_key);
        if key.is_empty() {
            return EnvPlan::MissingApiKey;
        }
        set.push(("ANTHROPIC_API_KEY".to_string(), key.to_string()));
        EnvPlan::Ready {
            set,
            remove: Vec::new(),
        }
    } else {
        // Even when the server's own environment has one: subscription means the login.
        EnvPlan::Ready {
            set,
            remove: vec!["ANTHROPIC_API_KEY".to_string()],
        }
    }
}

/// Node's `lineSplitter`: complete lines only, one trailing `\r` dropped, blank lines skipped.
pub fn split_lines(buf: &mut Vec<u8>, chunk: &[u8]) -> Vec<String> {
    buf.extend_from_slice(chunk);
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(offset) = buf[start..].iter().position(|&b| b == b'\n') {
        let end = start + offset;
        let mut line = &buf[start..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        let text = String::from_utf8_lossy(line).into_owned();
        if !js::js_trim(&text).is_empty() {
            out.push(text);
        }
        start = end + 1;
    }
    buf.drain(..start);
    out
}

/// End of stream: the unterminated tail, if any.
pub fn flush_line(buf: &mut Vec<u8>) -> Option<String> {
    let mut line = std::mem::take(buf);
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    let text = String::from_utf8_lossy(&line).into_owned();
    if js::js_trim(&text).is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The command every adapter starts: cwd, pipes, env, and the tree-killing wrappers.
pub(crate) fn engine_command(
    program: &str,
    argv: &[String],
    cwd: &Path,
    set: &[(String, String)],
    remove: &[String],
    stdin: Stdio,
) -> process_wrap::tokio::CommandWrap {
    let mut command = process_wrap::tokio::CommandWrap::with_new(program, |command| {
        command
            .args(argv)
            .current_dir(cwd)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in set {
            command.env(key, value);
        }
        for key in remove {
            command.env_remove(key);
        }
    });
    #[cfg(windows)]
    {
        // windowsHide, and the job so the whole tree dies with the handle.
        use process_wrap::tokio::{CreationFlags, JobObject, KillOnDrop};
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        command.wrap(JobObject);
        command.wrap(KillOnDrop);
        command.wrap(CreationFlags(CREATE_NO_WINDOW));
    }
    #[cfg(unix)]
    {
        use process_wrap::tokio::{KillOnDrop, ProcessGroup};
        command.wrap(ProcessGroup::leader());
        command.wrap(KillOnDrop);
    }
    command
}

/// Reads a pipe to EOF and hands over complete lines, the unterminated tail last.
pub(crate) async fn pump_lines<R: tokio::io::AsyncRead + Unpin>(
    reader: Option<R>,
    mut on_line: impl FnMut(String),
) {
    let Some(mut reader) = reader else {
        return;
    };
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        for line in split_lines(&mut buf, &chunk[..read]) {
            on_line(line);
        }
    }
    if let Some(line) = flush_line(&mut buf) {
        on_line(line);
    }
}

/// Waits for the engine, or kills it when the handle stops; `(None, None)` when the wait failed.
pub(crate) async fn wait_child(
    child: &mut Box<dyn process_wrap::tokio::ChildWrapper>,
    kill_rx: &mut mpsc::UnboundedReceiver<()>,
) -> (Option<i32>, Option<String>) {
    enum WaitOutcome {
        Done(std::io::Result<ExitStatus>),
        Kill,
    }
    let outcome = tokio::select! {
        status = child.wait() => WaitOutcome::Done(status),
        _ = kill_rx.recv() => WaitOutcome::Kill,
    };
    let status = match outcome {
        WaitOutcome::Done(status) => status,
        WaitOutcome::Kill => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    match &status {
        Ok(status) => (status.code(), exit_signal(status)),
        Err(_) => (None, None),
    }
}

fn prepare(l: &ClaudeLaunch) -> std::io::Result<()> {
    fs::create_dir_all(&l.scratch_dir)?;
    fs::write(l.scratch_dir.join("system.md"), &l.system_prompt)?;
    let mut headers = Map::new();
    headers.insert(
        "x-learner-token".to_string(),
        Value::String(l.mcp.token.clone()),
    );
    let mut server = Map::new();
    server.insert("type".to_string(), Value::String("http".to_string()));
    server.insert("url".to_string(), Value::String(l.mcp.url.clone()));
    server.insert("headers".to_string(), Value::Object(headers));
    let mut servers = Map::new();
    servers.insert(l.mcp.name.clone(), Value::Object(server));
    let mut root = Map::new();
    root.insert("mcpServers".to_string(), Value::Object(servers));
    let body = serde_json::to_string(&Value::Object(root))
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    fs::write(l.scratch_dir.join("mcp.json"), body)
}

#[cfg(unix)]
pub(crate) fn exit_signal(status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|signal| {
        match signal {
            1 => "SIGHUP",
            2 => "SIGINT",
            3 => "SIGQUIT",
            6 => "SIGABRT",
            9 => "SIGKILL",
            11 => "SIGSEGV",
            13 => "SIGPIPE",
            15 => "SIGTERM",
            _ => return format!("SIG{signal}"),
        }
        .to_string()
    })
}

#[cfg(not(unix))]
pub(crate) fn exit_signal(_status: &ExitStatus) -> Option<String> {
    None
}

pub(crate) struct DeadHandle;

impl EngineHandle for DeadHandle {
    fn send(&mut self, _text: &str) {}

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        Box::pin(async {})
    }

    fn pid(&self) -> Option<u32> {
        None
    }
}

/// The live process: writer for the stdin lines, kill switch, and the exit watch `stop` waits on.
pub(crate) struct LiveHandle {
    writer: Option<mpsc::UnboundedSender<String>>,
    kill: Option<mpsc::UnboundedSender<()>>,
    exit: watch::Receiver<bool>,
    pid: Option<u32>,
    /// The stdin line shape: each engine has its own (claude.ts, agy.ts, opencode.ts).
    line: fn(&str) -> String,
    stopped: Arc<AtomicBool>,
}

impl LiveHandle {
    pub(crate) fn new(
        writer: Option<mpsc::UnboundedSender<String>>,
        kill: Option<mpsc::UnboundedSender<()>>,
        exit: watch::Receiver<bool>,
        pid: Option<u32>,
        line: fn(&str) -> String,
        stopped: Arc<AtomicBool>,
    ) -> Self {
        Self {
            writer,
            kill,
            exit,
            pid,
            line,
            stopped,
        }
    }
}

impl EngineHandle for LiveHandle {
    fn send(&mut self, text: &str) {
        let Some(writer) = self.writer.as_ref() else {
            return;
        };
        let _ = writer.send((self.line)(text));
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.stopped.store(true, Ordering::SeqCst);
        // Closing stdin lets a well-behaved engine exit on its own; the kill covers the rest.
        self.writer.take();
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
        let mut exit = self.exit.clone();
        Box::pin(async move {
            if *exit.borrow() {
                return;
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), exit.wait_for(|done| *done)).await;
        })
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }
}

fn claude_user_line(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    })
    .to_string()
}

/// The dead-handle path shared by a missing key and a failed spawn: the caller attaches, the pump
/// then delivers the messages and the exit.
pub(crate) fn dead(
    tx: mpsc::UnboundedSender<EngineMsg>,
    rx: mpsc::UnboundedReceiver<EngineMsg>,
    stderr: Option<String>,
    code: Option<i32>,
) -> (Box<dyn EngineHandle>, mpsc::UnboundedReceiver<EngineMsg>) {
    if let Some(line) = stderr {
        let _ = tx.send(EngineMsg::Stderr(line));
    }
    let _ = tx.send(EngineMsg::Exit { code, signal: None });
    (Box::new(DeadHandle), rx)
}

/// Spawns Claude and pumps its output into `EngineMsg`s. Never panics on a bad launch: a dead
/// handle plus the error messages keeps the session's turn machinery uniform.
pub fn spawn_claude(
    l: ClaudeLaunch,
) -> (Box<dyn EngineHandle>, mpsc::UnboundedReceiver<EngineMsg>) {
    let (tx, rx) = mpsc::unbounded_channel();

    let (set, remove) = match child_env(&l.settings, l.data_root.as_deref()) {
        EnvPlan::MissingApiKey => {
            return dead(
                tx,
                rx,
                Some("Nessuna API key Anthropic configurata nelle Impostazioni.".to_string()),
                Some(1),
            );
        }
        EnvPlan::Ready { set, remove } => (set, remove),
    };

    // A `.cmd`/`.bat` shim (an npm-installed `claude.cmd`) is a valid program: std runs batch files
    // through cmd.exe with cmd-safe escaping, and refuses arguments it cannot escape (CVE-2024-24576).
    let program = if l.bin.cmd == "claude" {
        locate_from_process().unwrap_or_else(|| "claude".to_string())
    } else {
        l.bin.cmd.clone()
    };

    if let Err(err) = prepare(&l) {
        return dead(
            tx,
            rx,
            Some(format!(
                "spawn error: {}",
                crate::engine::spawn_error_message(&program, &err)
            )),
            None,
        );
    }

    let argv = build_claude_args(&l, &uuid_v4());

    let mut command = engine_command(
        &program,
        &argv,
        &l.repo_root,
        &set,
        &remove,
        Stdio::piped(),
    );

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return dead(
                tx,
                rx,
                Some(format!(
                    "spawn error: {}",
                    crate::engine::spawn_error_message(&program, &err)
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
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
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
        let on_limits = l.on_limits;
        async move {
            let mut state = ParseState::default();
            pump_lines(stdout, |line| {
                emit_line(&line, &mut state, &tx, &got_init, &on_limits);
            })
            .await;
        }
    });

    let stderr_task = tokio::spawn({
        let tx = tx.clone();
        let tail = tail.clone();
        async move {
            pump_lines(stderr, |line| {
                push_tail(&tail, &line);
                let _ = tx.send(EngineMsg::Stderr(line));
            })
            .await;
        }
    });

    let resume = l.resume;
    tokio::spawn(async move {
        let (code, signal) = wait_child(&mut child, &mut kill_rx).await;
        // The readers see EOF only once every process holding the pipes is gone.
        let _ = stdout_task.await;
        let _ = stderr_task.await;
        let combined = tail
            .lock()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        if resume
            .as_deref()
            .filter(|token| !token.is_empty())
            .is_some()
            && !got_init.load(Ordering::SeqCst)
            && is_resume_refused(&combined)
        {
            let _ = tx.send(EngineMsg::Event(EngineEvent::ResumeFailed));
        }
        if is_auth_error(&combined) {
            let _ = tx.send(EngineMsg::Event(EngineEvent::AuthFailed {
                message: Some(CLAUDE_AUTH_ERROR_MESSAGE.to_string()),
            }));
        }
        let _ = tx.send(EngineMsg::Exit { code, signal });
        let _ = exit_tx.send(true);
    });

    (
        Box::new(LiveHandle::new(
            Some(writer_tx),
            Some(kill_tx),
            exit_rx,
            pid,
            claude_user_line,
            Arc::new(AtomicBool::new(false)),
        )),
        rx,
    )
}

fn emit_line(
    line: &str,
    state: &mut ParseState,
    tx: &mpsc::UnboundedSender<EngineMsg>,
    got_init: &AtomicBool,
    on_limits: &Arc<dyn Fn(Value) + Send + Sync>,
) {
    for event in parse_stream_line(line, state) {
        match &event {
            EngineEvent::Ignored => continue,
            EngineEvent::Init { .. } => got_init.store(true, Ordering::SeqCst),
            // App-wide, not the turn's business.
            EngineEvent::Limits { limits } => {
                on_limits(limits.clone());
                continue;
            }
            EngineEvent::Result {
                is_error: true,
                text: Some(text),
                ..
            } if is_auth_error(text) => {
                let _ = tx.send(EngineMsg::Event(EngineEvent::AuthFailed {
                    message: Some(CLAUDE_AUTH_ERROR_MESSAGE.to_string()),
                }));
            }
            _ => {}
        }
        let _ = tx.send(EngineMsg::Event(event));
    }
}

fn push_tail(tail: &Mutex<VecDeque<String>>, line: &str) {
    if let Ok(mut tail) = tail.lock() {
        tail.push_back(line.to_string());
        if tail.len() > 20 {
            tail.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("proxi-claude-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Fixtures are assembled at runtime: the pre-flight scans source files for key prefixes.
    fn fake_key(rest: &str) -> String {
        ["sk", rest].concat()
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn launch_for_args() -> ClaudeLaunch {
        ClaudeLaunch {
            bin: ClaudeBin {
                cmd: "claude".to_string(),
                args: Vec::new(),
            },
            repo_root: PathBuf::from("/r"),
            data_root: None,
            scratch_dir: PathBuf::from("/s"),
            model: "sonnet".to_string(),
            effort: None,
            system_prompt: "x".to_string(),
            grant: vec![Grant::Read],
            mcp: McpServer {
                name: "app".to_string(),
                url: "http://x".to_string(),
                token: "t".to_string(),
            },
            add_dirs: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            max_turns: None,
            resume: None,
            settings: LearnerSettings::default(),
            on_limits: Arc::new(|_| {}),
        }
    }

    #[test]
    fn build_args_snapshot_first_launch() {
        let sys = Path::new("/s").join("system.md").display().to_string();
        let mcp = Path::new("/s").join("mcp.json").display().to_string();
        let mut want = strings(&[
            "-p",
            "--model",
            "sonnet",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--setting-sources",
            "",
            "--system-prompt-file",
        ]);
        want.push(sys);
        want.extend(strings(&[
            "--tools",
            "Read,Glob,Grep",
            "--disable-slash-commands",
            "--strict-mcp-config",
            "--mcp-config",
        ]));
        want.push(mcp);
        want.extend(strings(&[
            "--allowedTools",
            "Read,Glob,Grep,mcp__app",
            "--permission-prompts",
            "none",
            "--add-dir",
            "/a",
            "--add-dir",
            "/b",
            "--session-id",
            "uuid-1",
        ]));

        assert_eq!(build_claude_args(&launch_for_args(), "uuid-1"), want);
    }

    #[test]
    fn permission_prompts_none_comes_after_allowed_tools() {
        let args = build_claude_args(&launch_for_args(), "uuid-1");
        let perm = args
            .iter()
            .position(|arg| arg == "--permission-prompts")
            .expect("must include --permission-prompts");
        assert_eq!(args[perm + 1], "none");
        let allowed = args
            .iter()
            .position(|arg| arg == "--allowedTools")
            .expect("--allowedTools");
        assert!(allowed < perm);
    }

    #[test]
    fn max_turns_adds_the_flag_before_the_session_flag() {
        let mut l = launch_for_args();
        l.max_turns = Some(14);
        let args = build_claude_args(&l, "uuid-1");
        let max = args.iter().position(|arg| arg == "--max-turns").unwrap();
        assert_eq!(args[max + 1], "14");
        assert!(max < args.iter().position(|arg| arg == "--session-id").unwrap());
        assert!(
            !build_claude_args(&launch_for_args(), "uuid-1")
                .iter()
                .any(|arg| arg == "--max-turns")
        );
    }

    #[test]
    fn effort_follows_model_and_is_omitted_when_absent() {
        let mut l = launch_for_args();
        l.effort = Some("high".to_string());
        let args = build_claude_args(&l, "uuid-1");
        let effort = args.iter().position(|arg| arg == "--effort").unwrap();
        assert_eq!(args[effort + 1], "high");
        let model = args.iter().position(|arg| arg == "--model").unwrap();
        assert_eq!(effort, model + 2);
        assert!(
            !build_claude_args(&launch_for_args(), "uuid-1")
                .iter()
                .any(|arg| arg == "--effort")
        );
    }

    #[test]
    fn bin_args_come_first_and_resume_replaces_the_session_flag() {
        let mut l = launch_for_args();
        l.bin = ClaudeBin {
            cmd: "node".to_string(),
            args: vec!["script.mjs".to_string()],
        };
        l.resume = Some("tok-9".to_string());
        l.grant = Vec::new();
        l.add_dirs = Vec::new();
        l.mcp.name = "job".to_string();
        let args = build_claude_args(&l, "uuid-1");
        assert_eq!(args[0], "script.mjs");
        let tools = args.iter().position(|arg| arg == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
        let allowed = args.iter().position(|arg| arg == "--allowedTools").unwrap();
        assert_eq!(args[allowed + 1], "mcp__job");
        assert_eq!(
            &args[args.len() - 2..],
            strings(&["--resume", "tok-9"]).as_slice()
        );
        assert!(!args.iter().any(|arg| arg == "--session-id"));
        assert!(!args.iter().any(|arg| arg == "--add-dir"));
        assert!(!args.iter().any(|arg| arg == "--bare"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions")
        );

        let mut both = launch_for_args();
        both.grant = vec![Grant::Read, Grant::Web];
        let web: Vec<String> = build_claude_args(&both, "u")
            .into_iter()
            .filter(|arg| arg.contains("WebFetch"))
            .collect();
        assert_eq!(
            web,
            strings(&[
                "Read,Glob,Grep,WebSearch,WebFetch",
                "Read,Glob,Grep,WebSearch,WebFetch,mcp__app",
            ])
        );
    }

    #[test]
    fn data_root_added_only_when_it_differs_and_is_not_already_there() {
        let mut l = launch_for_args();
        l.add_dirs = Vec::new();
        l.grant = Vec::new();

        // Resolved equal to the repo root: no extra dir.
        l.data_root = Some(PathBuf::from("/r/"));
        let args = build_claude_args(&l, "u");
        assert!(!args.iter().any(|arg| arg == "--add-dir"));

        l.data_root = Some(PathBuf::from("/d"));
        let args = build_claude_args(&l, "u");
        let at = args.iter().position(|arg| arg == "--add-dir").unwrap();
        assert_eq!(args[at + 1], "/d");

        l.add_dirs = vec![PathBuf::from("/d")];
        let args = build_claude_args(&l, "u");
        assert_eq!(args.iter().filter(|arg| *arg == "--add-dir").count(), 1);
    }

    #[test]
    fn locate_respects_the_env_override() {
        let e = env(&[("LEARNER_CLAUDE_BIN", "C:\\custom\\claude-custom.exe")]);
        assert_eq!(
            locate_claude_binary(&e, false, None).as_deref(),
            Some("C:\\custom\\claude-custom.exe")
        );
        let e = env(&[("LEARNER_CLAUDE_BIN", "  /opt/claude  ")]);
        assert_eq!(
            locate_claude_binary(&e, false, None).as_deref(),
            Some("/opt/claude")
        );
    }

    #[test]
    fn locate_finds_claude_exe_on_the_windows_path() {
        let dir = temp_dir("bin-path-win");
        let target = dir.join("claude.exe");
        fs::write(&target, "").unwrap();
        let e = env(&[("PATH", &dir.display().to_string())]);
        assert_eq!(
            locate_claude_binary(&e, true, None),
            Some(
                crate::data_root::resolve_path(&target)
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn locate_ignores_cmd_bat_and_extensionless_shims_on_windows() {
        let shim = temp_dir("bin-shim");
        let exe = temp_dir("bin-exe");
        fs::write(shim.join("claude.cmd"), "@echo off").unwrap();
        fs::write(shim.join("claude.bat"), "@echo off").unwrap();
        fs::write(shim.join("claude"), "echo test").unwrap();
        let target = exe.join("claude.exe");
        fs::write(&target, "").unwrap();

        let path = format!("{};{}", shim.display(), exe.display());
        let e = env(&[("PATH", &path)]);
        assert_eq!(
            locate_claude_binary(&e, true, None),
            Some(
                crate::data_root::resolve_path(&target)
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn locate_falls_back_to_the_standard_home_dirs() {
        let home = temp_dir("bin-home");
        let local_bin = home.join(".local").join("bin");
        fs::create_dir_all(&local_bin).unwrap();
        let target = local_bin.join("claude.exe");
        fs::write(&target, "").unwrap();
        let e = env(&[("PATH", "")]);
        assert_eq!(
            locate_claude_binary(&e, true, Some(home.clone())),
            Some(
                crate::data_root::resolve_path(&target)
                    .display()
                    .to_string()
            )
        );

        // Same home, ~/.local/bin empty: the next standard dir answers.
        let second = temp_dir("bin-home-2");
        let local_claude = second.join(".claude").join("local");
        fs::create_dir_all(&local_claude).unwrap();
        let target = local_claude.join("claude.exe");
        fs::write(&target, "").unwrap();
        assert_eq!(
            locate_claude_binary(&env(&[("PATH", "")]), true, Some(second)),
            Some(
                crate::data_root::resolve_path(&target)
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn locate_reads_the_case_insensitive_path_key_and_unquotes_it() {
        let dir = temp_dir("bin-path-quoted");
        let target = dir.join("claude.exe");
        fs::write(&target, "").unwrap();
        let e = env(&[("Path", &format!("\"{}\"", dir.display()))]);
        assert_eq!(
            locate_claude_binary(&e, true, None),
            Some(
                crate::data_root::resolve_path(&target)
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn locate_uses_the_windows_appdata_dirs() {
        let home = temp_dir("bin-appdata-home");
        let app_data = temp_dir("bin-appdata");
        let npm = app_data.join("npm");
        fs::create_dir_all(&npm).unwrap();
        let target = npm.join("claude.exe");
        fs::write(&target, "").unwrap();
        let e = env(&[("PATH", ""), ("APPDATA", &app_data.display().to_string())]);
        assert_eq!(
            locate_claude_binary(&e, true, Some(home)),
            Some(
                crate::data_root::resolve_path(&target)
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn locate_finds_extensionless_claude_outside_windows() {
        let dir = temp_dir("bin-unix");
        let target = dir.join("claude");
        fs::write(&target, "").unwrap();
        let e = env(&[("PATH", &dir.display().to_string())]);
        assert_eq!(
            locate_claude_binary(&e, false, None),
            Some(
                crate::data_root::resolve_path(&target)
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn locate_returns_none_when_nothing_is_there() {
        let e = env(&[("PATH", "C:\\nonexistent\\path")]);
        assert_eq!(
            locate_claude_binary(&e, true, Some(PathBuf::from("C:\\nonexistent\\home"))),
            None
        );
    }

    #[test]
    fn child_env_sets_the_constants_and_removes_the_key_for_subscription() {
        let settings = LearnerSettings::default();
        let root = Path::new("/data");
        match child_env(&settings, Some(root)) {
            EnvPlan::Ready { set, remove } => {
                assert!(set.contains(&("MCP_TOOL_TIMEOUT".to_string(), "86400000".to_string())));
                assert!(
                    set.contains(&("LEARNER_DATA_ROOT".to_string(), root.display().to_string()))
                );
                assert!(!set.iter().any(|(key, _)| key == "ANTHROPIC_API_KEY"));
                assert_eq!(remove, ["ANTHROPIC_API_KEY"]);
            }
            EnvPlan::MissingApiKey => panic!("subscription never needs a key"),
        }

        match child_env(&settings, None) {
            EnvPlan::Ready { set, .. } => {
                assert!(!set.iter().any(|(key, _)| key == "LEARNER_DATA_ROOT"));
            }
            EnvPlan::MissingApiKey => panic!("subscription never needs a key"),
        }
    }

    #[test]
    fn child_env_sets_the_trimmed_key_for_apikey() {
        let key = fake_key("-ant-env-key");
        let settings = LearnerSettings {
            claude_auth: "apiKey".to_string(),
            anthropic_api_key: format!("  {key}  "),
            ..LearnerSettings::default()
        };
        match child_env(&settings, None) {
            EnvPlan::Ready { set, remove } => {
                assert!(set.contains(&("ANTHROPIC_API_KEY".to_string(), key)));
                assert!(remove.is_empty());
            }
            EnvPlan::MissingApiKey => panic!("a key is configured"),
        }
    }

    #[test]
    fn child_env_without_a_key_is_missing() {
        let settings = LearnerSettings {
            claude_auth: "apiKey".to_string(),
            anthropic_api_key: "   ".to_string(),
            ..LearnerSettings::default()
        };
        assert!(matches!(child_env(&settings, None), EnvPlan::MissingApiKey));
    }

    #[test]
    fn base_env_sets_the_timeout_and_the_data_root_only_when_given() {
        assert_eq!(
            base_env(None),
            vec![("MCP_TOOL_TIMEOUT".to_string(), "86400000".to_string())]
        );
        let root = Path::new("/data");
        assert_eq!(
            base_env(Some(root)),
            vec![
                ("MCP_TOOL_TIMEOUT".to_string(), "86400000".to_string()),
                (
                    "LEARNER_DATA_ROOT".to_string(),
                    root.display().to_string()
                ),
            ]
        );
    }

    #[test]
    fn line_splitter_handles_split_chunks_and_crlf() {
        let mut buf = Vec::new();
        assert_eq!(split_lines(&mut buf, b"a\nb"), strings(&["a"]));
        assert_eq!(split_lines(&mut buf, b"c\r\n"), strings(&["bc"]));
        assert_eq!(split_lines(&mut buf, b"\n \n"), Vec::<String>::new());
        assert_eq!(split_lines(&mut buf, b"x\r\n"), strings(&["x"]));
        assert_eq!(flush_line(&mut buf), None);

        // A multi-byte char split across chunks must survive.
        let text = "perché";
        let mut buf = Vec::new();
        let mut out = Vec::new();
        for byte in text.as_bytes() {
            out.extend(split_lines(&mut buf, std::slice::from_ref(byte)));
        }
        assert!(out.is_empty());
        assert_eq!(flush_line(&mut buf).as_deref(), Some(text));
        assert_eq!(flush_line(&mut buf), None);
    }

    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    fn fake_bin(extra: &[&str]) -> ClaudeBin {
        let fake = crate::data_root::repo_root()
            .join("app")
            .join("test")
            .join("fake-claude.mjs");
        let mut args = vec![fake.display().to_string()];
        args.extend(extra.iter().map(|arg| arg.to_string()));
        ClaudeBin {
            cmd: "node".to_string(),
            args,
        }
    }

    fn launch(repo: &Path) -> ClaudeLaunch {
        ClaudeLaunch {
            bin: fake_bin(&[]),
            repo_root: repo.to_path_buf(),
            data_root: None,
            scratch_dir: repo.join("scratch"),
            model: "haiku".to_string(),
            effort: None,
            system_prompt: "x".to_string(),
            grant: Vec::new(),
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

    async fn next(rx: &mut mpsc::UnboundedReceiver<EngineMsg>) -> EngineMsg {
        tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await
            .expect("an engine message within 15 s")
            .expect("the engine channel is open")
    }

    fn event_kind(msg: &EngineMsg) -> Option<&'static str> {
        match msg {
            EngineMsg::Event(EngineEvent::Init { .. }) => Some("init"),
            EngineMsg::Event(EngineEvent::TextStart { .. }) => Some("text-start"),
            EngineMsg::Event(EngineEvent::TextDelta { .. }) => Some("text-delta"),
            EngineMsg::Event(EngineEvent::Assistant { .. }) => Some("assistant"),
            EngineMsg::Event(EngineEvent::ToolResult { .. }) => Some("tool-result"),
            EngineMsg::Event(EngineEvent::Result { .. }) => Some("result"),
            EngineMsg::Event(EngineEvent::ControlResponse { .. }) => Some("control-response"),
            EngineMsg::Event(EngineEvent::ResumeFailed) => Some("resume-failed"),
            EngineMsg::Event(EngineEvent::ActionDenied { .. }) => Some("action-denied"),
            EngineMsg::Event(EngineEvent::AuthFailed { .. }) => Some("auth-failed"),
            EngineMsg::Event(EngineEvent::Limits { .. }) => Some("limits"),
            EngineMsg::Event(EngineEvent::Ignored) => Some("ignored"),
            _ => None,
        }
    }

    async fn collect_until(
        rx: &mut mpsc::UnboundedReceiver<EngineMsg>,
        mut done: impl FnMut(&EngineMsg) -> bool,
    ) -> Vec<EngineMsg> {
        let mut messages = Vec::new();
        loop {
            let msg = next(rx).await;
            let finished = done(&msg);
            messages.push(msg);
            if finished {
                return messages;
            }
        }
    }

    #[tokio::test]
    async fn hello_streams_the_full_event_sequence_and_writes_the_scratch_files() {
        if !node_available() {
            eprintln!("skipping: node is not on PATH");
            return;
        }
        let repo = temp_dir("hello");
        let (mut handle, mut rx) = spawn_claude(launch(&repo));
        handle.send("ciao");

        // The argv line and the result travel on different pipes: wait for both.
        let mut messages = Vec::new();
        let mut argv_line = None;
        let mut saw_result = false;
        while !saw_result || argv_line.is_none() {
            let msg = next(&mut rx).await;
            match &msg {
                EngineMsg::Event(EngineEvent::Result { .. }) => saw_result = true,
                EngineMsg::Stderr(line) if line.starts_with("fake-claude argv: ") => {
                    argv_line = Some(line.clone());
                }
                _ => {}
            }
            messages.push(msg);
        }
        let argv_line = argv_line.unwrap();
        assert!(
            argv_line.contains("--setting-sources") && argv_line.contains("--session-id"),
            "{argv_line}"
        );

        let kinds: Vec<&str> = messages.iter().filter_map(event_kind).collect();
        assert_eq!(
            kinds,
            [
                "init",
                "text-start",
                "text-delta",
                "text-delta",
                "text-delta",
                "assistant",
                "assistant",
                "result",
            ]
        );

        let deltas: String = messages
            .iter()
            .filter_map(|msg| match msg {
                EngineMsg::Event(EngineEvent::TextDelta { text, .. }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, "Ciao, sono il docente.");

        let text_start = messages
            .iter()
            .find_map(|msg| match msg {
                EngineMsg::Event(EngineEvent::TextStart { index, .. }) => Some(*index),
                _ => None,
            })
            .expect("a text-start");
        assert_eq!(text_start, 1.0);

        let assistants: Vec<&EngineEvent> = messages
            .iter()
            .filter_map(|msg| match msg {
                EngineMsg::Event(event @ EngineEvent::Assistant { .. }) => Some(event),
                _ => None,
            })
            .collect();
        let EngineEvent::Assistant { texts, .. } = assistants[0] else {
            unreachable!()
        };
        assert!(texts.is_empty(), "the thinking block carries no text");
        let EngineEvent::Assistant { texts, .. } = assistants[1] else {
            unreachable!()
        };
        assert_eq!(texts, &vec![(0, "Ciao, sono il docente.".to_string())]);

        let result = messages
            .iter()
            .find_map(|msg| match msg {
                EngineMsg::Event(event @ EngineEvent::Result { .. }) => Some(event),
                _ => None,
            })
            .expect("a result event");
        let EngineEvent::Result { is_error, .. } = result else {
            unreachable!()
        };
        assert!(!is_error);

        let scratch = repo.join("scratch");
        assert_eq!(fs::read_to_string(scratch.join("system.md")).unwrap(), "x");
        assert_eq!(
            fs::read_to_string(scratch.join("mcp.json")).unwrap(),
            r#"{"mcpServers":{"app":{"type":"http","url":"http://127.0.0.1:1/mcp","headers":{"x-learner-token":"tok"}}}}"#
        );

        handle.stop().await;
    }

    #[tokio::test]
    async fn env_out_dump_honours_child_env() {
        if !node_available() {
            eprintln!("skipping: node is not on PATH");
            return;
        }
        let repo = temp_dir("env-dump");
        let data = temp_dir("env-dump-data");
        let env_file = repo.join("env.json");
        let exit_file = repo.join("env2.json");

        let mut api = launch(&repo);
        api.data_root = Some(data.clone());
        api.settings.claude_auth = "apiKey".to_string();
        let key = fake_key("-ant-contract-key");
        api.settings.anthropic_api_key = format!("  {key}  ");
        api.scratch_dir = repo.join("scratch-api");
        let env_arg = env_file.display().to_string();
        api.bin = fake_bin(&["--env-out", &env_arg]);
        let (mut handle, mut rx) = spawn_claude(api);
        handle.send("ciao");
        collect_until(&mut rx, |msg| event_kind(msg) == Some("result")).await;
        let dumped: Value = serde_json::from_str(&fs::read_to_string(&env_file).unwrap()).unwrap();
        assert_eq!(dumped["ANTHROPIC_API_KEY"], json!(key));
        assert_eq!(dumped["MCP_TOOL_TIMEOUT"], json!("86400000"));
        assert_eq!(
            dumped["LEARNER_DATA_ROOT"],
            json!(data.display().to_string())
        );
        handle.stop().await;

        let mut subscription = launch(&repo);
        subscription.data_root = Some(data.clone());
        subscription.scratch_dir = repo.join("scratch-sub");
        let env_arg = exit_file.display().to_string();
        subscription.bin = fake_bin(&["--env-out", &env_arg]);
        let (mut handle, mut rx) = spawn_claude(subscription);
        handle.send("ciao");
        collect_until(&mut rx, |msg| event_kind(msg) == Some("result")).await;
        let dumped: Value = serde_json::from_str(&fs::read_to_string(&exit_file).unwrap()).unwrap();
        assert_eq!(dumped["ANTHROPIC_API_KEY"], Value::Null);
        assert_eq!(dumped["MCP_TOOL_TIMEOUT"], json!("86400000"));
        assert_eq!(
            dumped["LEARNER_DATA_ROOT"],
            json!(data.display().to_string())
        );
        handle.stop().await;
    }

    #[tokio::test]
    async fn crash_exits_with_code_one_and_no_result() {
        if !node_available() {
            eprintln!("skipping: node is not on PATH");
            return;
        }
        let repo = temp_dir("crash");
        let (mut handle, mut rx) = spawn_claude(launch(&repo));
        handle.send("FAKE:crash");

        let messages = collect_until(&mut rx, |msg| matches!(msg, EngineMsg::Exit { .. })).await;
        assert!(matches!(
            messages.last(),
            Some(EngineMsg::Exit {
                code: Some(1),
                signal: None
            })
        ));
        assert_eq!(
            messages.iter().filter_map(event_kind).collect::<Vec<_>>(),
            ["init"]
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .is_none()
        );
        handle.stop().await;
    }

    #[tokio::test]
    async fn a_refused_resume_reports_resume_failed_then_exits() {
        if !node_available() {
            eprintln!("skipping: node is not on PATH");
            return;
        }
        let repo = temp_dir("resume"); // the repo dir must exist: it is the child's cwd
        let mut l = launch(&repo);
        l.bin = ClaudeBin {
            cmd: "node".to_string(),
            args: vec![
                "-e".to_string(),
                "console.error('No conversation found with session ID x');process.exit(1)"
                    .to_string(),
                "--".to_string(),
            ],
        };
        l.resume = Some("old-token".to_string());
        let (mut handle, mut rx) = spawn_claude(l);

        let messages = collect_until(&mut rx, |msg| matches!(msg, EngineMsg::Exit { .. })).await;
        let tail: Vec<&str> = messages.iter().filter_map(event_kind).collect();
        assert_eq!(tail, ["resume-failed"]);
        assert!(matches!(messages.last(), Some(EngineMsg::Exit { .. })));
        handle.stop().await;
    }

    #[tokio::test]
    async fn stop_kills_a_running_turn_within_five_seconds_and_exits_once() {
        if !node_available() {
            eprintln!("skipping: node is not on PATH");
            return;
        }
        let repo = temp_dir("stop");
        let (mut handle, mut rx) = spawn_claude(launch(&repo));
        handle.send("FAKE:slow");
        collect_until(&mut rx, |msg| event_kind(msg) == Some("text-delta")).await;

        let started = Instant::now();
        handle.stop().await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stop took {:?}",
            started.elapsed()
        );

        let mut exits = 0;
        loop {
            match tokio::time::timeout(Duration::from_secs(6), rx.recv())
                .await
                .expect("the channel closes within 6 s")
            {
                Some(EngineMsg::Exit { .. }) => exits += 1,
                Some(_) => {}
                None => break,
            }
        }
        assert_eq!(exits, 1);
        handle.stop().await;
    }

    #[tokio::test]
    async fn a_missing_executable_reports_spawn_enoent() {
        let repo = temp_dir("missing-bin");
        let missing = repo.join("missing-claude.exe");
        let mut l = launch(&repo);
        l.bin = ClaudeBin {
            cmd: missing.display().to_string(),
            args: Vec::new(),
        };
        let (mut handle, mut rx) = spawn_claude(l);
        assert_eq!(handle.pid(), None);

        assert_eq!(
            next(&mut rx).await,
            EngineMsg::Stderr(format!(
                "spawn error: spawn {} ENOENT",
                missing.display()
            ))
        );
        assert_eq!(
            next(&mut rx).await,
            EngineMsg::Exit {
                code: None,
                signal: None
            }
        );
        assert!(rx.recv().await.is_none());
        handle.stop().await;
    }

    #[tokio::test]
    async fn a_missing_api_key_is_reported_through_the_channel() {
        let repo = temp_dir("missing-key");
        let mut l = launch(&repo);
        l.settings.claude_auth = "apiKey".to_string();
        l.settings.anthropic_api_key = "   ".to_string();
        let (mut handle, mut rx) = spawn_claude(l);
        assert_eq!(handle.pid(), None);

        assert_eq!(
            next(&mut rx).await,
            EngineMsg::Stderr(
                "Nessuna API key Anthropic configurata nelle Impostazioni.".to_string()
            )
        );
        assert_eq!(
            next(&mut rx).await,
            EngineMsg::Exit {
                code: Some(1),
                signal: None
            }
        );
        assert!(rx.recv().await.is_none());
        handle.stop().await;

        // The node-free path must not create the scratch files either.
        assert!(!repo.join("scratch").join("system.md").exists());
    }
}
