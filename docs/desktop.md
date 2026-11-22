# Desktop app

A Tauri v2 window (WebView2) around the `proxi` binary: the main process spawns `target\release\proxi.exe`
(`target/release/proxi` elsewhere) with `serve` and talks to it over HTTP. If the binary is missing,
the app shows an error naming `cargo build --release --locked` and exits. No terminal needed once installed.

## Install

Download the NSIS installer (`Proxi_<version>_x64-setup.exe`) from the latest GitHub Release and run it. On Windows it installs per-user under `%LOCALAPPDATA%` and adds the install directory to your user `PATH`, so `proxi` works from any new shell.

For local development:

```bash
npm run tauri:dev     # Tauri shell (src-tauri/) in dev mode
npm run tauri:build   # Tauri shell release build
```

## Update

`git pull`. At each start the app compares `app/web/dist` with `app/web/src` and `app/shared` and rebuilds the UI
behind a small splash window when stale. If a rebuild fails and an older build exists, the older build is used
(the error goes to the console).

## Uninstall

Uninstall via Windows Settings. Window position is stored in
`%APPDATA%\proxi\window.json`; delete that folder to forget it.

## Behaviour

- Server on `127.0.0.1:4317`. If that port already answers like Proxi, the window just shows that server (never a
  second server on the same `.learner/` state); a free port is used only when something else holds 4317. Second
  launch focuses the existing window. Closing the main window quits the app (lesson windows close with it).
- Quitting tree-kills the spawned `proxi` backend and every engine process it started.
- External links open in the default browser; `/files/...` (lessons, exercises) open in a new app window created by the main process with the same guards (no navigation away from the server origin, window cap, sandbox, no permissions granted).
- Env overrides (tests): `LEARNER_REPO_ROOT`, `LEARNER_PORT`, `LEARNER_USER_DATA`, `LEARNER_CLAUDE_BIN`,
  `LEARNER_CLAUDE_BIN_ARGS`, `LEARNER_ADD_DIRS`, `LEARNER_OPEN_HASH` (e.g. `#/c/demo`: the main window opens at that
  route; used by `npm run tauri:smoke`).
- The title bar is hidden with a 36px native overlay for the window buttons: the web shell must leave that strip
  free and mark a drag region (`-webkit-app-region: drag`).

## Tauri shell

The Tauri v2 shell (WebView2) in `src-tauri/` keeps the desktop contract verbatim: spawn argv
`serve --repo-root <abs> --dist-dir <abs> --port <N>` plus engine flags, readiness = stdout line
`Proxi: http://127.0.0.1:<port>` within 30 s, kill = `taskkill /PID <pid> /T /F`, relaunch = backend exit
code 0 while not quitting. The React UI is untouched; only routing changes.

