// Theme gate: every colour pair the UI draws must reach WCAG AA in all 10 palette × mode combinations, the
// token blocks of every combo must declare the same properties, the lesson template pairs must pass in both
// modes, and the desktop chrome constants must equal the Neutro tokens they mirror.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { PALETTES, readModePref, readPalette, type Mode, type Palette } from "../web/src/palettes.ts";
import { contrast, mix, parseColor, planPalette, CONFIDENCE_COLORS } from "../web/src/theme-colors.ts";
import { regionLabelColor } from "../web/src/map/graph-canvas.ts";

const here = import.meta.dirname;
const TEXT = 4.5;
const UI = 3.0;
const MODES: Mode[] = ["dark", "light"];
const REGION_HUES = Array.from({ length: 72 }, (_, i) => i * 5);

type Rule = { selectors: string[]; decls: Map<string, string> };
type Lookup = (name: string) => string;

/** Top-level rules of a stylesheet: comments stripped, @-rules skipped, selectors split on ",", declarations
 *  split on ";" (only the first ":" separates property and value). */
function stripComments(css: string): string {
  let out = "";
  let quote = "";
  for (let i = 0; i < css.length; i++) {
    const ch = css[i];
    if (quote) {
      out += ch;
      if (ch === quote) quote = "";
    } else if (ch === '"' || ch === "'") {
      quote = ch;
      out += ch;
    } else if (ch === "/" && css[i + 1] === "*") {
      const end = css.indexOf("*/", i + 2);
      i = end < 0 ? css.length : end + 1;
    } else {
      out += ch;
    }
  }
  return out;
}

function topLevelRules(css: string): Rule[] {
  const src = stripComments(css);
  const n = src.length;
  const rules: Rule[] = [];
  const skipBalanced = (open: number): number => {
    let depth = 0;
    for (let j = open; j < n; j++) {
      if (src[j] === "{") depth++;
      else if (src[j] === "}" && --depth === 0) return j + 1;
    }
    return n;
  };
  let i = 0;
  while (i < n) {
    while (i < n && /\s/.test(src[i])) i++;
    if (i >= n) break;
    if (src[i] === "@") {
      let j = i;
      while (j < n && src[j] !== ";" && src[j] !== "{") j++;
      i = j < n && src[j] === "{" ? skipBalanced(j) : j + 1;
      continue;
    }
    const open = src.indexOf("{", i);
    if (open < 0) break;
    const selectors = src
      .slice(i, open)
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const close = skipBalanced(open);
    const body = src.slice(open + 1, close - 1);
    i = close;
    const decls = new Map<string, string>();
    for (const d of body.split(";")) {
      const colon = d.indexOf(":");
      if (colon < 0) continue;
      decls.set(d.slice(0, colon).trim(), d.slice(colon + 1).trim());
    }
    rules.push({ selectors, decls });
  }
  return rules;
}

/** The single rule whose selector list contains `selector` exactly (never a substring match). */
function blockFor(rules: Rule[], selector: string): Map<string, string> {
  const hits = rules.filter((r) => r.selectors.includes(selector));
  assert.equal(hits.length, 1, `expected exactly one block for ${selector}, found ${hits.length}`);
  return hits[0].decls;
}

function lookupOf(decls: Map<string, string>, combo: string): Lookup {
  return (name) => {
    const value = decls.get(name);
    if (value === undefined) throw new Error(`missing ${name} in ${combo}`);
    return value;
  };
}

const APP_CSS = fs.readFileSync(path.join(here, "../web/src/styles.css"), "utf8");
const APP_RULES = topLevelRules(APP_CSS);
const TEMPLATE_HTML = fs.readFileSync(path.join(here, "../../assets/lesson-template.html"), "utf8");
const TEMPLATE_CSS = TEMPLATE_HTML.slice(
  TEMPLATE_HTML.indexOf("<style>") + "<style>".length,
  TEMPLATE_HTML.indexOf("</style>"),
);
const TEMPLATE_RULES = topLevelRules(TEMPLATE_CSS);

