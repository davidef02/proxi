// Web call sites of the guards ported to the Tauri shell (parity with app/desktop/main.mjs): copy goes through
// tauri-plugin-clipboard-manager with the 10 000-char bound at the call site, external links route through
// tauri-plugin-opener, the mic request stays audio-only. A recording __TAURI_INTERNALS__ shell stands in for the
// Tauri runtime; the call sites themselves are pinned against silent drift with source greps.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { after, test } from "node:test";
import { copyText } from "../web/src/clipboard.ts";
import { isTauri, openExternal } from "../web/src/desktop.ts";

const here = import.meta.dirname;
const root = path.join(here, "../..");

type Invoke = { cmd: string; args: Record<string, unknown> };
const calls: Invoke[] = [];
const opened: Array<{ url: string; target: string }> = [];
const electronCopies: string[] = [];
const clipboardWrites: string[] = [];

const realWindow = (globalThis as { window?: unknown }).window;
const realNavigator = globalThis.navigator;

// The web modules dereference the `window` identifier (absent in node), so the shell must exist before any call.
function setWindow(w: Record<string, unknown> | undefined): void {
  (globalThis as { window?: unknown }).window = w;
}

function tauriWindow(): Record<string, unknown> {
  return {
    __TAURI_INTERNALS__: {
      invoke: async (cmd: string, args: Record<string, unknown>) => {
        calls.push({ cmd, args });
        return null;
      },
    },
  };
}

function browserWindow(): Record<string, unknown> {
  return {
    open: (url: string, target: string) => {
      opened.push({ url, target });
      return {};
    },
  };
}

function clear(): void {
  calls.length = 0;
  opened.length = 0;
  electronCopies.length = 0;
  clipboardWrites.length = 0;
}

after(() => {
  setWindow(realWindow as Record<string, unknown> | undefined);
  Object.defineProperty(globalThis, "navigator", { value: realNavigator, configurable: true, writable: true });
});

test("isTauri keys on __TAURI_INTERNALS__ in window", () => {
  setWindow(tauriWindow());
  assert.equal(isTauri(), true);
  setWindow(browserWindow());
  assert.equal(isTauri(), false);
});

test("copyText in Tauri writes through the clipboard-manager plugin", async () => {
  clear();
  setWindow(tauriWindow());
  assert.equal(await copyText("hello"), true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].cmd, "plugin:clipboard-manager|write_text");
  assert.equal((calls[0].args as { text: string }).text, "hello");
});

test("copyText keeps the ia:copy 10000-char bound: at the bound ok, one char over rejected with no invoke", async () => {
  clear();
  setWindow(tauriWindow());
  assert.equal(await copyText("x".repeat(10_000)), true);
  assert.equal(calls.length, 1);
  clear();
  setWindow(tauriWindow());
  assert.equal(await copyText("x".repeat(10_001)), false);
  assert.equal(calls.length, 0);
});

test("copyText: the Electron bridge still wins, no plugin invoke", async () => {
  clear();
  const w = tauriWindow() as { iaDesktop?: { copyText: (t: string) => void } };
  w.iaDesktop = { copyText: (t) => electronCopies.push(t) };
  setWindow(w);
  assert.equal(await copyText("from-electron"), true);
  assert.deepEqual(electronCopies, ["from-electron"]);
  assert.equal(calls.length, 0);
});

test("copyText in a plain browser: navigator.clipboard serves, no plugin invoke", async () => {
  clear();
  setWindow(browserWindow());
  Object.defineProperty(globalThis, "navigator", {
    value: { clipboard: { writeText: async (t: string) => { clipboardWrites.push(t); } } },
    configurable: true,
  });
  assert.equal(await copyText("plain"), true);
  assert.deepEqual(clipboardWrites, ["plain"]);
  assert.equal(calls.length, 0);
});

test("openExternal in Tauri: opener plugin invoke, no in-app window.open", async () => {
  clear();
  setWindow(tauriWindow());
  await openExternal("https://example.com/connect");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].cmd, "plugin:opener|open_url");
  assert.equal((calls[0].args as { url: string }).url, "https://example.com/connect");
  assert.deepEqual(opened, []);
});

test("openExternal without Tauri: window.open(url, _blank), no plugin invoke", async () => {
  clear();
  setWindow(browserWindow());
  await openExternal("https://example.com/connect");
  assert.deepEqual(opened, [{ url: "https://example.com/connect", target: "_blank" }]);
  assert.equal(calls.length, 0);
});

test("Connect.tsx pins the Google consent on openExternal", () => {
  const src = fs.readFileSync(path.join(root, "app/web/src/shell/settings/Connect.tsx"), "utf8");
  assert.match(src, /void openExternal\(url\)/);
  assert.doesNotMatch(src, /window\.open\(/);
});

test("useDictation.ts pins an audio-only getUserMedia request", () => {
  const src = fs.readFileSync(path.join(root, "app/web/src/session/useDictation.ts"), "utf8");
  assert.match(src, /getUserMedia\(\{\s*\n?\s*audio: \{ echoCancellation/);
  assert.doesNotMatch(src, /audio: \{[^}]*\},\s*\n?\s*video/, "the Rust handler grants audio-only");
  assert.doesNotMatch(src, /video:/);
});

test("desktop capabilities and permissions pin the Tauri ACL", () => {
  const popup = JSON.parse(
    fs.readFileSync(path.join(root, "src-tauri/capabilities/popup.json"), "utf8"),
  ) as { identifier: string; windows: string[]; permissions: string[] };
  assert.equal(popup.identifier, "popup");
  assert.deepEqual(popup.permissions, [], "pop-ups get no IPC bridge");
  assert.deepEqual(popup.windows, ["lesson-*"]);

  const def = JSON.parse(
    fs.readFileSync(path.join(root, "src-tauri/capabilities/default.json"), "utf8"),
  ) as {
    windows: string[];
    permissions: unknown[];
    remote?: { urls: string[] };
  };
  assert.deepEqual(def.windows, ["main"]);
  assert.deepEqual(def.remote?.urls, [
    "http://127.0.0.1:*/*",
    "http://localhost:*/*",
  ]);
  assert.deepEqual(def.permissions, [
    "core:default",
    "clipboard-manager:allow-write-text",
    {
      identifier: "opener:allow-open-url",
      allow: [{ url: "http://*" }, { url: "https://*" }],
    },
    "core:window:allow-start-dragging",
    "window-controls-overlay:default",
  ]);

  assert.equal(
    fs.existsSync(path.join(root, "src-tauri/permissions/opener-http.toml")),
    false,
    "opener-http.toml removed in favor of capability scope",
  );
});
