// Theme system (#28) beyond the contrast gate: the pure colour maths, the derived palettes (side pane, mermaid,
// confidence rail), the desktop chrome payload edge cases, and source-level guarantees (no legacy theme bridge,
// no hard-coded palette colours in the consumers, no stray quiz literals in the lesson template).
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { LEGACY_KEY, MODE_KEY, PALETTE_KEY, PALETTE_LABEL, PALETTES } from "../web/src/palettes.ts";
import { composite, contrast, CONFIDENCE_COLORS, luminance, mermaidVars, mix, parseColor, planPalette, toHex } from "../web/src/theme-colors.ts";
import { regionLabelColor } from "../web/src/map/graph-canvas.ts";

const here = import.meta.dirname;
const read = (rel: string) => fs.readFileSync(path.join(here, rel), "utf8");
const chromeModule = () => import(pathToFileURL(path.join(here, "../desktop/chrome.mjs")).href);

/** Recursively lists files under `dir` with one of `exts`. */
function filesUnder(dir: string, exts: string[]): string[] {
  const out: string[] = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...filesUnder(p, exts));
    else if (exts.some((x) => e.name.endsWith(x))) out.push(p);
  }
  return out;
}

const near = (a: number, b: number, eps = 0.01) => assert.ok(Math.abs(a - b) <= eps, `${a} ≉ ${b}`);

test("palettes: five themes with the Italian names of the settings cards and independent storage keys", () => {
  assert.deepEqual([...PALETTES], ["neutral", "ocean", "grove", "ember", "iris"]);
  assert.deepEqual(PALETTES.map((p) => PALETTE_LABEL[p]), ["Neutro", "Oceano", "Bosco", "Brace", "Iris"]);
  assert.equal(MODE_KEY, "ia.mode");
  assert.equal(PALETTE_KEY, "ia.palette");
  assert.equal(LEGACY_KEY, "ia.theme");
  assert.notEqual(MODE_KEY, PALETTE_KEY);
});

test("parseColor: every accepted syntax, whitespace and case; rejects malformed input", () => {
  assert.deepEqual(parseColor("#fff"), { r: 255, g: 255, b: 255, a: 1 });
  assert.deepEqual(parseColor("  #0F1620 "), { r: 15, g: 22, b: 32, a: 1 });
  assert.deepEqual(parseColor("rgb(1 2 3)"), { r: 1, g: 2, b: 3, a: 1 });
  assert.deepEqual(parseColor("rgb(1, 2, 3)"), { r: 1, g: 2, b: 3, a: 1 });
  assert.deepEqual(parseColor("rgba(1, 2, 3, 0.5)"), { r: 1, g: 2, b: 3, a: 0.5 });
  assert.deepEqual(parseColor("rgb(1 2 3 / 25%)"), { r: 1, g: 2, b: 3, a: 0.25 });
  assert.equal(parseColor("rgba(1, 2, 3, 7)").a, 1, "alpha is clamped");
  for (const bad of ["", "red", "#ffff", "#12345", "#gggggg", "hsl(0 0% 0%)", "rgb(1 2)", "rgb(1 2 3 4 5)", "rgb(a b c)", "rgb(1 2 3 / x)", "var(--background)", "color-mix(in srgb, red, blue)"]) {
    assert.throws(() => parseColor(bad), /unsupported colour/, JSON.stringify(bad));
  }
});

