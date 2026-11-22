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
import { APP_ID, APP_NAME } from "../app/shared/identity.mjs";
import { DEFAULT_CHROME } from "../app/shared/chrome.mjs";
import { proxiBinary } from "../app/shared/proxi-bin.mjs";

const OPEN_HASH = "#/c/demo";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "..");
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
      "Add-Type -Namespace U -Name W -MemberDefinition '" +
        "[DllImport(\"user32.dll\")] public static extern IntPtr GetForegroundWindow(); " +
        "[DllImport(\"user32.dll\")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId); " +
        "[StructLayout(LayoutKind.Sequential)] public struct GUITHREADINFO { public int cbSize; public int flags; public IntPtr hwndActive; public IntPtr hwndFocus; public IntPtr hwndCapture; public IntPtr hwndMenuOwner; public IntPtr hwndMoveSize; public IntPtr hwndCaret; public System.Drawing.Rectangle rcCaret; } " +
        "[DllImport(\"user32.dll\")] public static extern bool GetGUIThreadInfo(uint idThread, ref GUITHREADINFO lpgui); " +
        "[DllImport(\"user32.dll\")] public static extern bool EnumDesktopWindows(IntPtr hDesktop, EnumDesktopWindowsProc lpfn, IntPtr lParam); " +
        "public delegate bool EnumDesktopWindowsProc(IntPtr hWnd, IntPtr lParam); " +
        "[DllImport(\"user32.dll\")] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount); " +
        "public static uint GetForegroundOrActivePid() { " +
        "  IntPtr fg = GetForegroundWindow(); " +
        "  if (fg != IntPtr.Zero) { uint p = 0; GetWindowThreadProcessId(fg, out p); if (p > 0) return p; } " +
        "  uint foundPid = 0; " +
        "  EnumDesktopWindows(IntPtr.Zero, (hWnd, lParam) => { " +
        "    System.Text.StringBuilder sb = new System.Text.StringBuilder(256); " +
        "    GetWindowText(hWnd, sb, 256); " +
        "    if (sb.ToString() == \"Proxi\") { " +
        "      uint p = 0; uint tid = GetWindowThreadProcessId(hWnd, out p); " +
        "      GUITHREADINFO gui = new GUITHREADINFO(); gui.cbSize = Marshal.SizeOf(gui); " +
        "      if (GetGUIThreadInfo(tid, ref gui) && (gui.hwndActive == hWnd || gui.hwndFocus != IntPtr.Zero)) { foundPid = p; return false; } " +
        "    } return true; " +
        "  }, IntPtr.Zero); return foundPid; " +
        "}' -ReferencedAssemblies System.Drawing; " +
        "$p = [U.W]::GetForegroundOrActivePid(); Write-Output $p",
    ],
    { encoding: "utf8", windowsHide: true },
  ).stdout;
  const pid = Number(String(out).trim());
  return Number.isInteger(pid) && pid > 0 ? pid : -1;
}

/**
 * Actively forces the Proxi window of targetPid to the foreground.
 * Windows denies a plain SetForegroundWindow from a background process
 * (foreground lock) whenever another app currently holds focus — the user's
 * editor, a terminal or a parallel agent — so a passive wait can time out
 * even on a healthy app. The AttachThreadInput + ALT-tap trick bypasses that
 * lock (standard foreground-restoration recipe, same class of workaround as
 * minimize+restore or SimulateInput hack).
 */
