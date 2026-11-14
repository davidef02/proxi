//! The /api/exercises data side: list, workspace, save, hints and runs (port of app/server/exercises.ts).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use sha1::{Digest, Sha1};
use tokio::sync::watch;

use crate::child;
use crate::events;
use crate::js;
use crate::vitest;

pub const LASTRUN_FILE: &str = ".app-lastrun.json";
/// `save_exercise_file` error when the file changed on disk since the version the editor loaded.
pub const CONFLICT: &str = "Il file è cambiato fuori dall'editor";
pub const HINTS_FILE: &str = "SUGGERIMENTI.json";
pub const INSTRUCTIONS_FILE: &str = "ISTRUZIONI.md";
pub const MAX_FILE: u64 = 200 * 1024;
pub const NODE_MISSING: &str =
    "Node.js non trovato nel PATH: installa Node.js (https://nodejs.org) e riapri Proxi per eseguire i test";

const EXT: [&str; 6] = [".vue", ".js", ".ts", ".json", ".css", ".md"];

/// `NAME_SAFE`: a single safe path segment.
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `NAME_RE`: an exercise directory starts with two digits and a dash, or `zz-`.
pub fn is_exercise_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    name.starts_with("zz-")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_digit()
            && bytes[1].is_ascii_digit()
            && bytes[2] == b'-')
}

pub fn exercises_base(data_root: &Path, course: &str) -> PathBuf {
    data_root.join("courses").join(course).join("exercises")
}

/// Node `path.extname` on the last `/` segment.
fn extname(p: &str) -> &str {
    let name = match p.rfind('/') {
        Some(slash) => &p[slash + 1..],
        None => p,
    };
    if name == ".." {
        return "";
    }
    match name.rfind('.') {
        None => "",
        Some(0) => "",
        Some(dot) => &name[dot..],
    }
}

fn has_allowed_ext(name: &str) -> bool {
    EXT.contains(&extname(name).to_lowercase().as_str())
}

/// Validate a learner-exercise file path (relative, forward slashes, allowed extension).
pub fn bad_exercise_path(p: &str) -> Option<String> {
    if p.is_empty() || p.contains('\\') || p.contains('\0') {
        return Some(format!(
            "Il percorso \"{p}\" deve essere relativo, con barre dirette (/)"
        ));
    }
    let bytes = p.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if p.starts_with('/') || drive {
        return Some(format!(
            "Il percorso \"{p}\" deve essere relativo (niente percorsi assoluti o lettere di unità)"
        ));
    }
    if p.split('/')
        .any(|segment| segment == ".." || segment == "." || segment.is_empty())
    {
        return Some(format!(
            "Il percorso \"{p}\" non può contenere \"..\", \".\" o segmenti vuoti"
        ));
    }
    if !has_allowed_ext(p) {
        return Some(format!(
            "Il percorso \"{p}\": l'estensione deve essere una tra {}",
            EXT.join(" ")
        ));
    }
    None
}

/// `\.(test|spec)\.[^/]+$` case-insensitive: NTFS is case-insensitive, `Esercizio.TEST.ts` is a test.
pub fn is_test_file(rel: &str) -> bool {
    let bytes = rel.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] != b'.' {
            continue;
        }
        for marker in [b"test".as_slice(), b"spec".as_slice()] {
            let start = i + 1;
            let end = start + marker.len();
            if end >= bytes.len()
                || !bytes[start..end].eq_ignore_ascii_case(marker)
                || bytes[end] != b'.'
            {
                continue;
            }
            let tail = &rel[end + 1..];
            if !tail.is_empty() && !tail.contains('/') {
                return true;
            }
        }
    }
    false
}

