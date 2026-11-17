// Proxi desktop shell (Electron 45, ESM main). Spawns the `proxi` binary and shows the UI in a window.
// No top-level await: Electron waits for an ESM main to finish loading before it fires `ready`, so awaiting
// app.whenReady() at the top level deadlocks. Everything async lives in start().
import { app, BrowserWindow, clipboard, dialog, ipcMain, nativeTheme, screen, session, shell } from "electron";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { APP_ID, APP_NAME, APP_USER_MODEL_ID } from "../shared/identity.mjs";
import { killTreeSync, locateEngineBin, proxiBinaryPath, probeEngine, serveArgs, spawnBackend } from "./backend.mjs";
import { DEFAULT_CHROME, OVERLAY_HEIGHT, parseChrome, resolveChrome } from "./chrome.mjs";
import { startUrl, uiIsStale } from "./startup.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const env = process.env;
const repoRoot = path.resolve(env.LEARNER_REPO_ROOT || path.resolve(here, "../.."));
const appRoot = path.resolve(here, ".."); // the app/ dir (UI sources + build live here, not in LEARNER_REPO_ROOT)
let chrome = DEFAULT_CHROME; // last chrome the UI reported (both modes of its palette); saved with the window bounds
/** The chrome for the mode in effect now, plus the overlay height, ready for setTitleBarOverlay. */
const currentChrome = () => ({ ...resolveChrome(chrome, nativeTheme.shouldUseDarkColors), height: OVERLAY_HEIGHT });
let zoom = 1.0;
const ICON = path.join(here, "icon.png");

app.setName(APP_ID);
app.setAppUserModelId(APP_USER_MODEL_ID);
if (env.LEARNER_USER_DATA) app.setPath("userData", env.LEARNER_USER_DATA); // smoke test: do not fight the real instance

// Shortcuts start us with a minimal PATH: make sure `claude` (native installer / npm global) resolves.
const extra = [path.join(env.USERPROFILE || "", ".local", "bin"), path.join(env.APPDATA || "", "npm")].filter((p) => p && fs.existsSync(p));
env.PATH = [...extra, env.PATH].filter(Boolean).join(path.delimiter);

let srv;
let mainWin;
let quitting = false;
let appOrigin; // origin of the Proxi server the windows show; unset until it is known (deny every pop-up before)
const MAX_WINDOWS = 8;
const WEB_PREFS = { contextIsolation: true, nodeIntegration: false, sandbox: true };

async function ensureUi() {
  if (!uiIsStale(appRoot)) return;
  const c = currentChrome();
  const splash = new BrowserWindow({ width: 420, height: 220, frame: false, resizable: false, backgroundColor: c.color, icon: ICON, show: false });
  splash.setMenuBarVisibility(false);
  await splash.loadURL(
    "data:text/html;charset=utf-8," +
      encodeURIComponent(`<body style="margin:0;background:${c.color};color:${c.symbolColor};font:14px ui-monospace,Consolas,monospace;display:grid;place-items:center;height:100vh"><div>${APP_NAME}<br><span style="color:${c.symbolColor}">building the interface...</span></div></body>`),
  );
  splash.show();
  try {
    const { build } = await import("vite");
    await build({ configFile: path.join(appRoot, "web", "vite.config.ts"), logLevel: "warn" });
  } catch (e) {
    // A broken source tree must not lock the learner out: fall back to the last good build when there is one.
    if (!fs.existsSync(path.join(appRoot, "web", "dist", "index.html"))) throw e;
    console.error("[desktop] UI build failed, using the previous build:", e?.message || e);
  } finally {
    splash.destroy();
  }
}

// Copy buttons (GitHub device code, data folder path): the main process writes the clipboard whether or not the page has focus.

function boundsFile() {
  return path.join(app.getPath("userData"), "window.json");
}
function loadBounds() {
  try {
    const b = JSON.parse(fs.readFileSync(boundsFile(), "utf8"));
    const ok = screen.getAllDisplays().some((d) => {
      const a = d.workArea;
      return b.x < a.x + a.width && b.x + b.width > a.x && b.y < a.y + a.height && b.y + b.height > a.y;
    });
    return ok ? b : { width: b.width, height: b.height, zoom: b.zoom, chrome: b.chrome };
  } catch {
    return { width: 1440, height: 920 };
  }
}
/** The chrome saved with the window bounds; DEFAULT_CHROME when missing or invalid (also for the legacy `theme` key). */
function loadChrome() {
  try {
    return parseChrome(JSON.parse(fs.readFileSync(boundsFile(), "utf8")).chrome) ?? DEFAULT_CHROME;
  } catch {
    return DEFAULT_CHROME;
  }
}
function saveBounds(win) {
  try {
    fs.writeFileSync(boundsFile(), JSON.stringify({ ...win.getNormalBounds(), maximized: win.isMaximized(), chrome, zoom }));
  } catch {}
}

