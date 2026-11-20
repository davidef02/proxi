// Copy text to the clipboard. The desktop app writes from the main process (navigator.clipboard rejects when the
// document is not focused, e.g. right after a link opened the browser); the Tauri build goes through the
// clipboard-manager plugin, and the browser build falls back to
// navigator.clipboard, then to a hidden textarea + execCommand (non-secure contexts have no navigator.clipboard).
import { isTauri } from "./desktop.ts";

export async function copyText(text: string): Promise<boolean> {
  if (window.iaDesktop?.copyText) {
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
