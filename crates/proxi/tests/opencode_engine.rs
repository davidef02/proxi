//! OpenCode adapter over the fake opencode: the turn, MCP, resume, a refused session, stop, env and
//! the model list (the opencode cases of `app/test/engine.test.ts` and `opencode.test.ts`).

mod common;

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use common::engine::{collect_until, fake_bin, inline_bin, kinds, launch, start_fake_mcp};
use proxi::engine::{EngineBin, EngineEvent, EngineLaunch, EngineMsg};
use proxi::opencode::{OPENCODE_MODELS_FALLBACK, list_opencode_models, spawn_opencode};

const TIMEOUT: Duration = Duration::from_secs(30);
const MCP_URL: &str = "http://127.0.0.1:1/mcp";

fn is_result(msg: &EngineMsg) -> bool {
    matches!(msg, EngineMsg::Event(EngineEvent::Result { .. }))
}

fn is_exit(msg: &EngineMsg) -> bool {
    matches!(msg, EngineMsg::Exit { .. })
}

fn opencode(dir: &Path, resume: Option<&str>) -> EngineLaunch {
    launch(fake_bin("fake-opencode.mjs", &[]), dir, MCP_URL, resume)
}

fn session_id(messages: &[EngineMsg]) -> String {
    messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Event(EngineEvent::Init { session_id, .. }) if !session_id.is_empty() => {
                Some(session_id.clone())
            }
            _ => None,
        })
        .expect("an init with the session id")
}

fn assistant_text(messages: &[EngineMsg]) -> String {
    messages
        .iter()
        .filter_map(|msg| match msg {
            EngineMsg::Event(EngineEvent::Assistant { texts, .. }) => Some(
                texts
                    .iter()
                    .map(|(_, text)| text.as_str())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_turn_issues_a_token_streams_text_and_one_success_result() {
    common::require_node();
    let dir = common::temp_dir("opencode-turn");
    let (mut handle, mut rx) = spawn_opencode(opencode(&dir.0, None));
    handle.send("ciao");

    let messages = collect_until(&mut rx, is_result, TIMEOUT).await;
    let session = session_id(&messages);
    assert!(!session.is_empty());
    assert!(
        messages.iter().any(|msg| matches!(
            msg,
            EngineMsg::Event(EngineEvent::TextDelta { text, .. }) if !text.is_empty()
        )),
        "the deltas carry the answer: {messages:?}"
    );
    assert!(
        assistant_text(&messages).contains("Ciao, sono il docente"),
        "the assistant text is the answer: {messages:?}"
    );
    assert!(
        assistant_text(&messages).contains(&session),
        "the answer names its session: {messages:?}"
    );
    assert!(matches!(
        messages.last(),
        Some(EngineMsg::Event(EngineEvent::Result {
            is_error: false,
            subtype,
            session_id: result_session,
            ..
        })) if subtype == "success" && result_session == &session
    ));
    handle.stop().await;
}

#[tokio::test]
async fn an_mcp_tool_call_reaches_the_endpoint_with_the_token() {
    common::require_node();
    let dir = common::temp_dir("opencode-mcp");
    let mcp = start_fake_mcp().await;
    let (mut handle, mut rx) = spawn_opencode(launch(
        fake_bin("fake-opencode.mjs", &[]),
        &dir.0,
        &mcp.url,
        None,
    ));
    handle.send(&format!("FAKE:tool:echo:{}", STANDARD.encode(r#"{"a":1}"#)));

    let messages = collect_until(&mut rx, is_result, TIMEOUT).await;
    let tool = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Event(EngineEvent::Assistant { tool_uses, .. }) => tool_uses.first(),
            _ => None,
        })
        .expect("a tool use")
        .clone();
    assert_eq!(tool.name, "mcp__app__echo");
    assert_eq!(tool.input, Some(serde_json::json!({ "a": 1 })));

    let result = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Event(EngineEvent::ToolResult {
                tool_use_id,
                is_error,
                text,
            }) if tool_use_id == &tool.id => Some((*is_error, text.clone())),
            _ => None,
        })
        .expect("the tool result with the same id");
    assert!(!result.0);
    assert!(result.1.contains(r#"eco {"a":1}"#), "{}", result.1);

    let tokens = mcp.tokens.lock().expect("lock MCP tokens").clone();
    assert!(!tokens.is_empty(), "the fake reached the endpoint");
    for token in tokens {
        assert_eq!(token.as_deref(), Some("tok"));
    }
    handle.stop().await;
}

#[tokio::test]
async fn a_new_handle_with_the_token_resumes_it() {
    common::require_node();
    let dir = common::temp_dir("opencode-resume");
    let (mut handle, mut rx) = spawn_opencode(opencode(&dir.0, None));
    handle.send("ciao");
    let first = collect_until(&mut rx, is_result, TIMEOUT).await;
    let session = session_id(&first);
    handle.stop().await;

    let (mut resumed, mut rx) = spawn_opencode(opencode(&dir.0, Some(&session)));
    resumed.send("ancora");
    let second = collect_until(&mut rx, is_result, TIMEOUT).await;
    assert_eq!(session_id(&second), session);
    assert!(
        assistant_text(&second).contains(&session),
        "the resumed engine knows the session: {second:?}"
    );
    resumed.stop().await;
}

#[tokio::test]
async fn a_refused_conversation_reports_resume_failed_then_exits_once() {
    common::require_node();
    let dir = common::temp_dir("opencode-refused");
    let bin = inline_bin("console.error('session not found');process.exit(1)");
    let (mut handle, mut rx) = spawn_opencode(launch(bin, &dir.0, MCP_URL, Some("sconosciuto")));
    handle.send("ciao");

    let messages = collect_until(&mut rx, is_exit, TIMEOUT).await;
    assert_eq!(kinds(&messages), ["resume-failed"]);
    assert!(matches!(messages.last(), Some(EngineMsg::Exit { .. })));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "the exit is reported once"
    );
    handle.stop().await;
}

#[tokio::test]
async fn stop_exits_exactly_once_and_kills_the_process() {
    common::require_node();
    let dir = common::temp_dir("opencode-stop");
    let (mut handle, mut rx) = spawn_opencode(opencode(&dir.0, None));
    handle.send("FAKE:slow");
    collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Event(EngineEvent::TextDelta { .. })),
        TIMEOUT,
    )
    .await;
    assert!(handle.pid().is_some(), "a live turn has a pid");

    let started = std::time::Instant::now();
    handle.stop().await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stop took {:?}",
        started.elapsed()
    );
    assert_eq!(handle.pid(), None, "the process was reaped");

    let mut exits = 0;
    let mut saw_resume_failed = false;
    loop {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(EngineMsg::Exit { .. })) => exits += 1,
            Ok(Some(EngineMsg::Event(EngineEvent::ResumeFailed))) => saw_resume_failed = true,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert_eq!(exits, 1);
    assert!(
        !saw_resume_failed,
        "an init already cleared the resume flag"
    );
    // A second stop returns at once.
    handle.stop().await;

    // Stopping a resumed handle likewise reports no refusal.
    let (mut resumed, mut rx) = spawn_opencode(opencode(&dir.0, Some("ses-1")));
    resumed.send("FAKE:slow");
    collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Event(EngineEvent::TextDelta { .. })),
        TIMEOUT,
    )
    .await;
    resumed.stop().await;
    let mut exits = 0;
    loop {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(EngineMsg::Exit { .. })) => exits += 1,
            Ok(Some(EngineMsg::Event(EngineEvent::ResumeFailed))) => {
                panic!("stopping a resumed handle is not a refused resume")
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert_eq!(exits, 1);
}