test("colour maths: toHex, composite, mix, luminance and contrast match WCAG reference values", () => {
  assert.equal(toHex({ r: 255, g: 0, b: 15.4, a: 1 }), "#ff000f");
  assert.equal(toHex({ r: 300, g: -4, b: 0, a: 1 }), "#ff0000", "channels clamp");
  assert.equal(composite("rgb(0 0 0 / 0.5)", "#ffffff"), "#808080");
  assert.equal(composite("#123456", "#ffffff"), "#123456", "opaque top wins");
  assert.equal(mix("#000000", 0.5, "#ffffff"), "#808080");
  assert.equal(mix("#000000", 0, "#abcdef"), "#abcdef");
  assert.equal(mix("#000000", 1, "#abcdef"), "#000000");
  assert.equal(mix("#000000", 2, "#ffffff"), "#000000", "opacity clamps to 1");
  assert.equal(mix("rgb(0 0 0 / 0.5)", 1, "#ffffff"), "#808080", "own alpha multiplies the opacity");
  near(luminance("#ffffff"), 1, 1e-9);
  near(luminance("#000000"), 0, 1e-9);
  near(contrast("#000000", "#ffffff"), 21);
  near(contrast("#ffffff", "#000000"), 21, 1e-9);
  near(contrast("#777777", "#ffffff"), 4.48);
  near(contrast("#767676", "#ffffff"), 4.54);
  near(contrast("#abcdef", "#abcdef"), 1, 1e-9);
  // a translucent foreground is composited over the background before measuring
  near(contrast("rgb(0 0 0 / 0.5)", "#ffffff"), contrast("#808080", "#ffffff"), 1e-9);
});

const TOKENS: Record<string, string> = {
  "--background": "#131416",
  "--card": "#1b1c1f",
  "--muted": "#1b1c1f",
  "--accent": "#26272b",
  "--border": "#68696f",
  "--foreground": "#e7e5e1",
  "--muted-foreground": "#96938d",
  "--good": "#82bf86",
  "--info": "#84aee3",
  "--bad": "#e5806f",
};
const lookup = (n: string) => {
  const v = TOKENS[n];
  if (v === undefined) throw new Error("missing " + n);
  return v;
};

test("planPalette: status colours derive from the palette tokens, in legend order", () => {
  const p = planPalette(lookup);
  assert.deepEqual(Object.keys(p), ["done", "current", "missed", "todo"]);
  for (const [status, tok] of [["done", "--good"], ["current", "--info"], ["missed", "--bad"]] as const) {
    assert.equal(p[status].stroke, TOKENS[tok]);
    assert.equal(p[status].fill, mix(TOKENS[tok], 0.18, TOKENS["--card"]));
    assert.equal(p[status].color, TOKENS["--foreground"]);
  }
  assert.deepEqual(p.todo, { fill: TOKENS["--card"], stroke: TOKENS["--border"], color: TOKENS["--foreground"] });
  // another palette's tokens give another result: nothing is hard-coded
  const other = planPalette((n) => (n === "--card" ? "#ffffff" : lookup(n)));
  assert.notEqual(other.done.fill, p.done.fill);
});

test("mermaidVars: every variable comes from a token and darkMode follows the mode", () => {
  const dark = mermaidVars(lookup, "dark");
  const light = mermaidVars(lookup, "light");
  assert.equal(dark.darkMode, true);
  assert.equal(light.darkMode, false);
  const { darkMode: _d, ...vars } = dark;
  const values = new Set(Object.values(TOKENS));
  for (const [k, v] of Object.entries(vars)) assert.ok(values.has(v), `${k}=${v} is not a token value`);
  assert.equal(dark.background, TOKENS["--background"]);
  assert.equal(dark.primaryTextColor, TOKENS["--foreground"]);
  assert.equal(dark.lineColor, TOKENS["--muted-foreground"]);
});

