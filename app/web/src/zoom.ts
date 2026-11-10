// Global text size (UI zoom): a UI preference, so it lives in localStorage (not in the repo's settings).
// In Electron, webContents.setZoomFactor scales the window and updates devicePixelRatio uniformly.
// In the browser, CSS zoom on document.documentElement scales the entire interface.
import { useSyncExternalStore } from "react";

export type ZoomPref = "90%" | "100%" | "110%" | "125%" | "150%";

export const ZOOM_FACTORS: Record<ZoomPref, number> = {
  "90%": 0.9,
  "100%": 1.0,
  "110%": 1.1,
  "125%": 1.25,
  "150%": 1.5,
};

export const ZOOM_PREFS: ZoomPref[] = ["90%", "100%", "110%", "125%", "150%"];
export const ZOOM_OPTIONS: [ZoomPref, string][] = [
  ["90%", "90%"],
  ["100%", "100%"],
  ["110%", "110%"],
  ["125%", "125%"],
  ["150%", "150%"],
];

const KEY = "ia.zoom";
const listeners = new Set<() => void>();

export function zoomPref(): ZoomPref {
  try {
    const v = typeof localStorage !== "undefined" ? localStorage.getItem(KEY) : null;
    return v && v in ZOOM_FACTORS ? (v as ZoomPref) : "100%";
  } catch {
    return "100%";
  }
}

export function zoomFactor(): number {
  return ZOOM_FACTORS[zoomPref()] ?? 1.0;
}

function apply() {
  const factor = zoomFactor();
  if (typeof window !== "undefined") {
    if (window.iaDesktop?.setZoom) {
      window.iaDesktop.setZoom(factor);
    } else {
      (document.documentElement.style as any).zoom = String(factor);
    }
  }
  for (const f of listeners) f();
}

export function setZoomPref(p: ZoomPref) {
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(KEY, p);
    }
  } catch {
    /* best effort */
  }
  apply();
}

export function stepZoom(direction: 1 | -1 | 0) {
  if (direction === 0) {
    setZoomPref("100%");
    return;
  }
  const current = zoomPref();
  const idx = ZOOM_PREFS.indexOf(current);
  const nextIdx = Math.max(0, Math.min(ZOOM_PREFS.length - 1, (idx === -1 ? 1 : idx) + direction));
  setZoomPref(ZOOM_PREFS[nextIdx]);
}

if (typeof window !== "undefined") {
  window.addEventListener("keydown", (e: KeyboardEvent) => {
    if (!e.ctrlKey && !e.metaKey) return;
    if (e.key === "=" || e.key === "+" || e.code === "Equal" || e.code === "NumpadAdd") {
      e.preventDefault();
      stepZoom(1);
    } else if (e.key === "-" || e.code === "Minus" || e.code === "NumpadSubtract") {
      e.preventDefault();
      stepZoom(-1);
    } else if (e.key === "0" || e.code === "Digit0" || e.code === "Numpad0") {
      e.preventDefault();
      stepZoom(0);
    }
  });

  apply();
}

const subscribe = (f: () => void) => {
  listeners.add(f);
  return () => void listeners.delete(f);
};

/** The resolved zoom preference; re-renders on change. */
export const useZoom = (): ZoomPref => useSyncExternalStore(subscribe, zoomPref);
