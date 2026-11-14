//! Child process trees that die as one unit (Windows Job Objects, Unix process groups).

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};

/// How long the reaper waits for a killed tree before giving up on it.
const REAP_LIMIT: Duration = Duration::from_secs(5);

pub struct TreeChild {
    inner: Box<dyn ChildWrapper>,
    done: bool,
    tree: bool,
}

fn spawn_with(make: &impl Fn() -> tokio::process::Command, tree: bool) -> io::Result<TreeChild> {
    let mut w = CommandWrap::from(make());
    #[cfg(windows)]
    {
        if tree {
            w.wrap(JobObject);
        }
    }
    #[cfg(unix)]
    {
        if tree {
            w.wrap(ProcessGroup::leader());
        }
    }
    w.wrap(KillOnDrop);
    let inner = w.spawn()?;
    Ok(TreeChild {
        inner,
        done: false,
        tree,
    })
}

/// Spawn through a Job Object (a process group on Unix) so the whole tree dies together.
pub fn spawn_tree(make: impl Fn() -> tokio::process::Command) -> io::Result<TreeChild> {
    #[cfg(windows)]
    {
        // Some hosts refuse a second job object; taskkill /T still gets the descendants.
        match spawn_with(&make, true) {
            Ok(child) => Ok(child),
            Err(_) => spawn_with(&make, false),
        }
    }
    #[cfg(not(windows))]
    {
        spawn_with(&make, true)
    }
}

impl TreeChild {
    pub fn id(&self) -> Option<u32> {
        self.inner.id()
    }

    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.inner.stderr().take()
    }

    pub fn uses_tree(&self) -> bool {
        self.tree
    }

    #[cfg(windows)]
    fn taskkill(pid: u32) {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let pid = pid.to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }

    pub fn kill_tree(&mut self) {
        #[cfg(windows)]
        {
            let pid = self.id();
            // /T walks the tree, so the root must still be alive when taskkill starts.
            if !self.tree {
                if let Some(pid) = pid {
                    Self::taskkill(pid);
                }
            }
            let r = self.inner.start_kill();
            if self.tree && r.is_err() {
                if let Some(pid) = pid {
                    Self::taskkill(pid);
                }
            }
        }
        #[cfg(unix)]
        {
            let _ = self.inner.start_kill();
        }
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.inner.wait().await;
        if status.is_ok() {
            self.done = true;
        }
        status
    }

    pub async fn wait_timeout(&mut self, limit: Duration) -> io::Result<Option<ExitStatus>> {
        match tokio::time::timeout(limit, self.inner.wait()).await {
            Ok(status) => {
                let status = status?;
                self.done = true;
                Ok(Some(status))
            }
            Err(_) => {
                self.kill_tree();
                let _ = tokio::time::timeout(REAP_LIMIT, self.inner.wait()).await;
                self.done = true;
                Ok(None)
            }
        }
    }
}

impl Drop for TreeChild {
    fn drop(&mut self) {
        // Cancellation path: dropping the handler future drops this and must kill the tree.
        if !self.done {
            self.kill_tree();
        }
    }
}

