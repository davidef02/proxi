//! The agy adapter over its fake script: turn, MCP tool call, resume, refused conversation,
//! stop, scratch files, child env, a missing executable and the model probe. Port of the agy
//! cases of `app/test/engine.test.ts` (no interrupt: that needs #20).

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use common::engine::{collect_until, fake_bin, inline_bin, kinds, launch, start_fake_mcp};
use proxi::agy::{AGY_MODELS_FALLBACK, list_agy_models, spawn_agy};
use proxi::engine::{EngineBin, EngineEvent, EngineMsg};

const TIMEOUT: Duration = Duration::from_secs(20);

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

fn fallback_models() -> Vec<String> {
    AGY_MODELS_FALLBACK
        .iter()
        .map(|model| model.to_string())
        .collect()
}

/// A pid that is still running: `tasklist` on Windows, `kill -0` elsewhere.
fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

fn missing_bin(dir: &Path, name: &str) -> EngineBin {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    EngineBin {
        cmd: dir.join(file).display().to_string(),
        args: Vec::new(),
    }
}

#[tokio::test]
async fn a_turn_issues_a_token_streams_text_and_one_success_result() {
    common::require_node();
    let dir = common::temp_dir("agy-turn");
    let mcp = start_fake_mcp().await;
    let (mut handle, mut rx) = spawn_agy(launch(
        fake_bin("fake-agy.mjs", &[]),
        &dir.0,
        &mcp.url,
        None,
    ));

    handle.send("ciao");
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Event(EngineEvent::Result { .. })),
        TIMEOUT,
    )
    .await;

    let token = messages
        .iter()
        .rev()
        .find_map(|msg| match msg {
            EngineMsg::Event(EngineEvent::Init { session_id, .. }) => Some(session_id.clone()),
            _ => None,
        })
        .expect("init carries the token");
    assert!(!token.is_empty(), "init carries the token");
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg, EngineMsg::Event(EngineEvent::TextDelta { .. }))),
        "text streams: {:?}",
        kinds(&messages)
    );
    assert!(
        messages.iter().any(|msg| matches!(
            msg,
            EngineMsg::Event(EngineEvent::Assistant { texts, .. }) if !texts.is_empty()
        )),
        "an assistant message with text: {:?}",
        kinds(&messages)
    );
    let is_error = messages.iter().find_map(|msg| match msg {
        EngineMsg::Event(EngineEvent::Result { is_error, .. }) => Some(*is_error),
        _ => None,
    });
    assert_eq!(is_error, Some(false));

    handle.stop().await;
}

#[tokio::test]
async fn an_mcp_tool_call_reaches_the_endpoint_with_the_token() {
    common::require_node();
    let dir = common::temp_dir("agy-mcp");
    let mcp = start_fake_mcp().await;
    let (mut handle, mut rx) = spawn_agy(launch(
        fake_bin("fake-agy.mjs", &[]),
        &dir.0,
        &mcp.url,
        None,
    ));

    let input = STANDARD.encode(r#"{"a":1}"#);
    handle.send(&format!("FAKE:tool:echo:{input}"));
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Event(EngineEvent::Result { .. })),
        TIMEOUT,
    )
    .await;

    let used = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Event(EngineEvent::Assistant { tool_uses, .. }) => tool_uses.first(),
            _ => None,
        })
        .expect("a tool use");
    assert_eq!(used.name, "mcp__app__echo");
    let result = messages
        .iter()
        .find(|msg| matches!(msg, EngineMsg::Event(EngineEvent::ToolResult { .. })))
        .expect("a tool result");
    let EngineMsg::Event(EngineEvent::ToolResult {
        tool_use_id,
        is_error,
        text,
    }) = result
    else {
        unreachable!()
    };
    assert_eq!(tool_use_id, &used.id);
    assert!(!is_error);
    assert!(text.contains("eco {\"a\":1}"), "{text}");

    {
        let tokens = mcp.tokens.lock().expect("lock MCP tokens");
        assert!(!tokens.is_empty(), "the engine called the endpoint");
        for token in tokens.iter() {
            assert_eq!(token.as_deref(), Some("tok"));
        }
    }

    handle.stop().await;
}

#[tokio::test]
async fn a_new_handle_with_the_token_resumes_it() {
    common::require_node();
    let dir = common::temp_dir("agy-resume");
    let mcp = start_fake_mcp().await;

    let (mut handle, mut rx) = spawn_agy(launch(
        fake_bin("fake-agy.mjs", &[]),
        &dir.0,
        &mcp.url,
        None,
    ));
    handle.send("ciao");
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Event(EngineEvent::Result { .. })),
        TIMEOUT,
    )
    .await;
    let token = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Event(EngineEvent::Init { session_id, .. }) => Some(session_id.clone()),
            _ => None,
        })
        .expect("a token");
    handle.stop().await;

    let (mut handle, mut rx) = spawn_agy(launch(
        fake_bin("fake-agy.mjs", &[]),
        &dir.0,
        &mcp.url,
        Some(&token),
    ));
    handle.send("ci sei?");
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Event(EngineEvent::Result { .. })),
        TIMEOUT,
    )
    .await;
    let resumed = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Event(EngineEvent::Init { session_id, .. }) => Some(session_id.clone()),
            _ => None,
        })
        .expect("an init");
    assert_eq!(resumed, token, "the same conversation");

    handle.stop().await;
}