function forceWindowFocus(targetPid) {
  spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Add-Type -Namespace U -Name WFocus -MemberDefinition '" +
        "[DllImport(\"user32.dll\")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx); " +
        "[DllImport(\"user32.dll\")] public static extern IntPtr GetForegroundWindow(); " +
        "[DllImport(\"user32.dll\")] public static extern bool SetForegroundWindow(IntPtr hWnd); " +
        "[DllImport(\"user32.dll\")] public static extern bool ShowWindowAsync(IntPtr hWnd, int nCmdShow); " +
        "[DllImport(\"user32.dll\")] public static extern bool IsIconic(IntPtr hWnd); " +
        "[DllImport(\"user32.dll\")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId); " +
        "[DllImport(\"user32.dll\")] public static extern uint GetCurrentThreadId(); " +
        "[DllImport(\"user32.dll\")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach); " +
        "[DllImport(\"user32.dll\")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo); " +
        "[DllImport(\"user32.dll\")] public static extern bool EnumDesktopWindows(IntPtr hDesktop, EnumDesktopWindowsProc lpfn, IntPtr lParam); " +
        "public delegate bool EnumDesktopWindowsProc(IntPtr hWnd, IntPtr lParam); " +
        "[DllImport(\"user32.dll\")] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount); " +
        "[DllImport(\"user32.dll\")] public static extern IntPtr GetAncestor(IntPtr hWnd, uint gaFlags); " +
        "public static IntPtr GetProxiHwnd(uint targetPid) { " +
        "  IntPtr found = IntPtr.Zero; EnumDesktopWindows(IntPtr.Zero, (hWnd, lParam) => { " +
        "    uint p = 0; GetWindowThreadProcessId(hWnd, out p); " +
        "    if (p == targetPid) { " +
        "      System.Text.StringBuilder sb = new System.Text.StringBuilder(256); " +
        "      GetWindowText(hWnd, sb, 256); " +
        "      if (sb.ToString() == \"Proxi\") { IntPtr root = GetAncestor(hWnd, 2); found = root != IntPtr.Zero ? root : hWnd; return false; } " +
        "    } return true; " +
        "  }, IntPtr.Zero); return found; " +
        "}'; [U.WFocus]::SetProcessDpiAwarenessContext([IntPtr]::new(-4)) | Out-Null; " +
        "$h = [U.WFocus]::GetProxiHwnd(" + targetPid + "); " +
        "if ($h -eq [IntPtr]::Zero) { exit 0 }; " +
        "if ([U.WFocus]::IsIconic($h)) { [U.WFocus]::ShowWindowAsync($h, 9) | Out-Null }; " +
        "$myTid = [U.WFocus]::GetCurrentThreadId(); " +
        "$dummy = 0; $fg = [U.WFocus]::GetForegroundWindow(); $fgTid = 0; " +
        "if ($fg -ne [IntPtr]::Zero) { $fgTid = [U.WFocus]::GetWindowThreadProcessId($fg, [ref]$dummy) }; " +
        "$hTid = [U.WFocus]::GetWindowThreadProcessId($h, [ref]$dummy); " +
        "[U.WFocus]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero) | Out-Null; " +
        "[U.WFocus]::AttachThreadInput($myTid, $hTid, $true) | Out-Null; " +
        "if ($fg -ne [IntPtr]::Zero -and $fgTid -ne 0) { [U.WFocus]::AttachThreadInput($myTid, $fgTid, $true) | Out-Null }; " +
        "[U.WFocus]::SetForegroundWindow($h) | Out-Null; " +
        "[U.WFocus]::AttachThreadInput($myTid, $hTid, $false) | Out-Null; " +
        "if ($fg -ne [IntPtr]::Zero -and $fgTid -ne 0) { [U.WFocus]::AttachThreadInput($myTid, $fgTid, $false) | Out-Null }; " +
        "[U.WFocus]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero) | Out-Null",
    ],
    { encoding: "utf8", windowsHide: true },
  );
}

/** Wait for the window belonging to targetPid to gain foreground focus, forcing it if needed. */
async function waitForWindowFocus(targetPid, budgetMs = 30_000) {
  await until("the Proxi window gains focus", budgetMs, () => {
    if (foregroundPid() === targetPid) return true;
    forceWindowFocus(targetPid);
    return foregroundPid() === targetPid;
  });
}

