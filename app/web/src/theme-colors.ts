// Pure colour maths for the theme system (no DOM). Used at runtime (side pane fills, mermaid variables,
// confidence rail) and by app/test/theme-tokens.test.ts, which checks every pair against WCAG thresholds.
import type { PlanNodeStatus } from "../../shared/protocol.ts";
import type { Mode } from "./palettes.ts";

export type RGBA = { r: number; g: number; b: number; a: number };

const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));
const hex2 = (n: number) => Math.round(clamp(n, 0, 255)).toString(16).padStart(2, "0");

/** Accepts #rgb, #rrggbb, rgb(r g b), rgb(r g b / a), rgb(r, g, b), rgba(r, g, b, a). Anything else throws. */
export function parseColor(s: string): RGBA {
  const src = s.trim().toLowerCase();
  const fail = () => new Error("unsupported colour: " + s);
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/.exec(src);
  if (hex) {
    const h = hex[1].length === 3 ? [...hex[1]].map((c) => c + c).join("") : hex[1];
    return { r: parseInt(h.slice(0, 2), 16), g: parseInt(h.slice(2, 4), 16), b: parseInt(h.slice(4, 6), 16), a: 1 };
  }
  const fn = /^rgba?\(\s*([^)]*)\)$/.exec(src);
  if (!fn) throw fail();
  const body = fn[1].trim();
  // rgb(r g b / a) or rgb(r, g, b[, a]); channels are plain numbers, alpha is a number or a percentage
  const parts = body.includes("/")
    ? body.split("/").flatMap((p, i) => (i === 0 ? p.trim().split(/\s+/) : [p.trim()]))
    : body.includes(",")
      ? body.split(",").map((p) => p.trim())
      : body.split(/\s+/);
  if (parts.length !== 3 && parts.length !== 4) throw fail();
  const num = (p: string) => (/^-?\d+(\.\d+)?$/.test(p) ? Number(p) : NaN);
  const r = num(parts[0]), g = num(parts[1]), b = num(parts[2]);
  let a = 1;
  if (parts.length === 4) {
    const raw = parts[3];
    const v = raw.endsWith("%") ? num(raw.slice(0, -1)) / 100 : num(raw);
    if (Number.isNaN(v)) throw fail();
    a = clamp(v, 0, 1);
  }
  if ([r, g, b].some(Number.isNaN)) throw fail();
  return { r: clamp(r, 0, 255), g: clamp(g, 0, 255), b: clamp(b, 0, 255), a };
}

/** #rrggbb lowercase; alpha ignored, channels rounded. */
export function toHex(c: RGBA): string {
  return "#" + hex2(c.r) + hex2(c.g) + hex2(c.b);
}

/** Alpha-blends `top` (using its own alpha) over the opaque `bottom`. Returns hex. */
export function composite(top: string, bottom: string): string {
  const t = parseColor(top), b = parseColor(bottom);
  const k = t.a;
  return toHex({ r: t.r * k + b.r * (1 - k), g: t.g * k + b.g * (1 - k), b: t.b * k + b.b * (1 - k), a: 1 });
}

/** `color` at opacity `p` (multiplied with its own alpha) over `over`. Returns hex. */
export function mix(color: string, p: number, over: string): string {
  const c = parseColor(color), o = parseColor(over);
  const k = clamp(p, 0, 1) * c.a;
  return toHex({ r: c.r * k + o.r * (1 - k), g: c.g * k + o.g * (1 - k), b: c.b * k + o.b * (1 - k), a: 1 });
}

function channelLinear(v: number): number {
  const c = v / 255;
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

/** WCAG 2.2 relative luminance of an opaque colour. */
export function luminance(hex: string): number {
  const c = parseColor(hex);
  return 0.2126 * channelLinear(c.r) + 0.7152 * channelLinear(c.g) + 0.0722 * channelLinear(c.b);
}

/** WCAG contrast ratio. Both colours are composited to opaque first if they carry alpha. */
export function contrast(a: string, b: string): number {
  const La = luminance(parseColor(a).a < 1 ? composite(a, b) : a);
  const Lb = luminance(parseColor(b).a < 1 ? composite(b, "#ffffff") : b);
  const [hi, lo] = La > Lb ? [La, Lb] : [Lb, La];
  return (hi + 0.05) / (lo + 0.05);
}

const plan = (v: (name: string) => string, status: "good" | "info" | "bad") => ({
  fill: mix(v(`--${status}`), 0.18, v("--card")),
  stroke: v(`--${status}`),
  color: v("--foreground"),
});

/** Side pane plan DAG colours per node status, derived from the active palette tokens. Key order is the legend order. */
export function planPalette(v: (name: string) => string): Record<PlanNodeStatus, { fill: string; stroke: string; color: string }> {
  return {
    done: plan(v, "good"),
    current: plan(v, "info"),
    missed: plan(v, "bad"),
    todo: { fill: v("--card"), stroke: v("--border"), color: v("--foreground") },
  };
}

/** Mermaid themeVariables derived from the active palette tokens. */
export function mermaidVars(v: (name: string) => string, mode: Mode) {
  return {
    darkMode: mode === "dark",
    background: v("--background"),
    primaryColor: v("--card"),
    primaryTextColor: v("--foreground"),
    primaryBorderColor: v("--border"),
    lineColor: v("--muted-foreground"),
    secondaryColor: v("--accent"),
    tertiaryColor: v("--card"),
    edgeLabelBackground: v("--background"),
    clusterBkg: v("--muted"),
    clusterBorder: v("--border"),
  };
}

/** Confidence rail stops, red → green. Per mode so each stays readable on that mode's background. */
export const CONFIDENCE_COLORS: Record<Mode, readonly string[]> = {
  dark: ["#ef4444", "#f97316", "#eab308", "#84cc16", "#22c55e", "#10b981"],
  light: ["#dc2626", "#c2410c", "#a16207", "#4d7c0f", "#15803d", "#047857"],
};