function openWindow(url, { main = false } = {}) {
  const { chrome: _c, theme: _t, zoom: savedZoom, ...b } = main ? loadBounds() : { width: 1100, height: 800 };
  if (typeof savedZoom === "number" && savedZoom >= 0.5 && savedZoom <= 3.0) zoom = savedZoom;
  const c = currentChrome();
  const win = new BrowserWindow({
    ...b,
    minWidth: 1000,
    minHeight: 680,
    backgroundColor: c.color,
    titleBarStyle: "hidden",
    titleBarOverlay: c,
    icon: ICON,
    show: false,
    autoHideMenuBar: true,
    // lesson pop-ups (LLM-authored HTML) get no bridge at all
    webPreferences: main ? { ...WEB_PREFS, preload: path.join(here, "preload.cjs") } : WEB_PREFS,
  });
  win.setMenuBarVisibility(false);
  win.once("ready-to-show", () => {
    if (main && b.maximized) win.maximize();
    if (main && zoom !== 1.0) win.webContents.setZoomFactor(zoom);
    win.show();
  });
  if (main) {
    win.webContents.on("did-finish-load", () => {
      if (zoom !== 1.0) win.webContents.setZoomFactor(zoom);
    });
    win.on("close", () => saveBounds(win));
    win.on("closed", () => {
      // the main window is the app: close lesson windows and quit
      for (const w of BrowserWindow.getAllWindows()) if (!w.isDestroyed()) w.destroy();
      app.quit();
    });
  }
  void win.loadURL(url);
  return win;
}

/** Recolours every open window (main + lesson pop-ups) to the chrome in effect now. */
function recolourAll() {
  const c = currentChrome();
  for (const w of BrowserWindow.getAllWindows()) {
    if (w.isDestroyed()) continue;
    try { w.setTitleBarOverlay(c); } catch {} // frameless windows (splash) have no overlay
    w.setBackgroundColor(c.color);
  }
}
ipcMain.on("ia:chrome", (e, payload) => {
  if (BrowserWindow.fromWebContents(e.sender) !== mainWin) return;
  const c = parseChrome(payload);
  if (!c) return;
  chrome = c;
  nativeTheme.themeSource = c.modePref; // the renderer's prefers-color-scheme and shouldUseDarkColors must agree
  recolourAll();
  if (mainWin && !mainWin.isDestroyed()) saveBounds(mainWin); // persist now: the next start paints the right colour even after a crash
});

// Copy buttons (GitHub device code, data folder path): the main process writes the clipboard whether or not the page has focus.
ipcMain.on("ia:copy", (e, t) => {
  if (BrowserWindow.fromWebContents(e.sender) !== mainWin || typeof t !== "string" || t.length > 10_000) return;
  clipboard.writeText(t);
});

ipcMain.on("ia:zoom", (e, z) => {
  const w = BrowserWindow.fromWebContents(e.sender);
  if (!w || w.isDestroyed() || w !== mainWin) return;
  if (typeof z === "number" && z >= 0.5 && z <= 3.0) {
    zoom = z;
    w.webContents.setZoomFactor(z);
  }
});

// Every webContents (app windows, lesson pop-ups, anything created later) gets the same guards: LLM-authored lesson HTML
// can open only /files/ pages of our own server (each in a guarded window of its own, capped), never navigate away.
app.on("web-contents-created", (_e, contents) => {
  contents.setWindowOpenHandler(({ url: u }) => {
    if (appOrigin && u.startsWith(appOrigin + "/files/") && BrowserWindow.getAllWindows().length < MAX_WINDOWS) openWindow(u);
    else if (/^https?:/i.test(u)) void shell.openExternal(u);
    return { action: "deny" };
  });
  // the UI flushes unsaved edits on pagehide and its beforeunload would otherwise cancel the close: let the unload proceed
  contents.on("will-prevent-unload", (e) => e.preventDefault());
  contents.on("will-navigate", (e, u) => {
    let origin;
    try { origin = new URL(u).origin; } catch {}
    if (origin !== appOrigin) {
      e.preventDefault();
      if (/^https?:/i.test(u)) void shell.openExternal(u);
    }
  });
});

async function isLearner(base) {
  try {
    const r = await fetch(base + "/api/settings", { signal: AbortSignal.timeout(2000) });
    return r.ok && typeof (await r.json())?.teacherModel === "string";
  } catch {
    return false;
  }
}

async function startServer() {
  const bin = proxiBinaryPath(repoRoot, env); // throws naming `cargo build --release --locked` when missing
  const wanted = env.LEARNER_PORT !== undefined ? Number(env.LEARNER_PORT) : 4317;
  const spawnOnce = (port) => spawnBackend(bin, serveArgs({ repoRoot, distDir: path.join(appRoot, "web", "dist"), port, env }), env);
  try {
    return await spawnOnce(wanted);
  } catch (e) {
    // The Rust listener fails before printing its URL on a busy port. If another Proxi holds it, just show
    // that window (a second server on the same .learner/ state would corrupt job records); only when
    // something else holds the port, take a free one.
    if (wanted !== 0) {
      const url = "http://127.0.0.1:" + wanted;
      if (await isLearner(url)) return { external: true, url };
      return await spawnOnce(0);
    }
    throw e;
  }
}

