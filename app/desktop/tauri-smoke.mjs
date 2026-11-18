// Real-run smoke of the Tauri shell: launches the built src-tauri/target/release/proxi-app.exe
// with a fake claude and a temp copy of the fixture repo, drives its WebView2 over the DevTools
// protocol (per-launch WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, no app code change), and asserts the
// spawn contract: backend-reported ephemeral URL, port fallback, single instance, and a quit that
// leaves no process behind. Run: npm run tauri:build && npm run tauri:smoke
import { chromium } from "playwright-core";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { APP_NAME } from "../shared/identity.mjs";
import { OPEN_HASH } from "./bench-lib.mjs";
import { proxiBinary } from "../shared/proxi-bin.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "../..");
const EXE = path.join(root, "src-tauri", "target", "release", "proxi-app.exe");

const URL_RE = /^http:\/\/127\.0\.0\.1:\d+/;
const POLL_MS = 50; // L49: multi-second budgets with ≈50 ms polls
const CDP_BUDGET_MS = 60_000; // WebView2 runtime up
const START_BUDGET_MS = 90_000; // backend ready ≤ 30 s + navigation
const EXIT_BUDGET_MS = 30_000; // L58: every wait has a deadline, a dead peer is a failure

const fail = (m) => {
  throw new Error("SMOKE FAIL: " + m);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Fixed deadline that retries `ok` until it holds, fails with a plain-English message when not. */
async function until(what, budgetMs, ok) {
  const deadline = Date.now() + budgetMs;
  while (true) {
    let good = false;
    try {
      good = await ok();
    } catch {
      good = false;
    }
    if (good) return;
    if (Date.now() > deadline) fail(`${what} within ${budgetMs / 1000} s`);
    await sleep(POLL_MS);
  }
}

const tasklist = (filter) => execFileSync("tasklist", ["/FI", filter, "/NH"], { encoding: "utf8" });
const alive = (pid) => tasklist(`PID eq ${pid}`).includes(String(pid));
// Only the backends of THIS run: each block spawns its backend with --repo-root pointing at the
// block's own tmp copy, so the command line identifies it — other runs on this machine (parallel
// gate/test:app sessions, a real app instance) must never count.
const proxiCount = (repo) => {
  const out = spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      `(Get-CimInstance Win32_Process -Filter "Name='proxi.exe'" | Where-Object { $_.CommandLine -like '*${repo.replace(/'/g, "''")}*' } | Measure-Object).Count`,
    ],
    { encoding: "utf8", windowsHide: true },
  ).stdout;
  const n = Number(String(out).trim());
  return Number.isInteger(n) ? n : -1;
};
const shellCount = () => (tasklist("IMAGENAME eq proxi-app.exe").match(/\bproxi-app\.exe\b/gi) ?? []).length;
/** Best-effort tree kill: the whole backend tree (and any engine child) goes with the shell. */
const killTree = (pid) => {
  try {
    execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
  } catch {
    /* already gone */
  }
};

// Only used when a run dies unexpectedly: AC3's own quit path must stay graceful (window close).
const tracked = [];
process.on("exit", () => {
  for (const pid of tracked) killTree(pid);
});

/** A free port for the WebView2 DevTools endpoint: the flag takes no port 0. */
function freePort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const port = s.address().port;
      s.close(() => resolve(port));
    });
  });
}

/** The pid owning the foreground window (user32 via PowerShell); -1 when it cannot be read. */
function foregroundPid() {
  const out = spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Add-Type -Namespace U -Name W -MemberDefinition '[DllImport(\"user32.dll\")] public static extern IntPtr GetForegroundWindow(); [DllImport(\"user32.dll\")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);';" +
        "$h = [U.W]::GetForegroundWindow(); $p = [uint32]0; [void][U.W]::GetWindowThreadProcessId($h, [ref]$p); Write-Output $p",
    ],
    { encoding: "utf8", windowsHide: true },
  ).stdout;
  const pid = Number(String(out).trim());
  return Number.isInteger(pid) && pid > 0 ? pid : -1;
}

/** The graceful close of a real user (WM_CLOSE); the fallback if the CDP close is ignored. */
function closeMainWindow(pid) {
  return spawnSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", `(Get-Process -Id ${pid}).CloseMainWindow() | Out-Null`], {
    windowsHide: true,
  }).status === 0;
}