/** Resizes and moves the foreground window using user32 SetWindowPos. */
function resizeForegroundWindow(x, y, w, h, targetPid = 0) {
  const res = spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Add-Type -Namespace U -Name WResize -MemberDefinition '" +
        "[DllImport(\"user32.dll\")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx); " +
        "[DllImport(\"user32.dll\")] public static extern IntPtr GetForegroundWindow(); " +
        "[DllImport(\"user32.dll\")] public static extern bool SetForegroundWindow(IntPtr hWnd); " +
        "[DllImport(\"user32.dll\")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId); " +
        "[DllImport(\"user32.dll\")] public static extern bool EnumDesktopWindows(IntPtr hDesktop, EnumDesktopWindowsProc lpfn, IntPtr lParam); " +
        "public delegate bool EnumDesktopWindowsProc(IntPtr hWnd, IntPtr lParam); " +
        "[DllImport(\"user32.dll\")] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount); " +
        "[DllImport(\"user32.dll\")] public static extern IntPtr GetAncestor(IntPtr hWnd, uint gaFlags); " +
        "[StructLayout(LayoutKind.Sequential)] public struct RECT { public int left; public int top; public int right; public int bottom; } " +
        "[DllImport(\"user32.dll\")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect); " +
        "[DllImport(\"user32.dll\")] public static extern bool GetClientRect(IntPtr hWnd, out RECT lpRect); " +
        "[DllImport(\"user32.dll\")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags); " +
        "public static IntPtr GetProxiHwnd(uint targetPid) { " +
        "  if (targetPid > 0) { try { IntPtr mh = System.Diagnostics.Process.GetProcessById((int)targetPid).MainWindowHandle; if (mh != IntPtr.Zero) return mh; } catch {} } " +
        "  IntPtr fg = GetForegroundWindow(); " +
        "  if (fg != IntPtr.Zero) { " +
        "    IntPtr root = GetAncestor(fg, 2); if (root != IntPtr.Zero) fg = root; " +
        "    uint p = 0; GetWindowThreadProcessId(fg, out p); if (targetPid == 0 || p == targetPid) return fg; " +
        "  } " +
        "  IntPtr found = IntPtr.Zero; " +
        "  EnumDesktopWindows(IntPtr.Zero, (hWnd, lParam) => { " +
        "    uint p = 0; GetWindowThreadProcessId(hWnd, out p); " +
        "    if (targetPid == 0 || p == targetPid) { " +
        "      System.Text.StringBuilder sb = new System.Text.StringBuilder(256); " +
        "      GetWindowText(hWnd, sb, 256); " +
        "      if (sb.ToString() == \"Proxi\") { " +
        "        IntPtr root = GetAncestor(hWnd, 2); found = root != IntPtr.Zero ? root : hWnd; return false; " +
        "      } " +
        "    } return true; " +
        "  }, IntPtr.Zero); return found != IntPtr.Zero ? found : fg; " +
        "}'; [U.WResize]::SetProcessDpiAwarenessContext([IntPtr]::new(-4)) | Out-Null; $h = [U.WResize]::GetProxiHwnd(" + targetPid + "); " +
        "if ($h -ne [IntPtr]::Zero) { " +
        "[U.WResize]::SetForegroundWindow($h) | Out-Null; " +
        "$wr = New-Object U.WResize+RECT; [U.WResize]::GetWindowRect($h, [ref]$wr) | Out-Null; " +
        "$cr = New-Object U.WResize+RECT; [U.WResize]::GetClientRect($h, [ref]$cr) | Out-Null; " +
        "$bw = ($wr.right - $wr.left) - $cr.right; $bh = ($wr.bottom - $wr.top) - $cr.bottom; " +
        `$newW = ${w} + $bw; $newH = ${h} + $bh; [U.WResize]::SetWindowPos($h, [IntPtr]::Zero, ${x}, ${y}, $newW, $newH, 0x0044) | Out-Null; ` +
        "} else { throw 'Proxi window not found'; }",
    ],
    { encoding: "utf8", windowsHide: true },
  );
  if (res.status !== 0) fail(`SetWindowPos failed: ${res.stderr || res.stdout}`);
}

