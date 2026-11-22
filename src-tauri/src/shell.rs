//! The desktop↔binary seam, ported from `app/desktop/backend.mjs` + `startup.mjs` as pure Rust:
//! locate the `proxi` binary, build its serve argv, spawn it and wait for the URL line, probe
//! whether an existing server is a Proxi. No `tauri` types here, so the unit tests exercise it
//! directly.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;

/// Snapshot of the environment the shell reads (LEARNER_* / PROXI_BIN / PATH).
pub type Vars = HashMap<String, String>;

pub fn vars() -> Vars {
    std::env::vars().collect()
}

/// How long `proxi serve` may take to print its URL (backend.mjs uses the same 30 s).
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout of the `isLearner` probe (main.mjs uses AbortSignal.timeout(2000)).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// (bin env var, bin-args env var, bin flag, per-arg flag), in serve_args order.
const ENGINES: [(&str, &str, &str, &str); 3] = [
    ("LEARNER_CLAUDE_BIN", "LEARNER_CLAUDE_BIN_ARGS", "claude-bin", "claude-bin-arg"),
    ("LEARNER_AGY_BIN", "LEARNER_AGY_BIN_ARGS", "agy-bin", "agy-bin-arg"),
    ("LEARNER_OPENCODE_BIN", "LEARNER_OPENCODE_BIN_ARGS", "opencode-bin", "opencode-bin-arg"),
];

/// The `proxi serve` argv; engine flags only when their env vars are set (backend.mjs serveArgs).
/// A malformed `LEARNER_*_BIN_ARGS` raises like backend.mjs (`JSON.parse(rawArgs)`, main.mjs shows
/// the error and exits): the raw value is never silently dropped.
pub fn serve_args(repo_root: &str, dist_dir: &str, port: u16, env: &Vars) -> Result<Vec<String>, String> {
    let mut args = vec![
        "serve".to_string(),
        "--repo-root".to_string(),
        repo_root.to_string(),
        "--dist-dir".to_string(),
        dist_dir.to_string(),
        "--port".to_string(),
        port.to_string(),
    ];
    for (bin_key, args_key, bin_flag, arg_flag) in ENGINES {
        // backend.mjs `if (bin)`: an empty value is not a binary name.
        if let Some(bin) = env.get(bin_key).filter(|b| !b.is_empty()) {
            args.push(format!("--{bin_flag}"));
            args.push(bin.clone());
        }
        if let Some(raw) = env.get(args_key).filter(|r| !r.is_empty()) {
            // backend.mjs parses the raw args even without a binary: the error must surface.
            let list: Vec<String> = serde_json::from_str(raw).map_err(|e| {
                format!("{args_key} is not a JSON array of strings: {e} (raw value: {raw:?})")
            })?;
            for arg in list {
                args.push(format!("--{arg_flag}"));
                args.push(arg);
            }
        }
    }
    Ok(args)
}

/// The built `proxi` executable: `PROXI_BIN`, else the newest of target/release and target/debug.
pub fn proxi_binary_path(repo_root: &Path, env: &Vars) -> Result<PathBuf, String> {
    if let Some(provided) = env.get("PROXI_BIN") {
        let path = PathBuf::from(provided);
        if path.exists() {
            return Ok(path);
        }
        return Err(format!(
            "PROXI_BIN not found: {provided} (unset it to use target/, built by cargo build --release --locked)"
        ));
    }
    let exe = if cfg!(windows) { "proxi.exe" } else { "proxi" };
    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = ["release", "debug"]
        .into_iter()
        .map(|profile| repo_root.join("target").join(profile).join(exe))
        .filter(|p| p.exists())
        .map(|p| {
            let mtime = p.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            (p, mtime)
        })
        .collect();
    if candidates.is_empty() {
        return Err("proxi binary not found under target/: run cargo build --release --locked".to_string());
    }
    candidates.sort_by_key(|(_, mtime)| *mtime);
    let (path, _) = candidates.pop().expect("non-empty above");
    Ok(path)
}

