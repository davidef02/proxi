# Proxi (the app)

A local web app that teaches through the Proxi engine. The LLM is your own `claude` or `agy` CLI
(logged in with your own account; Settings > Motore picks it), spawned headless with the vendor harness stripped:
no CLAUDE.md, skills, plugins, hooks or user settings, only our system prompt (`app/prompts/`),
read-only built-ins (claude: `Read`, `Glob`, `Grep`, `WebSearch`, `WebFetch`; agy: `view_file`, `list_dir`,
`grep_search`, `search_web`) and the app's own tools served over MCP by the local server.

## Run

Prerequisites: Node 24, `claude` (or `agy`) on PATH and logged in (once interactively), optionally
Anki with AnkiConnect for card sync, and `npm install` in the repo root. The exercises' dependencies in
`courses/<course>/exercises` are installed by the app itself on the first test run (it says so while it works).

**Desktop app (normal use):** `npm run desktop:install` once creates the "Proxi" shortcuts (Start menu and
Desktop). The window rebuilds the UI by itself when the sources changed, so after a `git pull` you just reopen it.
Closing the window stops every engine process it started. Details: `docs/desktop.md`.

Browser instead: `npm run app` (build + http://127.0.0.1:4317) or `npm run app:serve`; stop with Ctrl+C.
Dev: `npm run app:serve` + `npm run app:web` (Vite on :5173 with proxy).

## Use

- **Mappa** (home) is LearnHarness's knowledge map: the true ideas of every lesson as propositions, linked by
  reasoning chains, read bottom-up (foundations at the bottom, derived ideas above). Each course is a framed block
  with its lessons side by side; zoomed out you see the courses and their key concepts, zooming in brings up the
  lessons and then every idea. No idea is ever left unconnected. Search, **Catene** (chain table), click a node for its statement and the
  lessons it comes from; select nodes and **Esercizi sui selezionati** to start an Esercizi session on them.
  **Aggiorna mappa (N)** runs the map subagent on the N lessons not yet (or no longer) mapped; finished lessons are
  mapped automatically (setting *auto-mappa*). Ledgers: `<dataRoot>/graph/` (learner data, synced).
- **Corsi** in the sidebar → the course page shows one primary action: *Continua* (a lesson in progress), *Inizia la
  lezione N* (next planned lesson, title verbatim) or *Pianifica le prossime lezioni*; next to it Ripasso, both with
  the lesson count and last study date shown once in the lesson card; quieter: Lezione libera, Riorganizza corso,
  Esercizi (one session: a topic of his choice or his Anki weak spots, problems and coding exercises). Below, the
  Lezioni list stays open; Fuori dal piano, Esercizi and Come stai andando are closed accordions. The session starts
  at once; an open session for the same lesson is reused.
- In a teach session the first card asks the Anki opt-in (exact Phase 1c question). Questions appear in the dock at
  the bottom in place of the input (number keys, *Non lo so*, confidence, *Perché?*); answered ones stay as cards.
  Each node the teacher writes appears in the conversation as a document block (KaTeX, code, callouts, figures); the
  lesson can be reread in the app from the course page or the end of the session (`#/l/<lesson path>`). Piano /
  Esercizi open in the right panel. Lessons written as prose and the course's reference sheets (**Sintesi**,
  `#/r/<reference path>`) are read in the app too, sanitized from their HTML (`GET /api/doc`); no standalone page opens.
