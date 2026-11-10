// Proxi desktop shell (Electron 45, ESM main). Starts the Hono server in-process and shows the UI in a window.
// No top-level await: Electron waits for an ESM main to finish loading before it fires `ready`, so awaiting
// app.whenReady() at the top level deadlocks. Everything async lives in start().
import { app, BrowserWindow, clipboard, dialog, ipcMain, nativeTheme, safeStorage, screen, session, shell } from "electron";
import { execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { APP_ID, APP_NAME, APP_USER_MODEL_ID } from "../shared/identity.mjs";
import { DEFAULT_CHROME, OVERLAY_HEIGHT, parseChrome, resolveChrome } from "./chrome.mjs";

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
let killTreesSync = () => {};
let locateClaudeBinary;
let mainWin;
let quitting = false;
let appOrigin; // origin of the Proxi server the windows show; unset until it is known (deny every pop-up before)
const MAX_WINDOWS = 8;
const WEB_PREFS = { contextIsolation: true, nodeIntegration: false, sandbox: true };

function newestMtime(dir) {
  let t = 0;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    t = Math.max(t, e.isDirectory() ? newestMtime(p) : fs.statSync(p).mtimeMs);
  }
  return t;
}

function uiIsStale() {
  const index = path.join(appRoot, "web", "dist", "index.html");
  if (!fs.existsSync(index)) return true;
  const built = fs.statSync(index).mtimeMs;
  return [path.join(appRoot, "web", "src"), path.join(appRoot, "shared"), path.join(appRoot, "web", "index.html")].some(
    (p) => fs.existsSync(p) && (fs.statSync(p).isDirectory() ? newestMtime(p) : fs.statSync(p).mtimeMs) > built,
  );
}

async function ensureUi() {
  if (!uiIsStale()) return;
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

function probeEngine(bin, binArgs) {
  return new Promise((resolve) => {
    execFile(bin, [...binArgs, "--version"], { timeout: 15000, windowsHide: true, shell: process.platform === "win32" }, (err) => resolve(!err));
  });
}

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

/** Token cipher for <repo>/.learner/secrets.json (see app/server/sync/secrets.ts). Keys live in the OS keychain.
 *  Call only after app.whenReady(). */
function secretCipher() {
  const asyncApi = typeof safeStorage.isAsyncEncryptionAvailable === "function";
  return {
    available: async () => {
      // Linux without a keyring: safeStorage would use a hard-coded key, so treat it as unavailable.
      if (process.platform === "linux" && safeStorage.getSelectedStorageBackend?.() === "basic_text") return false;
      return asyncApi ? await safeStorage.isAsyncEncryptionAvailable() : safeStorage.isEncryptionAvailable();
    },
    encrypt: async (plain) => {
      const buf = asyncApi ? await safeStorage.encryptStringAsync(plain) : safeStorage.encryptString(plain);
      return Buffer.from(buf).toString("base64");
    },
    decrypt: async (b64) => {
      const buf = Buffer.from(b64, "base64");
      if (typeof safeStorage.decryptStringAsync !== "function") return safeStorage.decryptString(buf);
      const r = await safeStorage.decryptStringAsync(buf);
      return typeof r === "string" ? r : r.result;
    },
  };
}

async function startServer() {
  // tsx's ESM loader works in Electron's main process (verified); it lets us import the .ts server directly.
  const { register } = await import("tsx/esm/api");
  register();
  const mod = await import(pathToFileURL(path.join(appRoot, "server", "main.ts")).href);
  killTreesSync = mod.killTreesSync;
  locateClaudeBinary = mod.locateClaudeBinary;
  const claudeBin = env.LEARNER_CLAUDE_BIN || undefined;
  const claudeBinArgs = env.LEARNER_CLAUDE_BIN_ARGS ? JSON.parse(env.LEARNER_CLAUDE_BIN_ARGS) : undefined;
  const agyBin = env.LEARNER_AGY_BIN || undefined;
  const agyBinArgs = env.LEARNER_AGY_BIN_ARGS ? JSON.parse(env.LEARNER_AGY_BIN_ARGS) : undefined;
  const opencodeBin = env.LEARNER_OPENCODE_BIN || undefined;
  const opencodeBinArgs = env.LEARNER_OPENCODE_BIN_ARGS ? JSON.parse(env.LEARNER_OPENCODE_BIN_ARGS) : undefined;
  const base = {
    repoRoot, claudeBin, claudeBinArgs, agyBin, agyBinArgs, opencodeBin, opencodeBinArgs, openBrowser: false,
    addDirs: env.LEARNER_ADD_DIRS ? JSON.parse(env.LEARNER_ADD_DIRS) : undefined,
    relaunch: () => { app.relaunch(); app.quit(); },
    secretCipher: secretCipher(),
  };
  const wanted = env.LEARNER_PORT !== undefined ? Number(env.LEARNER_PORT) : 4317;
  try {
    return await mod.createServer({ ...base, port: wanted });
  } catch (e) {
    if (e?.code !== "EADDRINUSE" || wanted === 0) throw e;
    // Busy port: if it is another Proxi (npm run app) just show it: a second server on the same .learner/ state
    // would corrupt job records. Only when something else holds the port, take a free one.
    const url = "http://127.0.0.1:" + wanted;
    if (await isLearner(url)) return { external: true, url };
    return await mod.createServer({ ...base, port: 0 });
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
  process.on("exit", () => killTreesSync([...srv.manager.livePids(), ...srv.jobs.livePids()]));
  let provider = "claude";
  try {
    provider = JSON.parse(fs.readFileSync(path.join(repoRoot, ".learner", "settings.json"), "utf8")).provider || "claude";
  } catch { /* default: claude */ }
  const bin =
    (provider === "opencode" ? env.LEARNER_OPENCODE_BIN : provider === "agy" ? env.LEARNER_AGY_BIN : env.LEARNER_CLAUDE_BIN) ||
    (provider === "opencode" ? "opencode" : provider === "agy" ? "agy" : (locateClaudeBinary?.() || "claude"));
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
  mainWin = openWindow(srv.url, { main: true });
}

app.on("window-all-closed", () => { if (mainWin) app.quit(); }); // not before: the splash closing must not quit
app.on("before-quit", (e) => {
  if (quitting || !srv) return;
  e.preventDefault();
  quitting = true;
  const force = setTimeout(() => app.exit(0), 10000);
  srv.close().catch(() => {}).finally(() => {
    clearTimeout(force);
    app.exit(0);
  });
});

start().catch((e) => {
  console.error(e);
  app.exit(1);
});
