//! An opencode `.cmd` shim (an npm install without the `.exe`) is passed to the process launcher as
//! the program: std runs batch files through cmd.exe with cmd-safe escaping, so an argument with
//! `&`, spaces and parentheses reaches the engine intact (the Windows case of AC3).

mod common;

use std::fs;
use std::time::Duration;

use common::engine::{collect_until, launch};
use proxi::engine::{EngineBin, EngineEvent, EngineMsg};
use proxi::opencode::spawn_opencode;

#[tokio::test]
async fn a_cmd_shim_is_the_program_and_keeps_its_arguments_intact() {
    if !cfg!(windows) {
        // A `.cmd` is a Windows shim: the Unix engine is an extensionless executable.
        return;
    }
    common::require_node();

    let dir = common::temp_dir("opencode-cmd-shim");
    let fake = common::repo_root()
        .join("app")
        .join("test")
        .join("fake-opencode.mjs");
    fs::write(
        dir.0.join("opencode.cmd"),
        format!("@echo off\r\nnode \"{}\" %*\r\n", fake.display()),
    )
    .expect("write the shim");

    let bin = EngineBin {
        cmd: dir.0.join("opencode.cmd").display().to_string(),
        args: Vec::new(),
    };
    let (mut handle, mut rx) = spawn_opencode(launch(bin, &dir.0, "http://127.0.0.1:1/mcp", None));
    let text = "ciao & R&D (prova)";
    handle.send(text);

    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Event(EngineEvent::Result { .. })),
        Duration::from_secs(30),
    )
    .await;
    assert!(
        matches!(
            messages.last(),
            Some(EngineMsg::Event(EngineEvent::Result {
                is_error: false,
                ..
            }))
        ),
        "the shim ran the fake to a result: {messages:?}"
    );

    let argv_line = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Stderr(line) if line.starts_with("fake-opencode argv: ") => {
                Some(line.clone())
            }
            _ => None,
        })
        .expect("the fake logs its argv");
    let argv: Vec<String> =
        serde_json::from_str(argv_line.trim_start_matches("fake-opencode argv: ").trim())
            .expect("the argv line is a JSON array");
    assert_eq!(
        argv.last().map(String::as_str),
        Some(text),
        "the argument arrives whole: {argv:?}"
    );

    handle.stop().await;
    let mut exits = 0;
    loop {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(EngineMsg::Exit { .. })) => exits += 1,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert_eq!(exits, 1, "stop exits the session exactly once");
}