test("CONFIDENCE_COLORS: six parseable stops per mode, distinct between modes", () => {
  for (const mode of ["dark", "light"] as const) {
    assert.equal(CONFIDENCE_COLORS[mode].length, 6, mode);
    for (const c of CONFIDENCE_COLORS[mode]) assert.match(c, /^#[0-9a-f]{6}$/);
  }
  assert.notDeepEqual(CONFIDENCE_COLORS.dark, CONFIDENCE_COLORS.light);
});

test("regionLabelColor: one hsla per mode, hue and alpha passed through", () => {
  const d = regionLabelColor(120, true, 0.9);
  const l = regionLabelColor(120, false, 0.95);
  assert.match(d, /^hsla\(120, 55%, \d+%, 0\.9\)$/);
  assert.match(l, /^hsla\(120, 55%, \d+%, 0\.95\)$/);
  const lightness = (s: string) => Number(/, (\d+)%, [\d.]+\)$/.exec(s)![1]);
  assert.ok(lightness(d) > 50 && lightness(l) < 50, "light text on dark, dark text on light");
  assert.equal(lightness(regionLabelColor(0, true, 1)), lightness(regionLabelColor(355, true, 1)), "lightness independent of hue");
});

test("parseChrome: rejects malformed pairs and ignores extra keys", async () => {
  const { parseChrome, DEFAULT_CHROME, NEUTRAL_CHROME } = await chromeModule();
  const pair = { color: "#000000", symbolColor: "#ffffff" };
  const ok = { modePref: "system", dark: pair, light: pair };
  assert.deepEqual(parseChrome({ ...ok, theme: "light", extra: 1 }), ok, "unknown keys dropped");
  assert.equal(parseChrome(undefined), null);
  assert.equal(parseChrome(42), null);
  assert.equal(parseChrome({}), null);
  assert.equal(parseChrome({ theme: "light" }), null, "legacy window.json `theme` key alone is not chrome");
  assert.equal(parseChrome({ ...ok, modePref: undefined }), null);
  assert.equal(parseChrome({ ...ok, dark: null }), null);
  assert.equal(parseChrome({ ...ok, dark: "#000000" }), null);
  assert.equal(parseChrome({ ...ok, dark: { color: "#000000" } }), null, "missing symbolColor");
  assert.equal(parseChrome({ ...ok, light: { color: 0, symbolColor: "#ffffff" } }), null, "non-string colour");
  assert.equal(parseChrome({ ...ok, light: { color: "#0000000", symbolColor: "#ffffff" } }), null, "7-digit hex");
  assert.equal(parseChrome({ ...ok, light: { color: "rgb(0 0 0)", symbolColor: "#ffffff" } }), null, "rgb() not accepted");
  assert.equal(parseChrome({ ...ok, light: { color: " #000000", symbolColor: "#ffffff" } }), null, "untrimmed");
  assert.deepEqual(parseChrome(DEFAULT_CHROME), DEFAULT_CHROME, "the default round-trips");
  assert.equal(DEFAULT_CHROME.modePref, "system", "fresh install follows the OS");
  assert.deepEqual({ dark: DEFAULT_CHROME.dark, light: DEFAULT_CHROME.light }, NEUTRAL_CHROME, "fresh install is Neutro");
  // parseChrome returns a new object: mutating the result cannot corrupt the input
  const input = structuredClone(ok);
  const out = parseChrome(input);
  out.dark.color = "#123456";
  assert.equal(input.dark.color, "#000000");
});