async function start() {
  if (!app.requestSingleInstanceLock()) return app.quit();
  app.on("second-instance", () => {
    if (!mainWin || mainWin.isDestroyed()) return;
    if (mainWin.isMinimized()) mainWin.restore();
    mainWin.focus();
  });
  await app.whenReady();
  chrome = loadChrome();
  nativeTheme.themeSource = chrome.modePref; // before any window: "system" users get the right colour on frame one
  const ses = session.defaultSession; // no camera/mic/notifications/etc. for any page
  const isAllowedOrigin = (urlStr) => {
    if (!urlStr) return false;
    try {
      const o = new URL(urlStr).origin;
      return appOrigin ? o === appOrigin : /^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?$/.test(o);
    } catch {
      return false;
    }
  };
  ses.setPermissionRequestHandler((wc, perm, cb, details) => {
    if (perm === "media") {
      const url = details?.requestingUrl || wc.getURL();
      const types = details?.mediaTypes;
      const audioOnly = Array.isArray(types) && types.includes("audio") && !types.includes("video");
      if (isAllowedOrigin(url) && audioOnly) return cb(true);
    }
    cb(false);
  });
  ses.setPermissionCheckHandler((wc, perm, requestingOrigin, details) => {
    if (perm === "media") {
      const types = details?.mediaTypes;
      const audioOnly = Array.isArray(types) && types.includes("audio") && !types.includes("video");
      if (isAllowedOrigin(requestingOrigin || wc.getURL()) && audioOnly) return true;
    }
    return false;
  });
  try {
    await ensureUi();
    srv = await startServer();
  } catch (e) {
    dialog.showErrorBox(`${APP_NAME} could not start`, String(e?.stack || e));
    return app.exit(1);
  }
  appOrigin = new URL(srv.url).origin;
  if (srv.external) {
    console.log("[desktop] " + APP_NAME + " already running at " + srv.url + ": showing it");
    const w = srv; srv = undefined; // not ours: never close it on quit
    mainWin = openWindow(w.url, { main: true });
    return;
  }
  process.on("exit", () => killTreeSync(srv.pid)); // sync: nothing async can run in the exit hook
  // The backend exiting on its own while we are not quitting is the self-update relaunch signal
  // (serve_cli exits after the relaunch notify). backend.mjs does not expose the close code.
  void srv.exited.then(() => {
    if (quitting) return;
    app.relaunch();
    app.quit();
  });
  let provider = "claude";
  try {
    provider = JSON.parse(fs.readFileSync(path.join(repoRoot, ".learner", "settings.json"), "utf8")).provider || "claude";
  } catch { /* default: claude */ }
  const bin =
    (provider === "opencode" ? env.LEARNER_OPENCODE_BIN : provider === "agy" ? env.LEARNER_AGY_BIN : env.LEARNER_CLAUDE_BIN) ||
    locateEngineBin(provider, env) ||
    (provider === "opencode" ? "opencode" : provider === "agy" ? "agy" : "claude");
  const rawArgs =
    provider === "opencode" ? env.LEARNER_OPENCODE_BIN_ARGS : provider === "agy" ? env.LEARNER_AGY_BIN_ARGS : env.LEARNER_CLAUDE_BIN_ARGS;
  const binArgs = rawArgs ? JSON.parse(rawArgs) : [];
  if (!(await probeEngine(bin, binArgs))) {
    const isOpencode = provider === "opencode";
    const isAgy = provider === "agy";
    dialog.showMessageBoxSync({
      type: "warning",
      title: APP_NAME,
      message: isOpencode ? "OpenCode was not found." : isAgy ? "Antigravity (agy) was not found." : "Claude Code was not found.",
      detail: isOpencode
        ? `The command "${bin}" did not run. Install OpenCode, run \`opencode\` once in a terminal to verify it, then reopen ${APP_NAME}. The app opens now, but lessons cannot start until then.`
        : isAgy
        ? `The command "${bin}" did not run. Install Antigravity (agy), run \`agy\` once in a terminal to log in with your Google account, then reopen ${APP_NAME}. The app opens now, but lessons cannot start until then.`
        : `The command "${bin}" did not run. Install Claude Code (https://claude.com/claude-code), run \`claude\` once in a terminal to log in, then reopen ${APP_NAME}. The app opens now, but lessons cannot start until then.`,
    });
  }
  mainWin = openWindow(startUrl(srv.url, env.LEARNER_OPEN_HASH), { main: true });
}

app.on("window-all-closed", () => { if (mainWin) app.quit(); }); // not before: the splash closing must not quit
app.on("before-quit", (e) => {
  if (quitting || !srv) return;
  e.preventDefault();
  quitting = true;
  const force = setTimeout(() => app.exit(0), 10000);
  killTreeSync(srv.pid); // the tree kill also takes down any engine child the backend spawned
  srv.close().catch(() => {}).finally(() => {
    clearTimeout(force);
    app.exit(0);
  });
});

start().catch((e) => {
  console.error(e);
  app.exit(1);
});
