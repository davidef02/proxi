// Parity and transport matrix for desktop window features (chrome, zoom, clipboard, drag region).
// Verifies Tauri vs Electron bridge vs plain browser transports and pins source contracts.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { after, test } from "node:test";
import { pathToFileURL } from "node:url";
import { copyText, setChrome, setZoom } from "../web/src/desktop.ts";
import { type ChromePayload } from "../web/src/palettes.ts";

const here = import.meta.dirname;
const root = path.join(here, "../..");
const chromeModule = () => import(pathToFileURL(path.join(here, "../shared/chrome.mjs")).href);

type Invoke = { cmd: string; args: Record<string, unknown> };
const calls: Invoke[] = [];
const bridgeChrome: unknown[] = [];
const bridgeZoom: number[] = [];
const bridgeCopy: string[] = [];
const clipboardWrites: string[] = [];

const realWindow = (globalThis as { window?: unknown }).window;
const realNavigator = globalThis.navigator;

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

function electronWindow(): Record<string, unknown> {
  return {
    iaDesktop: {
      setChrome: (c: unknown) => {
        bridgeChrome.push(c);
      },
      setZoom: (z: number) => {
        bridgeZoom.push(z);
      },
      copyText: (t: string) => {
        bridgeCopy.push(t);
      },
    },
  };
}

function browserWindow(): Record<string, unknown> {
  return {};
}

function clear(): void {
  calls.length = 0;
  bridgeChrome.length = 0;
  bridgeZoom.length = 0;
  bridgeCopy.length = 0;
  clipboardWrites.length = 0;
}

async function flush(): Promise<void> {
  await new Promise((r) => setImmediate(r));
  await new Promise((r) => setImmediate(r));
}

after(() => {
  setWindow(realWindow as Record<string, unknown> | undefined);
  Object.defineProperty(globalThis, "navigator", { value: realNavigator, configurable: true, writable: true });
});

const sampleChrome: ChromePayload = {
  modePref: "system",
  dark: { color: "#131416", symbolColor: "#96938d" },
  light: { color: "#f6f4ef", symbolColor: "#6c675e" },
};

// Transport matrix: Tauri
test("transport matrix: Tauri invokes set_chrome, set_zoom, and clipboard plugin with no bridge call", async () => {
  clear();
  setWindow(tauriWindow());

  await setChrome(sampleChrome);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].cmd, "set_chrome");
  assert.deepEqual(calls[0].args, { chrome: sampleChrome });
  assert.equal(bridgeChrome.length, 0);

  clear();
  const zoomHandled = setZoom(1.25);
  assert.equal(zoomHandled, true);
  await flush();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].cmd, "set_zoom");
  assert.deepEqual(calls[0].args, { zoom: 1.25 });
  assert.equal(bridgeZoom.length, 0);

  clear();
  const copyHandled = await copyText("hello-tauri");
  assert.equal(copyHandled, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].cmd, "plugin:clipboard-manager|write_text");
  assert.equal((calls[0].args as unknown as { text: string }).text, "hello-tauri");
  assert.equal(bridgeCopy.length, 0);
});

// Transport matrix: Electron bridge present
test("transport matrix: Electron bridge present calls iaDesktop with no invoke", async () => {
  clear();
  setWindow(electronWindow());

  await setChrome(sampleChrome);
  assert.deepEqual(bridgeChrome, [sampleChrome]);
  assert.equal(calls.length, 0);

  clear();
  const zoomHandled = setZoom(1.5);
  assert.equal(zoomHandled, true);
  await flush();
  assert.deepEqual(bridgeZoom, [1.5]);
  assert.equal(calls.length, 0);

  clear();
  const copyHandled = await copyText("hello-electron");
  assert.equal(copyHandled, true);
  assert.deepEqual(bridgeCopy, ["hello-electron"]);
  assert.equal(calls.length, 0);
});

// Transport matrix: plain browser
test("transport matrix: plain browser setZoom returns false, setChrome resolves with no invoke, copyText falls back to navigator.clipboard", async () => {
  clear();
  setWindow(browserWindow());
  Object.defineProperty(globalThis, "navigator", {
    value: {
      clipboard: {
        writeText: async (t: string) => {
          clipboardWrites.push(t);
        },
      },
    },
    configurable: true,
  });

  await setChrome(sampleChrome);
  assert.equal(calls.length, 0);
  assert.equal(bridgeChrome.length, 0);

  const zoomHandled = setZoom(1.1);
  assert.equal(zoomHandled, false);
  await flush();
  assert.equal(calls.length, 0);
  assert.equal(bridgeZoom.length, 0);

  const copyHandled = await copyText("hello-browser");
  assert.equal(copyHandled, true);
  assert.deepEqual(clipboardWrites, ["hello-browser"]);
  assert.equal(calls.length, 0);
  assert.equal(bridgeCopy.length, 0);
});

// Both desktop rows return true for setZoom
test("both desktop rows return true for setZoom", async () => {
  setWindow(tauriWindow());
  assert.equal(setZoom(1.0), true);
  await flush();

  setWindow(electronWindow());
  assert.equal(setZoom(1.0), true);
  await flush();

  setWindow(browserWindow());
  assert.equal(setZoom(1.0), false);
});