/** Reads the foreground window's bounds and maximized state via user32 GetWindowRect and IsZoomed. */
function getForegroundWindowRect(targetPid = 0) {
  const out = spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Add-Type -Namespace U -Name WRect -MemberDefinition '" +
        "[DllImport(\"user32.dll\")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx); " +
        "[DllImport(\"user32.dll\")] public static extern IntPtr GetForegroundWindow(); " +
        "[DllImport(\"user32.dll\")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId); " +
        "[DllImport(\"user32.dll\")] public static extern bool EnumDesktopWindows(IntPtr hDesktop, EnumDesktopWindowsProc lpfn, IntPtr lParam); " +
        "public delegate bool EnumDesktopWindowsProc(IntPtr hWnd, IntPtr lParam); " +
        "[DllImport(\"user32.dll\")] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount); " +
        "[DllImport(\"user32.dll\")] public static extern IntPtr GetAncestor(IntPtr hWnd, uint gaFlags); " +
        "[StructLayout(LayoutKind.Sequential)] public struct RECT { public int left; public int top; public int right; public int bottom; } " +
        "[DllImport(\"user32.dll\")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect); " +
        "[DllImport(\"user32.dll\")] public static extern bool GetClientRect(IntPtr hWnd, out RECT lpRect); " +
        "[DllImport(\"user32.dll\")] public static extern bool IsZoomed(IntPtr hWnd); " +
        "public static IntPtr GetProxiHwnd(uint targetPid) { " +
        "  if (targetPid > 0) { try { IntPtr mh = System.Diagnostics.Process.GetProcessById((int)targetPid).MainWindowHandle; if (mh != IntPtr.Zero) return mh; } catch {} } " +
        "  IntPtr fg = GetForegroundWindow(); " +
        "  if (fg != IntPtr.Zero) { " +
        "    IntPtr root = GetAncestor(fg, 2); if (root != IntPtr.Zero) fg = root; " +
        "    uint p = 0; GetWindowThreadProcessId(fg, out p); if (targetPid == 0 || p == targetPid) return fg; " +
        "  } " +
        "  IntPtr found = IntPtr.Zero; " +
        "  EnumDesktopWindows(IntPtr.Zero, (hWnd, lParam) => { " +
        "    uint p = 0; GetWindowThreadProcessId(hWnd, out p); " +
        "    if (targetPid == 0 || p == targetPid) { " +
        "      System.Text.StringBuilder sb = new System.Text.StringBuilder(256); " +
        "      GetWindowText(hWnd, sb, 256); " +
        "      if (sb.ToString() == \"Proxi\") { " +
        "        IntPtr root = GetAncestor(hWnd, 2); found = root != IntPtr.Zero ? root : hWnd; return false; " +
        "      } " +
        "    } return true; " +
        "  }, IntPtr.Zero); return found != IntPtr.Zero ? found : fg; " +
        "}'; [U.WRect]::SetProcessDpiAwarenessContext([IntPtr]::new(-4)) | Out-Null; $h = [U.WRect]::GetProxiHwnd(" + targetPid + "); " +
        "if ($h -eq [IntPtr]::Zero) { throw 'Proxi window not found'; } " +
        "$wr = New-Object U.WRect+RECT; [U.WRect]::GetWindowRect($h, [ref]$wr) | Out-Null; " +
        "$cr = New-Object U.WRect+RECT; [U.WRect]::GetClientRect($h, [ref]$cr) | Out-Null; " +
        "$z = [U.WRect]::IsZoomed($h); " +
        "Write-Output \"$($wr.left),$($wr.top),$($cr.right),$($cr.bottom),$z\"",
    ],
    { encoding: "utf8", windowsHide: true },
  ).stdout;
  const parts = String(out).trim().split(",");
  if (parts.length !== 5) fail(`cannot read window rect: ${out}`);
  return {
    left: Number(parts[0]),
    top: Number(parts[1]),
    width: Number(parts[2]),
    height: Number(parts[3]),
    maximized: parts[4].toLowerCase() === "true",
  };
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
async function launchApp(env, { extraBrowserArgs = "" } = {}) {
  const cdpPort = await freePort();
  const args = [`--remote-debugging-port=${cdpPort}`, extraBrowserArgs].filter(Boolean).join(" ");
  const child = spawn(
    EXE,
    [],
    { env: { ...env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: args }, stdio: "inherit" },
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
  return { child, browser, page, cdpPort };
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

/** Teardown: close the browser, tree-kill the backend, then remove tmp only once no proxi.exe of
 * this run still holds files under tmp/repo — a just-killed backend can release its handles a
 * moment later (L13), so every other teardown waits and rmSync retries before giving up. */
async function cleanup({ browser, child, tmp, repo }) {
  try {
    if (browser) await browser.close();
  } catch {
    /* browser already gone */
  }
  if (child?.pid) killTree(child.pid);
  if (repo) await until("no proxi.exe of this run remains", EXIT_BUDGET_MS, () => proxiCount(repo) === 0);
  if (tmp) fs.rmSync(tmp, { recursive: true, force: true, maxRetries: 3 });
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
  const repo = path.join(tmp, "repo");
  handles = await launchApp(smokeEnv(tmp));
  const { courses, title } = await assertAppPage(handles.page, { ephemeral: true, hash: true });
  const syncOk = await handles.page.evaluate(() => fetch("/api/sync").then((r) => r.ok)); // AC4 smoke part
  if (!syncOk) fail("GET /api/sync did not answer ok");
  console.log(`ok [AC1]: window ${handles.page.url()} title=${JSON.stringify(title)} courses=${courses.length}, /api/sync answered ok`);
} finally {
  await cleanup({ ...handles, tmp, repo: tmp && path.join(tmp, "repo") });
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
  await cleanup({ ...handles, tmp, repo: tmp && path.join(tmp, "repo") });
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
  try {
    await waitForWindowFocus(first.pid);
  } catch {
    fail(`the existing Proxi window was not focused (foreground pid ${foregroundPid()}, want ${first.pid})`);
  }
  console.log("ok [AC2]: duplicate exited on its own, one shell + one backend left, existing window focused");

  await closeAppThroughCdp(page, first.pid, repo);
  console.log("ok [AC3]: window closed, no proxi.exe backend left behind");
} finally {
  if (first?.pid && tracked.includes(first.pid)) tracked.splice(tracked.indexOf(first.pid), 1);
  await cleanup({ browser, child: first, tmp, repo: tmp && path.join(tmp, "repo") });
  if (dup?.pid && alive(dup.pid)) killTree(dup.pid);
}

// Guards verification: clipboard bound + copy via tauri plugin (AC1), mic-only getUserMedia (AC2),
// external link denied in-app (AC3), and capped/locked popups with dead IPC (AC4).
let popupBrowser;
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-"));
  const repo = path.join(tmp, "repo");
  handles = await launchApp(smokeEnv(tmp), { extraBrowserArgs: "--use-fake-device-for-media-stream --disable-popup-blocking" });
  const { page, browser: b, cdpPort } = handles;
  await assertAppPage(page, { ephemeral: true, hash: true });

  // (a) AC1: open settings dialog, click data-path copy button, verify clipboard equals shown dataRoot
  spawnSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", "Set-Clipboard -Value ''"], {
    windowsHide: true,
  });
  const settingsBtn = page.getByRole("button", { name: "Impostazioni", exact: true });
  await settingsBtn.waitFor({ state: "visible", timeout: 10_000 });
  await settingsBtn.click();
  const dialog = page.getByRole("dialog", { name: "Impostazioni" });
  await dialog.waitFor({ state: "visible", timeout: 10_000 });
  const datiTab = dialog.getByRole("tab", { name: "Dati", exact: true });
  await datiTab.waitFor({ state: "visible", timeout: 10_000 });
  await datiTab.click();
  const copyBtn = dialog.getByRole("button", { name: "Copia percorso" });
  await copyBtn.waitFor({ state: "visible", timeout: 10_000 });
  const dataRootShown = (await dialog.locator("p.font-mono").first().textContent())?.trim();
  if (!dataRootShown) fail("dataRoot not shown in settings dialog");
  await copyBtn.click();
  await dialog.getByRole("button", { name: "Copiato!" }).waitFor({ state: "visible", timeout: 10_000 });
  let clip = "";
  await until("the clipboard matches the shown dataRoot", 5_000, () => {
    clip = spawnSync(
      "powershell",
      ["-NoProfile", "-NonInteractive", "-Command", "Get-Clipboard"],
      { encoding: "utf8", windowsHide: true },
    ).stdout.trim();
    return clip === dataRootShown;
  });
  if (clip !== dataRootShown) fail(`clipboard content ${JSON.stringify(clip)} != shown dataRoot ${JSON.stringify(dataRootShown)}`);
  console.log(`ok [guards AC1]: settings copy button wrote ${JSON.stringify(clip)} to clipboard`);
  await page.keyboard.press("Escape");

  // (b) AC2: audio-only getUserMedia resolves, audio+video getUserMedia rejects
  const audioOk = await page.evaluate(async () => {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      for (const track of stream.getTracks()) track.stop();
      return true;
    } catch (e) {
      return String(e);
    }
  });
  if (audioOk !== true) fail(`getUserMedia({ audio: true }) failed: ${audioOk}`);

  const videoRejected = await page.evaluate(async () => {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true, video: true });
      for (const track of stream.getTracks()) track.stop();
      return false;
    } catch {
      return true;
    }
  });
  if (!videoRejected) fail("getUserMedia({ audio: true, video: true }) was unexpectedly granted");
  console.log("ok [guards AC2]: audio-only getUserMedia granted, audio+video rejected");

  // (c) AC3: window.open external http link creates no new CDP page in-app
  await page.evaluate(() => window.open("https://example.com/", "_blank"));
  await sleep(1000);
  const extPages = (b.contexts() ?? [])
    .flatMap((c) => c.pages())
    .filter((p) => p.url().includes("example.com"));
  if (extPages.length > 0) fail(`an in-app window was created for external URL: ${extPages.map((p) => p.url()).join(", ")}`);
  console.log("ok [guards AC3]: window.open external http denied in-app");

  // (d) AC4: 9 distinct same-origin /files/... URLs capped at 7 popups (8 windows total), navigation locked, dead IPC
  await page.evaluate(() => {
    for (let i = 1; i <= 9; i++) {
      window.open(`/files/lesson-${i}.html`, "_blank");
    }
  });

  await until("CDP pages under /files/ number exactly 7", 20_000, async () => {
    const list = await fetch(`http://127.0.0.1:${cdpPort}/json/list`).then((r) => r.json());
    const fileTargets = list.filter((t) => t.type === "page" && t.url.includes("/files/"));
    return fileTargets.length === 7;
  });

  const cdpList = await fetch(`http://127.0.0.1:${cdpPort}/json/list`).then((r) => r.json());
  const fileTargets = cdpList.filter((t) => t.type === "page" && t.url.includes("/files/"));
  if (fileTargets.length !== 7) fail(`expected exactly 7 /files/ popups, found ${fileTargets.length}`);
  const allPageTargets = cdpList.filter((t) => t.type === "page");
  if (allPageTargets.length !== 8) fail(`expected exactly 8 total windows (1 main + 7 popups), found ${allPageTargets.length}`);

  popupBrowser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
  const popupPages = (popupBrowser.contexts() ?? []).flatMap((c) => c.pages());
  const popup = popupPages.find((p) => p.url().includes("/files/"));
  if (!popup) fail("could not locate popup page over CDP");
  const lockedUrl = popup.url();
  await popup.evaluate(() => {
    location.href = "https://example.com/";
  });
  await sleep(1000);
  if (popup.url() !== lockedUrl) fail(`popup navigated away from ${lockedUrl} to ${popup.url()}`);

  const ipcDead = await popup.evaluate(async () => {
    const internals = window.__TAURI_INTERNALS__;
    if (!internals) return true;
    try {
      await internals.invoke("plugin:clipboard-manager|write_text", { text: "x" });
      return false;
    } catch {
      return true;
    }
  });
  if (!ipcDead) fail("popup IPC bridge is unexpectedly alive");
  console.log("ok [guards AC4]: 9 popup requests capped at 7 (8 windows total), navigation locked, IPC dead");
} finally {
  if (popupBrowser) await popupBrowser.close().catch(() => {});
  await cleanup({ ...handles, tmp, repo: tmp && path.join(tmp, "repo") });
  handles = undefined;
}