/// The code repo root: `LEARNER_REPO_ROOT`, else walked up from the exe dir (the build layout is
/// `<repo>/src-tauri/target/<profile>/proxi-app.exe`) to the first dir containing `target/`.
/// A dir whose `target/` actually holds a `proxi` binary wins over a nearer one without, so the
/// repo root's own `target/` beats the shell's `src-tauri/target/`.
pub fn repo_root(env: &Vars, exe_dir: &Path) -> PathBuf {
    if let Some(root) = env.get("LEARNER_REPO_ROOT") {
        return PathBuf::from(root);
    }
    let proxi_exe = if cfg!(windows) { "proxi.exe" } else { "proxi" };
    let has_proxi = |dir: &Path| {
        ["release", "debug"]
            .into_iter()
            .any(|profile| dir.join("target").join(profile).join(proxi_exe).exists())
    };
    let mut first_with_target: Option<&Path> = None;
    let mut dir = exe_dir;
    loop {
        if dir.join("target").is_dir() {
            first_with_target.get_or_insert(dir);
            if has_proxi(dir) {
                return dir.to_path_buf();
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    first_with_target
        .map(Path::to_path_buf)
        .unwrap_or_else(|| exe_dir.to_path_buf())
}

/// `LEARNER_PORT` with the JS semantics of main.mjs (`env.LEARNER_PORT !== undefined ?
/// Number(env.LEARNER_PORT) : 4317`), trimmed; unset or not a plain number falls back to 4317;
/// "0" stays 0, the ephemeral request. Note the deliberate refinement over `|| 4317`: an empty or
/// non-numeric value would be 0 (`Number("")`) for Electron, here it is 4317.
pub fn wanted_port(env: &Vars) -> u16 {
    let Some(raw) = env.get("LEARNER_PORT").map(String::as_str) else {
        return 4317;
    };
    match raw.trim().parse::<f64>() {
        Ok(n) if n.is_finite() && n.fract() == 0.0 && (0.0..=65535.0).contains(&n) => n as u16,
        _ => 4317,
    }
}

fn reported_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^Proxi: (http://127\.0\.0\.1:(\d+))\s*$").expect("ready line regex"))
}

/// The line `proxi serve` prints on startup → (url, port); anything else is None.
pub fn parse_reported_url(line: &str) -> Option<(String, u16)> {
    let caps = reported_re().captures(line)?;
    Some((caps[1].to_string(), caps[2].parse().ok()?))
}

/// The URL to open at startup: base, or base + hash when hash is a plain in-app route (startup.mjs).
pub fn start_url(base: &str, hash: Option<&str>) -> String {
    let safe = hash.is_some_and(|h| Regex::new(r"^#/[A-Za-z0-9/_-]*$").unwrap().is_match(h));
    match (safe, hash) {
        (true, Some(hash)) => format!("{}/{}", base.trim_end_matches('/'), hash),
        _ => base.to_string(),
    }
}

/// Does a Proxi server answer at `base`? False on every failure, like the Electron probe.
pub fn is_learner(base: &str) -> bool {
    let client = reqwest::blocking::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .expect("probe client");
    let Ok(resp) = client.get(format!("{base}/api/settings")).send() else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    resp.json::<Value>().map(|v| is_learner_json(&v)).unwrap_or(false)
}

/// True when the JSON body of /api/settings says this is a Proxi server
/// (main.mjs isLearner: `typeof (await r.json())?.teacherModel === "string"`).
pub fn is_learner_json(v: &Value) -> bool {
    v.get("teacherModel").is_some_and(Value::is_string)
}

/// Spawn `bin` and wait for the `Proxi: <url>` line, `spawn_backend` of backend.mjs.
/// The stdout reader stops once the URL line is seen (L55) but keeps draining the pipe
/// so the child never blocks on a full buffer; stderr is kept for the error messages.
pub fn spawn_backend(bin: &str, args: &[String], env: &Vars) -> Result<Backend, String> {
    let mut cmd = Command::new(bin);
    #[cfg(windows)]
    {
        // windowsHide: true — a console must not flash for the backend or for taskkill.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.args(args)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start {bin}: {e}"))?;

    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (tx_url, rx_url) = mpsc::channel::<(String, u16)>();
    let (tx_stderr, rx_stderr) = mpsc::channel::<String>();

    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line_bytes: Vec<u8> = Vec::new();
        let mut found = false;
        loop {
            line_bytes.clear();
            match reader.read_until(b'\n', &mut line_bytes) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !found {
                let line = String::from_utf8_lossy(&line_bytes);
                if let Some(found_url) = parse_reported_url(line.trim_end_matches(['\r', '\n'])) {
                    found = true;
                    let _ = tx_url.send(found_url);
                }
            }
        }
    });
    std::thread::spawn(move || {
        // Kept for the error messages, capped so a chatty process cannot grow memory.
        let mut collected = String::new();
        let mut buf = [0u8; 8192];
        let mut reader = BufReader::new(stderr);
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) if collected.len() < 64 * 1024 => {
                    collected.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                Ok(_) => {}
            }
        }
        let _ = tx_stderr.send(collected);
    });

    // Stderr shows up in the error messages; give the pipe a moment to drain after the child is gone.
    let stderr_text = || rx_stderr.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    match rx_url.recv_timeout(READY_TIMEOUT) {
        Ok((url, port)) => Ok(finish_backend(child, url, port)),
        Err(RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!("proxi serve did not print its URL within 30 s: {}", stderr_text()))
        }
        Err(RecvTimeoutError::Disconnected) => {
            let code = child.wait().ok().and_then(|s| s.code());
            Err(format!("proxi serve exited with code {}: {}", exit_code_text(code), stderr_text()))
        }
    }
}