/** Launch env: temp copy of the fixture repo for data, fake claude, real built backend (L56). */
function smokeEnv(tmp, { singleInstance = false, noPort = false } = {}) {
  const repo = path.join(tmp, "repo");
  fs.cpSync(path.join(root, "app", "test", "fixtures", "repo"), repo, { recursive: true });
  // The shell derives dist-dir from the repo root, so the built UI rides along in the copy.
  fs.cpSync(path.join(root, "app", "web", "dist"), path.join(repo, "app", "web", "dist"), { recursive: true });
  const env = {
    ...process.env,
    LEARNER_REPO_ROOT: repo,
    LEARNER_DATA_ROOT: repo,
    LEARNER_PORT: "0",
    LEARNER_OPEN_HASH: OPEN_HASH,
    LEARNER_CLAUDE_BIN: process.execPath, // node + fake-claude: the real claude is never run
    LEARNER_CLAUDE_BIN_ARGS: JSON.stringify([path.join(root, "app", "test", "fake-claude.mjs")]),
    PROXI_BIN: proxiBinary(root, process.env), // the real backend binary, not the repo's (L56)
  };
  if (noPort) delete env.LEARNER_PORT; // AC5: the wanted port falls back to 4317
  if (singleInstance) {
    // R1+R2: APPDATA redirected keeps user data isolated while the identifier-derived
    // single-instance lock stays active (only LEARNER_USER_DATA would skip the plugin).
    delete env.LEARNER_USER_DATA;
    env.APPDATA = path.join(tmp, "appdata");
  } else {
    env.LEARNER_USER_DATA = path.join(tmp, "userdata");
  }
  return env;
}

/** Spawn the shell exe (unelevated: an elevated WebView2 ignores env-set flags) and connect over CDP. */
async function launchApp(env) {
  const cdpPort = await freePort();
  const child = spawn(
    EXE,
    [],
    { env: { ...env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${cdpPort}` }, stdio: "ignore" },
  );
  tracked.push(child.pid);
  // The host process gives no readiness signal: poll the DevTools runtime, then wait for the app page.
  await until("the WebView2 DevTools endpoint answers", CDP_BUDGET_MS, () =>
    fetch(`http://127.0.0.1:${cdpPort}/json/version`).then((r) => r.ok).catch(() => false),
  );
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
  let page;
  await until("the app page over CDP", START_BUDGET_MS, () => {
    page = (browser.contexts() ?? []).flatMap((c) => c.pages()).find((p) => URL_RE.test(p.url()));
    return page !== undefined;
  });
  return { child, browser, page };
}

/** Assertions shared by the launches: the route came from the backend, the UI is live. */
async function assertAppPage(page, { ephemeral, hash }) {
  const url = page.url();
  const port = new URL(url).port;
  if (!URL_RE.test(url)) fail(`window URL is not a backend-reported http://127.0.0.1:<port> page: ${url}`);
  if (ephemeral && port === "4317") fail(`the window opened on the hard-coded 4317 instead of the backend-reported port: ${url}`);
  if (hash && !url.endsWith("/" + OPEN_HASH)) fail(`window URL does not end with the open hash ${OPEN_HASH}: ${url}`);
  await page.waitForFunction(() => (document.getElementById("root")?.children.length ?? 0) > 0, null, { timeout: 30_000 });
  const courses = await page.evaluate(() => fetch("/api/courses").then((r) => r.json()));
  if (!Array.isArray(courses)) fail("/api/courses did not return a list");
  const title = await page.title();
  if (title !== APP_NAME) fail(`window title ${JSON.stringify(title)} != ${APP_NAME}`);
  return { courses, title };
}

/** AC3 close: window.close() over CDP first, the real user close as fallback, then verify. */
async function closeAppThroughCdp(page, pid, repo) {
  await page.evaluate(() => window.close()).catch(() => {});
  let closed = true;
  try {
    await until("the shell exits after the CDP close", 15_000, () => !alive(pid));
  } catch {
    // WebView2 ignores a script-initiated close: close it the way a real user does (WM_CLOSE),
    // which reaches the shell's own exit path (the tree kill) just the same.
    closed = false;
  }
  if (!closed) {
    closeMainWindow(pid);
    await until("the shell exits after the close request", EXIT_BUDGET_MS, () => !alive(pid));
  }
  await until("no proxi.exe of this run remains after quit", EXIT_BUDGET_MS, () => proxiCount(repo) === 0);
}

function cleanup({ browser, child, tmp }) {
  try {
    if (browser) browser.close().catch(() => {});
  } catch {
    /* browser already gone */
  }
  if (child?.pid) killTree(child.pid);
  if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
}

// Locate or build the exe; npm run tauri:build also rebuilds the UI before the shell binary.
if (!fs.existsSync(EXE)) {
  console.log("src-tauri/target/release/proxi-app.exe not found: building (npm run tauri:build)…");
  if (spawnSync("npm", ["run", "tauri:build"], { stdio: "inherit", shell: true, cwd: root }).status !== 0)
    fail("npm run tauri:build failed");
}
if (!fs.existsSync(EXE)) fail(`no Tauri shell exe at ${EXE}: run npm run tauri:build first`);

// AC1: LEARNER_PORT=0 → the window opens on the backend-reported ephemeral URL, not 4317,
// on the LEARNER_OPEN_HASH route, with the API answering and the keychain-backed sync reachable.
let tmp, handles;
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-"));
  handles = await launchApp(smokeEnv(tmp));
  const { courses, title } = await assertAppPage(handles.page, { ephemeral: true, hash: true });
  const syncOk = await handles.page.evaluate(() => fetch("/api/sync").then((r) => r.ok)); // AC4 smoke part
  if (!syncOk) fail("GET /api/sync did not answer ok");
  console.log(`ok [AC1]: window ${handles.page.url()} title=${JSON.stringify(title)} courses=${courses.length}, /api/sync answered ok`);
} finally {
  cleanup({ ...handles, tmp });
  handles = undefined;
}

// AC5: a foreign process holds 4317 → the shell never kills it, it starts the backend on a free port
// and opens the window on the URL that backend reports (LEARNER_PORT unset → the wanted port is 4317).
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-"));
  const gate = net.createServer((socket) => socket.destroy()); // the dummy "foreign" listener
  await new Promise((resolve, reject) => {
    gate.once("error", reject);
    gate.listen(4317, "127.0.0.1", resolve);
  });
  try {
    handles = await launchApp(smokeEnv(tmp, { noPort: true }));
    await assertAppPage(handles.page, { ephemeral: true, hash: true });
    console.log(`ok [AC5]: 4317 held by a foreign listener, started on ${handles.page.url()}`);
  } finally {
    gate.close();
  }
} finally {
  cleanup({ ...handles, tmp });
  handles = undefined;
}

