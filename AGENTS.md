# Proxi

Personalized teaching engine: 3Blue1Brown-style standalone HTML lessons and Anki spaced
repetition. Learner language is **Italian**; code, commits and docs are in English.

Mode: personal | Level: uso personale
Reliable in the user's daily use. Tests cover the behaviours that matter and errors are clear. No multi-user, scale or enterprise hardening unless asked.

**Product principle: reduce study friction.** Every screen makes the next useful study action
obvious and one click away: **Continua** (next lesson) and **Ripasso** (Anki review).

- Before developing this project (code, tests, docs), read [`docs/agents/development.md`](docs/agents/development.md).
- Before creating, editing or closing an issue or milestone, or editing `docs/ROADMAP.md`, read [`docs/agents/issues.md`](docs/agents/issues.md).

Everything below is for teaching.

## Orient first

```bash
npm run home -- chapters    # every course, its lessons, done vs to do; writes nothing
npm run data -- where       # the data folder: courses/, graph/, decks/, curriculum.json
```

Then read the target course's `<dataRoot>/courses/<course>/MISSION.md` (`<dataRoot>` from `npm run data -- where`):
destination, constraints, out of scope.
Use the vocabulary in [`CONTEXT.md`](CONTEXT.md) (chapter = course directory → lesson → node).

## Repo invariants

- A course owns everything under `<dataRoot>/courses/<course>/` (`MISSION.md`, `NOTES.md`, `lessons/`,
  `reference/`, `learning-records/`, `exercises/`, and `assets/` for pre-engine lessons). Keep
  `lessons/`, `reference/` and `learning-records/` out of the repo root; root `assets/` holds only
  the engine's `lesson-template.html`.
- Study content lives in the data folder, outside this repo, and is never committed here (sync is the app's job: Settings → Sincronizzazione).
- `curriculum.json` is the plan; `index.html` is generated: regenerate it, never hand-edit. Both live in
  `<dataRoot>`.
- CLI relative paths (`--out-dir courses/...`, `--node-file x.json`) resolve against the data folder (the
  CLIs chdir there); use absolute paths for files elsewhere.
- Reuse a planned lesson's title character for character (`lesson init --title`): its slug is the
  `(course, slug)` join key. A wrong title means re-planning the chapter.
- Create Anki cards only after his explicit yes in Probe (Phase 1c).
- Deck root is `Proxi`, one deck per course (`Proxi::<chapter title>`). Keep both names (`DECK_ROOT` in
  `app/shared/identity.mjs`, re-exported by `extensions/anki-sync.ts`), or every synced deck is orphaned.

## Read before

| When | Read |
| :--- | :--- |
| Planning a chapter / teaching a lesson | `skills/plan/SKILL.md`, `skills/teach/SKILL.md`, [`docs/agents/pedagogy.md`](docs/agents/pedagogy.md) |
| Writing any Italian text he reads (lessons, quizzes, DAG labels, prompts) | [`docs/agents/pedagogy.md`](docs/agents/pedagogy.md) § Voice |
| `lesson` / `anki` / `home` commands | [`docs/agents/lessons-and-anki.md`](docs/agents/lessons-and-anki.md) |
| Writing `ankiParams` | [`docs/agents/anki-cards.md`](docs/agents/anki-cards.md) |
| Diagrams | [`docs/agents/visuals.md`](docs/agents/visuals.md) |
| Committing, branching, opening a PR | [`docs/agents/git-workflow.md`](docs/agents/git-workflow.md) |
| Subagents, Antigravity / OpenCode / Pi | [`docs/agents/multi-agent.md`](docs/agents/multi-agent.md) |
