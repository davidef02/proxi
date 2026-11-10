// Copy text to the clipboard. The desktop app writes from the main process (navigator.clipboard rejects when the
// document is not focused, e.g. right after a link opened the browser); the browser build falls back to
// navigator.clipboard, then to a hidden textarea + execCommand (non-secure contexts have no navigator.clipboard).
export async function copyText(text: string): Promise<boolean> {
  if (window.iaDesktop?.copyText) {
    window.iaDesktop.copyText(text);
    return true;
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