fn is_hidden(rel: &str) -> bool {
    rel.eq_ignore_ascii_case(INSTRUCTIONS_FILE) || rel.eq_ignore_ascii_case(HINTS_FILE)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hints {
    pub hints: Vec<String>,
    pub final_: bool,
    pub revealed: f64,
}

pub fn read_hints(dir: &Path) -> Option<Hints> {
    let raw = fs::read(dir.join(HINTS_FILE)).ok()?;
    let value = js::parse(&String::from_utf8_lossy(&raw)).ok()?;
    let object = value.as_object()?;
    let hints = object
        .get("hints")?
        .as_array()?
        .iter()
        .map(|hint| js::js_string(Some(hint)))
        .collect::<Vec<_>>();
    let len = hints.len() as f64;
    let mut revealed = js::to_number(object.get("revealed"));
    if revealed.is_nan() {
        revealed = 0.0;
    }
    Some(Hints {
        hints,
        final_: js::truthy(object.get("final")),
        revealed: revealed.max(0.0).min(len),
    })
}

pub fn write_hints(dir: &Path, h: &Hints) -> io::Result<()> {
    let mut map = Map::new();
    map.insert(
        "hints".into(),
        Value::Array(h.hints.iter().map(|hint| Value::from(hint.as_str())).collect()),
    );
    map.insert("final".into(), Value::Bool(h.final_));
    map.insert("revealed".into(), js::number(h.revealed));
    let mut text =
        serde_json::to_string_pretty(&Value::Object(map)).map_err(io::Error::other)?;
    text.push('\n');
    fs::write(dir.join(HINTS_FILE), text)
}

/// Absolute dir of an existing exercise, or `None` (bad name, missing).
pub fn exercise_dir_of(data_root: &Path, course: &str, name: &str) -> Option<PathBuf> {
    if !safe_name(name) || matches!(name, "." | ".." | "node_modules") {
        return None;
    }
    let dir = exercises_base(data_root, course).join(name);
    dir.is_dir().then_some(dir)
}

/// Content hash of a file ("" when missing): the editor sends it back as baseVersion on save.
pub fn file_version(abs: &Path) -> String {
    let Ok(data) = fs::read(abs) else {
        return String::new();
    };
    let mut hasher = Sha1::new();
    hasher.update(&data);
    format!("{:x}", hasher.finalize())
}

fn walk(dir: &Path, rel: &str, depth: u32) -> io::Result<Vec<String>> {
    let mut entries = fs::read_dir(dir)?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| js::locale_compare(&a.0, &b.0));

    let mut out = Vec::new();
    for (name, entry) in entries {
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let r = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if depth < 3 {
                out.extend(walk(&entry.path(), &r, depth + 1)?);
            }
        } else if file_type.is_file() && has_allowed_ext(&name) && !is_hidden(&r) {
            out.push(r);
        }
    }
    Ok(out)
}

pub fn list_exercises(data_root: &Path, course: &str) -> io::Result<Vec<Value>> {
    let base = exercises_base(data_root, course);
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(&base)?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| js::locale_compare(&a.0, &b.0));

    let mut out = Vec::new();
    for (name, entry) in entries {
        if !entry.file_type()?.is_dir() || !is_exercise_name(&name) {
            continue;
        }
        let dir = base.join(&name);

        let mut files = fs::read_dir(&dir)?
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|file| {
                let file_name = file.file_name().to_string_lossy().into_owned();
                let is_file = file.file_type().map(|t| t.is_file()).unwrap_or(false);
                (is_file
                    && !file_name.starts_with('.')
                    && !file_name.eq_ignore_ascii_case(HINTS_FILE))
                .then_some(file_name)
            })
            .collect::<Vec<_>>();
        // `.sort()`: UTF-16 code units, not locale order.
        files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));

        let mut info = Map::new();
        info.insert("course".into(), Value::from(course));
        info.insert("name".into(), Value::from(name.as_str()));
        info.insert(
            "dir".into(),
            Value::from(format!("courses/{course}/exercises/{name}")),
        );
        info.insert(
            "files".into(),
            Value::Array(files.into_iter().map(Value::from).collect()),
        );

        if let Ok(raw) = fs::read(dir.join(LASTRUN_FILE)) {
            if let Ok(value) = js::parse(&String::from_utf8_lossy(&raw)) {
                if js::truthy(Some(&value)) {
                    info.insert("lastRun".into(), value);
                }
            }
        }

        if let Some(hints) = read_hints(&dir) {
            info.insert(
                "hints".into(),
                Value::Array(hints.hints.iter().map(|hint| Value::from(hint.as_str())).collect()),
            );
            info.insert("hintsRevealed".into(), js::number(hints.revealed));
            info.insert("final".into(), Value::Bool(hints.final_));
        }

        let instructions = dir.join(INSTRUCTIONS_FILE);
        if instructions.exists() {
            info.insert(
                "instructions".into(),
                Value::from(String::from_utf8_lossy(&fs::read(&instructions)?)),
            );
        }
        out.push(Value::Object(info));
    }
    Ok(out)
}

