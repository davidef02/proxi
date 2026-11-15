# PLAN 20: session control in Rust (attachments, retry, interrupt, close, delete, restart recovery)

## Goal
The Rust binary serves `POST /api/sessions/{id}/attachments`, `POST /api/sessions/{id}/retry`, `POST /api/sessions/{id}/interrupt`, `POST /api/sessions/{id}/close` and `DELETE /api/sessions/{id}` with Node's HTTP shapes, on-disk layout and turn events. After a restart, a session that was mid-turn comes back as a retryable error. The HTTP tests for these routes run against the spawned binary (`npm run test:rust`), and the TS unit tests of the turn module that were ported now live as Rust tests.

## Acceptance criteria
- AC1 An uploaded attachment is stored and reaches the fake engine as in Node. `POST /api/sessions/{id}/attachments` (multipart, `files` fields then `file` fields) returns `[{id,name,path,mimeType,size}]` in that key order. `path` is `.learner/sessions/<id>/attachments/<uuid[0..8]>_<safeBase>` with `/` separators, and the bytes on disk are identical. A message carrying those attachments gives a learner item with `attachments`, and the engine receives the user line `[Allegati forniti dallo studente:\n- <name> (percorso: <path>)]\n\n<text>`. (auto: test)
- AC2 `POST /api/sessions/{id}/retry` reruns the last failed turn. The SSE stream sends a `snapshot` first, then `status:running` … `status:idle`, with no new learner item. The error notice is gone, and the same test passes against Node and the binary. (auto: test)
- AC3 `POST /api/sessions/{id}/interrupt` stops the running turn. The engine receives exactly `{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}`, the session reaches `idle` with one `Turno interrotto.` info notice and no error notice, while the Claude process stays alive (`live_pids().len() == 1`). No engine process remains once the session is closed or deleted (`live_pids()` empty, pid gone). (auto: test)
- AC4 After the binary restarts, the saved sessions are listed. A session that was `running` with a saved `lastTurn` is `status:"error"`, `retryable:true`, `lastError:"Sessione interrotta prima del completamento del turno"`, and `retry` brings it to `idle`. (auto: test)
- AC5 `DELETE /api/sessions/{id}` returns `{"ok":true}`, removes `.learner/sessions/<id>/` (attachments included), and no later flush recreates it. `GET /api/sessions/{id}` is then 404 and the session is gone from `GET /api/sessions`. (auto: api)
- AC6 `app/test/attachments.test.ts` and `app/test/sessions.test.ts` are HTTP/SSE-only, listed in `RUST_PORTED`, and pass with `npm run test:rust` against the release binary. (auto: cli `npm run test:rust`)
- AC7 The `turns.test.ts` cases for interrupt/watchdog, attachments and retry exist as Rust unit tests in `crates/proxi/src/turns.rs`, pass under `cargo test --locked`, and are removed from `app/test/turns.test.ts` (cases with no Rust home stay). (auto: cli `cargo test`)

## Design
Approach: port Node's `Turns.retry`/`interrupt`/watchdog/attachment block into the synchronous Rust `Turns` (no spawning inside, same as today), add the manager methods in `sessions.rs`, then the axum routes. The TS tests get rewritten to HTTP/SSE so the same files gate both backends.