/// Moves the child process into a watcher thread and reports its exit code through a channel.
fn finish_backend(mut child: Child, url: String, port: u16) -> Backend {
    let pid = child.id();
    let (tx_exit, rx_exit) = mpsc::channel();
    std::thread::spawn(move || {
        let code = child.wait().ok().and_then(|s| s.code());
        let _ = tx_exit.send(code);
    });
    Backend { url, port, pid, exit_code: rx_exit }
}

pub fn exit_code_text(code: Option<i32>) -> String {
    code.map(|c| c.to_string()).unwrap_or_else(|| "null".to_string())
}

/// Port fallback of main.mjs startServer: try `wanted`; on failure, when the port is held by
/// another Proxi, show that one (never kill it); anything else → spawn on a free port.
pub fn start_server<F, P>(mut spawn: F, probe: P, env: &Vars) -> Result<ServerStart, String>
where
    F: FnMut(u16) -> Result<Backend, String>,
    P: Fn(&str) -> bool,
{
    let wanted = wanted_port(env);
    let error = match spawn(wanted) {
        Ok(backend) => return Ok(ServerStart::Own(backend)),
        Err(error) => error,
    };
    if wanted == 0 {
        // `--port 0` asks for a free port: nothing to fall back from.
        return Err(error);
    }
    let url = format!("http://127.0.0.1:{wanted}");
    // A second server on the same .learner/ state would corrupt job records: a foreign process on
    // the port is ignored and we take a free one, but a real Proxi is just shown.
    if probe(&url) {
        return Ok(ServerStart::External { url });
    }
    spawn(0).map(ServerStart::Own)
}

/// The result of starting the backend: ours, or one another Proxi process already serves.
#[derive(Debug)]
pub enum ServerStart {
    Own(Backend),
    External { url: String },
}

/// A backend the shell spawned and owns.
pub struct Backend {
    pub url: String,
    pub port: u16,
    pub pid: u32,
    /// Receives the child's exit code (`None` when it was killed by a signal) once, then closes.
    pub exit_code: Receiver<Option<i32>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").field("url", &self.url).field("port", &self.port).field("pid", &self.pid).finish()
    }
}

/// The silent flag for NSIS installer runs.
pub const NSIS_SILENT: [&str; 1] = ["/S"];

/// `<root>/target/release/proxi.update-pending`
pub fn pending_marker_path(root: &Path) -> PathBuf {
    root.join("target").join("release").join("proxi.update-pending")
}