pub fn find_on_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    for entry in std::env::split_paths(path?) {
        if entry.as_os_str().is_empty() {
            continue;
        }
        let direct = entry.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        #[cfg(windows)]
        {
            let exe = entry.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

pub fn node_on_path() -> Option<PathBuf> {
    find_on_path("node", std::env::var_os("PATH").as_deref())
}

pub fn shim_command(program: &Path) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let batch = program
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
        if batch {
            let mut cmd = tokio::process::Command::new("cmd.exe");
            cmd.args(["/d", "/c"]).arg(program);
            return cmd;
        }
    }
    tokio::process::Command::new(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("proxi-child-{}-{nanos}-{n}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    const PARENT: &str = r#"
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
spawn(process.execPath, [join(here, "child.mjs")], { stdio: "ignore" });
setInterval(() => {}, 1000);
"#;

    const CHILD: &str = r#"
import { appendFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
setInterval(() => appendFileSync(join(here, "beat.txt"), "."), 50);
"#;

    fn heartbeat_scripts(dir: &Path) {
        fs::write(dir.join("parent.mjs"), PARENT).expect("write parent");
        fs::write(dir.join("child.mjs"), CHILD).expect("write child");
    }

    fn node_command(node: &Path, script: &Path) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(node);
        cmd.arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    }

    fn beat_len(path: &Path) -> u64 {
        fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }

    async fn wait_for_beat(path: &Path, non_empty: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let exists = path.exists();
            if (exists && !non_empty) || beat_len(path) > 0 {
                return;
            }
            assert!(Instant::now() < deadline, "heartbeat never started");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn assert_stopped(path: &Path) {
        for _ in 0..10 {
            let before = beat_len(path);
            tokio::time::sleep(Duration::from_millis(400)).await;
            if beat_len(path) == before {
                return;
            }
        }
        panic!("heartbeat still growing: {}", path.display());
    }

    #[test]
    fn find_on_path_finds_the_first_match_and_skips_missing() {
        let dir = temp_dir();
        let empty = dir.0.join("empty");
        let tools = dir.0.join("tools");
        fs::create_dir_all(&empty).expect("create empty");
        fs::create_dir_all(&tools).expect("create tools");
        #[cfg(windows)]
        let tool = tools.join("tool.exe");
        #[cfg(not(windows))]
        let tool = tools.join("tool");
        fs::write(&tool, "").expect("write tool");

        let path = std::env::join_paths([&empty, &tools]).expect("join paths");
        assert_eq!(find_on_path("tool", Some(&path)), Some(tool));

        assert_eq!(find_on_path("tool", None), None);
        let empty_path = std::env::join_paths([&empty]).expect("join paths");
        assert_eq!(find_on_path("tool", Some(&empty_path)), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn normal_exit_returns_status_and_stderr() {
        let node = node_on_path().expect("node on PATH");
        let mut child = spawn_tree(|| {
            let mut cmd = tokio::process::Command::new(&node);
            cmd.args(["-e", "process.stderr.write('ciao')"])
                .stderr(Stdio::piped());
            cmd
        })
        .expect("spawn node");

        let mut stderr = child.take_stderr().expect("stderr piped");
        let read = tokio::spawn(async move {
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut buf)
                .await
                .expect("read stderr");
            String::from_utf8(buf).expect("utf-8 stderr")
        });

        let status = child
            .wait_timeout(Duration::from_secs(10))
            .await
            .expect("wait")
            .expect("exited before the timeout");
        assert!(status.success());
        assert_eq!(read.await.expect("stderr task"), "ciao");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_kills_the_grandchild() {
        let node = node_on_path().expect("node on PATH");
        let dir = temp_dir();
        heartbeat_scripts(&dir.0);
        let parent = dir.0.join("parent.mjs");
        let beat = dir.0.join("beat.txt");

        let mut child = spawn_tree(|| node_command(&node, &parent)).expect("spawn parent");
        assert!(child.uses_tree());
        wait_for_beat(&beat, false).await;

        let status = child
            .wait_timeout(Duration::from_millis(500))
            .await
            .expect("wait");
        assert!(status.is_none());
        assert_stopped(&beat).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_child_kills_the_tree() {
        let node = node_on_path().expect("node on PATH");
        let dir = temp_dir();
        heartbeat_scripts(&dir.0);
        let parent = dir.0.join("parent.mjs");
        let beat = dir.0.join("beat.txt");

        let handle = tokio::spawn(async move {
            let mut child = spawn_tree(|| node_command(&node, &parent)).expect("spawn parent");
            assert!(child.uses_tree());
            let _ = child.wait().await;
        });
        wait_for_beat(&beat, false).await;

        handle.abort();
        let _ = handle.await;
        assert_stopped(&beat).await;
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread")]
    async fn fallback_without_job_object_kills_the_tree_with_taskkill() {
        let node = node_on_path().expect("node on PATH");
        let dir = temp_dir();
        heartbeat_scripts(&dir.0);
        let parent = dir.0.join("parent.mjs");
        let beat = dir.0.join("beat.txt");
        let make = || node_command(&node, &parent);

        let mut child = spawn_with(&make, false).expect("spawn parent");
        assert!(!child.uses_tree());
        wait_for_beat(&beat, true).await;

        let status = child
            .wait_timeout(Duration::from_millis(500))
            .await
            .expect("wait");
        assert!(status.is_none());
        assert_stopped(&beat).await;
    }
}