function appBlock(palette: Palette | "neutral", mode: Mode): { combo: string; v: Lookup; decls: Map<string, string> } {
  const selector = palette === "neutral" ? `[data-theme="${mode}"]` : `[data-palette="${palette}"][data-theme="${mode}"]`;
  const combo = `${palette}-${mode}`;
  const decls = blockFor(APP_RULES, selector);
  return { combo, v: lookupOf(decls, combo), decls };
}

const COMBOS: { palette: Palette; mode: Mode }[] = PALETTES.flatMap((palette) => MODES.map((mode) => ({ palette, mode })));

// A colour in a pair: its label (for failure messages) and its resolved opaque-or-alpha value.
type Col = { name: string; value: string };
type Pair = { fg: Col; bg: Col; min: number };

const col = (name: string, value: string): Col => ({ name, value });

/** Parses the hsla() string produced by regionLabelColor back into hue/saturation/lightness/alpha. */
function parseHsla(s: string): { h: number; sat: number; light: number; alpha: number } {
  const m = /^hsla\(([\d.]+), ([\d.]+)%, ([\d.]+)%, ([\d.]+)\)$/.exec(s);
  if (!m) throw new Error("unexpected label colour: " + s);
  return { h: Number(m[1]), sat: Number(m[2]) / 100, light: Number(m[3]) / 100, alpha: Number(m[4]) };
}

function hslToHex(h: number, s: number, l: number): string {
  const a = s * Math.min(l, 1 - l);
  const channel = (n: number) => {
    const k = (n + h / 30) % 12;
    const c = l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(c * 255).toString(16).padStart(2, "0");
  };
  return `#${channel(0)}${channel(8)}${channel(4)}`;
}

/** Every pair of the app's contract table (Design section, "Contrast pair tables") for one combo. */
function appPairs(palette: Palette, mode: Mode): Pair[] {
  const { v } = appBlock(palette, mode);
  const tok = (n: string) => col(n, v(`--${n}`));
  const mixed = (n: string, p: number, over: string) => col(`mix(${n}, ${p}, ${over})`, mix(v(`--${n}`), p, v(`--${over}`)));
  const overlay = (n: string, over: string) => col(`mix(${n} over ${over})`, mix(v(`--${n}`), 1, v(`--${over}`)));
  const bg = tok("background");
  const card = tok("card");
  const out: Pair[] = [];
  const add = (fg: Col, back: Col, min: number) => out.push({ fg, bg: back, min });

  for (const f of ["foreground", "heading", "card-foreground", "muted-foreground"]) {
    for (const b of ["background", "card", "muted"]) add(tok(f), tok(b), TEXT);
  }
  add(tok("foreground"), tok("accent"), TEXT);
  add(tok("heading"), tok("accent"), TEXT);
  add(tok("primary-foreground"), tok("primary"), TEXT);
  for (const f of ["good", "warn", "bad", "info", "brand", "motivate", "connect", "truth", "derive", "quiz", "syn-keyword", "syn-attr"]) {
    add(tok(f), bg, TEXT);
    add(tok(f), card, TEXT);
  }
  add(bg, tok("info"), TEXT);
  add(tok("foreground"), overlay("mark", "background"), TEXT);
  add(tok("foreground"), overlay("mark", "card"), TEXT);
  add(tok("foreground"), overlay("selection", "background"), TEXT);
  for (const n of ["truth", "derive"]) add(tok(n), mixed(n, 0.15, "card"), TEXT);
  add(tok("warn"), mixed("warn", 0.1, "card"), TEXT);
  add(tok("foreground"), mixed("warn", 0.1, "card"), TEXT);
  for (const n of ["motivate", "connect", "quiz"]) add(tok(n), mixed(n, 0.09, "card"), TEXT);
  for (const n of ["truth", "derive"]) add(tok(n), mixed(n, 0.12, "card"), TEXT);
  for (const n of ["good", "warn", "bad", "info"]) add(tok(n), mixed(n, 0.09, "background"), TEXT);

  const plan = planPalette(v);
  for (const status of ["done", "current", "missed", "todo"] as const) {
    add(col(`plan ${status} colour`, plan[status].color), col(`plan ${status} fill`, plan[status].fill), TEXT);
    add(col(`plan ${status} stroke`, plan[status].stroke), col(`plan ${status} fill`, plan[status].fill), UI);
  }

  for (const h of REGION_HUES) {
    const { h: hue, sat, light, alpha } = parseHsla(regionLabelColor(h, mode === "dark", 0.9));
    const label = mix(hslToHex(hue, sat, light), alpha, v("--background"));
    add(col(`map label h=${h}`, label), bg, TEXT);
  }

  for (const n of ["border", "input", "primary", "brand"]) {
    add(tok(n), bg, UI);
    add(tok(n), card, UI);
  }
  CONFIDENCE_COLORS[mode].forEach((hex, i) => add(col(`confidence ${i}`, hex), bg, UI));
  return out;
}