Key decisions:
- **Retry reuses the begin/finish split** (CODEBASE Constraints: "Rust `Turns` does not own spawning"). `Turns::begin_retry` does the checks and returns `needs_launch`. `finish_send` becomes idempotent about learner bubbles (it pushes them only when `pushed_learner` is false), so `launch_turn` serves both message and retry unchanged. Rejected: a separate `finish_retry` plus a second `launch_turn` variant (duplicate code).
- **Interrupt never touches `stopped`** (Constraints: `stop()` sets `stopped`). New fields are `interrupting`, `watchdog_seq` and `watchdog_armed: Option<u64>`. `Turns::interrupt` returns `Some(ticket)` when a watchdog must be armed. The manager runs `spawn_delayed(turns::WATCHDOG, …)` and then `Turns::watchdog_fired(host, ticket)`. Disarming (Node's `clearTimeout`) sets `watchdog_armed = None`, so a stale ticket is a no-op. Unit tests call `watchdog_fired` directly, so the test path needs no timer. Rejected: a tokio timer inside `Turns` (it lives under a std mutex; there is no `test-util` feature to pause time).
- **`EngineHandle::interrupt(&mut self)` is a required trait method** (Constraints: "write the route against `EngineHandle::interrupt` so future adapters inherit"). Claude writes `{"type":"control_request","request_id":"int-N","request":{"subtype":"interrupt"}}` with a per-handle counter, as `app/server/claude.ts:329` does. `DeadHandle` does nothing. Rejected: a default no-op method (a kill-based adapter could silently forget it).
- **D-AC3** (AC3 as amended; Desired "in-band for Claude"): Claude's interrupt is in-band and keeps the process alive, as Node does (`claude.ts:329`, `turns.ts:161-178`). After the interrupt the process stays (`live_pids().len() == 1`, pinned by a test); "no engine process remains" is verified after `close` and after `delete` (empty `live_pids()`, pid no longer running). Rejected: killing Claude on interrupt (AC3 requires the process to stay).
- **Attachment parity** (Constraints, `sessions.ts:530-562`, `main.ts:376-385`):
  - allowed extensions `.png .jpg .jpeg .webp .gif .svg .pdf .txt .md`;
  - basename after the last `/` or `\` (win32 `path.basename`, the app runs on Windows);
  - each UTF-16 unit outside `[A-Za-z0-9._-]` becomes `_` (a JS regex without `u`, so an emoji gives `__`); an empty result becomes `attachment`;
  - extension per Node's `extname`: from the last `.`, except index 0 or the name `..`, then ASCII-lowercased;
  - error `Tipo di file non supportato: <ext|sconosciuto>`; files already written before the bad one stay on disk, as in Node;
  - `mimeType` is the part's content type, ASCII-lowercased (Blob semantics), or `application/octet-stream` when absent or empty;
  - the relative path is built with `format!` and `/`, never `PathBuf::to_string_lossy` (rule from Constraints).
- **Multipart order** (Constraints): all `files` file-parts, then all `file` file-parts, collected separately while streaming. A part with a filename counts toward the "Nessun file caricato" check even when the filename is empty; the manager then skips empty names (Node: `!file.name → continue`). The body limit is `DefaultBodyLimit::max(100 * 1024 * 1024)`, the same as `/api/transcribe`.
- **Failure boundary for malformed bodies** (rules.md "copy the original's failure boundary"):
  - In Node, `c.req.formData()` throws and `onError` answers 500 `{error: err.message}`. Rust answers 500 `{"error":"Failed to parse body as FormData."}` for a multipart rejection or a field read error.
  - `application/x-www-form-urlencoded` parses with no files in Node, so Rust answers 400 `Nessun file caricato`.
  - The TS test pins the status Node actually returns; Node is the reference if they differ.
- **Attachment metadata validation** (Constraints: "replace the explicit rejection with the `attachmentMetaSchema` field checks"):
  - zod issues are joined with `"; "` in schema order: `text`, then `attachments`, then per element `id,name,path,mimeType` (string) and `size` (number);
  - message format `<path>: Invalid input: expected <type>, received <jsType>`, where jsType is one of `undefined|null|string|number|boolean|array|object`, as in the settings golden;
  - a valid element is rebuilt as a fresh `Map` in `id,name,path,mimeType,size` order with unknown keys dropped (L17 lesson / rules.md key order).
  - The existing `text` messages stay byte-identical (rules.md: change only the needed lines). Only now `text` and `attachments` issues are collected together instead of returning early on `text`.
- **Delete guard** (Constraints: "Delete must not let a late flush recreate the dir"): `SessionData.deleted: bool`. `flush()` returns early, clearing `save_pending`, when it is set. `delete` runs halt → sets `deleted` under the lock → removes the session from the map → `remove_dir_all` (3 tries, 100 ms apart, `NotFound` = ok) → `touch_sessions`.
- **Check + decision + write under one lock** (rules.md) for `retry`: the status/`can_retry` checks, the item trim, the snapshot and `begin_retry` (plus `finish_send` when the engine is live) happen in one `session.lock()` scope.
- **Fake engine observation**: `app/test/fake-claude.mjs` gets a `--input-out <file>` test hook that appends every raw stdin line. It is the only way to see what reached the engine (AC1 block, AC3 request shape) over the binary. It goes in the test-hook flags slot of `fakeClaude([...])` / `claude_args`, like `--env-out`.
- **Test layout**:
  - `sessions.test.ts`'s quiz/answer case needs `/answer` and the MCP tools (#21, not in Rust). It moves verbatim, made HTTP-only, to `app/test/sessions-answer.test.ts` (node mode only, left out of `RUST_PORTED`).
  - `workflow.test.ts` (Anki question, `answer`, `livePids`) stays as is: it is #21's.
  - `turns.test.ts` keeps the two job-delivery cases and the action-denied case: no Rust home (jobs out of scope, action-denied belongs to agy/opencode #19, lesson L23).
- **Versions used**:
  - axum 0.8.9 (`Multipart::next_field`, `Field::name/file_name/content_type/bytes`, `DefaultBodyLimit`, `MultipartRejection`);
  - reqwest 0.13.5 (`multipart::{Form, Part}`: `Form::new().part(name, part)`, `Part::bytes(Vec<u8>).file_name(String).mime_str(&str)?`, `RequestBuilder::multipart`);
  - tokio 1.54.1; serde_json `preserve_order`;
  - process-wrap 10.0.1 (unchanged: JobObject/KillOnDrop stays as is, A10).

Rejected alternatives:
- A debug route to read `livePids` over HTTP: not needed, the manager-level Rust tests read `live_pids()` (A15 fallback unused).
- A new Node golden for retry: AC2 is shown by running the same SSE test file against both backends.
- Refactoring `shutdown` onto the new `halt`: out of the needed lines.

## W1

### T1
- **Files**: `crates/proxi/src/engine.rs`, `crates/proxi/src/claude.rs`, `crates/proxi/src/turns.rs`, `crates/proxi/src/sessions.rs`
- **Do**:
  1. `engine.rs`: add to `trait EngineHandle` the required method `fn interrupt(&mut self);` with the doc `/// Aborts the running turn: Claude asks in-band (control request); a kill-based adapter kills the tree and keeps the handle.`
  2. `claude.rs`:
     - `DeadHandle::interrupt` is a no-op.
     - `LiveHandle` gets a field `interrupts: u32` (initialise it to `0` where `LiveHandle` is built in `spawn_claude`). `interrupt` increments it and, if `writer` is `Some`, sends `serde_json::json!({"type": "control_request", "request_id": format!("int-{}", self.interrupts), "request": {"subtype": "interrupt"}}).to_string()` through `writer`.
     - Unit test in the `claude.rs` tests module, `interrupt_writes_the_in_band_control_request`: build a `LiveHandle` with `mpsc::unbounded_channel::<String>()` as writer, a dummy kill sender, `watch::channel(false).1` and `pid: None`. Call `interrupt()` twice and assert the two strings received are exactly `{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}` and the same with `int-2`. After `stop()` (writer taken), `interrupt()` sends nothing and does not panic.
  3. `turns.rs`:
     - Add `use std::time::Duration;` and `pub const WATCHDOG: Duration = Duration::from_secs(10); // Node's WATCHDOG_MS: an interrupted turn that never reports its result is given up`.
     - Change `TurnHost::learner` to `fn learner(&mut self, text: &str, attachments: Option<&Value>);` (doc: attachments ride on the last bubble of the turn; the host adds them only when it is a non-empty array).
     - Add the fields `interrupting: bool`, `watchdog_seq: u64` and `watchdog_armed: Option<u64>` to `Turns` (all false/0/None in `new`).
     - `begin_send`: compute `turn_text` like Node `turns.ts:92-98` without jobs. If `attachments` is `Some(Value::Array(a))` with `!a.is_empty()`, take `lines = a.iter().map(|x| format!("- {} (percorso: {})", x["name"].as_str().unwrap_or(""), x["path"].as_str().unwrap_or(""))).join("\n")` and `block = format!("[Allegati forniti dallo studente:\n{lines}]")`. Then `turn_text = match &text { Some(t) => format!("{block}\n\n{t}"), None => block }`. Otherwise keep `text.clone().unwrap_or_default()`. `LastTurn.text` stays the raw text.
     - `finish_send`: push the learner bubbles only when `!turn.pushed_learner`, calling `host.learner(line, if i == show.len() - 1 { turn.attachments.as_ref() } else { None })`, then set `pushed_learner = true`. Always write `turn_text` and set `Running`. If `last_turn` is `None` (interrupted while starting), write nothing and set `Idle` instead of `Running`.
     - New `pub fn begin_retry<H: TurnHost + ?Sized>(&mut self, host: &mut H) -> Result<bool, &'static str>`: return `Err("Session is closed")` when `stopped`, and `Err("Nessun turno fallito da riprovare")` when `!self.can_retry()`. Otherwise set `can_retry_flag = false`. If `handle.is_none()`, `set_status(Starting)` and return `Ok(true)`; else return `Ok(false)`. The caller then runs `attach` + `finish_send` (launch) or just `finish_send`.
     - New `pub fn interrupt<H: TurnHost + ?Sized>(&mut self, host: &mut H) -> Option<u64>`: `busy = matches!(host.status(), SessionStatus::Running | SessionStatus::Waiting) && self.handle.is_some()`. If busy, set `interrupting = true`. Set `can_retry_flag = false` and `last_turn = None`, call `self.handle.as_mut().map(|h| h.interrupt())`, then `host.cancel_pending()`. If not busy, return `None`. Else `watchdog_seq += 1`, `watchdog_armed = Some(watchdog_seq)`, and return `Some(watchdog_seq)`.
     - New `pub fn watchdog_fired<H: TurnHost + ?Sized>(&mut self, host: &mut H, ticket: u64)`: return if `watchdog_armed != Some(ticket)`. Set `watchdog_armed = None`, return if `!interrupting`, then `interrupting = false`, `host.notice("info", "Turno interrotto.")`, and `if host.status().is_busy() { self.set_status(host, SessionStatus::Idle, None) }`.
     - `on_event` `Result` branch, after `if late { return }`, follows Node `turns.ts:256-279`:
       - if `interrupting`: `host.notice("info", "Turno interrotto.")`, `can_retry_flag = false`, `last_turn = None`;
       - else if `is_error && !auth_failed`: the current error branch plus `interrupting = false; watchdog_armed = None;` before `return`;
       - else the current success branch;
       - then, for both non-error paths, `interrupting = false; watchdog_armed = None;` followed by the existing `pending == 0 && !auth_failed → Idle`.
     - `on_exit`: right after `self.handle = None`, set `interrupting = false; watchdog_armed = None;`.
     - `stop`: set `watchdog_armed = None`.
  4. `sessions.rs`, only these lines:
     - `impl TurnHost for SessionData::learner` gets the new signature and forwards `attachments`.
     - `SessionData::learner(&mut self, text: &str, attachments: Option<&Value>)` builds a fresh `Map` in the order `id` (`l:<uuid>`), `at`, `kind:"learner"`, `text`, then `attachments` (cloned) only when `attachments` is a non-empty array, and upserts it.
     - No other change.
  5. Tests in the `turns.rs` tests module:
     - `MemoryHandle` gets `interrupt` incrementing `EngineLog.interrupts: usize` (new field).
     - `Host.learner` records `(String, Option<Value>)`: update the existing assertions to compare the texts, e.g. `host.learner.iter().map(|l| l.0.as_str())`.
     - New cases, ported from `app/test/turns.test.ts`:
       - (a) `interrupt_then_the_late_result_of_the_given_up_turn_does_not_idle_the_newer_turn`: send "uno"; `interrupt` returns `Some(t)`; `log.interrupts == 1`; `watchdog_fired(t)` → Idle and notices `[("info","Turno interrotto.")]`; `begin_send` "due" (returns false) + `finish_send`; a result → still Running and late; a second result → Idle; `host.late == [true, false]`.
       - (b) `interrupt_with_a_prompt_result_gives_one_notice_and_disarms_the_watchdog`: send; interrupt → `Some(t)`; `error_result("error_during_execution", None)` → Idle, one info notice and no error notice; `watchdog_fired(t)` → no extra notice; `can_retry() == false`; `saved_last_turn()` is None.
       - (c) `interrupt_without_a_process_only_cancels_the_cards`: no handle, `host.status = Waiting`, `host.pending = 1` → `None`, `pending == 0`, `log.interrupts == 0`.
       - (d) `attachments_go_into_the_turn_prompt_and_onto_the_last_learner_bubble`: two attachments (`diagram.png` and `book.pdf`, paths `.learner/sessions/s/attachments/…`) with show `["apertura", "Spiegami questo diagramma"]`. The sent text equals exactly `"[Allegati forniti dallo studente:\n- diagram.png (percorso: .learner/sessions/s/attachments/diagram.png)\n- book.pdf (percorso: .learner/sessions/s/attachments/book.pdf)]\n\nSpiegami questo diagramma"` (`text` = that last line). The first bubble has `None`, the last has the attachments array, and `saved_last_turn().text == Some("Spiegami questo diagramma")`.
       - (e) `a_model_error_enables_retry_and_retry_resends_without_duplicating_bubbles`: error result → `can_retry`; `begin_retry` → `Ok(false)`; `finish_send` → Running; learner still 1; sent is `[q, q]`; a success result → Idle, `!can_retry`, `saved_last_turn()` None.
       - (f) `a_crash_enables_retry_and_retry_relaunches_with_the_resume_token`: init `tok-crash`, then exit 139; `begin_retry` → `Ok(true)` with status Starting and `resume == Some("tok-crash")`; `attach` + `finish_send` → the new handle got the text once (log sent `["Spiegazione", "Spiegazione"]` across both handles), learner still 1, Running.
       - (g) `retry_is_refused_when_nothing_failed_or_the_session_is_closed`: `begin_retry` on fresh turns → `Err("Nessun turno fallito da riprovare")`; after a failed launch then `stop()` → `Err("Session is closed")`.
       - (h) `finish_send_after_an_interrupt_during_start_idles_without_writing`: `begin_send` (needs launch), `interrupt` (not busy: status Starting), `attach`, `finish_send` → nothing sent, Idle.
- **Done when**: `cargo test --locked --lib turns` and `cargo test --locked --lib claude` pass with the new cases; `cargo test --locked --test session_manager` still passes (the learner change keeps today's item shape when there are no attachments); `cargo build --locked` has no new warnings in the touched files.

### T2
- **Files**: `app/test/fake-claude.mjs`, `app/test/attachments.test.ts`, `app/test/sessions.test.ts`, `app/test/sessions-answer.test.ts`
- **Do**:
  1. `fake-claude.mjs`: next to the `--env-out` hook, add `const inputOut = arg("--input-out");` with the comment `// Test hook: append every raw stdin line, so a test can see exactly what the server sent (attachment block, interrupt request).` In the `rl.on("line")` handler, first line: `if (inputOut) fs.appendFileSync(inputOut, line + "\n");`. Update the header comment's hook list if it has one.
  2. Rewrite `app/test/attachments.test.ts` HTTP/SSE-only (no `createServer`, no `srv.manager`):
     - Use `startServer({ repoRoot: repo, dataRoot: data, claude: fakeClaude(["--input-out", inputFile]) })` from `./harness.ts`, a separate temp `repo` and a seeded `data` (curriculum + `courses/demo/{lessons,MISSION.md,NOTES.md}`, as in `api-sessions.test.ts`'s `seedData`), and `process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"`.
     - Local helpers: `post(path, body)` (JSON plus the `x-learner-client` header), `upload(id, fd)`, `getState(id)` via `GET /api/sessions/:id`, and an async `waitFor`.
     - Session created with `POST /api/sessions {kind:"teach",course:"demo",model:"haiku",target:{mode:"planned",title:"Prima lezione"},ankiOptIn:false}`, waiting for `idle`.
     - Cases (stored files read at `path.join(repo, meta.path)`):
       - (a) the existing png + pdf upload: key order `Object.keys(meta)` deep-equals `["id","name","path","mimeType","size"]`; `meta.id` matches the UUID v4 pattern and the stored name starts with `meta.id.slice(0, 8) + "_"`;
       - (b) traversal `../../../etc/passwd.png` → name `passwd.png`, the path contains no `..`, the file exists;
       - (c) a name with spaces and an accent, `"mia foto è.PNG"` → the path ends with `_mia_foto__.PNG`;
       - (d) a `file` field appended before a `files` field → response order `[files one, file one]`;
       - (e) `.exe` → 400 `{error:"Tipo di file non supportato: .exe"}`; a name with no extension `"LEGGIMI"` → 400 `Tipo di file non supportato: sconosciuto`;
       - (f) an empty FormData → 400 `{error:"Nessun file caricato"}`;
       - (g) an unknown session id with one valid file → 404 `{error:"Session not found"}`;
       - (h) a malformed body: header `content-type: multipart/form-data; boundary=x`, body `"garbage"` → assert the status Node returns (expected 500) and that `error` is a non-empty string;
       - (i) a message with an uploaded attachment's meta (from (a)), text `"Ecco le slide della lezione"`: 200, wait idle; the learner item with that text has `attachments` deep-equal to the meta; the input file has a JSON line with `type === "user"` whose `message.content` equals `` `[Allegati forniti dallo studente:\n- diagram.png (percorso: ${meta.path})]\n\nEcco le slide della lezione` ``;
       - (j) `attachments: [{ name: "a" }]` → 400 and `error` starts with `"attachments.0.id: "`;
       - (k) `DELETE /api/sessions/:id` (with the header) → 200 `{ok:true}`; the `.learner/sessions/<id>` dir is gone; `GET` → 404.
  3. Rewrite `app/test/sessions.test.ts` HTTP/SSE-only:
     - Same `startServer` with `fakeClaude(["--input-out", inputFile])`, repo = data = one temp root seeded as today, and `const start = () => startServer({...})`.
     - `state(id)` becomes an async `GET`; `events` come from a local `sse(url)` client copied from `api-sessions.test.ts`; remove the `mod`/skip import dance.
     - Keep every case except the quiz one, with the same assertions over HTTP.
     - Additions:
       - (1) The interrupt case: after `idle`, items have exactly one `notice` with `level:"info"` and `text:"Turno interrotto."` and none with `level:"error"`; the input file contains the exact line `{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}`.
       - (2) The retry case: open `sse(.../events)` before `POST /retry`, record `events.length` just before the POST, wait for `idle`. The events after that mark start with `type:"snapshot"`, include a `status` event `running` before a `status` event `idle`, and include no `item` event whose `item.kind === "learner"`.
       - (3) New case `"a turn cut by a restart comes back as a retryable error"`:
         - practice session; `POST message {text:"FAKE:slow:15"}`; wait `running`;
         - poll `<root>/.learner/sessions/<id>/state.json` until `state.status === "running"` and `lastTurn` is set; `await srv.close(); srv = await start();`;
         - `GET` → `status:"error"`, `retryable:true`, `lastError:"Sessione interrotta prima del completamento del turno"`; `GET /api/sessions` contains the id;
         - `POST /retry` → 200; wait `idle` (timeout 20 s); the learner item count is unchanged.
     - The DELETE case also asserts that the id is absent from `GET /api/sessions`.
  4. New `app/test/sessions-answer.test.ts`: the quiz/answer case moved from `sessions.test.ts`, HTTP-only via `startServer` (it creates its own teach session first). Header comment: `// Needs /answer and the MCP tools (#21): node mode until then, not in RUST_PORTED.`
- **Done when**: `node --import tsx --test app/test/attachments.test.ts app/test/sessions.test.ts app/test/sessions-answer.test.ts` passes (node mode); `npm run typecheck:app` passes; `rg -n 'manager|createServer' app/test/attachments.test.ts app/test/sessions.test.ts app/test/sessions-answer.test.ts` prints nothing.

### T3
- **Files**: `crates/proxi/tests/common/sessions.rs`
- **Do**: add shared helpers (rules.md / L21: one copy for all session test binaries):
  - `pub struct UploadPart<'a> { pub field: &'a str, pub file_name: &'a str, pub mime: &'a str, pub bytes: &'a [u8] }`.
  - `pub async fn upload(client: &Client, server: &RunningServer, id: &str, parts: &[UploadPart<'_>]) -> Response`: builds a `reqwest::multipart::Form` from the parts in order (`Part::bytes(p.bytes.to_vec()).file_name(p.file_name.to_string()).mime_str(p.mime).expect("mime")`) and POSTs it to `/api/sessions/{id}/attachments` with `x-learner-client: 1`.
  - `pub async fn delete(client: &Client, server: &RunningServer, path: &str) -> Response`: a DELETE with `x-learner-client: 1`.
  - `pub fn fake_inputs(file: &Path) -> Vec<Value>`: every line of the `--input-out` file parsed as JSON (empty vec when the file is missing).
  - `pub fn user_texts(file: &Path) -> Vec<String>`: the `message.content` strings of the `type:"user"` lines.
  - `pub const INTERRUPT_REQUEST: &str = r#"{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}"#;`
  - `pub fn raw_inputs(file: &Path) -> Vec<String>`: the raw lines, for the exact `INTERRUPT_REQUEST` comparison.
  - `pub async fn wait_for_state_file(repo: &Path, id: &str, done: impl Fn(&Value) -> bool) -> Value`: polls `<repo>/.learner/sessions/<id>/state.json` every 25 ms; panics after 20 s with the last content.
  - `pub fn pid_running(pid: u32) -> bool`: Windows runs `tasklist /FI "PID eq <pid>" /NH` and checks the stdout contains `pid.to_string()` as a whitespace-separated token; Unix runs `kill -0 <pid>` and checks the exit status is success.
  - `pub fn fake_args(extra: &[&str]) -> Vec<String>`: the `fake-claude.mjs` path (`common::repo_root().join("app").join("test").join("fake-claude.mjs")`), then `extra`.
  - Unit-free: these are used by T4/T6.
- **Done when**: `cargo test --locked --test sessions_api --no-run` and `cargo test --locked --test session_manager --no-run` compile (`common/mod.rs` has `#![allow(dead_code)]`).

## W2

### T4
- **Files**: `crates/proxi/src/sessions.rs`, `crates/proxi/tests/session_manager.rs`
- **Do**: manager methods, Node `sessions.ts:530-657`; take `Session { data, turns }` borrows as the file already does.
  1. Attachment metadata validation in `validate_message`:
     - Collect issues into a `Vec<String>` (the `text` messages stay byte-identical) instead of returning early.
     - `attachments`: `None` (key absent) → `None`. `Some(Value::Null)` is an issue, as in Node (zod `.optional()` refuses `null`): `attachments: Invalid input: expected array, received null` (400). Any other non-array → `attachments: Invalid input: expected array, received <t>`. An array → for each element `i`:
       - a non-object → `attachments.{i}: Invalid input: expected object, received <t>`;
       - otherwise, for `id,name,path,mimeType`: not a string → `attachments.{i}.{key}: Invalid input: expected string, received <t>`; for `size`: not a number → `…expected number, received <t>`;
       - a valid element is pushed as a fresh `Map` in `id,name,path,mimeType,size` order.
     - `<t>` comes from a private `fn js_type(value: Option<&Value>) -> &'static str` (`undefined|null|boolean|number|string|array|object`).
     - Non-empty issues → `HttpError::new(400, issues.join("; "))`. Empty array → `Some(json!([]))`.
     - Update `message_validation_matches_text_body`. The `{"text":"x","attachments":null}` case (today asserting `.unwrap().1 == None`) becomes `let error = validate_message(&json!({"text": "x", "attachments": null})).unwrap_err();` with `error.status == StatusCode::BAD_REQUEST` and `error.message == "attachments: Invalid input: expected array, received null"`. The `[{"name":"a"}]` case now errors with exactly `attachments.0.id: Invalid input: expected string, received undefined; attachments.0.path: Invalid input: expected string, received undefined; attachments.0.mimeType: Invalid input: expected string, received undefined; attachments.0.size: Invalid input: expected number, received undefined`. Add a valid case with keys in a shuffled order plus an `extra` key → serialised exactly `[{"id":"i","name":"n","path":"p","mimeType":"m","size":3}]`.
  2. `pub struct AttachmentUpload { pub name: String, pub mime_type: Option<String>, pub bytes: Vec<u8> }`.
  3. Private helpers, with unit tests in the `sessions.rs` tests module:
     - `fn attachment_base(raw: &str) -> &str`: after the last `/` or `\`; strip trailing separators first.
     - `fn safe_attachment_name(base: &str) -> String`: keep ASCII `[A-Za-z0-9._-]`, else push `'_'` `c.len_utf16()` times; empty → `"attachment"`.
     - `fn attachment_ext(safe: &str) -> String`: `match safe.rfind('.') { Some(i) if i > 0 && safe != ".." => safe[i..].to_ascii_lowercase(), _ => String::new() }`.
     - Tests: `"../../../etc/passwd.png"` → `"passwd.png"`; `"C:\\x\\a b.PDF"` → base `"a b.PDF"` → safe `"a_b.PDF"` → ext `".pdf"`; `"😀.png"` → `"__.png"`; `""` → `"attachment"`; `".png"` → ext `""`; `"..png"` → `".png"`; `".."` → `""`.
  4. `pub fn save_attachments(&self, id: &str, files: Vec<AttachmentUpload>) -> Result<Value, HttpError>`:
     - Unknown id → 404 `Session not found`; status `Closed` (checked under the session lock, then released) → 409 `Session is closed`.
     - `create_dir_all(session_dir(id)/attachments)` (io error → 500 with the error text).
     - For each file: skip `name.is_empty()`; `raw = attachment_base(&name)`, `safe = safe_attachment_name(raw)`, `ext = attachment_ext(&safe)`; if not in `[".png",".jpg",".jpeg",".webp",".gif",".svg",".pdf",".txt",".md"]`, return 400 `Tipo di file non supportato: {ext or "sconosciuto"}` (files already written stay).
     - `file_id = engine::uuid_v4()`, `stored = format!("{}_{}", &file_id[..8], safe)`; `fs::write` (error → 500).
     - The meta is a fresh `Map`: `id` = file_id, `name` = raw, `path` = `format!(".learner/sessions/{id}/attachments/{stored}")`, `mimeType` = `mime_type.filter(|t| !t.is_empty()).map(|t| t.to_ascii_lowercase()).unwrap_or("application/octet-stream")`, `size` = `bytes.len()` as a JSON integer.
     - Return `Value::Array(metas)`.
  5. `SessionData.deleted: bool` (false in `make_session`). In `flush`, after `data.save_pending = false;`, add `if data.deleted { return; }`.
  6. `async fn halt(&self, session: &Arc<Mutex<Session>>)`: under the lock set `data.closing = true`, run `data.cancel_pending()` and `let handle = turns.stop()`; drop the lock; `if let Some(mut h) = handle { h.stop().await }`.
  7. `pub async fn close(&self, id: &str) -> Result<(), HttpError>`: 404; `halt`; under the lock `let retryable = turns.can_retry(); data.apply_status(SessionStatus::Closed, None, retryable); flush(...)`.
  8. `pub async fn delete(&self, id: &str) -> Result<(), HttpError>`:
     - 404; `halt`; under the lock `data.deleted = true; data.save_pending = false;`;
     - remove `id` from `self.0.sessions` (`shift_remove`);
     - up to 3 attempts `fs::remove_dir_all(self.0.session_dir(id))`, where `NotFound` = done and another error sleeps 100 ms (`tokio::time::sleep`); after the last failure → 500 with the error text;
     - finally `self.0.touch_sessions()`.
  9. `pub fn interrupt(&self, id: &str) -> Result<(), HttpError>`: 404; under the lock `let ticket = turns.interrupt(data);`. If `Some(ticket)`, `spawn_delayed(crate::turns::WATCHDOG, …)` with a closure holding `Arc::downgrade(&self.0)` and `id`, which on upgrade calls the new `Shared::watchdog_fired(&self, id: &str, ticket: u64)` (look up the session, lock, `turns.watchdog_fired(data, ticket)`).
  10. `pub async fn retry(&self, id: &str) -> Result<(), HttpError>` (Node `sessions.ts:607-628`). Under one lock:
      - status `Closed` → 409 `Session is closed`; busy → 409 `The teacher is busy: wait for the current turn to finish`; `!turns.can_retry()` → 400 `Nessun turno fallito da riprovare`.
      - If `turns.saved_last_turn().is_some_and(|t| t.pushed_learner)`: truncate `data.state.items` after the last `kind:"learner"` item (if any). Else `retain` the items that are neither (`kind:"notice"` and `level:"error"`) nor (`kind:"teacher"` and `streaming:true`).
      - `data.streamed.clear(); data.finalised.clear(); data.state.updated_at = events::iso_now(); let state = data.state.clone(); data.notify(ServerEvent::Snapshot { state });`
      - `let needs_launch = turns.begin_retry(data).map_err(|m| HttpError::new(if m == "Session is closed" { 409 } else { 400 }, m))?; if !needs_launch { turns.finish_send(data); }`
      - Drop the lock; `if needs_launch { self.launch_turn(&session).await.map_err(|m| HttpError::new(500, m))?; }`; then lock and `flush`.
  11. `pub fn live_pids(&self) -> Vec<u32>`: for each session in the map order, `turns.pid()` when `Some`.
  12. Tests in `crates/proxi/tests/session_manager.rs`:
      - Add `fn manager_with(env: &TestEnv, extra: &[&str]) -> SessionManager`, the same as `manager_for` but with `ClaudeBin { cmd: "node", args: common::sessions::fake_args(extra) }`, and use `fake_args(&[])` inside `fake_bin()`.
      - Cases:
        - (a) `attachments_are_stored_with_node_names_and_reach_the_engine`: `let input = env.repo.join("input.jsonl"); let input = input.to_str().unwrap(); let manager = manager_with(&env, &["--input-out", input]);`; `save_attachments` with a png part (bytes `[0x89,0x50]`, mime `IMAGE/PNG`) and `"../x/notes.pdf"` with mime `None`. Order and names: `name == "notes.pdf"`, `mimeType` `image/png` / `application/octet-stream`, `size`, the path prefix and the `<id[..8]>_` stored name, the bytes on disk, the serialised key order. Then `message(id, json!({"text":"guarda","attachments": metas}))` → idle: the last learner item's `attachments` equals the metas; `user_texts(input).last()` equals the exact block + `"\n\nguarda"`.
        - (b) `unsupported_or_closed_attachments_are_refused`: `.exe` → 400 message; an unknown id → 404; after `close` → 409.
        - (c) `retry_reruns_the_failed_turn_without_a_second_bubble`: `FAKE:error_once x` → error, retryable; `retry` → idle; one learner item with that text; no error notice; then poll the state file until `lastTurn` disappears (`let v = common::sessions::wait_for_state_file(&env.repo, &id, |v| v.get("lastTurn").is_none_or(Value::is_null)).await;`, the 25 ms poll T3 adds; the flush after idle is asynchronous) and only then assert `v["canRetry"] != json!(true)` (`canRetry == false`); a second `retry` → 400 `Nessun turno fallito da riprovare`.
        - (d) `retry_is_409_while_busy_or_closed`: `FAKE:slow:20` running → `retry` 409 busy message; `close` → `retry` 409 `Session is closed`.
        - (e) `a_loaded_interrupted_turn_retries_on_a_fresh_launch`: write a state file (via `hand_state`) with `status:"running"`, one learner item `{"id":"l:1","at":"2026-01-01T00:00:00.000Z","kind":"learner","text":"ciao"}`, `lastTurn {text:"ciao",show:["ciao"],turnText:"ciao",pushedLearner:true}`, `provider:"claude"`. The manager loads it → error/retryable; `retry` → idle; still one learner item; `usage.turns == 1`.
        - (f) `interrupt_stops_the_turn_in_band_and_close_leaves_no_process` (D-AC3):
          - `manager_with(&env, &["--input-out", input])`; `FAKE:slow` → running; `manager.interrupt(&id)` → Ok; wait idle within 8 s;
          - exactly one info notice `Turno interrotto.`, no error notice, `retryable == false`; `raw_inputs(input)` contains `INTERRUPT_REQUEST`;
          - `live_pids().len() == 1` (Claude stays up, as in Node); remember `pid`; `close(&id)` → `live_pids()` empty, `!pid_running(pid)`, `get(id)["status"] == "closed"`, `state_file` status `closed`.
        - (g) `delete_removes_the_session_and_no_late_flush_recreates_it`: `FAKE:slow` running; while it runs assert `live_pids().len() == 1` and capture `let pid = manager.live_pids()[0];`; `save_attachments` one file, `delete` → Ok; `get` None; the dir is absent; `list()` lacks the id; `live_pids()` empty and `!common::sessions::pid_running(pid)` (as (f) does for close); after `tokio::time::sleep(600 ms)` the dir is still absent; `delete` again → 404.
        - (h) `interrupt_close_delete_and_retry_on_a_missing_session_are_404`.
- **Done when**: `cargo test --locked --lib sessions` and `cargo test --locked --test session_manager` pass; `cargo test --locked --lib turns` still passes.

### T5
- **Files**: `app/test/turns.test.ts`
- **Do**: D10 + L23. Delete the cases ported to Rust in T1:
  - "interrupt, then the late result…" → T1 (a);
  - "interrupt with a prompt result…" → T1 (b);
  - "formats attachments into the turn prompt…" → T1 (d);
  - "model error … enables retry…" → T1 (e);
  - "process crash mid-turn enables retry…" → T1 (f);
  - "retry throws when not retryable" → T1 (g).

  Before deleting each one, check that its Rust counterpart in `crates/proxi/src/turns.rs` asserts the same facts. Keep "a failed relaunch leaves the owed results owed…", "a job settling mid-turn waits…" and "action-denied becomes an info notice…" (no Rust home: jobs are out of scope, action-denied belongs to #19). Remove imports that become unused (`mock`, `WATCHDOG_MS`). Update the file's header comment to: `// The per-session turn module, driven by an in-memory engine. Retry, interrupt and attachments moved to crates/proxi/src/turns.rs (#20); what stays has no Rust home yet (jobs, agy/opencode action-denied).`
- **Done when**: `node --import tsx --test app/test/turns.test.ts` passes with 3 tests; `npm run typecheck:app` passes.

## W3

### T6
- **Files**: `crates/proxi/src/server.rs`, `crates/proxi/tests/sessions_api.rs`
- **Do**:
  1. Router:
     - `.route("/api/sessions/{id}", get(get_session).delete(delete_session).fallback(not_found))`;
     - `.route("/api/sessions/{id}/attachments", post(post_session_attachments).layer(DefaultBodyLimit::max(100 * 1024 * 1024)).fallback(not_found))` with the comment `// Node has no body cap on uploads; axum's 2 MB default would refuse a PDF.`;
     - `.route("/api/sessions/{id}/retry", post(post_session_retry).fallback(not_found))`, the same for `interrupt` and `close`.
     - Update the doc comment above `get_sessions` to list the new routes (main.ts:354-409).
  2. `post_session_attachments(State, Path(id), headers: HeaderMap, multipart: Result<Multipart, MultipartRejection>) -> Response`:
     - If the request content-type starts with `application/x-www-form-urlencoded` → 400 `Nessun file caricato`.
     - `Err(_)` multipart → 500 `Failed to parse body as FormData.`.
     - Loop `next_field()`: `Err` → 500 with the same message. For a field with `file_name() == Some(_)` and `name()` `files` or `file`, read `content_type()` as a `String` and `bytes()` (`Err` → 500 same) into `AttachmentUpload { name: file_name, mime_type, bytes: bytes.to_vec() }`, pushed into `files_group` or `file_group`. Other fields are ignored.
     - `uploads = files_group ++ file_group`; empty → 400 `Nessun file caricato`.
     - `state.sessions.save_attachments(&id, uploads)` → `Json(array)` or `json_error(error.status, &error.message)`.
     - Check whether the router already reads the headers; if `HeaderMap` is unused elsewhere, import it from `axum::http`.
  3. `post_session_retry` (async, `state.sessions.retry(&id).await`), `post_session_interrupt` (`state.sessions.interrupt(&id)`), `post_session_close` (`close(&id).await`) and `delete_session` (`delete(&id).await`): each answers `Json(json!({"ok": true}))` or `json_error(error.status, &error.message)`, and none reads the body (Node ignores it).
  4. Tests in `crates/proxi/tests/sessions_api.rs`:
     - `Fixture::options_with(&self, extra: &[&str])` sets `claude_args = common::sessions::fake_args(extra)`; `options()` delegates with `&[]`.
     - Cases:
       - (a) `session_control_routes_need_the_client_header_and_an_existing_id`: without `x-learner-client`, POST `retry|interrupt|close|attachments` and `DELETE` → 403; with it, on the unknown id `nope` → 404 `{"error":"Session not found"}` (attachments with one valid png part).
       - (b) `attachments_upload_in_node_order_and_reach_the_engine`: `let input = fixture.repo.join("input.jsonl"); let input = input.to_str().unwrap();` and start the server with `fixture.options_with(&["--input-out", input])`; `upload` with parts `file:"b.txt"` then `files:"a.png"` → 200; the response order is `a.png`, then `b.txt`; key order checked through the raw body text starting `[{"id":"`; the files exist under `fixture.repo`; then a message with the metas → idle; `user_texts(input)` ends with the exact block; an empty form → 400 `Nessun file caricato`; `.exe` → 400 `Tipo di file non supportato: .exe`; a raw body `garbage` with `content-type: multipart/form-data; boundary=x` → 500 `{"error":"Failed to parse body as FormData."}`; `content-type: application/x-www-form-urlencoded` body `a=1` → 400 `Nessun file caricato`.
       - (c) `retry_route_sends_a_snapshot_then_the_rerun_turn`: `FAKE:error_once x` → error; open the SSE (`GET /events`, `read_until` the snapshot); POST `/retry` → 200 `{"ok":true}`; `read_until` a status idle. `data_events` after the initial snapshot: the first is `snapshot`, `describe` contains `status:running` before `status:idle`, with no `item:learner:` entry.
       - (d) `interrupt_route_stops_the_turn_with_one_notice`: `options_with(&["--input-out", input])`; `FAKE:slow` → running; POST `/interrupt` → 200; wait idle (< 8 s); one `Turno interrotto.` info notice, no error notice; `raw_inputs(input)` contains `INTERRUPT_REQUEST`.
       - (e) `close_route_closes_and_refuses_new_turns`: idle → POST `/close` 200 → `GET` status `closed` → message 409 `Session is closed`.
       - (f) `delete_route_removes_the_session_data`: `FAKE:slow` running + one upload → DELETE 200; `GET` 404; the list lacks it; `.learner/sessions/<id>` is absent and still absent after 600 ms.
       - (g) `a_restart_lists_the_session_and_makes_the_cut_turn_retryable` (AC4): `FAKE:slow:15` → running; `wait_for_state_file(repo, id, |v| v["state"]["status"] == "running" && v.get("lastTurn").is_some())`; `server.shutdown().await`; `start(fixture.options())` again → the list contains the id; `GET` → `error`, `retryable: true`, the exact `lastError`; POST `/retry` 200 → idle; still the same learner count.
- **Done when**: `cargo test --locked --test sessions_api` and `cargo test --locked --lib server` pass.

## W4

### T7
- **Files**: `scripts/test-rust.mjs`, `tests/test-rust.test.ts`
- **Do**:
  - Append `"app/test/attachments.test.ts"` and `"app/test/sessions.test.ts"` to `RUST_PORTED`, after `"app/test/api-sessions.test.ts"`.
  - In `tests/test-rust.test.ts`, add `it("runs the attachments and session-control tests against the spawned binary", …)` asserting both entries are in `RUST_PORTED` and `"app/test/sessions-answer.test.ts"` is not (it needs #21).
  - Then run the full binary suite and fix nothing but real defects found by it (in this task's files, or report them).
- **Done when**: `npm test` passes; `cargo build --release --locked` then `npm run test:rust` pass with the two new files included.

## Verification
Full suite (CI order):
```
npm run typecheck && npm run typecheck:app
npm test
npm run app:build && npm run test:app
cargo build --release --locked && npm run test:rust
cargo test --locked
```

Tests that must exist at the end:
- Rust unit tests: `turns.rs` T1 (a)–(h); `claude.rs` `interrupt_writes_the_in_band_control_request`; `sessions.rs` attachment-name helpers and the updated `message_validation_matches_text_body`.
- `crates/proxi/tests/session_manager.rs`: T4 (a)–(h).
- `crates/proxi/tests/sessions_api.rs`: T6 (a)–(g).
- `app/test/attachments.test.ts` (T2 a–k), `app/test/sessions.test.ts` (T2 incl. interrupt, retry-events and restart cases), `app/test/sessions-answer.test.ts`, the trimmed `app/test/turns.test.ts` (3 cases), and the `tests/test-rust.test.ts` new case.

Per criterion:
- AC1: `session_manager::attachments_are_stored_with_node_names_and_reach_the_engine`, `sessions_api::attachments_upload_in_node_order_and_reach_the_engine`, `attachments.test.ts` cases (a), (d), (i) in node mode and under `npm run test:rust`; `turns.rs` (d).
- AC2: `sessions.test.ts` retry case (SSE sequence) passing under both `node --import tsx --test app/test/sessions.test.ts` and `npm run test:rust`; `sessions_api::retry_route_sends_a_snapshot_then_the_rerun_turn`; `session_manager::retry_reruns_the_failed_turn_without_a_second_bubble`; `turns.rs` (e), (f).
- AC3: `session_manager::interrupt_stops_the_turn_in_band_and_close_leaves_no_process` (in-band request, idle, one notice, `live_pids` empty and pid gone after close); `sessions_api::interrupt_route_stops_the_turn_with_one_notice`; `sessions.test.ts` interrupt case in both modes; `turns.rs` (a), (b), (c), (h); `claude.rs` `interrupt_writes_the_in_band_control_request`. Process kept alive after the interrupt: the `live_pids().len() == 1` assertion of (f). No process after delete: `session_manager::delete_removes_the_session_and_no_late_flush_recreates_it` (`live_pids()` empty and `!pid_running(pid)` on the pid captured while `FAKE:slow` ran).
- AC4: `sessions.test.ts` "a turn cut by a restart comes back as a retryable error" under `npm run test:rust` (a real binary kill and restart); `sessions_api::a_restart_lists_the_session_and_makes_the_cut_turn_retryable`; `session_manager::a_loaded_interrupted_turn_retries_on_a_fresh_launch` and the existing `load_recovers_interrupted_sessions`.
- AC5: `sessions_api::delete_route_removes_the_session_data`, `session_manager::delete_removes_the_session_and_no_late_flush_recreates_it`, `attachments.test.ts` (k), and the `sessions.test.ts` DELETE case in both modes.
- AC6: `npm run test:rust` after `cargo build --release --locked`, with `RUST_PORTED` containing both files (checked by `tests/test-rust.test.ts` under `npm test`).
- AC7: `cargo test --locked` runs the `turns.rs` (a)–(h) cases; `app/test/turns.test.ts` no longer contains the six ported cases (T5).

## Out of scope
- Jobs (`deliver`, owed job results in the turn text) and the `turns.test.ts` job cases.
- The `/answer` route and the MCP tools (#21): `app/test/sessions-answer.test.ts` and `app/test/workflow.test.ts` stay node-only.
- The agy/opencode adapters and their kill-based interrupt / action-denied notice (#19); only the `EngineHandle::interrupt` seam is added.
- New Node goldens (`npm run golden:capture`) and changes to `sessions_parity.rs`.
- Refactoring `SessionManager::shutdown` onto `halt`.