// setZoom(2.5) sends { zoom: 2.5 } unclamped
test("setZoom(2.5) sends { zoom: 2.5 } unclamped to the shell command", async () => {
  clear();
  setWindow(tauriWindow());
  setZoom(2.5);
  await flush();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].cmd, "set_zoom");
  assert.deepEqual(calls[0].args, { zoom: 2.5 });
});

// Source pins
test("source pins: Shell.tsx header has data-tauri-drag-region", () => {
  const src = fs.readFileSync(path.join(root, "app/web/src/shell/Shell.tsx"), "utf8");
  assert.match(src, /<header[^>]*data-tauri-drag-region/);
});

test("source pins: styles.css keeps the two app-region lines", () => {
  const src = fs.readFileSync(path.join(root, "app/web/src/styles.css"), "utf8");
  assert.match(src, /\.topbar\s*\{\s*-webkit-app-region:\s*drag;\s*\}/);
  assert.match(src, /\.topbar\s*:is\([^)]*\)\s*\{\s*-webkit-app-region:\s*no-drag;\s*\}/);
});

test("source pins: theme.ts and zoom.ts no longer call window.iaDesktop for chrome/zoom", () => {
  const themeSrc = fs.readFileSync(path.join(root, "app/web/src/theme.ts"), "utf8");
  assert.doesNotMatch(themeSrc, /iaDesktop/);

  const zoomSrc = fs.readFileSync(path.join(root, "app/web/src/zoom.ts"), "utf8");
  assert.doesNotMatch(zoomSrc, /iaDesktop/);
});

test("source pins: clipboard.ts re-exports from desktop.ts", () => {
  const clipboardSrc = fs.readFileSync(path.join(root, "app/web/src/clipboard.ts"), "utf8");
  assert.match(clipboardSrc, /export\s*\{\s*copyText\s*\}\s*from\s*["']\.\/desktop(?:\.ts)?["'];/);
});

test("source pins: src-tauri/src/window.rs matches shared chrome and window state contracts", async () => {
  const windowRs = fs.readFileSync(path.join(root, "src-tauri/src/window.rs"), "utf8");
  const { DEFAULT_CHROME, OVERLAY_HEIGHT } = await chromeModule();

  // OVERLAY_HEIGHT equals app/shared/chrome.mjs's OVERLAY_HEIGHT (36)
  assert.equal(OVERLAY_HEIGHT, 36);
  const overlayMatch = /pub\s+const\s+OVERLAY_HEIGHT:\s*u32\s*=\s*(\d+);/.exec(windowRs);
  assert.ok(overlayMatch, "OVERLAY_HEIGHT constant in window.rs");
  assert.equal(Number(overlayMatch[1]), OVERLAY_HEIGHT);

  // DEFAULT_CHROME equals the shared module's
  assert.deepEqual(DEFAULT_CHROME, {
    modePref: "system",
    dark: { color: "#131416", symbolColor: "#96938d" },
    light: { color: "#f6f4ef", symbolColor: "#6c675e" },
  });
  assert.match(windowRs, /pub\s+const\s+DEFAULT_CHROME:\s*Chrome\s*=\s*Chrome\s*\{/);
  assert.match(windowRs, /mode_pref:\s*Cow::Borrowed\("system"\)/);
  assert.match(windowRs, /color:\s*Cow::Borrowed\("#131416"\)/);
  assert.match(windowRs, /symbol_color:\s*Cow::Borrowed\("#96938d"\)/);
  assert.match(windowRs, /color:\s*Cow::Borrowed\("#f6f4ef"\)/);
  assert.match(windowRs, /symbol_color:\s*Cow::Borrowed\("#6c675e"\)/);

  // MIN_ZOOM/MAX_ZOOM are 0.5/3.0
  const minZoomMatch = /pub\s+const\s+MIN_ZOOM:\s*f64\s*=\s*([0-9.]+);/.exec(windowRs);
  const maxZoomMatch = /pub\s+const\s+MAX_ZOOM:\s*f64\s*=\s*([0-9.]+);/.exec(windowRs);
  assert.ok(minZoomMatch, "MIN_ZOOM constant in window.rs");
  assert.ok(maxZoomMatch, "MAX_ZOOM constant in window.rs");
  assert.equal(Number(minZoomMatch[1]), 0.5);
  assert.equal(Number(maxZoomMatch[1]), 3.0);

  // SaveState field order is x,y,width,height,maximized,chrome,zoom
  const structMatch = /pub\s+struct\s+SaveState\s*\{([^}]+)\}/.exec(windowRs);
  assert.ok(structMatch, "SaveState struct found in window.rs");
  const fields = [...structMatch[1].matchAll(/pub\s+([a-zA-Z0-9_]+)\s*:/g)].map((m) => m[1]);
  assert.deepEqual(fields, ["x", "y", "width", "height", "maximized", "chrome", "zoom"]);
});
