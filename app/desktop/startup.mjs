// Startup helpers for the desktop shell (UI staleness, initial route) as a pure module: no electron import, so it is testable.
import fs from "node:fs";
import path from "node:path";

export function newestMtime(dir) {
  let t = 0;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    t = Math.max(t, e.isDirectory() ? newestMtime(p) : fs.statSync(p).mtimeMs);
  }
  return t;
}

/** True when app/web/dist/index.html is missing or older than the newest UI source. */
export function uiIsStale(appRoot) {
  const index = path.join(appRoot, "web", "dist", "index.html");
  if (!fs.existsSync(index)) return true;
  const built = fs.statSync(index).mtimeMs;
  return [path.join(appRoot, "web", "src"), path.join(appRoot, "shared"), path.join(appRoot, "web", "index.html")].some(
    (p) => fs.existsSync(p) && (fs.statSync(p).isDirectory() ? newestMtime(p) : fs.statSync(p).mtimeMs) > built,
  );
}

/** The URL to open at startup: base, or base + hash when hash is a plain in-app route. */
export function startUrl(base, hash) {
  return typeof hash === "string" && /^#\/[A-Za-z0-9/_-]*$/.test(hash) ? base.replace(/\/+$/, "") + "/" + hash : base;
}