test("desktop: legacy theme bridge is gone, chrome IPC replaces it, the overlay is always 36 px", () => {
  const main = read("../desktop/main.mjs");
  const preload = read("../desktop/preload.cjs");
  for (const f of filesUnder(path.join(here, "../desktop"), [".mjs", ".cjs", ".js"])) {
    const src = fs.readFileSync(f, "utf8");
    for (const legacy of ["ia:theme", "setTheme", "CHROME["]) assert.ok(!src.includes(legacy), `${legacy} left in ${path.basename(f)}`);
  }
  assert.match(preload, /setChrome:\s*\(c\)\s*=>\s*ipcRenderer\.send\("ia:chrome", c\)/);
  assert.match(main, /ipcMain\.on\("ia:chrome"/);
  assert.match(main, /parseChrome\(payload\)/, "the renderer payload is validated");
  // every overlay goes through currentChrome(), which carries OVERLAY_HEIGHT
  assert.match(main, /height:\s*OVERLAY_HEIGHT/);
  assert.ok(!/height:\s*\d+\s*\}/.test(main.replace(/width: \d+, height: \d+/g, "")), "no literal overlay height");
  assert.ok(!/setTitleBarOverlay\((?!c\))/.test(main), "setTitleBarOverlay only with the resolved chrome");
  // startup: the persisted chrome and the OS-mode source are set before any window is created
  const start = main.slice(main.indexOf("async function start()"));
  const ready = start.indexOf("await app.whenReady()");
  const load = start.indexOf("chrome = loadChrome()");
  const source = start.indexOf("nativeTheme.themeSource = chrome.modePref");
  const firstWindow = Math.min(...["openWindow(", "ensureUi("].map((s) => start.indexOf(s)).filter((i) => i >= 0));
  assert.ok(Number.isFinite(firstWindow), "start() creates a window");
  assert.ok(ready >= 0 && load > ready && source > load && firstWindow > source, "chrome loaded before the first window");
});

test("web: consumers use the appearance store, no hard-coded palette colours, no legacy theme API", () => {
  const src = path.join(here, "../web/src");
  for (const f of filesUnder(src, [".ts", ".tsx"])) {
    const text = fs.readFileSync(f, "utf8");
    assert.ok(!/\buseTheme\b|\bThemePref\b|\bsetThemePref\b|\bsetTheme\b/.test(text), `legacy theme API in ${path.relative(src, f)}`);
  }
  for (const rel of ["session/Mermaid.tsx", "session/SidePane.tsx", "session/ConfidenceRail.tsx"]) {
    const text = read(path.join("../web/src", rel));
    assert.ok(!/#[0-9a-f]{3,8}\b/i.test(text), `hex literal in ${rel}`);
  }
  const update = read("../web/src/shell/settings/UpdateSection.tsx");
  assert.ok(!/\btext-white\b/.test(update), "UpdateSection button text follows the palette");
});

test("styles.css: <html> and orbs share the same token blocks, neutral has no palette block", () => {
  const css = read("../web/src/styles.css");
  assert.ok(!css.includes('[data-palette="neutral"]'), "neutral uses the [data-theme] blocks");
  for (const p of PALETTES.filter((x) => x !== "neutral")) {
    for (const m of ["dark", "light"]) {
      assert.ok(css.includes(`[data-palette="${p}"][data-theme="${m}"] {`), `${p}-${m} block`);
      assert.ok(!css.includes(`:root[data-palette="${p}"][data-theme="${m}"]`), `${p}-${m} must not be :root-only (orbs)`);
    }
  }
});

test("lesson template: quiz and error literals live only in the token blocks (or print)", () => {
  const html = read("../../assets/lesson-template.html");
  const css = html.slice(html.indexOf("<style>"), html.indexOf("</style>"));
  // drop `:root[data-theme="…"] { … }` blocks and `@media print { … }` (balanced braces)
  const dropBlocks = (s: string, head: RegExp) => {
    let out = s;
    for (let m = head.exec(out); m; m = head.exec(out)) {
      const open = out.indexOf("{", m.index);
      let depth = 0, j = open;
      for (; j < out.length; j++) {
        if (out[j] === "{") depth++;
        else if (out[j] === "}" && --depth === 0) break;
      }
      out = out.slice(0, m.index) + out.slice(j + 1);
      head.lastIndex = 0;
    }
    return out;
  };
  const rest = dropBlocks(dropBlocks(css, /:root\[data-theme="(light|dark)"\]\s*\{/g), /@media print\s*\{/g);
  for (const lit of ["#b91c1c", "#10b981", "#ef4444"]) assert.ok(!rest.toLowerCase().includes(lit), `${lit} outside the token blocks`);
  assert.match(rest, /\.katex-error\s*\{[^}]*var\(--quiz-err\)/, "KaTeX errors use the readable error token");
});
