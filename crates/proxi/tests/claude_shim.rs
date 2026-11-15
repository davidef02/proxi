//! A `.cmd` shim (an npm-installed `claude.cmd`) is passed to `std::process::Command` as the program:
//! std runs batch files through cmd.exe with cmd-safe escaping, so an argument is never re-split by the
//! shell. The old `cmd.exe /c <shim> <args>` wrapping applied MSVC quoting to a line cmd.exe parses
//! again: `C:\R&D\dati` (no spaces, unquoted) became the command `D\dati`.

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::sessions::require_fake_engine;
use proxi::claude::{ClaudeBin, ClaudeLaunch, McpServer, spawn_claude};
use proxi::engine::EngineMsg;
use proxi::protocol::LearnerSettings;

fn launch(repo: &Path, bin: ClaudeBin) -> ClaudeLaunch {
    ClaudeLaunch {
        bin,
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

#[tokio::test]
async fn a_cmd_shim_is_the_program_and_keeps_its_arguments_intact() {
    if !cfg!(windows) {
        // A `.cmd` is a Windows shim: the Unix engine is an extensionless executable.
        return;
    }
    require_fake_engine();

    let dir = common::temp_dir("claude-cmd-shim");
    let dump = dir.0.join("argv.json");
    // The shim forwards its arguments to node, which writes them out verbatim.
    fs::write(
        dir.0.join("dump.mjs"),
        format!(
            "import fs from 'node:fs';\nfs.writeFileSync({}, JSON.stringify(process.argv.slice(2)));\n",
            serde_json::to_string(&dump.display().to_string()).expect("json path")
        ),
    )
    .expect("write dump script");
    fs::write(
        dir.0.join("claude.cmd"),
        "@echo off\r\nnode \"%~dp0dump.mjs\" %*\r\n",
    )
    .expect("write shim");

    let bin = ClaudeBin {
        cmd: dir.0.join("claude.cmd").display().to_string(),
        args: vec!["--probe".to_string(), r"C:\R&D\dati".to_string()],
    };
    let (_handle, mut rx) = spawn_claude(launch(&dir.0, bin));

    // Exit is sent exactly once and last.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("the shim exits within 15 s")
        {
            Some(EngineMsg::Exit { code, .. }) => {
                assert_eq!(code, Some(0), "the shim runs node");
                break;
            }
            Some(_) => {}
            None => panic!("the engine channel closed without an exit"),
        }
    }

    let argv: Vec<String> =
        serde_json::from_str(&fs::read_to_string(&dump).expect("the shim ran and wrote its argv"))
            .expect("argv is a JSON array");
    assert_eq!(
        &argv[..2],
        ["--probe", r"C:\R&D\dati"],
        "both bin args arrive whole, `&` included: {argv:?}"
    );
    assert!(
        argv.iter().any(|arg| arg == "--output-format"),
        "Claude's own argv follows the bin args: {argv:?}"
    );
}
