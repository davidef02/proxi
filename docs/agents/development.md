# Developing Proxi (engine and app)

For agents that change the code: the engine in `extensions/`, the app in `app/`, the Rust backend in `crates/proxi/`, tests, tooling.
Agents that teach or study never need this file.

## Principi

0. **Git.** Niente co-author, niente descrizione/body nei commit: solo titolo breve in formato Conventional Commits (`feat: ...`, `fix(scope): ...`). Mai fare push senza la mia approvazione; se te la do, non richiedermela per lo stesso lavoro. Flusso branch → PR → CI → auto-merge: [`git-workflow.md`](git-workflow.md).
1. **Queue.** Treat all work as a queue of todos. When I propose tweaks, changes or corrections, append them to the queue; do not break the queue to suddenly prioritize my correction. Only stop when 100% of your queue is truly clear.
2. **Mistakes.** Mistakes are not your fault, do not apologize, you are an LLM. Mistakes always mean a gap or issue in the instruction files. Describe which problem in the instruction files caused the mistake and suggest the fix. Be solution oriented.
3. **YAGNI.** Always hold a You Aren't Gonna Need It mentality. Do not overengineer. Never expand scope without permission. Always suggest possible scope improvements if they make sense. All work must be justifiable. Size the engineering to the Level line at the top of `AGENTS.md`.
4. **Workflow.** Vision e milestone: skill vision. Idee → issue: skill from-ideas-to-issues. Issue → merge: skill make-no-mistake. Stato: issue e milestone GitHub + `docs/ROADMAP.md`.

## UI/UX: reduce study friction

- Purpose: the app exists to reduce friction in studying. Anything that adds decisions, steps or
  noise before studying is a regression.
- Primary actions are **Continua** (resume/advance the next lesson: `nextAction()` in
  `app/web/src/next-action.ts`) and **Ripasso** (spaced review, done in Anki: the app shows the
  course deck's due count and opens the deck in Anki). Everything else is secondary.
- Review lives in Anki only; the in-app `practice` session is **Esercizi** (problems built from the
  lessons and his knowledge graph). Do not reintroduce an in-app review queue.
- Rules for UI work:
  - each page has one obvious primary action (on the course page: the Continua/Ripasso pair, one
    of them marked "Consigliato ora" by `recommend()`); secondary actions are visually quieter
    (small text buttons/links, no filled background);
  - minimize clicks and decisions between opening the app and studying;
  - show "what's due" (Anki due count, next lesson) before asking the learner for anything;
  - never rely on color alone to mark the recommended action: use a text badge too;
  - never truncate lesson titles (wrap them; see #4);
  - when in doubt, judge a change by "does this reduce study friction?".

## Commands

CI runs all of these on every PR (`.github/workflows/ci.yml`); locally run only the one you are
iterating on.

```bash
npm run typecheck:app
cargo build --release --locked
npm run app:build   # before test:app and e2e: both expect app/web/dist
npm run test:app    # app (app/test/); needs the release build first: the harness spawns the proxi binary
npm run test:rust   # ported HTTP/SSE tests against the proxi binary (needs the release build)
cargo test --locked # Rust backend (crates/proxi)
npm run e2e         # app, Playwright (Chromium)
```

`npm run eval -- [scenario] [--engine claude|agy|opencode] [--settings <file>] [--timeout <min>]` drives
the real configured engine on fixed scenarios (`app/eval/scenarios.ts`, fixture Vue graph in
`app/eval/fixture.ts`) through the built `proxi` binary over HTTP/SSE only (build it first:
`cargo build --release --locked`; `PROXI_BIN` overrides the path). Each scenario gets a temp repo and data
folder; the temp settings copy drops `syncProvider` so the throwaway data never syncs.
`EVAL_FAKE_ENGINE=<script.mjs>` runs a fake engine instead of Claude (`app/test/fake-claude.mjs`; until
#18/#21 serve the MCP tools no scenario reaches its stop card), `EVAL_KEEP=1` keeps the temp folders. Only
`claude` runs in the Rust backend for now. Never in CI; the agent runs it after changing teach/plan prompts.

Server tests start servers through `startServer` in `app/test/harness.ts` (binary-only: it spawns the built
`proxi` binary) with the external services faked by `app/test/fake-servers.ts`; a test file that runs against
the binary uses only HTTP/SSE and is listed in `RUST_PORTED` of `scripts/test-rust.mjs`. `app/test/binary.ts`
locates and spawns the binary with no server import; the eval uses it too.

## The `proxi` binary

`cargo build --release --locked` builds `target/release/proxi` (`proxi.exe` on Windows); `cargo test --locked`
runs its tests, and `npm run test:rust` runs the ported HTTP/SSE tests against that build.
`npm run desktop:install` runs the same build and adds `target/release` to the user PATH (Windows, no admin
rights; `npm run desktop:uninstall` removes it), so agents run `proxi lesson|anki|home|data` from any new
shell; check with `proxi --version`. The binary finds the repo from its own location: keep it in
`target/release`, never copy it elsewhere.

## Where the logic lives

| Concern | Module |
| :--- | :--- |
| The one slug rule (join identity) | `extensions/slug.ts` |
| The plan + its match onto taught lessons | `extensions/chapter.ts` |
| Status board rendering & generation | `extensions/homepage.ts` |
| Lesson artifacts, data island, reference sheets | `extensions/html-lesson.ts` |
| Card generation, TSV export, Anki sync | `extensions/anki-sync.ts` |
| AnkiConnect calls for the board | `extensions/anki-client.ts` (`anki-sync.ts` still has its own copy) |
| Data folder resolution | `extensions/data-root.ts` |
| Sync (engine, providers, secrets, service) | `crates/proxi/src/sync/` (GitHub and Google Drive; secrets in the OS keychain) |
| The app (server, web UI, desktop, engine prompts) | `app/`: read [`app/README.md`](../../app/README.md) first |

Server data modules in `crates/proxi/src/` (`courses.rs`, `knowledge.rs`, `lesson/`, `exercises.rs`, `graph/store.rs`) take the
data root (`dataRoot`) as their first argument. `.learner/` (settings, sessions, jobs, manifests, secrets)
stays under `repoRoot` and is never synced. `.learner/models` and `.learner/embeddings` hold the embedding
model and index, regenerated automatically. `proxi embed-bench` measures recall@3 and median query time of
that model on `<dataRoot>/bench/embed-bench-queries.json`.

## Keep the teaching context lean

The root `AGENTS.md` is loaded on every turn of every agent that studies in this repo. Put
development rules here, never in the root. The app's own engines already skip the root:
`claude` runs with `--setting-sources ""` and `agy` runs with its scratch dir as `cwd`; keep both
when changing their Rust ports `crates/proxi/src/claude.rs`/`agy.rs`.
