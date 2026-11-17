# Desktop app

An Electron window around the `proxi` binary: the main process spawns `target\release\proxi.exe`
(`target/release/proxi` elsewhere) with `serve` and talks to it over HTTP. If the binary is missing,
the app shows an error naming `cargo build --release --locked` and exits. No terminal needed once the shortcuts exist.

## Install

Prerequisites: `npm install` in the repo root; Rust stable (`cargo`); Claude Code installed and logged in (`claude` once in a terminal).

```bash
npm run desktop:install  # builds the UI and the proxi binary, adds target\release to the user PATH, creates "Proxi" in the Start menu and on the Desktop
npm run desktop          # run it right now without shortcuts
```

The shortcut runs `node_modules\electron\dist\electron.exe app\desktop\main.mjs` with the repo as working directory.
`node app/desktop/install-shortcuts.mjs --dir <folder>` writes a single shortcut into `<folder>` (tests).

The PATH step adds `<repo>\target\release` to your user `Path` (no admin rights) and builds it with `cargo build --release --locked` first; open a new terminal, then `proxi --version` answers. `--dir` (tests) skips the build and the PATH step.

## Update

`git pull`. At each start the app compares `app/web/dist` with `app/web/src` and `app/shared` and rebuilds the UI
behind a small splash window when stale. If a rebuild fails and an older build exists, the older build is used
(the error goes to the console).

## Uninstall

`npm run desktop:uninstall` removes both shortcuts and the PATH entry. Window position is stored in
`%APPDATA%\proxi\window.json`; delete that folder to forget it.

## Behaviour

- Server on `127.0.0.1:4317`. If that port already answers like Proxi, the window just shows that server (never a
  second server on the same `.learner/` state); a free port is used only when something else holds 4317. Second
  launch focuses the existing window. Closing the main window quits the app (lesson windows close with it).
- PATH gets `%USERPROFILE%\.local\bin` and `%APPDATA%\npm` prepended so `claude` resolves from a shortcut.
- Quitting tree-kills the spawned `proxi` backend and every engine process it started.
- External links open in the default browser; `/files/...` (lessons, exercises) open in a new app window created by the main process with the same guards (no navigation away from the server origin, window cap, sandbox, no permissions granted).
- Env overrides (tests): `LEARNER_REPO_ROOT`, `LEARNER_PORT`, `LEARNER_USER_DATA`, `LEARNER_CLAUDE_BIN`,
  `LEARNER_CLAUDE_BIN_ARGS`, `LEARNER_ADD_DIRS`, `LEARNER_OPEN_HASH` (e.g. `#/c/demo`: the main window opens at that
  route; used by `npm run bench:baseline`).
- The title bar is hidden with a 36px native overlay for the window buttons: the web shell must leave that strip
  free and mark a drag region (`-webkit-app-region: drag`).

## Smoke test

`npm run desktop:smoke` launches the app with fake claude and a temp copy of the fixture repo, checks the page
renders and `/api/courses` answers, quits, and verifies the process is gone.
Regenerate the icon (`icon.svg` -> png/ico): `node_modules\electron\dist\electron.exe app/desktop/make-icon.mjs`.

## Troubleshooting

- "Claude Code was not found": install it, run `claude` once in a terminal, reopen the app.
- Blank window or build error: run `npm run app:build` in a terminal to see the Vite error.
- Nothing happens on second launch: the first instance is alive (check the taskbar / Task Manager).