#[tokio::test]
async fn the_scratch_agent_file_and_cwd() {
    common::require_node();
    let dir = common::temp_dir("opencode-scratch");
    let (mut handle, _rx) = spawn_opencode(opencode(&dir.0, None));

    let scratch = dir.0.join("scratch");
    let agent = scratch
        .join(".opencode")
        .join("agents")
        .join("ia-learner.md");
    assert_eq!(
        std::fs::read_to_string(&agent).expect("the agent file"),
        "---\ndescription: ia-learner\nmode: primary\n---\n\nSei il docente.\n"
    );
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(scratch.join("opencode.json")).expect("the MCP config"),
    )
    .expect("opencode.json is JSON");
    assert_eq!(
        config["mcp"]["servers"]["app"]["headers"]["x-learner-token"],
        serde_json::json!("tok")
    );

    // The engine runs in the scratch dir: the fake reads opencode.json from its cwd.
    let script = "console.error('CWD=' + process.cwd());process.exit(0)";
    let (mut probe, mut probe_rx) =
        spawn_opencode(launch(inline_bin(script), &dir.0, MCP_URL, None));
    probe.send("ciao");
    let messages = collect_until(
        &mut probe_rx,
        |msg| matches!(msg, EngineMsg::Stderr(line) if line.starts_with("CWD=")),
        TIMEOUT,
    )
    .await;
    let line = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Stderr(line) if line.starts_with("CWD=") => Some(line.clone()),
            _ => None,
        })
        .expect("the child printed its cwd");
    assert_eq!(line, format!("CWD={}", scratch.display()));
    probe.stop().await;
    handle.stop().await;
}