// (a) Window AC1: bounds + maximized persist across restarts in window.json;
// resize via SetWindowPos on foreground hwnd, close, verify JSON keys/values;
// relaunch, assert GetWindowRect matches saved bounds (logical * scale factor) and maximized restores.
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-window-ac1-"));
  const repo = path.join(tmp, "repo");
  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });
  await waitForWindowFocus(handles.child.pid);

  const scale = await handles.page.evaluate(() => window.devicePixelRatio);
  const targetLogical = { x: 120, y: 120, width: 1100, height: 750 };
  const physX = Math.round(targetLogical.x * scale);
  const physY = Math.round(targetLogical.y * scale);
  const physW = Math.round(targetLogical.width * scale);
  const physH = Math.round(targetLogical.height * scale);

  resizeForegroundWindow(physX, physY, physW, physH, handles.child.pid);
  await sleep(300);

  await closeAppThroughCdp(handles.page, handles.child.pid, repo);

  const windowJsonPath = path.join(tmp, "userdata", APP_ID, "window.json");
  if (!fs.existsSync(windowJsonPath)) fail(`window.json not found at ${windowJsonPath}`);
  const raw1 = fs.readFileSync(windowJsonPath, "utf8");
  const json1 = JSON.parse(raw1);
  const keys1 = Object.keys(json1);
  const expectedKeys = ["x", "y", "width", "height", "maximized", "chrome", "zoom"];
  if (JSON.stringify(keys1) !== JSON.stringify(expectedKeys)) {
    fail(`window.json keys ${JSON.stringify(keys1)} != ${JSON.stringify(expectedKeys)}`);
  }
  if (json1.maximized !== false) fail(`expected maximized:false, got ${json1.maximized}`);
  if (Math.abs(json1.width - targetLogical.width) > 4) fail(`saved width ${json1.width} != ${targetLogical.width}`);
  if (Math.abs(json1.height - targetLogical.height) > 4) fail(`saved height ${json1.height} != ${targetLogical.height}`);

  // Relaunch with same userdata: assert GetWindowRect matches saved bounds
  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });
  await waitForWindowFocus(handles.child.pid);

  const rect1 = getForegroundWindowRect(handles.child.pid);
  if (Math.abs(rect1.width - json1.width * scale) > 4) {
    fail(`GetWindowRect width ${rect1.width} != saved ${json1.width} * scale ${scale}`);
  }
  if (Math.abs(rect1.height - json1.height * scale) > 4) {
    fail(`GetWindowRect height ${rect1.height} != saved ${json1.height} * scale ${scale}`);
  }
  if (Math.abs(rect1.left - json1.x * scale) > 4) {
    fail(`GetWindowRect left ${rect1.left} != saved ${json1.x} * scale ${scale}`);
  }
  if (Math.abs(rect1.top - json1.y * scale) > 4) {
    fail(`GetWindowRect top ${rect1.top} != saved ${json1.y} * scale ${scale}`);
  }

  // Close and set maximized:true in window.json, then relaunch and assert restored maximized
  await closeAppThroughCdp(handles.page, handles.child.pid, repo);
  json1.maximized = true;
  fs.writeFileSync(windowJsonPath, JSON.stringify(json1), "utf8");

  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });
  await waitForWindowFocus(handles.child.pid);

  const maxRect = getForegroundWindowRect(handles.child.pid);
  if (!maxRect.maximized) fail("saved maximized:true did not restore maximized state");

  await closeAppThroughCdp(handles.page, handles.child.pid, repo);
  console.log("ok [window/AC1]: bounds and maximized persisted in window.json and restored across restarts");
} finally {
  await cleanup({ ...handles, tmp, repo: tmp && path.join(tmp, "repo") });
  handles = undefined;
}

