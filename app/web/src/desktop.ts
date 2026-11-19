// Tauri vs browser detection and external-link routing. Inside the Tauri shell links go through the
// opener plugin (the Rust side also catches anchors and window.open targets that skip this helper);
// in the plain browser build we keep the normal window.open.
export const isTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function openExternal(url: string): Promise<void> {
  if (isTauri()) {
    await (await import("@tauri-apps/plugin-opener")).openUrl(url);
  } else {
    window.open(url, "_blank");
  }
}
