# PLAN 19: agy and opencode engines, model lists and Claude status from the Rust binary

## Goal

The Rust `proxi serve` runs teacher sessions with `agy` and `opencode` exactly like Node (same argv, scratch
files, environment, events, lifecycle) and serves `GET /api/agy/models`, `GET /api/opencode/models` and
`GET /api/claude/status` with Node's JSON. Node (`app/server/*`) is the source of truth and is never edited.

## Acceptance criteria

- AC1 A session with the fake agy (`app/test/fake-agy.mjs`) produces the same HTTP responses, SSE events and
  `state.json` as Node: `crates/proxi/tests/fixtures/golden/sessions-agy.json` (captured from Node) is replayed
  by `sessions_parity.rs::agy_sessions_match_the_node_golden` and matches value for value and in key order.
- AC2 Same for the fake opencode: `sessions-opencode.json`, replayed by
  `sessions_parity.rs::opencode_sessions_match_the_node_golden`.
- AC3 On Windows an opencode `.cmd` shim is launched by `spawn_opencode`, its turn ends with a successful
  `Result`, and an argument with `&`, spaces and parentheses reaches the engine intact
  (`crates/proxi/tests/opencode_shim.rs`; a no-op off Windows like `claude_shim.rs`).
- AC4 `GET /api/agy/models`, `GET /api/opencode/models` (fake list and static fallback, cached per server) and
  `GET /api/claude/status` (meets-minimum, update-required, unrecognised output, not found) return the same JSON
  text as Node, asserted by HTTP tests that run in both `PROXI_TEST_SERVER` modes.
- AC5 A missing engine executable gives the same result as Node: agy and claude sessions end `error` with
  `lastError` = `teacher process exited (null): spawn error: spawn <path> ENOENT`; an opencode session ends
  `error` with `lastError` = `Errore del docente (error)`; `/api/claude/status` answers
  `{"status":"not-found","minVersion":…,"binPath":<path>,"error":"spawn <path> ENOENT"}`; both model lists fall
  back to their static list. Asserted in both modes.
