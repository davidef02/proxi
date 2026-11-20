// One typed desktop client for Tauri and Electron bridges with browser fallbacks.
// Inside the Tauri shell links go through opener, clipboard goes through clipboard-manager,
// chrome recolouring and zoom route to the shell commands (set_chrome, set_zoom).
// In plain browser builds we keep normal browser APIs (window.open, navigator.clipboard, CSS zoom).
import type { ChromePayload } from "./palettes.ts";

declare global {
  interface Window {
    /** Set by app/desktop/preload.cjs in the desktop app only. */
    iaDesktop?: { setChrome?(c: ChromePayload): void; setZoom?(z: number): void; copyText?(t: string): void };
  }
}

export const isTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function openExternal(url: string): Promise<void> {
  if (isTauri()) {
    await (await import("@tauri-apps/plugin-opener")).openUrl(url);
  } else {
    window.open(url, "_blank");
  }
}

export async function setChrome(c: ChromePayload): Promise<void> {
  if (isTauri()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("set_chrome", { chrome: c });
  } else if (typeof window !== "undefined" && window.iaDesktop?.setChrome) {
    window.iaDesktop.setChrome(c);
  }
}

export function setZoom(z: number): boolean {
  if (isTauri()) {
    void import("@tauri-apps/api/core").then(({ invoke }) => invoke("set_zoom", { zoom: z }));
    return true;
  }
  if (typeof window !== "undefined" && window.iaDesktop?.setZoom) {
    window.iaDesktop.setZoom(z);
    return true;
  }
  return false;
}

export async function copyText(text: string): Promise<boolean> {
  if (typeof window !== "undefined" && window.iaDesktop?.copyText) {
    window.iaDesktop.copyText(text);
    return true;
  }
  if (isTauri()) {
    // The plugin command has no size bound; keep the Electron ia:copy bound here.
    if (text.length > 10_000) return false;
    try {
      await (await import("@tauri-apps/plugin-clipboard-manager")).writeText(text);
      return true;
    } catch {
      return false;
    }
  }
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.cssText = "position:fixed;opacity:0;pointer-events:none";
    document.body.appendChild(ta);
    ta.select();
    try {
      return document.execCommand("copy");
    } catch {
      return false;
    } finally {
      ta.remove();
    }
  }
}
