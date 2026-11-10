// Native window chrome (title bar overlay + window background) as a pure module: no electron import, so it is testable.

/** Title bar overlay height: the app's top bar is h-9 = 36px (app/web/src/shell/Shell.tsx). */
export const OVERLAY_HEIGHT = 36;
/** Neutro chrome = --background / --muted-foreground of the Neutro blocks in app/web/src/styles.css (checked by app/test/theme-tokens.test.ts). */
export const NEUTRAL_CHROME = { dark: { color: "#131416", symbolColor: "#96938d" }, light: { color: "#f6f4ef", symbolColor: "#6c675e" } };
export const DEFAULT_CHROME = { modePref: "system", ...NEUTRAL_CHROME };

const HEX = /^#[0-9a-f]{6}$/i;
const pair = (v) =>
  v && typeof v === "object" && typeof v.color === "string" && typeof v.symbolColor === "string" && HEX.test(v.color) && HEX.test(v.symbolColor)
    ? { color: v.color.toLowerCase(), symbolColor: v.symbolColor.toLowerCase() }
    : null;

/** Validates a chrome payload (from the renderer or window.json); null when anything is off. */
export function parseChrome(v) {
  if (!v || typeof v !== "object") return null;
  const modePref = ["system", "dark", "light"].includes(v.modePref) ? v.modePref : null;
  const dark = pair(v.dark), light = pair(v.light);
  return modePref && dark && light ? { modePref, dark, light } : null;
}

/** The colours for the current mode: the OS decides when the preference is "system". */
export const resolveChrome = (c, osDark) => c[c.modePref === "system" ? (osDark ? "dark" : "light") : c.modePref];