/// Reads the pending update marker and returns the installer path if it exists on disk.
/// Missing or corrupt marker, or a missing installer file, returns `None`.
pub fn read_pending_installer(root: &Path) -> Option<PathBuf> {
    let marker = pending_marker_path(root);
    let content = std::fs::read_to_string(&marker).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;
    let installer_str = value.get("installer")?.as_str()?;
    let installer = PathBuf::from(installer_str);
    if installer.exists() {
        Some(installer)
    } else {
        None
    }
}

/// Removes the pending update marker file if present.
pub fn clear_pending(root: &Path) {
    let _ = std::fs::remove_file(pending_marker_path(root));
}

pub const DEFAULT_INSTALLER_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs the NSIS installer silently (`/S`) and waits for completion up to `timeout`.
/// If the installer does not exit before the timeout, kills its process tree.
pub fn run_installer_with_timeout(
    path: &Path,
    timeout: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    let mut cmd = Command::new(path);
    cmd.args(NSIS_SILENT);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status),
            None => {
                if start.elapsed() >= timeout {
                    kill_tree(pid);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("installer timed out after {timeout:?}"),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Runs the NSIS installer silently (`/S`) and waits for completion (bounded by DEFAULT_INSTALLER_TIMEOUT).
pub fn run_installer(path: &Path) -> std::io::Result<std::process::ExitStatus> {
    run_installer_with_timeout(path, DEFAULT_INSTALLER_TIMEOUT)
}

/// Best-effort kill of a whole process tree; run these args on exit (backend.mjs killTreeSync).
pub fn kill_tree_args(pid: u32) -> Vec<String> {
    vec![
        "taskkill".to_string(),
        "/PID".to_string(),
        pid.to_string(),
        "/T".to_string(),
        "/F".to_string(),
    ]
}

pub fn kill_tree(pid: u32) {
    let args = kill_tree_args(pid);
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd.status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(pairs: &[(&str, &str)]) -> Vars {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn fake_backend(url: &str, port: u16) -> Backend {
        Backend { url: url.to_string(), port, pid: 1, exit_code: mpsc::channel().1 }
    }

    #[test]
    fn serve_args_minimum() {
        let args = serve_args("C:/repo", "C:/repo/app/web/dist", 4317, &Vars::new()).unwrap();
        assert_eq!(
            args,
            ["serve", "--repo-root", "C:/repo", "--dist-dir", "C:/repo/app/web/dist", "--port", "4317"]
        );
    }

    #[test]
    fn serve_args_engine_flags() {
        let env = env_from(&[
            ("LEARNER_CLAUDE_BIN", "node.exe"),
            ("LEARNER_CLAUDE_BIN_ARGS", r#"["app/test/fake-claude.mjs"]"#),
            ("LEARNER_AGY_BIN", "C:/bin/agy.exe"),
            ("LEARNER_OPENCODE_BIN", ""),
        ]);
        let args = serve_args("R", "D", 0, &env).unwrap();
        assert!(args.windows(2).any(|w| w == ["--claude-bin", "node.exe"]));
        assert!(args.windows(2).any(|w| w == ["--claude-bin-arg", "app/test/fake-claude.mjs"]));
        assert!(args.windows(2).any(|w| w == ["--agy-bin", "C:/bin/agy.exe"]));
        assert!(!args.contains(&"--opencode-bin".to_string()), "empty bin value is skipped");
    }

    #[test]
    fn serve_args_rejects_malformed_bin_args_json() {
        let env = env_from(&[("LEARNER_AGY_BIN_ARGS", "not json")]);
        let err = serve_args("R", "D", 0, &env).unwrap_err();
        assert!(err.contains("LEARNER_AGY_BIN_ARGS"), "{err}");
        assert!(err.contains("not json"), "{err}");
    }

    #[test]
    fn serve_args_empty_bin_args_value_is_skipped_like_unset() {
        // backend.mjs:33 `rawArgs ? JSON.parse(rawArgs) : []` — the empty string is falsy there,
        // so no flag is emitted and no parse error is raised.
        let env = env_from(&[("LEARNER_CLAUDE_BIN_ARGS", ""), ("LEARNER_AGY_BIN_ARGS", "")]);
        let args = serve_args("R", "D", 0, &env).unwrap();
        assert!(!args.contains(&"--claude-bin-arg".to_string()), "empty value skipped: {args:?}");
        assert!(!args.contains(&"--agy-bin-arg".to_string()), "empty value skipped: {args:?}");
    }

    #[test]
    fn serve_args_repeated_bin_args() {
        let env = env_from(&[("LEARNER_AGY_BIN_ARGS", r#"["a","b"]"#)]);
        let args = serve_args("R", "D", 5, &env).unwrap();
        assert_eq!(args.iter().filter(|a| *a == "--agy-bin-arg").count(), 2, "one flag per element");
    }

    #[test]
    fn parse_reported_url_accepts_the_ready_line() {
        for (line, port) in [
            ("Proxi: http://127.0.0.1:4317", 4317),
            ("Proxi: http://127.0.0.1:0", 0),
            ("Proxi: http://127.0.0.1:52311\r", 52311),
            ("Proxi: http://127.0.0.1:4317 ", 4317),
        ] {
            let (url, found) =
                parse_reported_url(line).unwrap_or_else(|| panic!("rejects {line:?}"));
            assert_eq!(found, port, "{line:?}");
            assert_eq!(url, format!("http://127.0.0.1:{port}"));
        }
    }

    #[test]
    fn parse_reported_url_rejects_other_lines() {
        for line in [
            "",
            "error: boom",
            "Proxi: http://localhost:4317",
            "Proxi: https://127.0.0.1:4317",
            "Proxi: http://127.0.0.1:",
            "Proxi:  http://127.0.0.1:4317",
            " starting Proxi: http://127.0.0.1:4317",
        ] {
            assert!(parse_reported_url(line).is_none(), "accepts {line:?}");
        }
    }

    #[test]
    fn start_url_appends_safe_routes() {
        assert_eq!(start_url("http://127.0.0.1:5", Some("#/c/demo")), "http://127.0.0.1:5/#/c/demo");
        assert_eq!(start_url("http://127.0.0.1:5/", Some("#/c/demo")), "http://127.0.0.1:5/#/c/demo");
        assert_eq!(start_url("http://127.0.0.1:5//", Some("#/c/demo")), "http://127.0.0.1:5/#/c/demo");
    }

    #[test]
    fn start_url_keeps_base_on_unsafe_or_absent_hash() {
        for bad in [None, Some(""), Some("#/c/demo?x=1"), Some("javascript:alert(1)"), Some("#/c/<b>"), Some("/c/demo")] {
            assert_eq!(start_url("http://127.0.0.1:5", bad), "http://127.0.0.1:5", "{bad:?}");
        }
    }

    #[test]
    fn wanted_port_cases() {
        assert_eq!(wanted_port(&env_from(&[("LEARNER_PORT", "0")])), 0);
        assert_eq!(wanted_port(&Vars::new()), 4317);
        assert_eq!(wanted_port(&env_from(&[("LEARNER_PORT", "garbage")])), 4317);
        assert_eq!(wanted_port(&env_from(&[("LEARNER_PORT", "5000")])), 5000);
    }

    #[test]
    fn start_server_reports_the_spawned_backend() {
        let spawn = |_port: u16| -> Result<Backend, String> {
            Ok(fake_backend("http://127.0.0.1:4317", 4317))
        };
        let started =
            start_server(spawn, |_| unreachable!("no probe when the spawn works"), &Vars::new())
                .unwrap();
        match started {
            ServerStart::Own(Backend { url, .. }) => assert_eq!(url, "http://127.0.0.1:4317"),
            ServerStart::External { .. } => panic!("expected our own backend"),
        }
    }

    #[test]
    fn start_server_shows_a_foreign_proxi_on_the_wanted_port() {
        let mut ports = Vec::new();
        let spawn = |port: u16| -> Result<Backend, String> {
            ports.push(port);
            Err("port busy".into())
        };
        let env = env_from(&[("LEARNER_PORT", "5000")]);
        let started = start_server(spawn, |base| base == "http://127.0.0.1:5000", &env).unwrap();
        assert_eq!(ports, vec![5000], "no second spawn: the running Proxi is just shown");
        match started {
            ServerStart::External { url } => assert_eq!(url, "http://127.0.0.1:5000"),
            _ => panic!("expected external"),
        }
    }

    #[test]
    fn start_server_falls_back_to_a_free_port_when_a_foreign_process_holds_wanted() {
        let mut ports = Vec::new();
        let spawn = |port: u16| -> Result<Backend, String> {
            ports.push(port);
            if port == 0 {
                Ok(fake_backend("http://127.0.0.1:1234", 1234))
            } else {
                Err("port busy".into())
            }
        };
        let started = start_server(spawn, |_| false, &Vars::new()).unwrap();
        assert_eq!(ports, vec![4317, 0], "first wanted, then the free-port request");
        match started {
            ServerStart::Own(Backend { url, .. }) => assert_eq!(url, "http://127.0.0.1:1234"),
            _ => panic!("expected our own backend"),
        }
    }

    #[test]
    fn start_server_rethrows_the_ephemeral_spawn_error() {
        let spawn = |_port: u16| -> Result<Backend, String> { Err("could not bind".into()) };
        let err = start_server(
            spawn,
            |_| panic!("probe must not run for LEARNER_PORT=0"),
            &env_from(&[("LEARNER_PORT", "0")]),
        )
        .unwrap_err();
        assert_eq!(err, "could not bind");
    }

    #[test]
    fn start_server_reports_the_second_spawn_failure() {
        let mut calls = 0;
        let spawn = move |_port: u16| -> Result<Backend, String> {
            calls += 1;
            Err(format!("failure {calls}"))
        };
        let err = start_server(spawn, |_| false, &Vars::new()).unwrap_err();
        assert_eq!(err, "failure 2", "the free-port retry is the error that surfaces");
    }

    #[test]
    fn is_learner_json_fixtures() {
        let yes: Value = serde_json::from_str(r#"{"teacherModel":"claude","other":1}"#).unwrap();
        let no_missing: Value = serde_json::from_str(r#"{"provider":"claude"}"#).unwrap();
        let no_type: Value = serde_json::from_str(r#"{"teacherModel":3}"#).unwrap();
        let no_null: Value = serde_json::from_str(r#"{"teacherModel":null}"#).unwrap();
        let not_object: Value = serde_json::from_str(r#"["teacherModel"]"#).unwrap();
        assert!(is_learner_json(&yes));
        assert!(!is_learner_json(&no_missing));
        assert!(!is_learner_json(&no_type));
        assert!(!is_learner_json(&no_null));
        assert!(!is_learner_json(&not_object));
    }

    #[test]
    fn kill_tree_args_kill_the_process_tree_forced() {
        assert_eq!(kill_tree_args(1234), ["taskkill", "/PID", "1234", "/T", "/F"]);
    }

    #[test]
    fn proxi_binary_path_prefers_proxi_bin_and_errors_naming_the_build() {
        let tmp = std::env::temp_dir().join("proxi-shell-bin");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("target").join("release")).unwrap();
        let exe = tmp.join("target").join("release").join("proxi.exe");
        std::fs::File::create(&exe).unwrap();

        let found = proxi_binary_path(&tmp, &env_from(&[("PROXI_BIN", exe.to_str().unwrap())])).unwrap();
        assert_eq!(found, exe);

        let missing = tmp.join("nope.exe");
        let err = proxi_binary_path(&tmp, &env_from(&[("PROXI_BIN", missing.to_str().unwrap())])).unwrap_err();
        assert!(err.contains("cargo build --release --locked"), "{err}");
        assert!(err.contains("nope.exe"), "{err}");

        let found = proxi_binary_path(&tmp, &Vars::new());
        assert_eq!(found.unwrap(), exe, "the release build under target/ is the default binary");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn proxi_binary_path_errors_when_nothing_is_built() {
        let tmp = std::env::temp_dir().join("proxi-shell-bin-none");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let err = proxi_binary_path(&tmp, &Vars::new()).unwrap_err();
        assert!(err.contains("cargo build --release --locked"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn repo_root_walks_up_to_a_target_dir_that_has_a_proxi_binary() {
        let tmp = std::env::temp_dir().join("proxi-shell-root");
        let _ = std::fs::remove_dir_all(&tmp);
        // The dev layout: the shell exe in <repo>/src-tauri/target/, proxi.exe in <repo>/target/.
        for file in ["src-tauri/target/release/proxi-app.exe", "target/release/proxi.exe"] {
            let p = tmp.join(file);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::File::create(&p).unwrap();
        }
        let exe_dir = tmp.join("src-tauri").join("target").join("release");
        assert_eq!(repo_root(&Vars::new(), &exe_dir), tmp);
        assert_eq!(
            repo_root(&env_from(&[("LEARNER_REPO_ROOT", "C:/other")]), &exe_dir),
            PathBuf::from("C:/other")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn repo_root_falls_back_to_the_first_target_dir_when_no_binary_is_built() {
        let tmp = std::env::temp_dir().join("proxi-shell-root-empty");
        let _ = std::fs::remove_dir_all(&tmp);
        let exe_dir = tmp.join("src-tauri").join("target").join("release");
        std::fs::create_dir_all(&exe_dir).unwrap();
        assert_eq!(repo_root(&Vars::new(), &exe_dir), tmp.join("src-tauri"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn nsis_silent_flag_matches_contract() {
        assert_eq!(NSIS_SILENT, ["/S"]);
    }

    #[test]
    fn pending_marker_path_format() {
        let root = Path::new("C:/proxi");
        assert_eq!(
            pending_marker_path(root),
            PathBuf::from("C:/proxi/target/release/proxi.update-pending")
        );
    }

    #[test]
    fn pending_installer_roundtrip_and_clear() {
        let tmp = std::env::temp_dir().join("proxi-pending-roundtrip");
        let _ = std::fs::remove_dir_all(&tmp);
        let release_dir = tmp.join("target").join("release");
        std::fs::create_dir_all(&release_dir).unwrap();

        let installer_path = release_dir.join("proxi-setup.exe");
        std::fs::write(&installer_path, b"mock-installer").unwrap();

        let marker_content = serde_json::json!({
            "installer": installer_path.to_str().unwrap(),
            "version": "1.2.3"
        });
        std::fs::write(pending_marker_path(&tmp), marker_content.to_string()).unwrap();

        let found = read_pending_installer(&tmp);
        assert_eq!(found, Some(installer_path));

        clear_pending(&tmp);
        assert!(!pending_marker_path(&tmp).exists());
        assert_eq!(read_pending_installer(&tmp), None);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pending_installer_missing_or_corrupt_returns_none() {
        let tmp = std::env::temp_dir().join("proxi-pending-corrupt");
        let _ = std::fs::remove_dir_all(&tmp);
        let release_dir = tmp.join("target").join("release");
        std::fs::create_dir_all(&release_dir).unwrap();

        // 1. Missing marker
        assert_eq!(read_pending_installer(&tmp), None);

        // 2. Corrupt JSON
        std::fs::write(pending_marker_path(&tmp), "not-json").unwrap();
        assert_eq!(read_pending_installer(&tmp), None);

        // 3. JSON missing "installer" key
        std::fs::write(pending_marker_path(&tmp), r#"{"version":"1.2.3"}"#).unwrap();
        assert_eq!(read_pending_installer(&tmp), None);

        // 4. Installer file does not exist on disk
        let non_existent = release_dir.join("missing-setup.exe");
        let marker_content = serde_json::json!({
            "installer": non_existent.to_str().unwrap(),
            "version": "1.2.3"
        });
        std::fs::write(pending_marker_path(&tmp), marker_content.to_string()).unwrap();
        assert_eq!(read_pending_installer(&tmp), None);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_installer_with_timeout_kills_hung_process() {
        let tmp = std::env::temp_dir().join(format!("proxi-test-hang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        #[cfg(windows)]
        let script = tmp.join("hang.cmd");
        #[cfg(windows)]
        std::fs::write(&script, "@echo off\r\nping 127.0.0.1 -n 10 >nul\r\n").unwrap();

        #[cfg(not(windows))]
        let script = tmp.join("hang.sh");
        #[cfg(not(windows))]
        std::fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();

        let res = run_installer_with_timeout(&script, Duration::from_millis(100));
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), std::io::ErrorKind::TimedOut);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