To install the pre-built desktop app without compiling, download the installer from the GitHub Release.
See [Release (tag → Windows installer)](#release-tag--windows-installer) below for details.

Build and try it with:

```bash
npm run tauri:build   # tauri build (src-tauri/)
npm run tauri:dev     # tauri dev mode
npm run tauri:smoke   # real-run smoke of the built exe (scripts/tauri-smoke.mjs)
```

Four guards are ported 1:1 from the former Electron main process:

- **Clipboard** — copy writes through `tauri-plugin-clipboard-manager` from the web call site. The Electron
  10 000-char bound is enforced in `app/web/src/desktop.ts` (the plugin command itself has none): longer
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

## Release (tag → Windows installer)

Pushing a git tag matching `v*` (e.g. `v0.1.0`) triggers `.github/workflows/release.yml` on `windows-latest`.

### Release workflow step by step

1. **Environment setup**: Checks out the repository, provisions Node 24 (with npm cache), and installs the minimal stable Rust toolchain.
2. **Dependencies**: Runs `npm ci` with `PROXI_SKIP_MODEL=1` (skips downloading the embedding model during CI).
3. **Backend build**: Builds the release backend binary `target/release/proxi.exe` via `cargo build --release --locked`.
4. **Tauri build & packaging**: Runs `tauri-apps/tauri-action@v1`, which:
   - Executes `tauri build` (triggering `beforeBuildCommand` to compile the web UI into `app/web/dist`).
   - Packages the NSIS installer with the backend binary and web distribution as bundled resources.
   - Signs updater artifacts with the minisign private key using secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
   - Publishes a non-draft GitHub Release containing the installer and signature artifacts. Note that `tauri-action@v1` outputs `latest.json` at the repository root (`./latest.json`) rather than in the bundle directory.
5. **Artifact & size verification**: Runs `node scripts/verify-release.mjs --bundle-dir src-tauri/target/release/bundle --latest-json ./latest.json --config src-tauri/tauri.conf.json` to verify the offline signature and ensure the installer satisfies the size gate.
6. **Silent install smoke test**: Executes the installer silently with `/S`, verifies the per-user installation under `%LOCALAPPDATA%` and HKCU registry entry, launches the installed executable, verifies that `http://127.0.0.1:4317/api/settings` responds within 90 seconds, and terminates the process.

### Published assets

Each GitHub Release publishes:
- **NSIS installer**: `Proxi_<version>_x64-setup.exe` (or `*-setup.exe`)
- **Minisign signature**: `*-setup.exe.sig`
- **Updater manifest**: `latest.json` (containing `version`, `notes`, `pub_date`, signature, and installer download URL)

### Size gate

The installer package is strictly gated under 15 000 000 bytes (15 MB). The release workflow fails if the installer equals or exceeds 15,000,000 bytes. To maintain this size bound, release profiles in both `Cargo.toml` and `src-tauri/Cargo.toml` are configured with:
- `opt-level = "z"`
- `lto = true`
- `codegen-units = 1`
- `strip = true`
- `panic = "abort"`

### Signing keys

The updater uses minisign (Ed25519) signatures.
- **Generate once**:
  ```bash
  npx @tauri-apps/cli signer generate --ci -p <password> -w src-tauri/proxi-updater.key
  ```
  (`npx tauri` cannot determine the executable in this repo; password-protected). This writes private key `src-tauri/proxi-updater.key` and public key `src-tauri/proxi-updater.key.pub`.
- **Public key**: Committed to `src-tauri/tauri.conf.json` under `plugins.updater.pubkey`.
- **Private key & password**: Never committed to git (both `*.key` and `*.key.pub` are gitignored). Stored only as GitHub repository secrets:
  ```bash
  gh secret set TAURI_SIGNING_PRIVATE_KEY --repo davidef02/proxi --body-file src-tauri/proxi-updater.key
  gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --repo davidef02/proxi --body-file .env.tauri-signing
  ```
  The signing password can be stored in `.env.tauri-signing` (gitignored).

### Key rotation

When rotating the signing key pair, publish a bridge release: build and sign a release using the existing (old) private key with `src-tauri/tauri.conf.json` updated to contain the new public key. Once installed, all subsequent updates can be signed using the new private key.

### Offline verification

Verify updater signatures and size constraints locally or in CI:
```bash
node scripts/verify-release.mjs --bundle-dir src-tauri/target/release/bundle --config src-tauri/tauri.conf.json
```
- **In CI**: Add `--latest-json ./latest.json` (since `tauri-action` writes `latest.json` to the repo root, never into the bundle directory).
- **Locally**: `tauri build` emits only the `.sig` sidecar and no `latest.json`. Add `--from-sig` to synthesize `latest.json` from the `.sig` sidecar and the version in `tauri.conf.json`.

Verification validates the minisign signature, verifies that the trusted comment binds the version (`version:<version>`), confirms Ed25519 signature validity against the embedded pubkey, and checks that the installer is below 15 000 000 bytes.

### Local rehearsal

To test the complete release build locally on Windows:
```bash
# 1. Install dependencies without downloading the model
PROXI_SKIP_MODEL=1 npm ci

# 2. Build the backend binary
cargo build --release --locked

# 3. Build the Tauri shell and package the installer
TAURI_SIGNING_PRIVATE_KEY=src-tauri/proxi-updater.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD=<pw> npm run tauri:build

# 4. Verify offline with synthesized latest.json
node scripts/verify-release.mjs --bundle-dir src-tauri/target/release/bundle --from-sig --config src-tauri/tauri.conf.json
```

### Install layout

The NSIS installer runs in per-user mode (`currentUser`):
- Installs to `%LOCALAPPDATA%` without requesting admin elevation.
- Registry keys are stored in `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall`.
- Bundled resources layout inside the install folder:
  - Backend binary: `target/release/proxi.exe`
  - Web frontend: `app/web/dist`
  - Shell executable: `proxi-app.exe`
  - User data and `.learner/` state remain in the installation directory.
- The WebView2 bootstrapper is downloaded at install time if WebView2 runtime is missing (adding 0 MB to the installer size).

## Smoke test

`npm run tauri:smoke` (`scripts/tauri-smoke.mjs`) launches the app with fake claude and a temp copy of the fixture repo, checks the page
renders and `/api/courses` answers, quits, and verifies the process is gone.

## Troubleshooting

- "Claude Code was not found": install it, run `claude` once in a terminal, reopen the app.
- Blank window or build error: run `npm run app:build` in a terminal to see the Vite error.
- Nothing happens on second launch: the first instance is alive (check the taskbar / Task Manager).
