// Source contract of the Tauri shell (src-tauri/): it must keep the desktop contract the Electron
// shell implements — single instance focused, backend spawned from stdout, tree-killed on quit.
// The Rust unit tests own the logic; these tests pin the seams against silent drift.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

const here = import.meta.dirname;
const root = path.join(here, "../..");

const source = (rel: string): string => fs.readFileSync(path.join(root, rel), "utf8");

const main = source("src-tauri/src/main.rs");
const shell = source("src-tauri/src/shell.rs");
const conf = JSON.parse(source("src-tauri/tauri.conf.json")) as {
  productName: string;
  version: string;
  identifier: string;
  build: { frontendDist: string; beforeBuildCommand: string; beforeDevCommand: string };
  app: { windows: Array<{ label: string; title: string; visible: boolean }> };
  bundle: { active: boolean; targets: string[] };
};
const caps = JSON.parse(source("src-tauri/capabilities/default.json")) as {
  windows: string[];
  permissions: string[];
};
const pkg = JSON.parse(source("package.json")) as {
  scripts: Record<string, string>;
  devDependencies: Record<string, string>;
};
const gitignore = source(".gitignore");
const secrets = source("crates/proxi/src/sync/secrets.rs");
const identity = source("app/shared/identity.mjs");

test("single instance: the plugin is registered before run, focusing main when a duplicate starts", () => {
  assert.match(main, /tauri_plugin_single_instance::init/);
  assert.match(main, /get_webview_window\("main"\)/, "the duplicate focuses the existing window");
  assert.match(main, /set_focus\(\)/);
  // The skip is keyed on the user-data override, so smoke/e2e runs beside a real instance; an
  // empty override behaves like an unset one (Electron's falsy check) in both uses of the value.
  assert.match(
    main,
    /LEARNER_USER_DATA"\)\s*\.filter\(\|d\| !d\.is_empty\(\)\)[\s\S]*user_data\.is_none\(\)/,
  );
});

test("the shell spawns proxi serve and reads its URL from stdout", () => {
  assert.match(main, /spawn_backend/);
  assert.match(shell, /"serve"\.to_string\(\)/);
  assert.match(shell, /"--repo-root"/);
  assert.match(shell, /"--dist-dir"/);
  assert.match(shell, /"--port"/);
  assert.match(shell, /Proxi: \(http:\/\/127\\.0\\.0\\.1:\(\\d\+\)\)\\s\*\$/, "the readiness regex");
  assert.match(shell, /READY_TIMEOUT: Duration = Duration::from_secs\(30\)/, "30 s deadline");
});

test("every exit path tree-kills the owned backend with taskkill (AC3)", () => {
  assert.match(
    main,
    /RunEvent::ExitRequested[\s\S]*kill_backend_tree/,
    "the run event handler kills the backend on the exit events",
  );
  assert.match(main, /fn kill_backend_tree[\s\S]*kill_tree\(pid\)/, "the tree kill uses kill_tree");
  assert.match(
    shell,
    /fn kill_tree_args\(pid: u32\)[\s\S]*"taskkill"[\s\S]*"\/T"[\s\S]*"\/F"/,
    "the kill is taskkill /PID <pid> /T /F",
  );
  assert.match(
    shell,
    /fn kill_tree\(pid: u32\)[\s\S]*kill_tree_args\(pid\)/,
    "kill_tree delegates to kill_tree_args",
  );
});

test("a clean backend exit code 0 relaunches, anything else is logged, never relaunched", () => {
  assert.match(main, /code == Some\(0\)[\s\S]*?app\.restart\(\)/);
  assert.match(main, /backend exited with code/);
  assert.match(main, /if state\.quitting\.load\(Ordering::SeqCst\)/);
});

