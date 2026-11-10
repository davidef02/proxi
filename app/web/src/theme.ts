// Colour scheme (Sistema/Chiaro/Scuro) and palette (Neutro, Oceano, …): UI preferences in localStorage, so they are per
// computer. <html data-theme> carries the resolved mode (dark|light: Tailwind `dark:`, CodeMirror, canvas maths read it);
// <html data-palette> carries the palette. styles.css keys every token off both attributes.
import { useSyncExternalStore } from "react";
import { LEGACY_KEY, MODE_KEY, PALETTE_KEY, readModePref, readPalette, type ChromePair, type ChromePayload, type Mode, type ModePref, type Palette } from "./palettes.ts";

export type { Mode, ModePref, Palette } from "./palettes.ts";
export type Appearance = { mode: Mode; palette: Palette; key: string /* `${palette}-${mode}` */ };

declare global {
  interface Window {
    /** Set by app/desktop/preload.cjs in the desktop app only. */
    iaDesktop?: { setChrome?(c: ChromePayload): void; setZoom?(z: number): void; copyText?(t: string): void };
  }
}

const HEX = /^#[0-9a-f]{6}$/;
const mq = window.matchMedia("(prefers-color-scheme: dark)");
const listeners = new Set<() => void>();

// Migration: the single `ia.theme` key of the previous version is ignored.
try {
  localStorage.removeItem(LEGACY_KEY);
} catch {
  /* best effort */
}

export function modePref(): ModePref {
  try {
    return readModePref(localStorage.getItem(MODE_KEY));
  } catch {
    return "system";
  }
}

export function palette(): Palette {
  try {
    return readPalette(localStorage.getItem(PALETTE_KEY));
  } catch {
    return "neutral";
  }
}

const resolveMode = (): Mode => {
  const p = modePref();
  return p === "system" ? (mq.matches ? "dark" : "light") : p;
};

/** Reads one palette × mode combo's chrome colours from a hidden probe carrying both attributes. */
function chromeOf(p: Palette, m: Mode): ChromePair {
  const probe = document.createElement("span");
  probe.hidden = true;
  probe.dataset.palette = p;
  probe.dataset.theme = m;
  document.documentElement.appendChild(probe);
  const read = (name: string) => getComputedStyle(probe).getPropertyValue(name).trim().toLowerCase();
  const pair = { color: read("--background"), symbolColor: read("--muted-foreground") };
  probe.remove();
  return pair;
}

function apply() {
  const mode = resolveMode();
  const p = palette();
  const el = document.documentElement;
  el.dataset.theme = mode;
  el.dataset.palette = p;
  el.style.colorScheme = mode;
  if (window.iaDesktop?.setChrome) {
    // desktop app: recolour the native window controls. Before the stylesheet loads the tokens are missing: skip, so
    // main keeps its persisted colours. An older preload may lack setChrome: then there is nothing to send.
    const payload: ChromePayload = { modePref: modePref(), dark: chromeOf(p, "dark"), light: chromeOf(p, "light") };
    const ok = [payload.dark.color, payload.dark.symbolColor, payload.light.color, payload.light.symbolColor].every((c) => HEX.test(c));
    if (ok) window.iaDesktop.setChrome(payload);
  }
  for (const f of listeners) f();
}

export function setModePref(p: ModePref) {
  try {
    localStorage.setItem(MODE_KEY, p);
  } catch {
    /* best effort */
  }
  apply();
}

export function setPalette(p: Palette) {
  try {
    localStorage.setItem(PALETTE_KEY, p);
  } catch {
    /* best effort */
  }
  apply();
}

mq.addEventListener("change", apply);
apply();

const subscribe = (f: () => void) => {
  listeners.add(f);
  return () => void listeners.delete(f);
};

/** The resolved mode; re-renders on change. */
export const useMode = (): Mode => useSyncExternalStore(subscribe, resolveMode);

// One object per key, so useSyncExternalStore gets a stable reference while nothing changes.
const appearances = new Map<string, Appearance>();
function appearance(): Appearance {
  const mode = resolveMode();
  const p = palette();
  const key = `${p}-${mode}`;
  let a = appearances.get(key);
  if (!a) {
    a = { mode, palette: p, key };
    appearances.set(key, a);
  }
  return a;
}

/** The resolved palette × mode; re-renders on a change of either (canvas, mermaid and the editor read colours once). */
export const useAppearance = (): Appearance => useSyncExternalStore(subscribe, appearance);

/** A CSS custom property of the active palette × mode, as declared in styles.css (e.g. `--background`). */
export function token(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}