- **Esercizi**: an exercise is a directory `courses/<course>/exercises/NN-<slug>/` (statement `ISTRUZIONI.md`, starter
  file, test, optional `SUGGERIMENTI.json` hints). It opens in the app's built-in workspace (`#/e/<course>/<exercise>`):
  statement, code editor with autosave, **Esegui test** (vitest, the first run installs the course's dependencies) and
  hints revealed one at a time. The teacher receives the results as "Esito test …" messages.
  Per-exercise learner progress (`SUGGERIMENTI.json` `revealed`, `.app-lastrun.json`) lives inside
  `courses/<course>/exercises/<name>/` on purpose, like `courses/<course>/progress.jsonl`, so sync carries it
  across machines; `.app-lastrun.json` is git-ignored. Sync conflicts are kept in `.learner/drive-conflicts` (see [Data and sync](#data-and-sync)).
- **Subagents**: the teacher can dispatch background jobs (headless `claude -p` or `agy` processes, per the engine setting, with their own prompt and tools):
  *research* (web, with sources), *visual* (an SVG figure it renders and checks), *demo* (an interactive single-file
  HTML it tests headless; shown in a sandboxed iframe with a no-network CSP). They show up in **Attività** and in the
  chat, and their results are handed back to the teacher when it is idle. Models per role in **Impostazioni**.

Everything written is ordinary Proxi data: lesson HTML, reference sheets, learning records,
NOTES.md additions, `curriculum.json`, `decks/*.tsv`, `graph/*`, Anki `Proxi::<chapter title>` (one deck per course). Runtime state
(settings, session transcripts, job records, generated prompts, MCP config) lives in `.learner/` (gitignored).

The map code (`app/shared/graph`, `app/web/src/map`) is adapted from LearnHarness (AGPL-3.0-only), like the
rest of Proxi, which is distributed under the same licence (root `LICENSE` and `NOTICE`).

## Semantic concept search

The tutor's concept search (`search_concepts`) ranks concepts by meaning, in Italian or English, with a local
multilingual embedding model (`Xenova/paraphrase-multilingual-MiniLM-L12-v2`; model, dtype and thresholds live in
`app/server/graph/embedding-model.json`). `npm install` downloads it once into `.learner/models`
(about 129 MB); after that search runs offline. With no model on disk, search falls back to lexical
matching and says so in the result (`(Semantic search unavailable: …)`); the app retries the download
in the background at start. `PROXI_SKIP_MODEL=1` skips the download, and `npm run embed-bench`
re-runs the benchmark to revisit the choice.

The in-app update sets `PROXI_SKIP_MODEL=1` for its `npm install`, so the download never eats its
time budget: the app fetches the model in the background after the restart.

## Environment

| Variable | Default | Purpose |
| :--- | :--- | :--- |
| `LEARNER_PORT` | `4317` | server port (127.0.0.1 only) |
| `LEARNER_REPO_ROOT` | this repo | code and `.learner/` |
| `LEARNER_DATA_ROOT` | see [Data and sync](#data-and-sync) | the data folder: courses/, graph/, decks/, curriculum.json |
| `LEARNER_CLAUDE_BIN` / `LEARNER_CLAUDE_BIN_ARGS` | `claude` / `[]` | tests point these at `app/test/fake-claude.mjs` |
| `LEARNER_AGY_BIN` / `LEARNER_AGY_BIN_ARGS` | `agy` / `[]` | engine binary for provider `agy`; tests point these at `app/test/fake-agy.mjs` |
| `LEARNER_ADD_DIRS` | `[<parent of repo>]` | extra read-only dirs for the teacher (work repos) |
| `LEARNER_NPM_BIN` | `npm` | tests: a node script run instead of `npm install` when an exercise's dependencies are missing; the Rust binary also takes `--npm-bin <exe>` (an npm-like executable, wins over the env) and needs `node` on PATH to run exercises |
| `LEARNER_ANKI_URL` | `http://127.0.0.1:8765` | AnkiConnect |
| `LEARNER_NO_OPEN` | unset | `1` = don't open the browser |

## Data and sync

Learner data (`courses/`, `graph/`, `decks/`, `curriculum.json`, the generated `index.html`) lives in the **data
folder** `dataRoot`, outside the code repo. Resolution order (`extensions/data-root.ts`): env `LEARNER_DATA_ROOT`,
then `<repo>/.learner/config.json` `{dataRoot}`, then `~/Documents/proxi-data`. `npm run data -- where` prints it.
Settings → Dati changes the folder; the server restarts to use it. `.learner/` (settings, sessions, jobs, manifests,
secrets, conflicts, trash) stays under the repo and is never synced.

Sync is chosen in Settings → Sincronizzazione, one provider at a time:

- **local**: no remote sync.
- **github**: private repo `proxi-data` on your account (created on connect, `private: true`; a public repo with
  that name is refused). Device flow, scope `repo`. One commit per sync.
- **gdrive**: Google Drive, scope `drive.file`, PKCE + loopback. Flat folder, `name` = relative path.

Each sync is a manifest diff: only changed files move, deletions propagate and land in `.learner/trash/<ms>/` (kept
30 days), and a conflict keeps the losing copy in `.learner/drive-conflicts/`. More than 30% of files deleted at once
stops the pass until you confirm. The tokens are in `.learner/secrets.json`: encrypted with Electron `safeStorage` in
the desktop app; plaintext under plain Node (the UI shows a warning). They are never written under `dataRoot`.

Human prerequisites (once, by the owner of the app):

- **GitHub**: register an OAuth App with **Device flow** enabled; put its Client ID in the constant
  `GITHUB_CLIENT_ID` in `app/server/sync/github.ts`, or set `githubClientId` in `.learner/settings.json`.
- **Google**: a Google Cloud project with the **Drive API** enabled, a **Desktop** OAuth client, and the consent
  screen published to production; put the id and secret in the constants `GOOGLE_CLIENT_ID` and
  `GOOGLE_CLIENT_SECRET` in `app/server/sync/gdrive.ts`, or set `googleClientId` and `googleClientSecret` in
  `.learner/settings.json`.

Empty constants make "Connetti" fail with a clear Italian error that names both places.

## Checks

```bash
npm run typecheck && npm run typecheck:app
npm test && npm run test:app
npm run app:build && npm run e2e
```

Tests never call the real `claude` (they use `app/test/fake-claude.mjs`) and never touch the real
courses (temp repo copies).

## Engine: agy

Settings > Motore switches the teacher sessions and the background subagents (research, visual, demo, map)
from `claude` to Google's `agy` (Antigravity), signed in with your own Google AI Pro account. Models per role
are listed from `agy models` (static fallback) and saved in `settings.json` as `provider` and `agyModels`.
The engine is pinned per session and per job when it is created: switching it affects only new ones (a session
keeps resuming the conversation its own engine issued).

- Launch: `agy --agent ia-learner --input-format stream-json --output-format stream-json --model <id> --disable-slash-commands --add-dir <repo> [--add-dir …] [--conversation <id>]`.
  No `-p`, no `--dangerously-skip-permissions`, no `--mode auto`; global agy config is never touched.
- agy reads system prompt, tools and MCP servers only from an agent file: the app writes
  `.learner/sessions/<id>/.agents/agents/ia-learner.md` (or `.learner/jobs/<id>/`) and uses that scratch dir as cwd;
  the repo is reached through `--add-dir`. Tools: `view_file`, `list_dir`, `grep_search`, `search_web`; never a shell or write tool.
- MCP tools: our `tools/list` marks every tool `_meta: {"antigravity/alwaysLoad": true}`, so agy exposes them directly
  as `mcp_app_<tool>` / `mcp_job_<tool>` and runs them without the denied generic `call_mcp_tool`. The adapter maps the
  names back to `mcp__app__<tool>` / `mcp__job__<tool>`.
- The conversation id comes from agy's `init` event; it is the session's resume token (`resume` in `state.json`), so it
  survives interrupts and app restarts. A process that dies before `init` while resuming counts as a refused
  conversation: the session restarts fresh once, with the lesson context.
- Limits: no interrupt mid-turn (interrupt = kill the process, the next message relaunches with `--conversation` once the
  killed one has exited); no max-turns flag (the app counts tool steps and stops the job past the role limit); no cost,
  only quota (the UI shows "—"); WebFetch is unavailable (agy denies `read_url_content` without a global rule); a denied
  action ends a turn silently and shows as a notice in the chat.

## Compliance

The app only spawns the unmodified `claude` binary (or, if you choose it in Settings, the unmodified `agy`
binary) installed and logged in by you with your own account, never reads or
stores credentials, never uses `--bare` or API keys. Usage counts against your plan's limits like
any Claude Code session.