test("the window is main, hidden until the start URL is known (built in setup for guard handlers)", () => {
  assert.equal(conf.productName, "Proxi");
  assert.equal(conf.app.windows.length, 0, "window moved to setup so guard handlers can attach to builder");
  assert.match(main, /WebviewWindowBuilder::new\(\s*app,\s*"main"/);
  assert.match(main, /\.title\("Proxi"\)/);
  assert.match(main, /\.visible\(false\)/);
});

test("the bundle is active for NSIS installer in the release pipeline", () => {
  assert.equal(conf.bundle.active, true);
  assert.deepEqual(conf.bundle.targets, ["nsis"]);
});

test("the frontendDist placeholder and dev commands match the vite app", () => {
  assert.equal(conf.build.frontendDist, "capabilities");
  assert.equal(conf.build.beforeDevCommand, "npm run app:web");
  assert.equal(conf.build.beforeBuildCommand, "npm run app:build");
});

test("the identifier is proxi so %APPDATA%\\proxi and the proxi-sync keychain keep working", () => {
  assert.equal(conf.identifier, "proxi");
  assert.match(secrets, /KEYCHAIN_SERVICE: &str = "proxi-sync"/);
});

test("the crate stays out of the root workspace so CI never compiles tauri", () => {
  assert.match(source("src-tauri/Cargo.toml"), /\[workspace\]\s*\n/);
  assert.match(gitignore, /\/src-tauri\/target\//);
  assert.match(gitignore, /\/src-tauri\/gen\//);
});

test("capabilities grant core:default, clipboard and opener to the main window only", () => {
  assert.deepEqual(caps.windows, ["main"]);
  assert.deepEqual(caps.permissions, [
    "core:default",
    "clipboard-manager:allow-write-text",
    {
      identifier: "opener:allow-open-url",
      allow: [{ url: "http://*" }, { url: "https://*" }],
    },
    "core:window:allow-start-dragging",
    "window-controls-overlay:default",
    "allow-set-chrome",
    "allow-set-zoom",
  ]);
});

test("the npm scripts exist and the CLI twin is a devDependency", () => {
  assert.equal(pkg.scripts["tauri:dev"], "tauri dev");
  assert.equal(pkg.scripts["tauri:build"], "tauri build");
  assert.equal(pkg.scripts["tauri:smoke"], "node scripts/tauri-smoke.mjs");
  assert.match(String(pkg.devDependencies?.["@tauri-apps/cli"]), /\^2\./);
  assert.equal(pkg.devDependencies?.electron, undefined);
  assert.deepEqual(
    Object.keys(pkg.scripts).filter((s) => s.startsWith("desktop")),
    [],
  );
});

test("the identity contract is untouched: deck root, app name and user model id", () => {
  assert.match(identity, /export const APP_NAME = "Proxi"/);
  assert.match(identity, /export const DECK_ROOT = "Proxi"/);
  assert.match(identity, /export const APP_USER_MODEL_ID = "DavideF02\.Proxi"/);
});

test("the Rust dependency set matches the locked versions", () => {
  const cargo = source("src-tauri/Cargo.toml");
  assert.match(cargo, /^name = "proxi-app"$/m);
  assert.match(cargo, /^tauri = \{ version = "2"/m);
  assert.match(cargo, /^tauri-plugin-single-instance = "2\.5\.5"$/m);
  assert.match(cargo, /^tauri-plugin-window-controls-overlay = "0\.1\.0"$/m);
  assert.match(cargo, /^reqwest = \{ version = "0\.13\.5"/m);
});

test("window-controls-overlay plugin and window commands are registered", () => {
  assert.match(main, /tauri_plugin_window_controls_overlay::init\(\)/);
  assert.match(main, /generate_handler!\[set_chrome,\s*set_zoom\]/);
});

test("set_chrome and set_zoom guard on label == main", () => {
  assert.match(main, /fn set_chrome[\s\S]*?webview\.label\(\)\s*!=\s*"main"/);
  assert.match(main, /fn set_zoom[\s\S]*?webview\.label\(\)\s*!=\s*"main"/);
});

test("the main window builder disables decorations for the overlay plugin", () => {
  assert.match(main, /WebviewWindowBuilder::new[\s\S]*?\.decorations\(false\)/);
});

test("window.json is saved on CloseRequested for the main window", () => {
  assert.match(main, /WindowEvent::CloseRequested[\s\S]*?save_current_window_state/);
  assert.match(main, /save_window_state/);
});

test("build.rs embeds the Windows app manifest", () => {
  const buildRs = source("src-tauri/build.rs");
  assert.match(buildRs, /app_manifest/);
});

test("app.manifest declares Windows supportedOS GUIDs", () => {
  const manifest = source("src-tauri/app.manifest");
  assert.match(manifest, /supportedOS/);
  assert.match(manifest, /\{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a\}/);
});

test("default.json contains dragging and overlay plugin default permissions", () => {
  assert.ok(caps.permissions.includes("core:window:allow-start-dragging"));
  assert.ok(caps.permissions.includes("window-controls-overlay:default"));
  assert.ok(caps.permissions.includes("allow-set-chrome"));
  assert.ok(caps.permissions.includes("allow-set-zoom"));
});

test("the shell keeps the engine spawn flags and the two-step port fallback", () => {
  assert.match(shell, /LEARNER_CLAUDE_BIN/);
  assert.match(shell, /LEARNER_AGY_BIN/);
  assert.match(shell, /LEARNER_OPENCODE_BIN/);
  assert.match(shell, /--port 0/);
});
