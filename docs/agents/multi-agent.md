# Runtimes & Subagents

## Runtime Execution Matrix

System rules bind automatically from `AGENTS.md` (Antigravity, OpenCode / Pi) and from `CLAUDE.md`
(Claude Code, which imports `AGENTS.md`). Skills live in `skills/`, subagent briefs in `agents/`.

| Capability | Google Antigravity (Agy) | OpenCode / Pi | Claude Code / CLI |
| :--- | :--- | :--- | :--- |
| **Probing & Quizzes** | `ask_question` / chat turns | `quiz` & `ask_user_question` TUI (`extensions/`) | Chat turns (symmetric bare claims) |
| **Subagent Dispatch** | Native `invoke_subagent` | `subagent(agent="...", task="...")` / `pi-interactive-subagents` | Task delegation / `agents/*.md` |
| **Visual Rendering** | Local CLI render + inspect | Legacy makers (render tools removed); else local CLI render + inspect | Local CLI render + inspect |
| **Lessons, Anki, Board** | `npm run lesson` / `anki` / `home` | Programmatic TS or the same CLI | `npm run lesson` / `anki` / `home` |
| **Skills** | `skills/` or `.agents/skills/` | `skills/` | `skills/` as slash commands (`/teach`, `/plan`) |

## Data folder

Learner data (`courses/`, `graph/`, `decks/`, `curriculum.json`) is not in the repo. Agents outside the app run
`npm run data -- where` to get `<dataRoot>`, then read and write `<dataRoot>/...` with absolute paths. Set
`LEARNER_DATA_ROOT` to override the folder.

---

## Subagent Roster

| Subagent | Role | Responsibilities | Key Tools |
| :--- | :--- | :--- | :--- |
| **`researcher`** | Fact-checking & verification | Conducts multi-angle web research to verify first principles, formulas, and historical facts before teaching. | `web_search`, `web_fetch` |
| **`mermaid-maker`** | Relational diagrams | Authors structural visuals (DAGs, state machines, flows, trees) and verifies them via render-and-inspect loop. | `read` (legacy Pi: render tools removed) |
| **`svg-maker`** | Spatial & geometric visuals | Hand-authors SVG code for coordinate geometry, vectors, function plots, and custom shapes, visually verifying rendered PNGs. | `read` (legacy Pi: render tools removed) |
| **`anki-maker`** | Atomic card formulation | Formulates Minimum Information Principle flashcards (Front/Back & Cloze) from established lesson nodes. | `anki_export`, `anki_sync` |

---

## Delegation Protocol

1. **Minimal, concrete briefs:** name the single idea to visualize or research.
2. **Pedagogy stays with the teacher:** the teacher decides *what* to teach and which visual or fact
   is needed; subagents compose and verify.
3. **Verify deliverables:** visual makers return inspected PNG paths; the researcher cites primary sources.