// (b) Window AC2: pre-seeded legacy {"theme":"dark"} opens at 1440x920 inside primary monitor
// and close rewrites full schema with DEFAULT_CHROME; pre-seeded off-screen bounds degrade to on-screen.
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-window-ac2-"));
  const repo = path.join(tmp, "repo");
  const windowJsonPath = path.join(tmp, "userdata", APP_ID, "window.json");
  fs.mkdirSync(path.dirname(windowJsonPath), { recursive: true });
  fs.writeFileSync(windowJsonPath, JSON.stringify({ theme: "dark" }), "utf8");

  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });
  await waitForWindowFocus(handles.child.pid);

  const scale = await handles.page.evaluate(() => window.devicePixelRatio);
  const rect = getForegroundWindowRect(handles.child.pid);
  if (Math.abs(rect.width - 1440 * scale) > 4 || Math.abs(rect.height - 920 * scale) > 4) {
    fail(`default size ${rect.width}x${rect.height} != 1440x920 * scale ${scale}`);
  }
  if (rect.left < 0 || rect.top < 0) {
    fail(`window opened outside primary monitor: left=${rect.left}, top=${rect.top}`);
  }

  await closeAppThroughCdp(handles.page, handles.child.pid, repo);

  const rewritten = JSON.parse(fs.readFileSync(windowJsonPath, "utf8"));
  const expectedKeys = ["x", "y", "width", "height", "maximized", "chrome", "zoom"];
  if (JSON.stringify(Object.keys(rewritten)) !== JSON.stringify(expectedKeys)) {
    fail(`rewritten window.json keys ${JSON.stringify(Object.keys(rewritten))} != ${JSON.stringify(expectedKeys)}`);
  }
  if (JSON.stringify(rewritten.chrome) !== JSON.stringify(DEFAULT_CHROME)) {
    fail(`rewritten chrome ${JSON.stringify(rewritten.chrome)} != DEFAULT_CHROME ${JSON.stringify(DEFAULT_CHROME)}`);
  }

  // Pre-seed off-screen: degrades to on-screen keeping 1100x750 size (>= min_inner_size 1000x680)
  const offscreen = {
    x: -32000,
    y: -32000,
    width: 1100,
    height: 750,
    maximized: false,
    chrome: DEFAULT_CHROME,
    zoom: 1,
  };
  fs.writeFileSync(windowJsonPath, JSON.stringify(offscreen), "utf8");

  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });
  await waitForWindowFocus(handles.child.pid);

  const degRect = getForegroundWindowRect(handles.child.pid);
  if (degRect.left < -1000 || degRect.top < -1000) {
    fail(`window opened off-screen at ${degRect.left},${degRect.top}`);
  }
  if (Math.abs(degRect.width - 1100 * scale) > 4 || Math.abs(degRect.height - 750 * scale) > 4) {
    fail(`degraded window size ${degRect.width}x${degRect.height} != 1100x750 * scale ${scale}`);
  }

  await closeAppThroughCdp(handles.page, handles.child.pid, repo);
  console.log("ok [window/AC2]: legacy theme rewritten with DEFAULT_CHROME, off-screen bounds degraded on-screen");
} finally {
  await cleanup({ ...handles, tmp, repo: tmp && path.join(tmp, "repo") });
  handles = undefined;
}