/// Files for the in-app editor: tests are readOnly, everything else is editable.
pub fn get_workspace(
    data_root: &Path,
    course: &str,
    name: &str,
) -> io::Result<Option<Value>> {
    let Some(dir) = exercise_dir_of(data_root, course, name) else {
        return Ok(None);
    };
    let Some(info) = list_exercises(data_root, course)?
        .into_iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
    else {
        return Ok(None);
    };

    let mut files = Vec::new();
    for rel in walk(&dir, "", 0)? {
        let abs = dir.join(&rel);
        if fs::metadata(&abs)?.len() > MAX_FILE {
            continue;
        }
        let mut file = Map::new();
        file.insert("path".into(), Value::from(rel.as_str()));
        file.insert(
            "content".into(),
            Value::from(String::from_utf8_lossy(&fs::read(&abs)?)),
        );
        file.insert("version".into(), Value::from(file_version(&abs)));
        file.insert("readOnly".into(), Value::Bool(is_test_file(&rel)));
        files.push(Value::Object(file));
    }

    let mut out = Map::new();
    out.insert("info".into(), info);
    out.insert("files".into(), Value::Array(files));
    Ok(Some(Value::Object(out)))
}

#[derive(Debug)]
pub enum SaveError {
    Invalid(String),
    Conflict,
    Io(io::Error),
}

/// Save one learner file. Editable files only, or new files with allowed extensions.
pub fn save_exercise_file(
    data_root: &Path,
    course: &str,
    name: &str,
    rel: &str,
    content: &str,
    base_version: Option<&str>,
) -> Result<(), SaveError> {
    let Some(dir) = exercise_dir_of(data_root, course, name) else {
        return Err(SaveError::Invalid("Esercizio sconosciuto".into()));
    };
    if let Some(message) = bad_exercise_path(rel) {
        return Err(SaveError::Invalid(message));
    }
    if rel
        .split('/')
        .any(|segment| segment == "node_modules" || segment.starts_with('.'))
    {
        return Err(SaveError::Invalid("Percorso non consentito".into()));
    }
    if is_test_file(rel) || is_hidden(rel) {
        return Err(SaveError::Invalid("File di sola lettura".into()));
    }
    if content.len() as u64 > MAX_FILE {
        return Err(SaveError::Invalid("File troppo grande".into()));
    }
    let target = dir.join(rel);
    if !target.starts_with(&dir) {
        return Err(SaveError::Invalid("Percorso non consentito".into()));
    }
    if let Some(base) = base_version {
        if file_version(&target) != base {
            return Err(SaveError::Conflict);
        }
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(SaveError::Io)?;
    }
    fs::write(&target, content).map_err(SaveError::Io)
}

/// Reveal the next hint (capped at hints.length); `None` when the exercise has none.
pub fn reveal_hint(data_root: &Path, course: &str, name: &str) -> io::Result<Option<Value>> {
    let Some(dir) = exercise_dir_of(data_root, course, name) else {
        return Ok(None);
    };
    let Some(hints) = read_hints(&dir) else {
        return Ok(None);
    };
    if hints.hints.is_empty() {
        return Ok(None);
    }
    if hints.revealed < hints.hints.len() as f64 {
        let next = Hints {
            revealed: hints.revealed + 1.0,
            ..hints
        };
        write_hints(&dir, &next)?;
    }
    Ok(list_exercises(data_root, course)?
        .into_iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name)))
}

/// `code "<abs dir>"`; `None` when the path contains a quote the shell would eat.
pub fn editor_command_line(abs_dir: &Path) -> Option<String> {
    let dir = abs_dir.to_string_lossy();
    if dir.contains('"') {
        return None;
    }
    Some(format!("code \"{dir}\""))
}

