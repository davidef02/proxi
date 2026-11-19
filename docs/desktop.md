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

## Tauri shell (M06)

An alternative Tauri v2 shell (WebView2) in `src-tauri/` keeps the Electron contract verbatim: spawn argv
`serve --repo-root <abs> --dist-dir <abs> --port <N>` plus engine flags, readiness = stdout line
`Proxi: http://127.0.0.1:<port>` within 30 s, kill = `taskkill /PID <pid> /T /F`, relaunch = backend exit
code 0 while not quitting. The React UI is untouched; only routing changes. Build and try it with:

```bash
npm run tauri:build   # tauri build (src-tauri/)
npm run tauri:dev     # tauri dev mode
npm run tauri:smoke   # real-run smoke of the built exe (app/desktop/tauri-smoke.mjs)
```

Four guards are ported 1:1 from the Electron main process (`app/desktop/main.mjs`, kept as the reference):

- **Clipboard** — copy writes through `tauri-plugin-clipboard-manager` from the web call site. The Electron
  10 000-char bound is enforced in `app/web/src/clipboard.ts` (the plugin command itself has none): longer
  strings return `false` before the plugin is invoked. The `iaDesktop` Electron branch and the
  `navigator.clipboard`/`execCommand` fallback are unchanged.
- **External links** — `tauri-plugin-opener` opens http(s) URLs in the system browser (JS call sites route
  through `openUrl`; `mailto:`/`tel:` stay denied, parity with Electron's `/^https?:/i`), and the Rust
  `on_new_window` handler denies in-app creation of any window from those links.
- **Microphone** — a Rust permission handler grants `Microphone` only when the requesting webview's origin
  matches the backend origin (the localhost fallback before it is known); `Camera` and every other
  permission kind is denied. WebView2 splits a `getUserMedia({audio, video})` request into separate
  Microphone and Camera permission requests, so a video+audio capture fails on the denied Camera request.
- **Popup guards** — every `/files/...` link goes through a single admission gate under one lock: the
  requested URL is registered as label `lesson-<n>`, the window is built with `on_navigation` locked to that
  exact URL (navigation elsewhere is cancelled) and no IPC capabilities (`capabilities/popup.json` with empty
  `permissions`, so a popup cannot invoke Tauri commands), and `MAX_WINDOWS = 8` caps the number of live
  popups — requests past the cap are denied atomically.

One Tauri 2.12.6 consequence: the guard handlers exist only on the window builders, so the main window is
built in the Rust `setup` hook instead of coming from `tauri.conf.json` `app.windows` (that list is empty).

```bash
npm run tauri:build   # tauri build (src-tauri/)
npm run tauri:dev     # tauri dev mode
npm run tauri:smoke   # real-run smoke of the built exe (app/desktop/tauri-smoke.mjs)
```

## Smoke test

`npm run desktop:smoke` launches the app with fake claude and a temp copy of the fixture repo, checks the page
renders and `/api/courses` answers, quits, and verifies the process is gone.
Regenerate the icon (`icon.svg` -> png/ico): `node_modules\electron\dist\electron.exe app/desktop/make-icon.mjs`.

## Troubleshooting

- "Claude Code was not found": install it, run `claude` once in a terminal, reopen the app.
- Blank window or build error: run `npm run app:build` in a terminal to see the Vite error.
- Nothing happens on second launch: the first instance is alive (check the taskbar / Task Manager).
