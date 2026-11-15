//! A scratch-dir filesystem error is reported as itself, never as a missing executable: Claude's
//! `prepare` must not turn an unavailable folder into `spawn error: spawn claude ENOENT`, which
//! tells the learner to install Claude when the folder is the problem (AC5's ENOENT is for a
//! missing binary only).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use proxi::claude::{ClaudeBin, ClaudeLaunch, McpServer, spawn_claude};
use proxi::engine::EngineMsg;
use proxi::protocol::LearnerSettings;

fn launch(repo: &Path, scratch_dir: PathBuf) -> ClaudeLaunch {
    ClaudeLaunch {
        bin: ClaudeBin {
            cmd: "claude".to_string(),
            args: Vec::new(),
        },
        repo_root: repo.to_path_buf(),
        data_root: None,
        scratch_dir,
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

/// An unused drive letter: creating a directory under it fails with a `NotFound` io error, the
/// same kind a missing executable gives, so only the prepare branch's own text tells them apart.
#[cfg(windows)]
fn unused_drive() -> Option<PathBuf> {
    ('Q'..='Z').rev().find_map(|letter| {
        let root = PathBuf::from(format!("{letter}:\\"));
        fs::metadata(&root).is_err().then_some(root)
    })
}

#[tokio::test]
async fn a_scratch_dir_not_found_is_not_reported_as_a_missing_executable() {
    if !cfg!(windows) {
        // The exact `NotFound` filesystem error below is Windows' `ERROR_PATH_NOT_FOUND`.
        return;
    }
    let Some(drive) = unused_drive() else {
        // Every letter is taken: no deterministic `NotFound` to build here.
        return;
    };

    let dir = common::temp_dir("claude-prepare-not-found");
    let (_handle, mut rx) = spawn_claude(launch(&dir.0, drive.join("proxi-scratch")));

    let mut messages = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("spawn_claude reports the prepare error within 10 s")
        {
            Some(message) => {
                let exit = matches!(
                    message,
                    EngineMsg::Exit {
                        code: None,
                        signal: None
                    }
                );
                messages.push(message);
                if exit {
                    break;
                }
            }
            None => panic!("the engine channel closed without an exit: {messages:?}"),
        }
    }

    let stderr = messages
        .iter()
        .find_map(|message| match message {
            EngineMsg::Stderr(text) => Some(text.clone()),
            _ => None,
        })
        .expect("a spawn error line");
    assert!(
        stderr.starts_with("spawn error: "),
        "the prepare failure is a spawn error: {stderr}"
    );
    assert!(
        !stderr.contains("ENOENT"),
        "a filesystem NotFound must not read as a missing executable: {stderr}"
    );
    assert!(
        stderr.contains("os error 3"),
        "the real io error is kept (ERROR_PATH_NOT_FOUND): {stderr}"
    );
    assert!(
        matches!(
            messages.last(),
            Some(EngineMsg::Exit {
                code: None,
                signal: None
            })
        ),
        "one exit, last: {messages:?}"
    );
}