/// Open the exercise dir in VS Code: detached, through the shell, never awaited (tests avoid it).
pub fn open_in_editor(abs_dir: &Path) {
    let Some(line) = editor_command_line(abs_dir) else {
        return;
    };

    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        let mut cmd = std::process::Command::new("cmd.exe");
        cmd.raw_arg(format!("/d /s /c \"{line}\""))
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        cmd
    };
    #[cfg(unix)]
    let mut cmd = {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(&line).process_group(0);
        cmd
    };

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// How a first run installs the course dependencies (`LEARNER_NPM_BIN` in tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NpmCommand {
    /// Real npm, through the shell.
    Default,
    /// An npm-like executable, run as `<bin> install`.
    Bin(PathBuf),
    /// A Node script, run as `node <script> install`.
    Script(PathBuf),
}

/// The flag wins over the env, like every other engine binary (`server.rs` `start`).
pub fn npm_command(flag: Option<&Path>, env: Option<&OsStr>) -> NpmCommand {
    if let Some(flag) = flag {
        return NpmCommand::Bin(flag.to_path_buf());
    }
    match env {
        Some(value) if !value.is_empty() => NpmCommand::Script(PathBuf::from(value)),
        _ => NpmCommand::Default,
    }
}

pub struct RunConfig {
    pub node: Option<PathBuf>,
    pub npm: NpmCommand,
    pub run_timeout: Duration,
    pub install_timeout: Duration,
}

impl RunConfig {
    pub fn new(node: Option<PathBuf>, npm: NpmCommand) -> Self {
        Self {
            node,
            npm,
            run_timeout: Duration::from_secs(120),
            install_timeout: Duration::from_secs(300),
        }
    }
}

/// The same shape Node returns for a run that never started: `error` comes last.
pub fn run_failure(error: &str) -> Value {
    let mut map = Map::new();
    map.insert("ok".into(), Value::Bool(false));
    map.insert("passed".into(), Value::from(0));
    map.insert("failed".into(), Value::from(0));
    map.insert("total".into(), Value::from(0));
    map.insert("failures".into(), Value::Array(Vec::new()));
    map.insert("durationMs".into(), Value::from(0));
    map.insert("at".into(), Value::from(events::iso_now()));
    map.insert("error".into(), Value::from(error));
    Value::Object(map)
}

/// One `npm install` per exercises dir, shared by concurrent runs like Node shared the promise.
#[derive(Clone, Default)]
pub struct Installs(Arc<Mutex<HashMap<PathBuf, watch::Receiver<Option<Option<String>>>>>>);

impl Installs {
    pub async fn install_once(&self, base: &Path, cfg: &RunConfig) -> Option<String> {
        let (mut rx, started) = {
            let mut installs = self.0.lock().expect("install registry lock");
            match installs.get(base).cloned() {
                Some(rx) => (rx, None),
                None => {
                    let (tx, rx) = watch::channel(None);
                    installs.insert(base.to_path_buf(), rx.clone());
                    (rx, Some(tx))
                }
            }
        };

        if let Some(tx) = started {
            // Own task: a cancelled request must not kill an install another request waits on.
            let registry = self.clone();
            let base = base.to_path_buf();
            let node = cfg.node.clone();
            let npm = cfg.npm.clone();
            let limit = cfg.install_timeout;
            tokio::spawn(async move {
                let result = run_install(&base, node.as_deref(), &npm, limit).await;
                // Drop the key before publishing: a later run starts a fresh install, like Node.
                registry.0.lock().expect("install registry lock").remove(&base);
                let _ = tx.send(Some(result));
            });
        }

        match rx.wait_for(|value| value.is_some()).await {
            Ok(value) => value.as_ref().expect("install result").clone(),
            Err(_) => Some("npm install non è partito: interrotto".into()),
        }
    }
}

async fn run_install(
    base: &Path,
    node: Option<&Path>,
    npm: &NpmCommand,
    limit: Duration,
) -> Option<String> {
    if !base.join("package.json").exists() {
        return Some(format!("Manca package.json in {}", base.display()));
    }

    let make = || {
        let mut cmd = match npm {
            NpmCommand::Script(script) => {
                let node = node.expect("run_exercise checks node before installing");
                let mut cmd = tokio::process::Command::new(node);
                cmd.arg(script);
                cmd
            }
            NpmCommand::Bin(bin) => child::shim_command(bin),
            NpmCommand::Default => default_npm(),
        };
        cmd.arg("install")
            .current_dir(base)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    };

    let mut child = match child::spawn_tree(make) {
        Ok(child) => child,
        Err(e) => return Some(format!("npm install non è partito: {e}")),
    };
    match child.wait_timeout(limit).await {
        Ok(Some(status)) => match status.code() {
            Some(0) => None,
            Some(code) => Some(format!("npm install è fallito (codice {code})")),
            None => Some("npm install è fallito (codice null)".into()),
        },
        Ok(None) => Some("Timeout: npm install ha superato 5 minuti".into()),
        Err(e) => Some(format!("npm install non è partito: {e}")),
    }
}