// (c) Window AC3: invoke("set_chrome", ...) applies palette, topbar background matches
// the --background token, and window.json persists the lowercased payload.
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-window-ac3-"));
  const repo = path.join(tmp, "repo");
  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });

  const testPayload = {
    modePref: "dark",
    dark: { color: "#2B2C2D", symbolColor: "#A1A2A3" },
    light: { color: "#F1F2F3", symbolColor: "#616263" },
  };

  await handles.page.evaluate(async (chrome) => {
    await window.__TAURI_INTERNALS__.invoke("set_chrome", { chrome });
  }, testPayload);

  const { topbarBg, rootBg, backgroundToken } = await handles.page.evaluate(() => {
    const topbar = document.querySelector(".topbar");
    const root = document.documentElement;
    return {
      topbarBg: topbar ? getComputedStyle(topbar).backgroundColor : null,
      rootBg: getComputedStyle(root).backgroundColor,
      backgroundToken: getComputedStyle(root).getPropertyValue("--background").trim().toLowerCase(),
    };
  });
  if (!topbarBg) fail("topbar element not found in DOM");
  const hexToRgb = (hex) => {
    const m = hex.match(/^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i);
    return m ? `rgb(${parseInt(m[1], 16)}, ${parseInt(m[2], 16)}, ${parseInt(m[3], 16)})` : hex;
  };
  const rgbToken = hexToRgb(backgroundToken);
  const matches =
    topbarBg === backgroundToken ||
    topbarBg === rgbToken ||
    (topbarBg === "rgba(0, 0, 0, 0)" && rootBg === rgbToken);
  if (!matches) {
    fail(`topbar background ${topbarBg} does not match --background token ${backgroundToken} (root ${rootBg})`);
  }

  await closeAppThroughCdp(handles.page, handles.child.pid, repo);

  const windowJsonPath = path.join(tmp, "userdata", APP_ID, "window.json");
  const saved = JSON.parse(fs.readFileSync(windowJsonPath, "utf8"));
  if (
    saved.chrome?.modePref !== "dark" ||
    saved.chrome?.dark?.color !== "#2b2c2d" ||
    saved.chrome?.dark?.symbolColor !== "#a1a2a3" ||
    saved.chrome?.light?.color !== "#f1f2f3" ||
    saved.chrome?.light?.symbolColor !== "#616263"
  ) {
    fail(`window.json did not carry lowercased chrome payload: ${JSON.stringify(saved.chrome)}`);
  }
  console.log("ok [window/AC3]: set_chrome applied, topbar background matches --background, payload persisted lowercased");
} finally {
  await cleanup({ ...handles, tmp, repo: tmp && path.join(tmp, "repo") });
  handles = undefined;
}