#[tokio::test]
async fn a_refused_conversation_reports_resume_failed_then_exits_once() {
    common::require_node();
    let dir = common::temp_dir("agy-refused");
    let bin = inline_bin("console.error('conversation not found');process.exit(1)");
    let (_handle, mut rx) = spawn_agy(launch(
        bin,
        &dir.0,
        "http://127.0.0.1:1/mcp",
        Some("sconosciuto"),
    ));

    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Exit { .. }),
        TIMEOUT,
    )
    .await;
    assert_eq!(kinds(&messages), ["resume-failed"]);
    assert!(matches!(messages.last(), Some(EngineMsg::Exit { .. })));
    assert!(rx.recv().await.is_none(), "the exit is the last message");
}

#[tokio::test]
async fn stop_exits_exactly_once_and_kills_the_process() {
    common::require_node();
    let dir = common::temp_dir("agy-stop");
    // Never inits: stopping is not a refused conversation, whatever the process wrote.
    let bin = inline_bin("process.stdin.resume();");
    let (mut handle, mut rx) = spawn_agy(launch(
        bin,
        &dir.0,
        "http://127.0.0.1:1/mcp",
        Some("conv-vecchia"),
    ));
    let pid = handle.pid().expect("the eager spawn reports a pid");
    assert!(alive(pid), "the process is running");

    handle.stop().await;

    let mut messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Exit { .. }),
        Duration::from_secs(10),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(msg)) => messages.push(msg),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        messages
            .iter()
            .filter(|msg| matches!(msg, EngineMsg::Exit { .. }))
            .count(),
        1
    );
    assert!(
        !messages
            .iter()
            .any(|msg| matches!(msg, EngineMsg::Event(EngineEvent::ResumeFailed))),
        "stopping must not read as a refused conversation"
    );
    assert!(!alive(pid), "the process tree is gone");
}

#[tokio::test]
async fn the_scratch_agent_file_and_cwd() {
    common::require_node();
    let dir = common::temp_dir("agy-scratch");
    let mcp = start_fake_mcp().await;
    let (mut handle, mut rx) = spawn_agy(launch(
        fake_bin("fake-agy.mjs", &[]),
        &dir.0,
        &mcp.url,
        None,
    ));
    handle.send("ciao");
    collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Event(EngineEvent::Result { .. })),
        TIMEOUT,
    )
    .await;

    let scratch = dir.0.join("scratch");
    let agent = scratch.join(".agents").join("agents").join("ia-learner.md");
    let text = fs::read_to_string(&agent).expect("the agent file lives in the scratch dir");
    assert!(text.starts_with("---\nname: ia-learner\n"), "{text}");
    assert!(
        text.contains("tools: [\"view_file\",\"list_dir\",\"grep_search\"]"),
        "{text}"
    );
    handle.stop().await;

    // agy reads the agent file from its cwd: the process must run in the scratch dir.
    let (_handle, mut rx) = spawn_agy(launch(
        inline_bin("console.error('cwd=' + process.cwd());"),
        &dir.0,
        "http://127.0.0.1:1/mcp",
        None,
    ));
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Exit { .. }),
        TIMEOUT,
    )
    .await;
    let cwd = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Stderr(line) => line.strip_prefix("cwd=").map(str::to_string),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the child printed its cwd; got {messages:?}"));
    assert_eq!(
        fs::canonicalize(Path::new(&cwd)).expect("the reported cwd exists"),
        fs::canonicalize(&scratch).expect("the scratch dir exists")
    );
}

#[tokio::test]
async fn data_root_reaches_the_child_env() {
    common::require_node();
    let dir = common::temp_dir("agy-env");
    let data = dir.0.join("dati");
    let script = "console.error('DATA_ROOT=' + (process.env.LEARNER_DATA_ROOT ?? 'UNSET'));";

    let mut l = launch(inline_bin(script), &dir.0, "http://127.0.0.1:1/mcp", None);
    l.data_root = Some(data.clone());
    let (_handle, mut rx) = spawn_agy(l);
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Exit { .. }),
        TIMEOUT,
    )
    .await;
    assert_eq!(data_root_line(&messages), data.display().to_string());

    let mut l = launch(inline_bin(script), &dir.0, "http://127.0.0.1:1/mcp", None);
    l.data_root = None;
    let (_handle, mut rx) = spawn_agy(l);
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Exit { .. }),
        TIMEOUT,
    )
    .await;
    assert_eq!(
        data_root_line(&messages),
        std::env::var("LEARNER_DATA_ROOT").unwrap_or_else(|_| "UNSET".to_string())
    );
}

fn data_root_line(messages: &[EngineMsg]) -> String {
    messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Stderr(line) => line.strip_prefix("DATA_ROOT=").map(str::to_string),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the child printed its data root; got {messages:?}"))
}

#[tokio::test]
async fn a_missing_executable_reports_spawn_enoent() {
    common::require_node();
    let dir = common::temp_dir("agy-missing-bin");
    let bin = missing_bin(&dir.0, "missing-agy");
    let missing = bin.cmd.clone();
    let (handle, mut rx) = spawn_agy(launch(bin, &dir.0, "http://127.0.0.1:1/mcp", None));
    assert_eq!(handle.pid(), None);

    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Exit { .. }),
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        messages,
        vec![
            EngineMsg::Stderr(format!("spawn error: spawn {missing} ENOENT")),
            EngineMsg::Exit {
                code: None,
                signal: None
            },
        ]
    );
}

#[tokio::test]
async fn missing_bin_lists_the_fallback_models() {
    common::require_node();
    let dir = common::temp_dir("agy-models-missing");
    assert_eq!(
        list_agy_models(&missing_bin(&dir.0, "missing-agy")).await,
        fallback_models()
    );
}

#[tokio::test]
async fn fake_bin_lists_its_models() {
    common::require_node();
    assert_eq!(
        list_agy_models(&fake_bin("fake-agy.mjs", &[])).await,
        strings(&["gemini-3.8-flash-high", "claude-sonnet-4-6"])
    );
}