// AC2 + AC3: with the single-instance lock active (LEARNER_USER_DATA unset, APPDATA redirected) a
// second launch exits on its own, leaves exactly one shell + one backend, and the existing window
// keeps the focus; closing that window (through CDP) takes the whole backend tree with it.
let first, dup, browser, page;
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-"));
  const repo = path.join(tmp, "repo");
  ({ child: first, browser, page } = await launchApp(smokeEnv(tmp, { singleInstance: true })));
  await assertAppPage(page, { ephemeral: true, hash: true });
  // A bare spawn: the duplicate exits inside the plugin before any WebView2 exists, so there is
  // nothing to connect to — its CDP poll would hang forever. The env must match the first launch.
  dup = spawn(EXE, [], { env: smokeEnv(tmp, { singleInstance: true }), stdio: "ignore" });
  tracked.push(dup.pid);
  await until("the duplicate instance exits on its own", EXIT_BUDGET_MS, () => !alive(dup.pid));
  await until("exactly one proxi-app.exe shell remains", EXIT_BUDGET_MS, () => shellCount() === 1);
  await until("exactly one proxi.exe backend of this run remains", EXIT_BUDGET_MS, () => proxiCount(repo) === 1);
  const windows = (browser.contexts() ?? []).flatMap((c) => c.pages()).filter((p) => URL_RE.test(p.url()));
  if (windows.length !== 1) fail(`expected exactly one CDP window after the duplicate start, found ${windows.length}`);
  const focusDeadline = Date.now() + 30_000;
  while (Date.now() < focusDeadline && foregroundPid() !== first.pid) await sleep(500);
  if (foregroundPid() !== first.pid) fail(`the existing Proxi window was not focused (foreground pid ${foregroundPid()}, want ${first.pid})`);
  console.log("ok [AC2]: duplicate exited on its own, one shell + one backend left, existing window focused");

  await closeAppThroughCdp(page, first.pid, repo);
  console.log("ok [AC3]: window closed, no proxi.exe backend left behind");
} finally {
  if (first?.pid && tracked.includes(first.pid)) tracked.splice(tracked.indexOf(first.pid), 1);
  cleanup({ browser, child: first, tmp });
  if (dup?.pid && alive(dup.pid)) killTree(dup.pid);
}

console.log("tauri smoke passed");