/** Every pair of the lesson template (Design section, "Lesson template" table) for one of its two blocks. */
function templatePairs(mode: Mode): { combo: string; pairs: Pair[] } {
  const selector = `:root[data-theme="${mode}"]`;
  const decls = blockFor(TEMPLATE_RULES, selector);
  const v = lookupOf(decls, `template-${mode}`);
  const tok = (n: string) => col(n, v(`--${n}`));
  const mixedRgb = (rgb: string, p: number, over: string) => col(`mix(${rgb}, ${p}, ${over})`, mix(rgb, p, v(`--${over}`)));
  const out: Pair[] = [];
  const add = (fg: Col, bg: Col, min: number) => out.push({ fg, bg, min });

  for (const f of ["text-primary", "text-secondary", "text-muted", "text-link"]) {
    for (const b of ["bg-canvas", "bg-surface", "bg-surface-elevated", "bg-subtle", "bg-code"]) add(tok(f), tok(b), TEXT);
  }
  add(tok("step-motivate-text"), tok("step-motivate-bg"), TEXT);
  add(tok("step-connect-text"), tok("step-connect-bg"), TEXT);
  add(tok("step-quiz-text"), tok("step-quiz-bg"), TEXT);
  add(tok("badge-axiom-text"), tok("badge-axiom-bg"), TEXT);
  add(tok("badge-derivation-text"), tok("badge-derivation-bg"), TEXT);
  add(tok("quiz-ok"), tok("bg-surface"), TEXT);
  add(tok("quiz-err"), tok("bg-surface"), TEXT);
  add(tok("step-connect-text"), mixedRgb("rgb(16 185 129)", 0.1, "bg-surface"), TEXT);
  add(tok("quiz-err"), mixedRgb("rgb(239 68 68)", 0.1, "bg-surface"), TEXT);
  add(tok("quiz-ok-on"), tok("quiz-ok"), TEXT);
  add(tok("quiz-err-on"), tok("quiz-err"), TEXT);
  add(tok("text-primary"), tok("step-motivate-bg"), TEXT);
  return { combo: `template-${mode}`, pairs: out };
}

/** Runs every pair and returns the failures, each as "combo: fg / bg = 3.21 < 4.5". */
function failuresOf(combo: string, pairs: Pair[]): string[] {
  const failures: string[] = [];
  for (const { fg, bg, min } of pairs) {
    const ratio = contrast(fg.value, bg.value);
    if (ratio < min) failures.push(`${combo}: ${fg.name} / ${bg.name} = ${ratio.toFixed(2)} < ${min}`);
  }
  return failures;
}

test("parity: every combo declares the same custom properties as neutral dark, and color-scheme matches the mode", () => {
  const reference = [...blockFor(APP_RULES, '[data-theme="dark"]').keys()].filter((k) => k.startsWith("--") && k !== "--radius").sort();
  for (const { palette, mode } of COMBOS) {
    const { combo, decls } = appBlock(palette, mode);
    const props = [...decls.keys()].filter((k) => k.startsWith("--") && k !== "--radius").sort();
    assert.deepEqual(props, reference, `property set of ${combo}`);
    assert.equal(decls.get("color-scheme"), mode, `color-scheme of ${combo}`);
  }
});

