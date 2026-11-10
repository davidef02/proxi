// Creates (or with --uninstall removes) "Proxi" shortcuts in the Start menu and on the Desktop.
// Usage: node app/desktop/install-shortcuts.mjs [--uninstall] [--dir <folder>] [--no-build]
// --dir puts the single shortcut in <folder> instead of the real Start menu/Desktop (used by tests).
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { APP_NAME, SHORTCUT_NAME } from "../shared/identity.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "../..");
const argv = process.argv.slice(2);
const uninstall = argv.includes("--uninstall");
const di = argv.indexOf("--dir");
const dirArg = di >= 0 ? argv[di + 1] : undefined;
if (di >= 0 && !dirArg) throw new Error("--dir needs a path");

const ps = (script) => execFileSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8" }).trim();
const NAME = SHORTCUT_NAME;
const dirs = dirArg
  ? [path.resolve(dirArg)]
  : [path.join(ps("[Environment]::GetFolderPath('Programs')"), ""), ps("[Environment]::GetFolderPath('Desktop')")];

if (uninstall) {
  for (const d of dirs) {
    const f = path.join(d, NAME);
    if (fs.existsSync(f)) { fs.rmSync(f); console.log("removed " + f); }
  }
  process.exit(0);
}

const electron = path.join(root, "node_modules", "electron", "dist", "electron.exe");
if (!fs.existsSync(electron)) throw new Error("Electron missing: run `npm install` first.");
if (!argv.includes("--no-build")) {
  // Build now so the first launch from the shortcut is instant (main.mjs would rebuild a stale UI anyway).
  execFileSync(process.execPath, [path.join(root, "node_modules", "vite", "bin", "vite.js"), "build", "--config", path.join(root, "app", "web", "vite.config.ts")], { stdio: "inherit", cwd: root });
}
const q = (s) => "'" + s.replace(/'/g, "''") + "'";
for (const d of dirs) {
  fs.mkdirSync(d, { recursive: true });
  const f = path.join(d, NAME);
  ps(
    `$s=(New-Object -ComObject WScript.Shell).CreateShortcut(${q(f)});` +
      `$s.TargetPath=${q(electron)};$s.Arguments=${q('"' + path.join(here, "main.mjs") + '"')};` +
      `$s.WorkingDirectory=${q(root)};$s.IconLocation=${q(path.join(here, "icon.ico"))};$s.Description=${q(APP_NAME)};$s.Save()`,
  );
  console.log("created " + f);
}