// (d) Window AC4: invoke("set_zoom", { zoom: 2.5 }) applied (window.innerWidth shrinks by ≈2.5x)
// and persisted; invoke("set_zoom", { zoom: 0.1 }) clamped to 0.5 in window.json.
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-window-ac4-"));
  const repo = path.join(tmp, "repo");
  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });

  const initialWidth = await handles.page.evaluate(() => window.innerWidth);
  await handles.page.evaluate(async () => {
    await window.__TAURI_INTERNALS__.invoke("set_zoom", { zoom: 2.5 });
  });

  await until("zoom 2.5 applied", 5_000, async () => {
    const currentWidth = await handles.page.evaluate(() => window.innerWidth);
    return Math.abs(initialWidth / currentWidth - 2.5) < 0.2;
  });

  await closeAppThroughCdp(handles.page, handles.child.pid, repo);

  const windowJsonPath = path.join(tmp, "userdata", APP_ID, "window.json");
  const saved25 = JSON.parse(fs.readFileSync(windowJsonPath, "utf8"));
  if (Math.abs(saved25.zoom - 2.5) > 0.001) {
    fail(`expected zoom 2.5 in window.json, got ${saved25.zoom}`);
  }

  // Relaunch to test clamp of 0.1 to 0.5
  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });

  await handles.page.evaluate(async () => {
    await window.__TAURI_INTERNALS__.invoke("set_zoom", { zoom: 0.1 });
  });

  await closeAppThroughCdp(handles.page, handles.child.pid, repo);

  const saved01 = JSON.parse(fs.readFileSync(windowJsonPath, "utf8"));
  if (Math.abs(saved01.zoom - 0.5) > 0.001) {
    fail(`expected clamped zoom 0.5 in window.json, got ${saved01.zoom}`);
  }
  console.log("ok [window/AC4]: set_zoom 2.5 applied and persisted, 0.1 clamped to 0.5");
} finally {
  await cleanup({ ...handles, tmp, repo: tmp && path.join(tmp, "repo") });
  handles = undefined;
}

// (e) Window AC5: topbar has data-tauri-drag-region attribute over CDP.
try {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-tauri-smoke-window-ac5-"));
  const repo = path.join(tmp, "repo");
  handles = await launchApp(smokeEnv(tmp));
  await assertAppPage(handles.page, { ephemeral: true, hash: true });

  const hasDrag = await handles.page.evaluate(() => {
    const bar = document.querySelector(".topbar");
    return bar !== null && bar.hasAttribute("data-tauri-drag-region");
  });
  if (!hasDrag) fail("topbar does not have data-tauri-drag-region attribute");

  await closeAppThroughCdp(handles.page, handles.child.pid, repo);
  console.log("ok [window/AC5]: topbar has data-tauri-drag-region attribute");
} finally {
  await cleanup({ ...handles, tmp, repo: tmp && path.join(tmp, "repo") });
  handles = undefined;
}

console.log("tauri smoke passed");