#[tokio::test]
async fn data_root_reaches_the_child_env() {
    common::require_node();
    let script =
        "console.error('DATA_ROOT=' + (process.env.LEARNER_DATA_ROOT ?? 'UNSET'));process.exit(0)";
    let dir = common::temp_dir("opencode-env");
    let data = common::temp_dir("opencode-env-data");

    let mut with_root = launch(inline_bin(script), &dir.0, MCP_URL, None);
    with_root.data_root = Some(data.0.clone());
    let (mut handle, mut rx) = spawn_opencode(with_root);
    handle.send("ciao");
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Stderr(line) if line.starts_with("DATA_ROOT=")),
        TIMEOUT,
    )
    .await;
    let line = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Stderr(line) if line.starts_with("DATA_ROOT=") => Some(line.clone()),
            _ => None,
        })
        .expect("the child printed its data root");
    assert_eq!(line, format!("DATA_ROOT={}", data.0.display()));
    handle.stop().await;

    // Without a data root the child inherits the test process's environment, as in Node.
    let (mut handle, mut rx) = spawn_opencode(launch(inline_bin(script), &dir.0, MCP_URL, None));
    handle.send("ciao");
    let messages = collect_until(
        &mut rx,
        |msg| matches!(msg, EngineMsg::Stderr(line) if line.starts_with("DATA_ROOT=")),
        TIMEOUT,
    )
    .await;
    let inherited = std::env::var("LEARNER_DATA_ROOT").unwrap_or_else(|_| "UNSET".to_string());
    let line = messages
        .iter()
        .find_map(|msg| match msg {
            EngineMsg::Stderr(line) if line.starts_with("DATA_ROOT=") => Some(line.clone()),
            _ => None,
        })
        .expect("the child printed its data root");
    assert_eq!(line, format!("DATA_ROOT={inherited}"));
    handle.stop().await;
}

#[tokio::test]
async fn a_missing_executable_reports_spawn_enoent() {
    common::require_node();
    let dir = common::temp_dir("opencode-missing");
    let name = if cfg!(windows) {
        "missing-opencode.exe"
    } else {
        "missing-opencode"
    };
    let missing = dir.0.join(name);
    let bin = EngineBin {
        cmd: missing.display().to_string(),
        args: Vec::new(),
    };
    let (mut handle, mut rx) = spawn_opencode(launch(bin, &dir.0, MCP_URL, None));
    assert_eq!(handle.pid(), None);
    handle.send("ciao");

    let messages = collect_until(&mut rx, is_result, TIMEOUT).await;
    assert_eq!(
        messages[0],
        EngineMsg::Stderr(format!("spawn error: spawn {} ENOENT", missing.display()))
    );
    assert!(matches!(
        messages.last(),
        Some(EngineMsg::Event(EngineEvent::Result {
            is_error: true,
            subtype,
            ..
        })) if subtype == "error"
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "a failed spawn is not a session exit"
    );
    handle.stop().await;
    let messages = collect_until(&mut rx, is_exit, TIMEOUT).await;
    assert!(matches!(
        messages.last(),
        Some(EngineMsg::Exit {
            code: None,
            signal: None
        })
    ));
}

#[tokio::test]
async fn a_normal_turn_exit_is_not_a_session_exit() {
    common::require_node();
    let dir = common::temp_dir("opencode-two-turns");
    let (mut handle, mut rx) = spawn_opencode(opencode(&dir.0, None));

    handle.send("ciao");
    let first = collect_until(&mut rx, is_result, TIMEOUT).await;
    let session = session_id(&first);
    handle.send("ancora");
    let second = collect_until(&mut rx, is_result, TIMEOUT).await;
    assert!(matches!(
        second.last(),
        Some(EngineMsg::Event(EngineEvent::Result {
            is_error: false,
            ..
        }))
    ));
    assert_eq!(session_id(&second), session, "the second turn resumes it");
    assert!(
        assistant_text(&second).contains(&session),
        "both answers name the same session: {second:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "neither turn exit is a session exit"
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

#[tokio::test]
async fn an_exit_before_the_result_is_a_synthetic_error_result() {
    common::require_node();
    let dir = common::temp_dir("opencode-crash");
    let (mut handle, mut rx) =
        spawn_opencode(launch(inline_bin("process.exit(3)"), &dir.0, MCP_URL, None));
    handle.send("ciao");

    let messages = collect_until(&mut rx, is_result, TIMEOUT).await;
    assert!(matches!(
        messages.last(),
        Some(EngineMsg::Event(EngineEvent::Result {
            is_error: true,
            subtype,
            ..
        })) if subtype == "error"
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "a crashed turn does not exit the session"
    );
    handle.stop().await;
}

#[tokio::test]
async fn missing_bin_lists_the_fallback_models() {
    let dir = common::temp_dir("opencode-models-missing");
    let name = if cfg!(windows) {
        "missing-opencode.exe"
    } else {
        "missing-opencode"
    };
    let bin = EngineBin {
        cmd: dir.0.join(name).display().to_string(),
        args: Vec::new(),
    };
    let models = list_opencode_models(&bin).await;
    assert_eq!(
        models,
        OPENCODE_MODELS_FALLBACK
            .iter()
            .map(|model| model.to_string())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn fake_bin_lists_its_models() {
    common::require_node();
    let models = list_opencode_models(&fake_bin("fake-opencode.mjs", &[])).await;
    assert_eq!(
        models,
        [
            "omniroute/auto/best-coding",
            "omniroute/auto/fast",
            "google/gemini-2.5-pro",
            "opencode/big-pickle",
        ]
        .iter()
        .map(|model| model.to_string())
        .collect::<Vec<_>>()
    );
}
