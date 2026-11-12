# Git workflow

`main` is the only long-lived branch. All repository changes land on it via a feature branch PR.

## Commits & PR titles

- PR titles use the same Conventional Commits format as commits (`feat: ...`, `fix(scope): ...`,
  `chore: ...`): the squash merge uses the PR title as the commit on `main`, with an empty body.

## Code: branch → PR → CI → auto-merge

Code is `extensions/`, `app/`, `crates/`, `tests/`, `skills/`, `agents/`, `docs/`, `.github/`, `assets/`,
`viz/`, `package*.json`, `Cargo.toml`, `Cargo.lock`, `tsconfig.json` and any root agent file (`AGENTS.md`, `CLAUDE.md`, `CONTEXT.md`).

```bash
git switch main && git pull --ff-only
git switch -c <type>/<issue>-<slug>        # e.g. feat/31-study-friction-hero; no issue: fix/<slug>
# ...commit...
git push -u origin HEAD                    # pushing a feature branch needs no approval
gh pr create --base main --title "<type>(<scope>): <summary>" --body "Closes #<issue>"
gh pr merge --auto --squash                # GitHub merges once the `checks` job is green
```

- The gate is CI (`.github/workflows/ci.yml`, job `checks`): typecheck, typecheck:app, `npm test`,
  `app:build`, `test:app`, `cargo build --release --locked`, `cargo test --locked`, Playwright e2e.
  Locally run only the test you are iterating on; CI runs the full set.
- CI red: read the failing step with `gh run view <id> --log-failed`, fix it on the same branch and
  push. Auto-merge stays armed and fires on the next green run.
- After the merge GitHub deletes the remote branch. Clean up locally:
  `git switch main && git pull --ff-only && git branch -d <branch> && git fetch --prune`.
- One branch per PR; delete a branch as soon as its PR is merged or closed. The repo holds `main`
  plus open PR branches only.

## Study content is not in this repo

Learner data (`courses/`, `curriculum.json`, `index.html`, `decks/`, `graph/`) lives in the data folder
(`npm run data -- where`), outside the repo, and is never committed here. Sync is the app's job (Settings →
Sincronizzazione). `viz/` is repo content and goes the code path.

## Repo settings (GitHub)

- Ruleset `main`: no deletion, no force push, status check `checks` required.
- Merge: squash only, auto-merge on, head branches deleted on merge.