#[cfg(windows)]
fn default_npm() -> tokio::process::Command {
    // npm is npm.cmd: only the shell finds it from PATH.
    let mut cmd = tokio::process::Command::new("cmd.exe");
    cmd.args(["/d", "/c", "npm"]);
    cmd
}

#[cfg(not(windows))]
fn default_npm() -> tokio::process::Command {
    tokio::process::Command::new("npm")
}

/// The vitest report dir; removed on drop so a cancelled run leaves nothing behind.
struct ReportDir(PathBuf);

impl ReportDir {
    fn new() -> io::Result<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "learner-ex-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for ReportDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Run the exercise's tests with vitest and persist the result for the list.
pub async fn run_exercise(
    data_root: &Path,
    course: &str,
    name: &str,
    cfg: &RunConfig,
    installs: &Installs,
) -> Value {
    let base = exercises_base(data_root, course);
    let dir = base.join(name);
    if !is_exercise_name(name) || name.contains('/') || name.contains('\\') || !dir.is_dir() {
        return run_failure(&format!("Esercizio sconosciuto: {name}"));
    }
    let Some(node) = cfg.node.as_deref() else {
        return run_failure(NODE_MISSING);
    };

    let vitest_mjs = base.join("node_modules").join("vitest").join("vitest.mjs");
    if !vitest_mjs.exists() {
        let error = installs.install_once(&base, cfg).await;
        if error.is_some() || !vitest_mjs.exists() {
            return run_failure(&error.unwrap_or_else(|| {
                format!("Installazione non riuscita: esegui npm install in courses/{course}/exercises")
            }));
        }
    }

    let tmp = match ReportDir::new() {
        Ok(tmp) => tmp,
        Err(e) => return run_failure(&e.to_string()),
    };
    let out = tmp.0.join("report.json");
    let start = Instant::now();
    let make = || {
        // The trailing slash is the path filter: "01-a/" must not match "01-ab/".
        let mut cmd = tokio::process::Command::new(node);
        cmd.arg(&vitest_mjs)
            .arg("run")
            .arg(format!("{name}/"))
            .arg("--reporter=json")
            .arg(format!("--outputFile={}", out.display()))
            .current_dir(&base)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        cmd
    };
    let mut child = match child::spawn_tree(make) {
        Ok(child) => child,
        Err(e) => return run_failure(&e.to_string()),
    };
    let stderr = child.take_stderr().map(|mut pipe| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf).await;
            buf
        })
    });

    let status = match child.wait_timeout(cfg.run_timeout).await {
        Ok(status) => status,
        Err(e) => return run_failure(&e.to_string()),
    };
    if status.is_none() {
        return run_failure(&format!(
            "Timeout: i test hanno superato {} s",
            cfg.run_timeout.as_secs()
        ));
    }
    let stderr = match stderr {
        Some(task) => task
            .await
            .map(|buf| String::from_utf8_lossy(&buf).into_owned())
            .unwrap_or_default(),
        None => String::new(),
    };
    if !out.exists() {
        return run_failure(&vitest::clean_message(&format!(
            "vitest non ha prodotto un report. {stderr}"
        )));
    }
    let report = match fs::read(&out) {
        Ok(raw) => String::from_utf8_lossy(&raw).into_owned(),
        Err(e) => return run_failure(&e.to_string()),
    };
    let result = match vitest::parse_vitest_report(
        &report,
        start.elapsed().as_millis() as u64,
        &events::iso_now(),
    ) {
        Ok(result) => result,
        Err(message) => return run_failure(&message),
    };
    // Best effort, like Node: an unwritable dir must not turn a finished run into an error.
    if let Ok(text) = serde_json::to_string(&result) {
        let _ = fs::write(dir.join(LASTRUN_FILE), text);
    }
    result
}
