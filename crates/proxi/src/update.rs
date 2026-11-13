//! App update: git checks, fast-forward pull, cargo build, relaunch.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::events::{self, EventBus};
use crate::protocol::{
    GlobalEvent, UpdateApplyResponse, UpdateCommitInfo, UpdateState, UpdateStatus,
};

pub const SESSIONS_RUNNING: &str = "C'è una sessione in corso. Chiudila o conferma per interromperla prima di aggiornare.";
pub const DIRTY_TREE: &str = "Modifiche locali non salvate nel repository. Impossibile aggiornare senza rischiare di sovrascrivere il tuo lavoro.";
pub const NO_UPSTREAM: &str = "Nessun branch upstream configurato.";
pub const DIVERGED: &str = "I commit locali sono divergenti da quelli remoti. Impossibile eseguire un aggiornamento fast-forward.";

const CHECK_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const APPLY_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const PULL_TIMEOUT: Duration = Duration::from_secs(60);
const BUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const RELAUNCH_DELAY: Duration = Duration::from_millis(500);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug)]
pub struct UpdateError {
    pub status: u16,
    pub message: String,
    pub code: Option<&'static str>,
}

/// Seam for the sessions/jobs managers: the update refuses to run while work is live.
pub trait ActiveWork: Send + Sync {
    fn count(&self) -> usize;
    fn shutdown(&self) -> BoxFuture<'_, ()>;
}

/// The real binary until sessions/jobs are ported: nothing is ever running.
pub struct NoActiveWork;