- AC6 `app/test/agy.test.ts`, `app/test/opencode.test.ts`, `app/test/claude-version.test.ts`,
  `app/test/api-engines.test.ts`, `app/test/api-engine-models.test.ts` use HTTP/SSE only, are in `RUST_PORTED`
  and pass with `npm run test:rust`; the agy parts needing `/answer`, `/interrupt`, `/mcp/{id}` live in the
  Node-only `app/test/agy-control.test.ts` (header names #18/#20).
- AC7 Every TS unit case of the ported modules (`agy.test.ts` parse/args, `opencode.test.ts` unit block,
  `claude-version.test.ts` parse/compare/probe, the agy/opencode cases of `engine.test.ts` and
  `engine-data-root-env.test.ts` that need no interrupt/jobs) exists as a Rust test and passes with
  `cargo test --locked`; the TS copies are removed.

## Design

Approach: port `agy.ts`, `opencode.ts` and the Claude probe as new modules behind the existing seam
(`EngineHandle` + `UnboundedReceiver<EngineMsg>`, `Exit` exactly once and last), reusing the process plumbing
of `claude.rs` instead of copying it, then wire them into `SessionManager::launch` and the router.

Key decisions:
- One launch type for all engines (Node has one `EngineLaunch`): `engine.rs` re-exports
  `ClaudeBin as EngineBin`, `ClaudeLaunch as EngineLaunch`, `Grant`, `McpServer` from `claude.rs`; agy/opencode
  ignore `effort`, `settings`, `on_limits`. No struct is moved (rule: change only needed lines). Source: D4.
- Shared process helpers are extracted from `spawn_claude` and called by all three adapters (lesson: never copy
  a helper): `engine_command`, `base_env`, `pump_lines`, `wait_child`, `exit_signal`, `dead`, a generic
  `LiveHandle`. Process trees use process-wrap 10.0.1 (`process_wrap::tokio::{CommandWrap, ChildWrapper,
  JobObject, KillOnDrop, CreationFlags, ProcessGroup}`; `CommandWrap::with_new(program, |cmd| …)`, `.wrap(..)`,
  `.spawn() -> io::Result<Box<dyn ChildWrapper>>`, `child.stdin()/stdout()/stderr() -> &mut Option<_>`,
  `child.id()`, `child.start_kill()`, `child.wait()`), exactly as `claude.rs` uses them today. Source: A10, Research A10.
- Missing executable text: `engine::spawn_error_message(program, &io::Error)` returns `spawn <program> ENOENT`
  for `ErrorKind::NotFound` (Node's message), else `err.to_string()`. Adapters emit
  `Stderr("spawn error: " + that)`; the probe puts it in `error`. Claude's spawn uses it too (AC5).
- `.cmd`/`.bat` opencode shims are passed as the program to `CommandWrap` (std ≥ 1.77.2 runs batch files with
  cmd-safe escaping and refuses unescapable arguments, CVE-2024-24576), not `cmd.exe /c` (Node's `shell: true`).
  Source: A10 + lesson L22 + `claude_shim.rs`. Consequence: an argument std cannot escape (e.g. one with a
  newline) fails the spawn with `InvalidInput` → `spawn error: …` + a synthetic error result, where Node's shell
  would silently truncate; the npm `.exe` lookup keeps the common install off this path.
- Bare `opencode` on Windows: `%APPDATA%\npm\node_modules\@opencode\cli\bin\opencode.exe` if it exists (Node);
  otherwise, instead of Node's `shell: true` PATH lookup, search each `PATH` dir for `opencode.exe`, then
  `opencode.bat`, then `opencode.cmd` (cmd's PATHEXT order); none → `opencode` as-is (NotFound).
- Interrupt and `max_turns` are not ported: `EngineHandle` has no `interrupt` (#20) and sessions launch with
  `max_turns: None` (jobs are #22). Without interrupt, agy is one process for the handle's life; `send` after
  exit/stop is dropped.
- opencode lifecycle (Node): one process per `send`; a newer turn makes the older process "not live" (its output
  and exit are ignored, it is not killed); a normal turn exit is never `EngineMsg::Exit`; a refused `--session`
  (started with a session id, no `Init`, exit code ≠ 0 or signal) is `ResumeFailed` + `Exit`; an exit before the
  turn's `Result` is a synthetic `Result { is_error: code != Some(0), subtype: "success"|"error" }`; `Exit` only
  after `stop()`.
- agy lifecycle (Node): eager single spawn in `scratch_dir`, stdin JSON lines; death = session `Exit`;
  died before `Init` while started with `--conversation` and not stopped = `ResumeFailed` first.
- Model rule copied from `engine.ts:19-30` into `engine::model_for`/`effort_for`; the agy engine note from
  `withEngineNote`. Source: CODEBASE "Model id rule".
- Model lists cached per server in `AppState` (`tokio::sync::OnceCell`), Node caches per process = per server
  in production; a per-server cache keeps in-process Rust tests independent.
- Claude probe (`claude.ts:158-240`) in a new `claude_status.rs`: `.js`/`.mjs` → `node <script> --version`
  (`child::node_on_path()`, else `"node"`), 5 s timeout, `Command failed: <cmd> <args>\n<stderr>` on non-zero
  exit (Node's `execFile` text; not byte-compared because Node's `<cmd>` is its own execPath).
- New `EngineEvent::ActionDenied { action }` → `Turns::on_event` notice `info` `Il motore ha bloccato un'azione
  (<action>).` (`turns.ts:250`).
- Goldens: `captureSessions` gains an engine parameter and writes `sessions-agy.json` / `sessions-opencode.json`;
  `normaliseSession` (TS) and its Rust port gain `conv-\d+`→`conv-<pid>`, `ses-\d+`→`ses-<pid>`,
  `:a\d+_\d+_`→`:a<pid>_<run>_` (fake pids and agy's `a<pid>_<run>_<step>` ids). Source: A2.
- CLI: flags `--agy-bin-arg`, `--opencode-bin-arg`; env `LEARNER_AGY_BIN`, `LEARNER_AGY_BIN_ARGS` only. Node's
  `main()` (`main.ts:627-636`) reads only `LEARNER_CLAUDE_BIN[_ARGS]`, `LEARNER_AGY_BIN[_ARGS]`, `LEARNER_ADD_DIRS`,
  `LEARNER_NO_OPEN` and hard-wires opencode to `"opencode"`, so Rust reads no opencode env: opencode comes only from
  `--opencode-bin`/`--opencode-bin-arg`, else `"opencode"`. Flags win; an empty env value is unset (Node `||`/`?:`).
  Key interface `LEARNER_AGY_BIN`.
- TS tests needing manager internals are rewritten over HTTP/SSE (A15); the agy answer/interrupt parts stay Node-only (D11).

Rejected: `cmd.exe /d /c` for `.cmd` (mangles `&`, `claude_shim.rs`); a `debug` route (A15 fallback, not needed);
moving `ClaudeBin`/`ClaudeLaunch` into `engine.rs` (churn); a static model cache (shared across Rust tests);
golden-comparing the agent files (system prompt holds the date, token and port: exact-text unit tests instead).

## W1

### T1
- **Files**: `crates/proxi/src/engine.rs`, `crates/proxi/src/claude.rs`, `crates/proxi/src/turns.rs`, `crates/proxi/src/lib.rs`, `crates/proxi/src/agy.rs`, `crates/proxi/src/opencode.rs`, `crates/proxi/src/claude_status.rs`, `crates/proxi/tests/common/engine.rs`, `crates/proxi/tests/common/mod.rs`
- **Do**: Engine seam and shared plumbing.
  1. `engine.rs`: add variant `ActionDenied { action: String }` to `EngineEvent` (after `ResumeFailed`). Add
     `pub use crate::claude::{ClaudeBin as EngineBin, ClaudeLaunch as EngineLaunch, Grant, McpServer};`.
     Add `pub fn model_for(settings: &LearnerSettings, provider: &str, role: &str, alias: Option<&str>) -> String`
     (`"agy"` → `settings.agy_models.<role>`, `"opencode"` → `settings.opencode_models.<role>`, else
     `alias` or `{teacher: teacher_model, subagent: subagent_model, map: map_model}[role]`; role is
     `"teacher"|"subagent"|"map"`, any other role behaves as `"teacher"`).
     `pub fn effort_for(settings, provider, role) -> Option<String>`: agy/opencode → `None`; else
     `settings.effort.<role>` unless it is `"default"` or empty.
     `pub fn with_engine_note(system_prompt: &str, provider: &str, grant: &[Grant]) -> String`: unchanged unless
     `provider == "agy"` and `grant` non-empty; then `format!("{system_prompt}\n\nOn this engine the read-only tools are named view_file (Read), list_dir (Glob), grep_search (Grep), search_web (WebSearch); there is no page fetch: cite search results with their URLs and say when a claim rests only on a snippet.")` (byte-copy of `engine.ts:13-16`).
     `pub fn spawn_error_message(program: &str, err: &std::io::Error) -> String`: `NotFound` → `format!("spawn {program} ENOENT")`, else `err.to_string()`.
  2. `claude.rs` (keep every unrelated line byte-identical): extract from `spawn_claude`, make `pub(crate)` and
     call them from `spawn_claude` so behaviour is unchanged:
     - `fn engine_command(program: &str, argv: &[String], cwd: &Path, set: &[(String, String)], remove: &[String], stdin: Stdio) -> process_wrap::tokio::CommandWrap` — the `with_new` closure (args, current_dir, stdin as given, stdout/stderr piped, env set/remove) plus the existing `#[cfg(windows)]` JobObject+KillOnDrop+CreationFlags(CREATE_NO_WINDOW) and `#[cfg(unix)]` ProcessGroup::leader+KillOnDrop wraps.
     - `fn base_env(data_root: Option<&Path>) -> Vec<(String, String)>` = `[("MCP_TOOL_TIMEOUT","86400000"), ("LEARNER_DATA_ROOT", root)]` (second only when `Some`); `child_env` starts from it.
     - `async fn pump_lines<R: tokio::io::AsyncRead + Unpin>(reader: Option<R>, mut on_line: impl FnMut(String))` — the 8 KiB read loop with `split_lines` and the final `flush_line`; both reader tasks of `spawn_claude` use it.
     - `async fn wait_child(child: &mut Box<dyn process_wrap::tokio::ChildWrapper>, kill_rx: &mut mpsc::UnboundedReceiver<()>) -> (Option<i32>, Option<String>)` — the `select!` wait/kill, `start_kill` + `wait` on kill, then `(status.code(), exit_signal(&status))`, `(None, None)` on wait error.
     - `exit_signal` → `pub(crate)`; `DeadHandle`, `dead(..)` → `pub(crate)`.
     - `LiveHandle` → `pub(crate)` with two new fields: `line: fn(&str) -> String` (formats the stdin line; claude passes a new `fn claude_user_line(text) -> String` holding today's `json!` body) and `stopped: Arc<AtomicBool>` (set `true` first thing in `stop()`); add `pub(crate) fn new(writer, kill, exit, pid, line, stopped) -> Self`.
     - The `spawn error` texts become `format!("spawn error: {}", crate::engine::spawn_error_message(&program, &err))` for the spawn failure (the `prepare` failure keeps `err.to_string()` via the same helper with `program`).
     Add tests in `mod tests`: `base_env_sets_the_timeout_and_the_data_root_only_when_given`; `a_missing_executable_reports_spawn_enoent` (bin cmd = a non-existent path under a temp dir, `spawn_claude`, collect until `Exit`: messages are `Stderr("spawn error: spawn <path> ENOENT")` then `Exit { code: None, signal: None }`). Existing claude tests must stay green; fix any exhaustive `match` on `EngineEvent` in the test helpers (e.g. `event_kind` → `"action-denied"`).
  3. `turns.rs` `on_event`: new arm before `Init`: `EngineEvent::ActionDenied { action } => host.notice("info", &format!("Il motore ha bloccato un'azione ({action})."))` (no resume update, no `host.event`). Test in `mod tests`: `action_denied_is_an_info_notice_and_not_forwarded` (notice text exact, host saw no event, status unchanged).
  4. `engine.rs` tests: `model_for_follows_the_engine_rule` (agy/opencode read their role model and ignore the alias; claude uses the alias, else the role model), `effort_for_is_none_for_agy_opencode_and_default`, `engine_note_only_for_agy_with_grants` (exact text; unchanged for claude, opencode, agy with empty grant), `spawn_error_message_maps_not_found_to_enoent`.
  5. `lib.rs`: add `pub mod agy;`, `pub mod claude_status;`, `pub mod opencode;` in alphabetical position. Create the three files with only a module doc comment: `//! Antigravity (agy) adapter (port of app/server/agy.ts).`, `//! OpenCode adapter (port of app/server/opencode.ts).`, `//! Claude CLI version probe (port of app/server/claude.ts:158-240).`
  6. `tests/common/engine.rs` (+ `pub mod engine;` in `tests/common/mod.rs`): shared helpers for the adapter tests of T4/T5:
     `pub fn fake_bin(script: &str, extra: &[&str]) -> EngineBin` (cmd `"node"`, args `[<repo>/app/test/<script>, extra…]`);
     `pub fn inline_bin(js: &str) -> EngineBin` (cmd `"node"`, args `["-e", js, "--"]`);
     `pub fn launch(bin: EngineBin, dir: &Path, mcp_url: &str, resume: Option<&str>) -> EngineLaunch` (repo_root = dir, data_root None, scratch_dir = dir/"scratch", model "m", effort None, system_prompt "Sei il docente.", grant `[Grant::Read]`, mcp `{ name: "app", url, token: "tok" }`, add_dirs empty, max_turns None, resume, settings default, on_limits no-op);
     `pub async fn collect_until(rx, pred: impl Fn(&EngineMsg) -> bool, timeout: Duration) -> Vec<EngineMsg>` (panics on timeout with the messages so far);
     `pub fn kinds(msgs) -> Vec<&'static str>` (event kinds in Node spelling: `init`, `text-start`, `text-delta`, `assistant`, `tool-result`, `result`, `resume-failed`, `action-denied`, …; stderr/exit skipped);
     `pub struct FakeMcp { pub url: String, pub tokens: Arc<Mutex<Vec<Option<String>>>> }` and `pub async fn start_fake_mcp() -> FakeMcp`: an axum server on `127.0.0.1:0` spawned on the current runtime, `POST /mcp/x`, records header `x-learner-token`; body without `id` → 202 empty; `initialize` → `{"jsonrpc":"2.0","id":…,"result":{"protocolVersion":<params.protocolVersion>,"capabilities":{"tools":{}},"serverInfo":{"name":"app","version":"1"}}}`; `tools/list` → one tool `{"name":"echo","inputSchema":{"type":"object"},"_meta":{"antigravity/alwaysLoad":true}}`; `tools/call` → `{"content":[{"type":"text","text":"eco " + compact JSON of params.arguments}]}` (mirror of `app/test/engine.test.ts:29-45`).
- **Done when**: `cargo build --locked` and `cargo test --locked --lib -- engine:: claude:: turns::` pass; `cargo test --locked --test claude_shim --test session_manager --test sessions_parity` still pass.

### T2
- **Files**: `app/test/harness.ts`, `app/test/fake-agy.mjs`
- **Do**: 1. `StartOptions` gains `agy?: { cmd: string; args?: string[] }` and `opencode?: { cmd: string; args?: string[] }` (doc: the engines to run instead of the real ones). `startServer` passes them to both modes. `startNode` passes `agyBin: agy?.cmd, agyBinArgs: agy?.args, opencodeBin: opencode?.cmd, opencodeBinArgs: opencode?.args` to `createServer` (fields exist at `app/server/main.ts:57-60`). `startBinary` pushes `--agy-bin <cmd>` and one `--agy-bin-arg <arg>` per arg, same for `--opencode-bin`/`--opencode-bin-arg`. Export `FAKE_AGY`, `FAKE_OPENCODE` paths and `fakeAgy(extra = [])`, `fakeOpencode(extra = [])` returning `{ cmd: process.execPath, args: [FAKE_x, ...extra] }` (like `fakeClaude`).
  2. `fake-agy.mjs`: right after the `fake-agy argv` stderr line add a `models` mode: `if (argv.includes("models")) { process.stdout.write("gemini-3.8-flash-high\tGemini 3.8 Flash (High)\nModelli disponibili\nclaude-sonnet-4-6\tClaude Sonnet 4.6\n"); process.exit(0); }` (a line without a tab is dropped by the parser). Update the header comment.
- **Done when**: `npm run typecheck:app` passes; `node --import tsx --test app/test/harness.test.ts app/test/agy.test.ts` passes; `node app/test/fake-agy.mjs models` prints the three lines.

### T3
- **Files**: `crates/proxi/src/server.rs`, `crates/proxi/tests/serve_cli.rs`
- **Do**: CLI and env plumbing only (no wiring into the manager yet).
  1. `EngineBins`: add `agy_args: Vec<String>` after `agy` and `opencode_args: Vec<String>` after `opencode`.
  2. `ServeArgs`: after `agy_bin` add `/// Extra argument before Antigravity's own (repeatable; tests: the fake script)` `#[arg(long = "agy-bin-arg", value_name = "ARG", allow_hyphen_values = true)] pub agy_bin_arg: Vec<String>`; after `opencode_bin` the same for `opencode_bin_arg` (`--opencode-bin-arg`, "OpenCode's own").
  3. New `#[derive(Debug, Default, Clone)] pub struct ServeEnv { pub port, pub claude_bin_args, pub agy_bin, pub agy_bin_args: Option<String> }` (no opencode fields: Node `main.ts:627-636` reads no opencode env) with `pub fn from_process() -> Self` reading `LEARNER_PORT`, `LEARNER_CLAUDE_BIN_ARGS`, `LEARNER_AGY_BIN`, `LEARNER_AGY_BIN_ARGS`. Change `into_options(self, env: &ServeEnv)`; `serve_cli` passes `&ServeEnv::from_process()`.
  4. Rename `parse_claude_args_env(raw)` to `parse_bin_args_env(name: &str, raw: &str)`; error `format!("{name} non valido: {raw}")` (claude message unchanged). Rule per engine: flags non-empty win; else env value, where `None` or `""` → empty vec, otherwise parse. Agy: `--agy-bin` wins, else non-empty `LEARNER_AGY_BIN` as `PathBuf`, else `None`; `--agy-bin-arg` non-empty wins, else `LEARNER_AGY_BIN_ARGS` per the rule. Opencode: flags only (`--opencode-bin` else `None`; `--opencode-bin-arg` else empty), never env. Claude bin unchanged (its env is read by `locate_claude_binary`).
  5. Update every existing test call to `into_options(&ServeEnv { port: …, claude_bin_args: …, ..Default::default() })`, the full-flags test (`--agy-bin-arg x --opencode-bin-arg y` → `agy_args: ["x"]`, `opencode_args: ["y"]`). New tests: `serve_args_agy_falls_back_to_the_env` (agy bin + JSON args from `ServeEnv`; with no opencode flags `opencode` is `None` and `opencode_args` empty), `serve_args_engine_flags_win_over_the_env` (`--agy-bin`/`--agy-bin-arg` beat the `ServeEnv` agy values), `serve_args_invalid_agy_args_env_is_an_error` (message starts with `LEARNER_AGY_BIN_ARGS non valido:` and contains the raw value), `serve_args_empty_env_values_are_unset` (agy `Some("")` bin and args → `None` / empty vec).
  6. `serve_cli.rs` `FLAGS`: add `"--agy-bin-arg"` after `"--agy-bin"` and `"--opencode-bin-arg"` after `"--opencode-bin"`.
- **Done when**: `cargo test --locked --lib server::` and `cargo test --locked --test serve_cli` pass.

## W2

### T4
- **Files**: `crates/proxi/src/agy.rs`, `crates/proxi/tests/agy_engine.rs`
- **Do**: Port `app/server/agy.ts` (doc-comment the origin). Use `js::parse` for JSON (Node `JSON.parse`), local helpers `num(v)` (finite number else 0) and `str(v)` (string else ""), `js::js_string` for init tools, `js::js_number_string` for step keys.
  - `pub const AGY_MODELS_FALLBACK: [&str; 6]` = the exact list of `agy.ts:10-17`; `const AGENT: &str = "ia-learner"`; agy tools per grant: Read → `view_file, list_dir, grep_search`, Web → `search_web`.
  - `pub fn map_tool_name(n: &str) -> String`: `mcp_app_` / `mcp_job_` prefix → `mcp__app__` / `mcp__job__`, else unchanged.
  - `pub struct AgyParseState { run: String, conversation_id: String, tools: HashMap<String,String>, text: IndexMap<String,String>, done: HashSet<String>, last: (f64,f64,f64), pub notes: Vec<String> }`; `AgyParseState::new()` sets `run = format!("{}_{}", std::process::id(), <static AtomicU64 counter, pre-incremented>)`.
  - `pub fn parse_agy_line(line: &str, st: &mut AgyParseState) -> Vec<EngineEvent>`: not JSON or not an object → `vec![]`; never panics. `init`: conversation id = `str(m.conversation_id)` || `str(m.init.conversation_id)` || previous; `Init { session_id, tools: init.tools array mapped with js_string else [], mcp_servers: [] }`. `step_update` (`u` = `m.step_update`, absent fields act as `{}`): step = js_number_string(num(u.step_index)); update conversation id; `agent_response`: message id `a{run}_{step}`; non-empty delta on a step not in `text` → insert "" and `TextStart{index:0.0}`; non-empty delta → append and `TextDelta`; `state` `DONE`/`ERROR` → finalise. Finalise(step): once per step (`done`), non-empty text → `Assistant { message_id, texts: [(0, text)], tool_uses: [] }`. `tool`: id `agy{run}_{step}` announced once as `Assistant { message_id: "m"+id, texts: [], tool_uses: [ToolUse{index:0,id,name: map_tool_name(str(u.tool_name) || str(info.name)), input: Some(info.parameters or {})}] }`; `DONE` → `ToolResult{is_error:false, text: str(info.output)}`; `ERROR` → `is_error:true`, text `str(info.error.message)` or `"errore"`. `error_message` → `notes.push("agy error: " + (str(u.text_delta) or compact JSON of u))`. `result` (`r` = `m.result`): finalise every step of `text` in insertion order; cur = (input_tokens, output_tokens + thinking_tokens, cache_read_tokens); status = `str(r.status)` or `"SUCCESS"`; one `ActionDenied { action: str(a.display_name) || str(a.action) }` per `r.denied_actions` entry; then `Result { is_error: status != "SUCCESS" || js::truthy(r.error), subtype: "success" or status lowercased, text: str(r.error) || str(r.response) || None, session_id: str(r.conversation_id) || conversation id, input/output/cache_read = max(0, cur - last), cache_creation 0, cost 0 }`; then `last = cur`, clear tools/text/done, update conversation id.
  - `pub fn agent_file(l: &EngineLaunch) -> String`: lines of `agentFile` (`agy.ts:151-171`) joined with `\n`: `---`, `name: ia-learner`, `description: ia-learner`, `mainAgent: true`, `subagent: false`, `inheritMcp: false`, `inheritCustomizations: false`, `excludeDefaultComponents: false`, `tools: <compact JSON array>`, `mcpServers:`, `  - name: <JSON string>`, `    url: <JSON string>`, `    headers: {"x-learner-token":<JSON string>}`, `---`, system prompt, `` (so it ends with `\n`).
  - `pub fn build_agy_args(l: &EngineLaunch, conversation_id: Option<&str>) -> Vec<String>`: `bin.args…`, `--agent ia-learner`, `--input-format stream-json`, `--output-format stream-json`, `--model <model>`, `--disable-slash-commands`, `--add-dir` for repo_root, then data_root when `data_root::resolve_path` differs from repo_root's, then each add_dir; `--conversation <id>` when non-empty.
  - `pub fn spawn_agy(l: EngineLaunch) -> (Box<dyn EngineHandle>, UnboundedReceiver<EngineMsg>)`: create `scratch/.agents/agents`, write `ia-learner.md` (error → `claude::dead(.., Some("spawn error: <err>"), None)`); argv with `l.resume`; `claude::engine_command(&l.bin.cmd, &argv, &l.scratch_dir, &claude::base_env(..), &[], Stdio::piped())` (do NOT strip `ANTHROPIC_API_KEY`); spawn error → `dead(.., Some("spawn error: " + spawn_error_message(&l.bin.cmd, &err)), None)`. Stdin writer task as in claude; stdout via `pump_lines` with one `AgyParseState`: `Init` sets `got_init`; forward every event; after each line send `Stderr` for each drained note. Stderr lines forwarded. Waiter: `wait_child`, await both readers, then if resume was non-empty && !got_init && !stopped → `Event(ResumeFailed)`; then `Exit{code, signal}` and the exit watch. Handle = `LiveHandle::new(.., line: agy_user_line, stopped)` where `agy_user_line(text)` = `json!({"event":"user","message":{"content":text}}).to_string()`.
  - `pub fn parse_agy_models(stdout: &str) -> Vec<String>` (split on `\n` with optional `\r`, keep lines containing `\t`, first tab field trimmed with `js::js_trim`; empty → fallback list) and `pub async fn list_agy_models(bin: &EngineBin) -> Vec<String>`: `tokio::process::Command::new(&bin.cmd)` args `bin.args + ["models"]`, stdin/stderr null, stdout piped, `kill_on_drop(true)`, Windows `creation_flags(CREATE_NO_WINDOW)`; spawn error → fallback; read stdout with `read_to_end` under `tokio::time::timeout(10 s)` (on timeout keep what was read and `start_kill`); parse.
  - Unit tests (`mod tests`, port of `agy.test.ts:21-68`): `maps_tool_names_back_to_the_claude_style`; `init_text_deltas_and_one_assistant_per_step` (kinds `init,text-start,text-delta,text-delta,assistant,text-start,text-delta,assistant`, session id `c1`, texts `Ciao`, `Solo in DONE`); `tool_steps_announce_once_and_result_with_the_same_id`; `result_usage_is_the_per_turn_diff_with_denied_actions` (`[10,6,2,0,false,"success"]`, `ActionDenied{"ReadUrlContent"}` before the second result `[20,12,4,true,"error","no quota"]`); `malformed_lines_give_no_events` (`not json`, `null`, `[1]`, `{"event":"step_update","step_update":"x"}` → none; `{"event":"result","result":null}` → one success `Result` with zeros); `error_message_steps_become_notes`; `args_have_no_p_and_conversation_on_resume` (exact vector incl. bin args first, data-root dedup); `agent_file_matches_the_node_layout` (exact string for a fixed launch); `agy_models_keep_tab_lines_first_field_else_fallback`.
  - Integration tests `tests/agy_engine.rs` (`mod common;` `common::require_node()`, helpers of `common::engine`, port of the agy cases of `engine.test.ts`): `a_turn_issues_a_token_streams_text_and_one_success_result` (fake-agy, send "ciao", until one `Result`; token = last `Init` session id non-empty; `text-delta` and an `Assistant` with text; then `stop`); `an_mcp_tool_call_reaches_the_endpoint_with_the_token` (`start_fake_mcp`, send `FAKE:tool:echo:` + base64 of `{"a":1}`; tool use name `mcp__app__echo`, `ToolResult` same id, text contains `eco {"a":1}`, every recorded token `tok`); `a_new_handle_with_the_token_resumes_it`; `a_refused_conversation_reports_resume_failed_then_exits_once` (`inline_bin("console.error('conversation not found');process.exit(1)")`, resume `sconosciuto`: kinds `["resume-failed"]`, last msg `Exit`); `stop_exits_exactly_once_and_kills_the_process` (pid gone after stop; no second `Exit` within 100 ms; no `ResumeFailed` after stopping a resumed handle); `the_scratch_agent_file_and_cwd` (file exists at `scratch/.agents/agents/ia-learner.md`); `data_root_reaches_the_child_env` (inline script printing `DATA_ROOT=` + `process.env.LEARNER_DATA_ROOT ?? 'UNSET'` to stderr: with `data_root` set → that path; without → the test process's own `LEARNER_DATA_ROOT` or `UNSET`); `a_missing_executable_reports_spawn_enoent` (Stderr `spawn error: spawn <path> ENOENT`, then `Exit{None,None}`); `missing_bin_lists_the_fallback_models` and `fake_bin_lists_its_models` (`["gemini-3.8-flash-high","claude-sonnet-4-6"]`).
- **Done when**: `cargo test --locked --lib agy::` and `cargo test --locked --test agy_engine` pass.

### T5
- **Files**: `crates/proxi/src/opencode.rs`, `crates/proxi/tests/opencode_engine.rs`, `crates/proxi/tests/opencode_shim.rs`
- **Do**: Port `app/server/opencode.ts` (same helper conventions as T4).
  - `pub const OPENCODE_MODELS_FALLBACK: [&str; 6]` = exact list of `opencode.ts:10-17`.
  - `pub fn map_opencode_tool_name(name)`: starts with `mcp__` → unchanged; else `^(app|job)[._]` → `mcp__$1__`.
  - `pub struct OpencodeParseState { run, pub session_id: String, has_init: bool, seen_text: HashSet<String>, message_text: HashMap<String,String>, tools: HashMap<String,String>, tokens: (f64,f64,f64), cost_usd: f64, pub notes: Vec<String> }`, `new()` with `run` as in T4 (own counter).
  - `pub fn parse_opencode_line(line, st) -> Vec<EngineEvent>` (`opencode.ts:61-153`, never panics): not JSON / not object → []; `sid = str(m.sessionID) || str(m.sessionId)` updates `session_id` when non-empty; ensure-init pushes `Init { session_id, tools: [], mcp_servers: [] }` once. `step_start` → ensure-init. `text`: mid = `str(part.messageID)` or `msg_{run}`; ensure-init; first time per mid `TextStart{index:0}`; non-empty text → append, `TextDelta`, `Assistant{texts:[(0, accumulated)]}`. `tool_use`: raw = `str(part.state.metadata.metadata.toolCalls[0].tool)` || `str(part.tool)`; id = `str(part.id)` || `str(part.partID)` || `opencode_{run}_call`; ensure-init; announce once `Assistant{message_id:"m"+id, tool_uses:[{index 0, id, name: mapped, input: Some(part.state.input or {})}]}`; status `completed`/`error` → `ToolResult { is_error: status=="error", text: output if string else compact JSON of (output or "") }`. `step_finish`: accumulate input, output+reasoning, cache.read, cost; `part.reason == "stop"` → `Result{is_error:false, subtype:"success", text:None, session_id, cumulative tokens, cache_creation 0, cost}`. `error` → note `"opencode error: " + (str(m.error) or compact JSON of m)`.
  - `pub fn agent_file(system_prompt) -> String` = `"---\ndescription: ia-learner\nmode: primary\n---\n\n{prompt}\n"`; `pub fn mcp_config(l) -> String` = `serde_json::to_string_pretty` of a fresh map in order `$schema: "https://opencode.ai/config.json"`, `mcp.servers.<name> = {type:"remote", url, headers:{"x-learner-token": token}}` (JSON.stringify(…, null, 2) layout, no trailing newline).
  - `pub fn build_opencode_args(l, text, session_id: Option<&str>)`: `bin.args…, run, --format, json, --auto, --agent, ia-learner, --model, <model>, [--session, id], text`.
  - `pub fn resolve_opencode_cmd(cmd: &str, windows: bool, app_data: Option<&Path>, path: Option<&OsStr>) -> String` (Design rules; `.exe`/`.cmd`/`.bat` suffix checks on the lowercased name), plus `resolve_from_process(cmd)` using `cfg!(windows)`, `APPDATA`, `PATH`.
  - `pub fn spawn_opencode(l: EngineLaunch) -> (Box<dyn EngineHandle>, UnboundedReceiver<EngineMsg>)`: write `scratch/.opencode/agents/ia-learner.md` and `scratch/opencode.json` (error → `claude::dead`). Return an `OpencodeHandle` holding `Arc<Mutex<Inner>>` { session_id: Option<String> (= resume), stopped, exit_reported, in_flight, generation: u64, current: Option<Turn{ gen, pid, kill: UnboundedSender<()>, done: watch::Receiver<bool> }> }, the launch, and `tx`. `send(text)`: if stopped or exit_reported → return; `in_flight = true`; spawn a turn process: argv `build_opencode_args(l, text, session_id)`, program `resolve_from_process(&l.bin.cmd)`, `engine_command(program, argv, scratch, base_env, &[], Stdio::null())`; new generation becomes `current` (an older process is left running, its output/exit ignored). Spawn error → `Stderr("spawn error: " + spawn_error_message(program, err))` then the exit logic with `(None, None)`. Per process: parse state seeded with `session_id`; stdout lines only while live: `Init` → resuming=false, session_id = event id or kept; `Result` → in_flight=false, turn_finished=true, session_id update; forward; notes → `Stderr`. Stderr lines forwarded while live. Exit logic (after both readers end; only if still live; then `current = None`): resuming && (code != Some(0) || signal.is_some()) → `ResumeFailed` + report exit; else if in_flight && !turn_finished → in_flight=false and `Result{ is_error: code != Some(0), subtype: if code == Some(0) {"success"} else {"error"}, text: None, session_id: session_id or "", zeros }`; then if stopped → report exit. Report exit = send `Exit{code, signal}` once (`exit_reported`) and flip an `exit` watch. `stop()`: stopped=true; current → send kill and await its exit report ≤ 5 s; no current → report `Exit{None,None}` now; a second call returns at once. `pid()` = current turn's pid.
  - `pub fn parse_opencode_models(stdout)` (lines trimmed with `js::js_trim`, non-empty, not starting with `error`/`warn`/`info`; empty → fallback) and `pub async fn list_opencode_models(bin)` (as T4's, program via `resolve_from_process`).
  - Unit tests (port of `opencode.test.ts:13-151`): `map_opencode_tool_name_handles_prefixes` (6 cases), `build_args_fresh_and_resume` (exact vectors), `resolve_cmd_rules` (non-Windows unchanged; `.exe` unchanged; `custom.cmd`/`X.BAT` unchanged on Windows; `opencode` → npm exe when it exists in a temp `APPDATA`; else first PATH hit `opencode.exe` > `.bat` > `.cmd`; else `opencode`), `parse_step_start_text_tool_use_step_finish` (the four-step case with exact values: init `ses_abc123`; 3 events; tool name `mcp__app__ask`, text `risultato`; result 150/60/20/0.005), `malformed_lines_give_no_events` (`not json`, `null`, `{"type":"text","part":null}` → init + text-start only), `error_lines_become_notes`, `agent_file_and_mcp_config_match_node` (exact strings), `opencode_models_filter_and_fallback`.
  - Integration `tests/opencode_engine.rs` (node required): the same seven behaviours as T4's file with fake-opencode (`fake_bin("fake-opencode.mjs", &[])`), plus `a_normal_turn_exit_is_not_a_session_exit` (two sends, two success results, no `Exit` until `stop`, then exactly one `Exit`), `an_exit_before_the_result_is_a_synthetic_error_result` (`inline_bin("process.exit(3)")`: `Result{is_error:true, subtype:"error"}`, no `Exit`), refused session via `inline_bin("console.error('session not found');process.exit(1)")` + resume → kinds `["resume-failed"]` then `Exit`, missing executable → `Stderr("spawn error: spawn <path> ENOENT")` + synthetic error `Result`, no `Exit` until stop, models fake list `["omniroute/auto/best-coding","omniroute/auto/fast","google/gemini-2.5-pro","opencode/big-pickle"]` and fallback.
  - `tests/opencode_shim.rs` (AC3, header like `claude_shim.rs`; `if !cfg!(windows) { return; }`; `common::require_node()`): write `<tmp>\opencode.cmd` = `@echo off\r\nnode "<repo>\app\test\fake-opencode.mjs" %*\r\n`; launch with that bin, send `ciao & R&D (prova)`; collect until a `Result`: success; a `Stderr` line starts with `fake-opencode argv:` and its JSON array's last element equals the sent text; `stop` → one `Exit`.
- **Done when**: `cargo test --locked --lib opencode::` and `cargo test --locked --test opencode_engine --test opencode_shim` pass.

### T6
- **Files**: `crates/proxi/src/claude_status.rs`, `crates/proxi/src/protocol.rs`, `crates/proxi/tests/claude_status.rs`
- **Do**: 1. `protocol.rs`: `#[derive(Serialize, Debug, Clone, PartialEq)] #[serde(rename_all = "camelCase")] pub struct ClaudeStatusResponse { pub status: String, #[serde(skip_serializing_if = "Option::is_none")] pub version: Option<String>, pub min_version: String, #[serde(skip_serializing_if = "Option::is_none")] pub bin_path: Option<String>, #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<String> }` (Node `protocol.ts:566-572` order). Test `claude_status_serialises_in_node_order` (exact strings for meets-minimum and not-found shapes).
  2. `claude_status.rs`: `pub const CLAUDE_MIN_VERSION: &str = "2.1.259"`; `pub struct Semver { pub major: u64, pub minor: u64, pub patch: u64, pub raw: String }`; `pub fn parse_claude_version(output) -> Option<Semver>` (`js::js_trim`, then leading `digits.digits.digits`; anything after is ignored; `raw` = `"M.m.p"` from the matched digits as written); `pub fn compare_semver(a, b) -> std::cmp::Ordering`; `pub async fn probe_claude_cli(bin_path: &str, timeout: Duration) -> ClaudeStatusResponse` per `claude.ts:197-240`: empty bin → `{status:"not-found", min_version}`; `"claude"` → `claude::locate_from_process()` if found; `.js`/`.mjs` → program `child::node_on_path()` display or `"node"`, args `[bin, "--version"]`, else program bin, args `["--version"]`; `tokio::process::Command` stdin null, stdout/stderr piped, `kill_on_drop(true)`, Windows `CREATE_NO_WINDOW`; spawn `NotFound` → `{not-found, min_version, bin_path, error: spawn_error_message(program, err)}`; other spawn error → `version-unchecked` with `err.to_string()`; `timeout(wait_with_output)` elapsed or non-zero exit → `version-unchecked` with `format!("Command failed: {program} {args joined by ' '}\n{stderr}")`; output unparsable → `version-unchecked`, error `Output non riconosciuto: {trimmed stdout}`; ≥ min → `meets-minimum` with `version`, `bin_path`; else `update-required`. Unit tests (port of `claude-version.test.ts:1-66`): parse (`2.1.314 (Claude Code)`, leading/trailing whitespace, `v2.1.0` → None, `garbage` → None), compare (equal, higher/lower in each part).
  3. `tests/claude_status.rs` (`common::require_node()`, `.mjs` mocks in a temp dir, port of `claude-version.test.ts:68-120`): `meets_minimum` (`2.1.314 (Claude Code)` → status, version, minVersion, binPath = script path), `update_required` (`2.0.100`), `failing_process_is_version_unchecked` (exit 1 → error starts with `Command failed: `), `unrecognised_output` (`hello` → error `Output non riconosciuto: hello`), `missing_binary_is_not_found` (non-existent `<tmp>/missing-claude.exe` → JSON equals `{"status":"not-found","minVersion":"2.1.259","binPath":<p>,"error":"spawn <p> ENOENT"}`).
- **Done when**: `cargo test --locked --lib -- claude_status:: protocol::` and `cargo test --locked --test claude_status` pass.

### T7
- **Files**: `app/test/capture-golden.ts`, `app/test/capture-golden.test.ts`, `crates/proxi/tests/fixtures/golden/sessions-agy.json`, `crates/proxi/tests/fixtures/golden/sessions-opencode.json`
- **Do**: 1. `normaliseSession`: after the existing replaces add `.replace(/conv-\d+/g, "conv-<pid>").replace(/ses-\d+/g, "ses-<pid>").replace(/:a\d+_\d+_/g, ":a<pid>_<run>_")`.
  2. `captureSessions(mode = "node", engine: "claude" | "agy" | "opencode" = "claude")`: `route` = `"sessions"` for claude else `` `sessions-${engine}` ``; for agy/opencode `seed` = `{ ".learner/settings.json": { provider: engine } }`, written under the repo as `JSON.stringify(v, null, 2) + "\n"` before the server starts (same as `captureScript`); start with `claude: fakeClaude()` plus `agy: fakeAgy()` / `opencode: fakeOpencode()` (T2's harness helpers). Same create body, flow and cases. `captureGolden` writes `sessions.json`, `sessions-agy.json`, `sessions-opencode.json`.
  3. `capture-golden.test.ts`: add `"sessions-agy", "sessions-opencode"` to `ROUTES`; add test `agy and opencode session goldens pin the engine, its token and the turn`: for each, `stateFile.provider` = engine, `stateFile.resume` = `conv-<pid>` / `ses-<pid>`, keys `["state","provider","resume","canRetry"]`, the snapshot has a teacher item `Ciao, sono il docente (conv-<pid>).` / `(ses-<pid>).`, the message case's live events start with `item:learner` and end with `status` idle, no ISO and no UUID strings (same walk as the sessions test).
  4. Run `node --import tsx app/test/capture-golden.ts` and commit only the two new fixtures. `git status --porcelain crates/proxi/tests/fixtures/golden` must list only them; if a tracked fixture changed, stop and report it (do not commit it).
- **Done when**: both fixtures exist; `git diff --exit-code crates/proxi/tests/fixtures/golden` passes (no tracked fixture changed); `node --import tsx --test app/test/capture-golden.test.ts` and `npm run typecheck:app` pass.

## W3

### T8
- **Files**: `crates/proxi/src/sessions.rs`, `crates/proxi/src/server.rs`, `crates/proxi/tests/session_manager.rs`
- **Do**: 1. `ManagerOptions`: add `pub agy: ClaudeBin` and `pub opencode: ClaudeBin` after `claude`. In `launch()`: delete the `provider != "claude"` rejection; `let settings = settings::read_settings(&opts.repo_root); let alias = config.get("model").and_then(Value::as_str); let model = engine::model_for(&settings, &provider, "teacher", alias); let effort = engine::effort_for(&settings, &provider, "teacher"); let grant = vec![Grant::Read, Grant::Web]; let system_prompt = engine::with_engine_note(&prompt, &provider, &grant);` bin by provider (`agy`/`opencode`/else claude); build the launch as today with those values; dispatch `"agy" => agy::spawn_agy(launch)`, `"opencode" => opencode::spawn_opencode(launch)`, `_ => claude::spawn_claude(launch)` (Node `sessions.ts:703-731`).
  2. `server.rs` `start()`: build `agy = ClaudeBin { cmd: opts.bins.agy display or "agy", args: opts.bins.agy_args }`, `opencode` likewise (`"opencode"`), pass to `ManagerOptions`. `AppState` gains `claude_bin: String` (= the `ClaudeBin.cmd` computed today), `agy_bin: ClaudeBin`, `opencode_bin: ClaudeBin`, `agy_models: tokio::sync::OnceCell<Vec<String>>`, `opencode_models: tokio::sync::OnceCell<Vec<String>>`. Routes next to the others: `.route("/api/agy/models", get(get_agy_models).fallback(not_found))`, `.route("/api/opencode/models", get(get_opencode_models).fallback(not_found))`, `.route("/api/claude/status", get(get_claude_status).fallback(not_found))`; handlers return `Json(state.agy_models.get_or_init(|| agy::list_agy_models(&state.agy_bin)).await.clone())`, the opencode twin, and `Json(claude_status::probe_claude_cli(&state.claude_bin, Duration::from_secs(5)).await)`.
  3. `session_manager.rs`: `manager_for` fills `agy`/`opencode` with node + `app/test/fake-agy.mjs` / `fake-opencode.mjs`. Replace `unsupported_provider_fails_the_create_and_marks_the_session` with: `agy_provider_runs_a_session` (settings `{"provider":"agy"}`, create planned teach → idle; a teacher item contains `(conv-`; `state.json` provider `agy` and resume = that conv); `opencode_provider_runs_two_turns_on_one_session` (create → idle, message → idle; both teacher answers name the same `ses-` id; status never `error`); `a_missing_agy_executable_fails_like_node` (agy cmd = `<tmp>/missing-agy` (+`.exe` on Windows) → status `error`, `lastError` = `teacher process exited (null): spawn error: spawn <path> ENOENT`); `agy_sessions_keep_their_engine_after_a_settings_switch` (after the first turn write settings `{"provider":"claude"}`, new manager on the same folders, message → answered by agy: text names the same conv).
- **Done when**: `cargo build --locked`, `cargo test --locked --test session_manager --test sessions_api --test sessions_parity --test server_api` pass.

### T9
- **Files**: `app/test/agy.test.ts`, `app/test/agy-control.test.ts`, `app/test/opencode.test.ts`
- **Do**: 1. `agy.test.ts`: remove the `parseAgyLine` describe (ported in T4) and the `createServer`/`srv.manager` use; rewrite the integration over HTTP/SSE with `startServer({ repoRoot, dataRoot, agy: fakeAgy(), claude: fakeClaude() })` (pattern of `app/test/api-sessions.test.ts`: seed data folder, `waitFor` polling `GET /api/sessions/:id`), repo seeded with `.learner/settings.json` `{"provider":"agy"}`. Cases: opening turn → idle, first teacher text names `conv-N`; `<repo>/.learner/sessions/<id>/.agents/agents/ia-learner.md` exists; `state.json` resume = conv and provider `agy`; `usage.outputTokens > 0`; a second message is answered on the same conv; POST `/api/settings` `{provider:"claude"}`, close, start a new server on the same folders, message → answered with the same conv.
  2. `agy-control.test.ts` (new, Node-only; header: `needs /answer, /interrupt and /mcp/{id}: moves to test:rust with #18/#20`): the old in-process test trimmed to the tool card + `/answer` (`alwaysLoad=true`, `"other":"si"`) and the `FAKE:slow` + `/interrupt` part (notice `interrotto`), keeping `createServer` and `srv.manager`.
  3. `opencode.test.ts`: delete the unit describe (ported in T5) and write the HTTP/SSE twin of the agy test with `opencode: fakeOpencode()` and provider `opencode`: opening turn → idle with `ses-N`; `.opencode/agents/ia-learner.md` and `opencode.json` exist in the scratch dir, `opencode.json` parses with `mcp.servers.app.url` ending in `/mcp/<id>`; a second message answered with the same `ses-N`; `state.json` provider `opencode`, resume `ses-N`.
- **Done when**: `npm run typecheck:app` and `node --import tsx --test app/test/agy.test.ts app/test/agy-control.test.ts app/test/opencode.test.ts` pass (Node mode).

### T10
- **Files**: `app/test/claude-version.test.ts`, `app/test/api-engines.test.ts`, `app/test/api-engine-models.test.ts`
- **Do**: HTTP-only files (both modes), each server via `startServer` and closed in `after`/`finally`; compare JSON **text** (`await res.text()` vs `JSON.stringify(expected)`) so key order is checked.
  1. `claude-version.test.ts`: delete the parse/compare/probe unit describes (ported in T6); keep `CLAUDE_MIN_VERSION` import. `GET /api/claude/status` cases, each with `claude: { cmd: <tmp>/mock-claude.mjs }`: meets-minimum (`2.1.300 (Claude Code)` → `{status,version:"2.1.300",minVersion,binPath}`), update-required (`2.0.100`), unrecognised (`console.log("hello")` → `{status:"version-unchecked",minVersion,binPath,error:"Output non riconosciuto: hello"}`), not found (`claude: { cmd: <tmp>/missing-claude.exe }` → `{status:"not-found",minVersion,binPath,error:"spawn <path> ENOENT"}`).
  2. `api-engines.test.ts`: one server with `agy` = the temp models script below, `opencode: { cmd: <missing> }` (`<tmp>/missing-opencode` + `.exe` on win32), `claude: { cmd: <tmp>/missing-claude.exe }`, data folder seeded like `api-sessions.test.ts`: `GET /api/agy/models` cache check: `agy` = `{ cmd: process.execPath, args: [<tmp>/agy-models.mjs] }`, a temp script written before the server starts with `if (process.argv.includes("models")) process.stdout.write("model-a\tA\nmodel-b\tB\n");`; first GET text = `["model-a","model-b"]`; then rewrite the script to print `"model-z\tZ\n"`, GET again and assert the text is still `["model-a","model-b"]` (cached per server; uncached it would be `["model-z"]`); `GET /api/opencode/models` = `OPENCODE_MODELS_FALLBACK` (import from `../server/opencode.ts`). AC5 sessions on fresh repos (settings provider seeded): agy with `agy: { cmd: <missing-agy> }` → `error`, `lastError` = `` `teacher process exited (null): spawn error: spawn ${missing} ENOENT` ``; opencode with the missing bin → `error`, `lastError` = `Errore del docente (error)`; claude with the missing claude → `error`, `lastError` = the ENOENT text.
  3. `api-engine-models.test.ts` (separate file: Node caches model lists per process): one server with `agy: { cmd: <missing-agy> }` → `AGY_MODELS_FALLBACK` (import from `../server/agy.ts`); a second server with `agy: fakeAgy()` → `["gemini-3.8-flash-high","claude-sonnet-4-6"]`; `opencode: fakeOpencode()` → `["omniroute/auto/best-coding","omniroute/auto/fast","google/gemini-2.5-pro","opencode/big-pickle"]`.
- **Done when**: `npm run typecheck:app` and `node --import tsx --test app/test/claude-version.test.ts app/test/api-engines.test.ts app/test/api-engine-models.test.ts` pass (Node mode).

### T11
- **Files**: `app/test/engine.test.ts`, `app/test/engine-data-root-env.test.ts`
- **Do**: 1. `engine.test.ts`: inside the provider loop run every case for `claude` unchanged; for `agy`/`opencode` keep only `interrupt ends the turn…` (no Rust home until #20). Add a header line: agy/opencode turn, MCP, resume, refused resume and stop are Rust tests (`crates/proxi/tests/agy_engine.rs`, `opencode_engine.rs`); their relaunch-after-refusal is engine-agnostic (`turns.rs` tests). Keep both JobManager describes (#22).
  2. `engine-data-root-env.test.ts`: remove the agy and opencode entries of `ENGINES` and their imports (ported in T4/T5 `data_root_reaches_the_child_env`); claude stays.
- **Done when**: `npm run typecheck:app` and `node --import tsx --test app/test/engine.test.ts app/test/engine-data-root-env.test.ts` pass.

## W4

### T12
- **Files**: `crates/proxi/tests/sessions_parity.rs`
- **Do**: Extract the body of `sessions_match_the_node_golden` into `async fn replay(golden: &str, configure: impl FnOnce(&mut ServerOptions))` (`golden_file(name)` takes the file name); the claude test calls it with today's bins. Add `agy_sessions_match_the_node_golden` (`sessions-agy.json`; claude bins as today plus `options.bins.agy = Some("node")`, `agy_args = [<repo>/app/test/fake-agy.mjs]`) and `opencode_sessions_match_the_node_golden` (`fake-opencode.mjs`). The golden `seed` writes `.learner/settings.json` through the existing `write_seed`. Port the three new normaliser rules into `normalise_text` (after the `toolu_fake_` rule, same order as TS): `conv-` + digits → `conv-<pid>`, `ses-` + digits → `ses-<pid>`, `:a` + digits + `_` + digits + `_` → `:a<pid>_<run>_` (hand-rolled scans like `replace_pid_counter`, earliest match first, retry at `start + 1`); add `#[test] fn engine_id_normalisers_match_the_js_regex` (`conv-123`→`conv-<pid>`, `conv-x` unchanged, `t:a12_3_1:0`→`t:a<pid>_<run>_1:0`, `t:a12_x` unchanged, `ses-9.`→`ses-<pid>.`).
- **Done when**: `cargo test --locked --test sessions_parity` passes (all three goldens).

### T13
- **Files**: `scripts/test-rust.mjs`
- **Do**: Append to `RUST_PORTED`: `app/test/agy.test.ts`, `app/test/opencode.test.ts`, `app/test/claude-version.test.ts`, `app/test/api-engines.test.ts`, `app/test/api-engine-models.test.ts`. Then build and run the binary suite; fix nothing in other tasks' files — report failures.
- **Done when**: `cargo build --release --locked` then `npm run test:rust` pass.

## Verification

Full suite (CI order): `npm run typecheck && npm run typecheck:app`, `npm test`, `npm run app:build`, `npm run test:app`,
`cargo build --release --locked`, `npm run test:rust`, `cargo test --locked`.

Tests that must exist at the end: `crates/proxi/tests/agy_engine.rs`, `opencode_engine.rs`, `opencode_shim.rs`,
`claude_status.rs`; new tests in `engine.rs`, `claude.rs`, `turns.rs`, `agy.rs`, `opencode.rs`,
`claude_status.rs`, `protocol.rs`, `server.rs`, `session_manager.rs`, `sessions_parity.rs`; fixtures
`sessions-agy.json`, `sessions-opencode.json`; TS `agy.test.ts`, `agy-control.test.ts`, `opencode.test.ts`,
`claude-version.test.ts`, `api-engines.test.ts`, `api-engine-models.test.ts`.

- AC1: `sessions_parity.rs::agy_sessions_match_the_node_golden` (golden from Node, T7) + `capture-golden.test.ts` fresh-capture match; also `agy.test.ts` in both modes.
- AC2: `sessions_parity.rs::opencode_sessions_match_the_node_golden` + capture test; `opencode.test.ts` both modes.
- AC3: `opencode_shim.rs::a_cmd_shim_is_the_program_and_keeps_its_arguments_intact` (Windows; real run on this Windows machine during T5).
- AC4: `api-engines.test.ts`, `api-engine-models.test.ts`, `claude-version.test.ts` in `npm run test:app` (Node) and `npm run test:rust` (binary); Rust `agy::`/`opencode::` model parser tests, `tests/claude_status.rs`.
- AC5: `api-engines.test.ts` session cases, `claude-version.test.ts` not-found case (both modes); `session_manager.rs::a_missing_agy_executable_fails_like_node`; `a_missing_executable_reports_spawn_enoent` in `claude.rs`, `agy_engine.rs`, `opencode_engine.rs`.
- AC6: `npm run test:rust` with the five files in `RUST_PORTED` (T13); `agy-control.test.ts` runs in `npm run test:app`.
- AC7: `cargo test --locked`: `agy::tests` (= `agy.test.ts:21-68`), `opencode::tests` (= `opencode.test.ts:13-151`), `claude_status::tests` + `tests/claude_status.rs` (= `claude-version.test.ts:1-120`), `agy_engine.rs`/`opencode_engine.rs` (= `engine.test.ts` turn/MCP/resume/refused/stop and `engine-data-root-env.test.ts` agy/opencode cases); the TS copies are gone (T9-T11).

## Out of scope

- Session-control routes `/answer`, `/interrupt`, attachments, retry and `/mcp/{id}` (#18, #20) and `EngineHandle::interrupt`; the TS cases needing them stay Node-only (`agy-control.test.ts`, the interrupt case of `engine.test.ts`).
- `max_turns` / `error_max_turns` and JobManager pid tracking for agy/opencode (#22).
- The pre-existing D10 debt in `app/test/claude.test.ts`, `fix2.test.ts`, `data-root-threading.test.ts` and the claude cases of `engine.test.ts`/`engine-data-root-env.test.ts`.
- Any change to `app/server/*` (Node stays the golden source until the cutover).