test("app contrast: every token pair reaches AA in all 10 palette × mode combinations", () => {
  const failures: string[] = [];
  for (const { palette, mode } of COMBOS) {
    failures.push(...failuresOf(`${palette}-${mode}`, appPairs(palette, mode)));
  }
  assert.deepEqual(failures, []);
});

test("template contrast: lesson template pairs reach AA in both modes", () => {
  const failures: string[] = [];
  for (const mode of MODES) {
    const { combo, pairs } = templatePairs(mode);
    failures.push(...failuresOf(combo, pairs));
  }
  assert.deepEqual(failures, []);
});

test("chrome constants: NEUTRAL_CHROME mirrors the Neutro tokens and the overlay is 36 px", async () => {
  const chrome = await import(pathToFileURL(path.join(here, "../desktop/chrome.mjs")).href);
  assert.equal(chrome.OVERLAY_HEIGHT, 36);
  for (const mode of MODES) {
    const { v } = appBlock("neutral", mode);
    assert.deepEqual(chrome.NEUTRAL_CHROME[mode], {
      color: v("--background").toLowerCase(),
      symbolColor: v("--muted-foreground").toLowerCase(),
    }, `NEUTRAL_CHROME.${mode}`);
  }
});

test("parseChrome and resolveChrome validate the payload and pick the mode", async () => {
  const { parseChrome, resolveChrome, DEFAULT_CHROME } = await import(pathToFileURL(path.join(here, "../desktop/chrome.mjs")).href);
  const valid = {
    modePref: "dark",
    dark: { color: "#0F1620", symbolColor: "#8FA1B5" },
    light: { color: "#F2F5F9", symbolColor: "#4F5E70" },
  };
  assert.deepEqual(parseChrome(valid), {
    modePref: "dark",
    dark: { color: "#0f1620", symbolColor: "#8fa1b5" },
    light: { color: "#f2f5f9", symbolColor: "#4f5e70" },
  });
  assert.equal(parseChrome(null), null);
  assert.equal(parseChrome("dark"), null, "legacy string payload");
  assert.equal(parseChrome({ ...valid, modePref: "sepia" }), null);
  assert.equal(parseChrome({ ...valid, dark: { color: "#fff", symbolColor: "#000000" } }), null, "3-digit hex");
  assert.equal(parseChrome({ modePref: "system", dark: valid.dark }), null, "missing light");

  const both = { modePref: "system", dark: { color: "#000000", symbolColor: "#111111" }, light: { color: "#ffffff", symbolColor: "#222222" } };
  assert.equal(resolveChrome({ ...both, modePref: "system" }, true), both.dark);
  assert.equal(resolveChrome({ ...both, modePref: "system" }, false), both.light);
  assert.equal(resolveChrome({ ...both, modePref: "light" }, true), both.light, "explicit light ignores the OS");
  assert.equal(resolveChrome({ ...both, modePref: "dark" }, false), both.dark, "explicit dark ignores the OS");
  assert.deepEqual(resolveChrome(DEFAULT_CHROME, true), DEFAULT_CHROME.dark);
});

test("palettes: stored values parse with defaults and PALETTES lists five themes", () => {
  assert.equal(PALETTES.length, 5);
  assert.equal(readModePref(null), "system");
  assert.equal(readModePref("x"), "system");
  for (const m of ["system", "dark", "light"] as const) assert.equal(readModePref(m), m);
  assert.equal(readPalette(null), "neutral");
  assert.equal(readPalette("blue"), "neutral");
  for (const p of PALETTES) assert.equal(readPalette(p), p);
});

test("parseColor rejects syntax the contrast gate cannot measure", () => {
  assert.throws(() => parseColor("oklch(0.5 0.1 200)"), /unsupported colour/);
  assert.deepEqual(parseColor("rgb(224 183 101 / 0.22)"), { r: 224, g: 183, b: 101, a: 0.22 });
  assert.deepEqual(parseColor("rgba(16, 185, 129, 50%)"), { r: 16, g: 185, b: 129, a: 0.5 });
});