impl ActiveWork for NoActiveWork {
    fn count(&self) -> usize {
        0
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The executable the update rebuilds, keyed to the repo (never `current_exe`).
pub fn release_exe(repo: &Path) -> PathBuf {
    repo.join("target")
        .join("release")
        .join(format!("proxi{}", std::env::consts::EXE_SUFFIX))
}

pub fn aside_exe(repo: &Path) -> PathBuf {
    repo.join("target")
        .join("release")
        .join(format!("proxi.old{}", std::env::consts::EXE_SUFFIX))
}

/// Previous build left aside by `rebuild`; best effort, it may still be the running exe.
pub fn remove_leftover(repo: &Path) {
    let _ = fs::remove_file(aside_exe(repo));
}

/// `parseInt(stdout.trim(), 10) || 0`: leading ASCII digits, otherwise zero.
pub fn parse_count(stdout: &str) -> u64 {
    let digits: String = stdout.trim().chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

/// One `%h%x09%s%x09%cr` line per commit; missing columns become empty strings.
pub fn parse_log(stdout: &str) -> Vec<UpdateCommitInfo> {
    stdout
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut parts = line.split('\t');
            UpdateCommitInfo {
                hash: parts.next().unwrap_or("").to_string(),
                subject: parts.next().unwrap_or("").to_string(),
                relative_time: parts.next().unwrap_or("").to_string(),
            }
        })
        .collect()
}

/// JS truthiness of `body.force`: only an object body can ask for force.
pub fn force_from_body(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Value::Object(map) = value else {
        return false;
    };
    match map.get("force") {
        Some(Value::Bool(force)) => *force,
        Some(Value::Number(force)) => force.as_f64().is_some_and(|force| force != 0.0),
        Some(Value::String(force)) => !force.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
        Some(Value::Null) | None => false,
    }
}

pub struct UpdateConfig {
    pub repo_root: PathBuf,
    pub git: PathBuf,
    pub cargo: PathBuf,
    pub events: EventBus,
    pub active_work: Arc<dyn ActiveWork>,
    /// None = no relaunch signal (library and tests).
    pub relaunch: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub struct UpdateManager {
    cfg: UpdateConfig,
    status: Mutex<UpdateStatus>,
    check_lock: tokio::sync::Mutex<()>,
    check_gen: AtomicU64,
    last_check: Mutex<Option<UpdateStatus>>,
}

impl UpdateManager {
    pub fn new(cfg: UpdateConfig) -> Self {
        Self {
            cfg,
            status: Mutex::new(UpdateStatus::default()),
            check_lock: tokio::sync::Mutex::new(()),
            check_gen: AtomicU64::new(0),
            last_check: Mutex::new(None),
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.status
            .lock()
            .expect("update status poisoned")
            .clone()
    }

    pub fn active_sessions(&self) -> usize {
        self.cfg.active_work.count()
    }

    fn set(&self, status: UpdateStatus) {
        *self.status.lock().expect("update status poisoned") = status.clone();
        self.cfg.events.emit(&GlobalEvent::Update { status });
    }

    fn fail(&self, text: &str) {
        let mut status = self.status();
        status.state = UpdateState::Error;
        status.error = Some(text.to_string());
        self.set(status);
    }

    fn finish(&self, status: UpdateStatus) -> UpdateStatus {
        self.set(status.clone());
        status
    }

    async fn git(&self, args: &[&str], timeout: Option<Duration>) -> Result<Output, RunError> {
        run(&self.cfg.git, args, &self.cfg.repo_root, timeout, true).await
    }

    /// Single flight per manager: a caller that waited for a running check gets its result.
    pub async fn check(&self) -> UpdateStatus {
        let generation = self.check_gen.load(Ordering::SeqCst);
        let _guard = self.check_lock.lock().await;
        if self.check_gen.load(Ordering::SeqCst) != generation {
            return self
                .last_check
                .lock()
                .expect("update check result poisoned")
                .clone()
                .unwrap_or_default();
        }
        let status = self.do_check().await;
        *self.last_check.lock().expect("update check result poisoned") = Some(status.clone());
        self.check_gen.fetch_add(1, Ordering::SeqCst);
        status
    }

    async fn do_check(&self) -> UpdateStatus {
        let repo = &self.cfg.repo_root;
        if !repo.join(".git").exists() {
            return self.finish(idle_status());
        }
        if self
            .git(&["rev-parse", "--is-inside-work-tree"], None)
            .await
            .is_err()
        {
            return self.finish(idle_status());
        }

        // No upstream on a feature branch: still report commits landing on main.
        let target = match self
            .git(&["rev-parse", "--symbolic-full-name", "@{u}"], None)
            .await
        {
            Ok(_) => "@{u}".to_string(),
            Err(_) => {
                let mut fallback = None;
                for candidate in ["origin/main", "origin/master", "origin/HEAD"] {
                    if self
                        .git(&["rev-parse", "--verify", candidate], None)
                        .await
                        .is_ok()
                    {
                        fallback = Some(candidate.to_string());
                        break;
                    }
                }
                match fallback {
                    Some(candidate) => candidate,
                    None => return self.finish(idle_status()),
                }
            }
        };

        // Offline or a transient remote error: keep reading whatever is local.
        let _ = self
            .git(&["fetch", "--quiet"], Some(CHECK_FETCH_TIMEOUT))
            .await;

        let range = format!("HEAD..{target}");
        let behind = match self.git(&["rev-list", "--count", &range], None).await {
            Ok(output) => parse_count(&stdout(&output)),
            Err(_) => return self.finish(idle_status()),
        };

        if behind > 0 {
            let commits = self
                .git(
                    &["log", "-n", "10", "--pretty=format:%h%x09%s%x09%cr", &range],
                    None,
                )
                .await
                .map(|output| parse_log(&stdout(&output)))
                .unwrap_or_default();
            return self.finish(UpdateStatus {
                state: UpdateState::Available,
                behind,
                commits: Some(commits),
                checked_at: Some(events::iso_now()),
                error: None,
            });
        }

        self.finish(UpdateStatus {
            state: UpdateState::UpToDate,
            behind: 0,
            commits: None,
            checked_at: Some(events::iso_now()),
            error: None,
        })
    }

    pub async fn apply(&self, force: bool) -> Result<UpdateApplyResponse, UpdateError> {
        let active = self.cfg.active_work.count();
        if active > 0 {
            if !force {
                return Err(UpdateError {
                    status: 409,
                    message: SESSIONS_RUNNING.to_string(),
                    code: Some("sessions_running"),
                });
            }
            self.cfg.active_work.shutdown().await;
        }

        let porcelain = self
            .git(&["status", "--porcelain"], None)
            .await
            .map_err(|error| UpdateError {
                status: 500,
                message: format!("Errore verifica repository: {}", error.message),
                code: None,
            })?;
        if !stdout(&porcelain).trim().is_empty() {
            return Err(UpdateError {
                status: 409,
                message: DIRTY_TREE.to_string(),
                code: Some("dirty_tree"),
            });
        }

        let branch = self
            .git(&["rev-parse", "--abbrev-ref", "HEAD"], None)
            .await
            .map(|output| stdout(&output).trim().to_string())
            .unwrap_or_default();
        if !branch.is_empty() && branch != "main" && branch != "master" {
            let main = if self
                .git(&["rev-parse", "--verify", "main"], None)
                .await
                .is_ok()
            {
                Some("main")
            } else if self
                .git(&["rev-parse", "--verify", "master"], None)
                .await
                .is_ok()
            {
                Some("master")
            } else {
                None
            };
            if let Some(main) = main {
                if let Err(error) = self.git(&["checkout", main], None).await {
                    return Err(UpdateError {
                        status: 409,
                        message: format!("Impossibile passare al branch {main}: {}", detail(&error)),
                        code: None,
                    });
                }
            }
        }

        let _ = self
            .git(&["fetch", "--quiet"], Some(APPLY_FETCH_TIMEOUT))
            .await;

        if self
            .git(&["rev-parse", "--symbolic-full-name", "@{u}"], None)
            .await
            .is_err()
        {
            return Err(UpdateError {
                status: 400,
                message: NO_UPSTREAM.to_string(),
                code: None,
            });
        }
        if self
            .git(&["merge-base", "--is-ancestor", "HEAD", "@{u}"], None)
            .await
            .is_err()
        {
            return Err(UpdateError {
                status: 409,
                message: DIVERGED.to_string(),
                code: Some("diverged"),
            });
        }

        let old = self
            .git(&["rev-parse", "HEAD"], None)
            .await
            .map_err(|error| UpdateError {
                status: 500,
                message: format!("Impossibile determinare il commit corrente: {}", error.message),
                code: None,
            })
            .map(|output| stdout(&output).trim().to_string())?;

        let mut updating = self.status();
        updating.state = UpdateState::Updating;
        self.set(updating);

        if let Err(error) = self.git(&["pull", "--ff-only"], Some(PULL_TIMEOUT)).await {
            let text = format!("git pull fallito: {}", detail(&error));
            self.fail(&text);
            return Err(UpdateError {
                status: 500,
                message: text,
                code: None,
            });
        }

        let new = self
            .git(&["rev-parse", "HEAD"], None)
            .await
            .map_err(|error| UpdateError {
                status: 500,
                message: format!("Impossibile determinare il nuovo commit: {}", error.message),
                code: None,
            })
            .map(|output| stdout(&output).trim().to_string())?;

        // Node ran npm install only for package.json changes; the binary rebuilds instead.
        if old != new {
            if let Err(text) = self.rebuild().await {
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        }

        self.set(UpdateStatus {
            state: UpdateState::Relaunching,
            behind: 0,
            commits: None,
            checked_at: Some(events::iso_now()),
            error: None,
        });
        if let Some(relaunch) = self.cfg.relaunch.clone() {
            // Let the HTTP reply go out, then exit: the shell restarts the app.
            tokio::spawn(async move {
                tokio::time::sleep(RELAUNCH_DELAY).await;
                relaunch();
            });
        }
        Ok(UpdateApplyResponse {
            ok: true,
            relaunching: true,
        })
    }

    /// Rename a running exe aside, rebuild, keep the old bytes if the build fails (A9).
    async fn rebuild(&self) -> Result<(), String> {
        let repo = &self.cfg.repo_root;
        let exe = release_exe(repo);
        let aside = aside_exe(repo);

        let moved = exe.exists();
        if moved {
            match fs::remove_file(&aside) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("cargo build fallito: {error}")),
            }
            if let Err(error) = fs::rename(&exe, &aside) {
                return Err(format!("cargo build fallito: {error}"));
            }
        }

        let text = match run(
            &self.cfg.cargo,
            &["build", "--release"],
            repo,
            Some(BUILD_TIMEOUT),
            true,
        )
        .await
        {
            Ok(_) if exe.is_file() => return Ok(()),
            Ok(_) => format!(
                "cargo build fallito: eseguibile non trovato: {}",
                exe.display()
            ),
            Err(error) => format!("cargo build fallito: {}", detail(&error)),
        };

        if moved {
            let _ = fs::remove_file(&exe);
            let _ = fs::rename(&aside, &exe);
        }
        Err(text)
    }
}

fn idle_status() -> UpdateStatus {
    UpdateStatus {
        checked_at: Some(events::iso_now()),
        ..UpdateStatus::default()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Node's `err.stderr.trim() || err.message`.
fn detail(error: &RunError) -> &str {
    if error.stderr.is_empty() {
        &error.message
    } else {
        &error.stderr
    }
}

fn command_line(bin: &Path, args: &[&str]) -> String {
    format!("Command failed: {} {}", bin.display(), args.join(" "))
}

pub struct RunError {
    /// Trimmed stderr, empty when the command never ran.
    pub stderr: String,
    pub message: String,
}

async fn run(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    timeout: Option<Duration>,
    git_env: bool,
) -> Result<Output, RunError> {
    let mut command = tokio::process::Command::new(bin);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    if git_env {
        // A background update must never sit on a credential prompt.
        command.env("GIT_TERMINAL_PROMPT", "0");
    }

    let child = command.spawn().map_err(|error| RunError {
        stderr: String::new(),
        message: error.to_string(),
    })?;
    let pid = child.id();
    let mut waiter = Box::pin(child.wait_with_output());

    let output = match timeout {
        None => waiter.await,
        Some(limit) => match tokio::time::timeout(limit, &mut waiter).await {
            Ok(output) => output,
            Err(_) => {
                // cargo spawns rustc: kill the tree before dropping the child.
                kill_tree(pid);
                drop(waiter);
                return Err(RunError {
                    stderr: String::new(),
                    message: format!("{}\n", command_line(bin, args)),
                });
            }
        },
    };
    let output = output.map_err(|error| RunError {
        stderr: String::new(),
        message: error.to_string(),
    })?;
    if output.status.success() {
        return Ok(output);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(RunError {
        stderr: stderr.trim().to_string(),
        message: format!("{}\n{}", command_line(bin, args), stderr),
    })
}

#[cfg(windows)]
fn kill_tree(pid: Option<u32>) {
    use std::os::windows::process::CommandExt;

    let Some(pid) = pid else {
        return;
    };
    let _ = std::process::Command::new("taskkill")
        .arg("/T")
        .arg("/F")
        .arg("/PID")
        .arg(pid.to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

#[cfg(not(windows))]
fn kill_tree(_pid: Option<u32>) {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-update-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// `.cmd` on Windows, an executable `sh` script on unix.
    fn script(dir: &Path, name: &str, unix_body: &str, windows_body: &str) -> PathBuf {
        let _ = (unix_body, windows_body);
        #[cfg(windows)]
        {
            let path = dir.join(format!("{name}.cmd"));
            fs::write(&path, format!("@echo off\r\n{windows_body}")).unwrap();
            path
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let path = dir.join(format!("{name}.sh"));
            fs::write(&path, format!("#!/bin/sh\n{unix_body}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }

    fn manager(repo: &Path, cargo: PathBuf) -> UpdateManager {
        UpdateManager::new(UpdateConfig {
            repo_root: repo.to_path_buf(),
            git: PathBuf::from("git"),
            cargo,
            events: EventBus::new(),
            active_work: Arc::new(NoActiveWork),
            relaunch: None,
        })
    }

    fn assert_one_build_line(dir: &Path) {
        let log = fs::read_to_string(dir.join("cargo-log.txt")).unwrap_or_default();
        let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "cargo log: {log:?}");
        assert!(lines[0].contains("build --release"), "cargo log: {log:?}");
        assert!(
            lines[0].contains("GIT_TERMINAL_PROMPT=0"),
            "cargo log: {log:?}"
        );
    }

    const UNIX_LOG: &str =
        "echo \"$* GIT_TERMINAL_PROMPT=$GIT_TERMINAL_PROMPT\" >> \"$(dirname \"$0\")/cargo-log.txt\"\n";
    const WINDOWS_LOG: &str =
        ">>\"%~dp0cargo-log.txt\" echo %* GIT_TERMINAL_PROMPT=%GIT_TERMINAL_PROMPT%\r\n";

    #[test]
    fn parse_count_is_parse_int() {
        assert_eq!(parse_count("3\n"), 3);
        assert_eq!(parse_count(""), 0);
        assert_eq!(parse_count("x"), 0);
        assert_eq!(parse_count("12abc"), 12);
    }

    #[test]
    fn parse_log_splits_tabs() {
        let commits = parse_log("h1\tsubject one\t2 days ago\nh2\n");
        assert_eq!(
            commits,
            [
                UpdateCommitInfo {
                    hash: "h1".to_string(),
                    subject: "subject one".to_string(),
                    relative_time: "2 days ago".to_string(),
                },
                UpdateCommitInfo {
                    hash: "h2".to_string(),
                    subject: String::new(),
                    relative_time: String::new(),
                },
            ]
        );
    }

    #[test]
    fn force_follows_js_truthiness() {
        for body in [
            r#"{"force":true}"#,
            r#"{"force":1}"#,
            r#"{"force":-1}"#,
            r#"{"force":0.5}"#,
            r#"{"force":"yes"}"#,
            r#"{"force":[]}"#,
            r#"{"force":{}}"#,
        ] {
            assert!(force_from_body(body.as_bytes()), "{body}");
        }
        for body in [
            r#"{"force":false}"#,
            r#"{"force":0}"#,
            r#"{"force":0.0}"#,
            r#"{"force":""}"#,
            r#"{"force":null}"#,
            r#"{}"#,
            "not json",
            "[]",
            r#""true""#,
            "true",
        ] {
            assert!(!force_from_body(body.as_bytes()), "{body}");
        }
    }

    #[tokio::test]
    async fn no_active_work_is_zero() {
        let work = NoActiveWork;
        assert_eq!(work.count(), 0);
        work.shutdown().await;
    }

    #[tokio::test]
    async fn rebuild_swaps_the_executable() {
        let repo = temp_dir("rebuild-swap");
        let exe = release_exe(&repo);
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"old").unwrap();
        let cargo = script(
            &repo,
            "cargo",
            &format!("{UNIX_LOG}mkdir -p target/release\necho new > target/release/proxi\n"),
            &format!(
                "{WINDOWS_LOG}if not exist target\\release mkdir target\\release\r\necho new> target\\release\\proxi.exe\r\n"
            ),
        );

        manager(&repo, cargo).rebuild().await.unwrap();

        assert_eq!(fs::read_to_string(&exe).unwrap().trim(), "new");
        assert_eq!(fs::read_to_string(aside_exe(&repo)).unwrap(), "old");
        assert_one_build_line(&repo);
    }

    #[tokio::test]
    async fn rebuild_failure_restores_the_executable() {
        let repo = temp_dir("rebuild-failure");
        let exe = release_exe(&repo);
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"old").unwrap();
        let cargo = script(
            &repo,
            "cargo",
            &format!("{UNIX_LOG}echo boom >&2\nexit 1\n"),
            &format!("{WINDOWS_LOG}echo boom 1>&2\r\nexit /b 1\r\n"),
        );

        let error = manager(&repo, cargo).rebuild().await.unwrap_err();

        assert_eq!(error, "cargo build fallito: boom");
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
        assert!(!aside_exe(&repo).exists());
        assert_one_build_line(&repo);
    }

    #[tokio::test]
    async fn rebuild_without_output_restores() {
        let repo = temp_dir("rebuild-no-output");
        let exe = release_exe(&repo);
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"old").unwrap();
        let cargo = script(
            &repo,
            "cargo",
            &format!("{UNIX_LOG}exit 0\n"),
            &format!("{WINDOWS_LOG}exit /b 0\r\n"),
        );

        let error = manager(&repo, cargo).rebuild().await.unwrap_err();

        assert!(
            error.starts_with("cargo build fallito: eseguibile non trovato"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
        assert!(!aside_exe(&repo).exists());
        assert_one_build_line(&repo);
    }

    #[tokio::test]
    async fn rebuild_with_no_previous_exe() {
        let repo = temp_dir("rebuild-fresh");
        let exe = release_exe(&repo);
        let cargo = script(
            &repo,
            "cargo",
            &format!("{UNIX_LOG}mkdir -p target/release\necho new > target/release/proxi\n"),
            &format!(
                "{WINDOWS_LOG}if not exist target\\release mkdir target\\release\r\necho new> target\\release\\proxi.exe\r\n"
            ),
        );

        manager(&repo, cargo).rebuild().await.unwrap();

        assert_eq!(fs::read_to_string(&exe).unwrap().trim(), "new");
        assert!(!aside_exe(&repo).exists());
        assert_one_build_line(&repo);
    }

    #[test]
    fn remove_leftover_deletes_aside() {
        let repo = temp_dir("remove-leftover");
        let aside = aside_exe(&repo);
        fs::create_dir_all(aside.parent().unwrap()).unwrap();
        fs::write(&aside, b"old").unwrap();

        remove_leftover(&repo);

        assert!(!aside.exists());
        // Nothing to remove is not an error.
        remove_leftover(&repo);
    }
}
