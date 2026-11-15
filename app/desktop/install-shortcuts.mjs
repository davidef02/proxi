// Creates (or with --uninstall removes) "Proxi" shortcuts in the Start menu and on the Desktop, builds the
// `proxi` binary and puts <repo>\target\release on the user PATH (--uninstall removes that entry too).
// Usage: node app/desktop/install-shortcuts.mjs [--uninstall] [--dir <folder>] [--no-build]
// --dir puts the single shortcut in <folder> instead of the real Start menu/Desktop (used by tests):
// no PATH change, no Rust build.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { APP_NAME, SHORTCUT_NAME } from "../shared/identity.mjs";
import { psQuote, readUserPath, withPathEntry, withoutPathEntry, writeUserPath } from "./user-path.mjs";

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
const binDir = path.join(root, "target", "release");
const proxiExe = path.join(binDir, "proxi.exe");
// --dir is the test mode: it must not touch the real PATH nor run cargo.
const userPath = !dirArg;

if (uninstall) {
  for (const d of dirs) {
    const f = path.join(d, NAME);
    if (fs.existsSync(f)) { fs.rmSync(f); console.log("removed " + f); }
  }
  if (userPath) {
    const r = withoutPathEntry(readUserPath(ps), binDir);
    if (r.changed) { writeUserPath(ps, r.value); console.log("removed " + binDir + " from the user PATH"); }
  }
  process.exit(0);
}

const electron = path.join(root, "node_modules", "electron", "dist", "electron.exe");
if (!fs.existsSync(electron)) throw new Error("Electron missing: run `npm install` first.");
if (!argv.includes("--no-build")) {
  // Build now so the first launch from the shortcut is instant (main.mjs would rebuild a stale UI anyway).
  execFileSync(process.execPath, [path.join(root, "node_modules", "vite", "bin", "vite.js"), "build", "--config", path.join(root, "app", "web", "vite.config.ts")], { stdio: "inherit", cwd: root });
}
if (userPath) {
  if (!argv.includes("--no-build")) {
    // A fresh clone has no target/release/proxi.exe. No shell: cargo must be the real binary, not a shell builtin.
    try {
      execFileSync("cargo", ["build", "--release", "--locked"], { stdio: "inherit", cwd: root });
    } catch (e) {
      if (e.code === "ENOENT") throw new Error("cargo not found: install Rust from https://rustup.rs, then rerun `npm run desktop:install`.");
      throw e;
    }
  }
  if (!fs.existsSync(proxiExe)) throw new Error("proxi missing at " + proxiExe + ": run `cargo build --release --locked` first.");
}
for (const d of dirs) {
  fs.mkdirSync(d, { recursive: true });
  const f = path.join(d, NAME);
  ps(
    `$s=(New-Object -ComObject WScript.Shell).CreateShortcut(${psQuote(f)});` +
      `$s.TargetPath=${psQuote(electron)};$s.Arguments=${psQuote('"' + path.join(here, "main.mjs") + '"')};` +
      `$s.WorkingDirectory=${psQuote(root)};$s.IconLocation=${psQuote(path.join(here, "icon.ico"))};$s.Description=${psQuote(APP_NAME)};$s.Save()`,
  );
  console.log("created " + f);
}
if (userPath) {
  const r = withPathEntry(readUserPath(ps), binDir);
  if (r.changed) {
    writeUserPath(ps, r.value);
    console.log("added " + binDir + " to the user PATH: open a new terminal and run `proxi --version`");
  } else {
    console.log(binDir + " is already on the user PATH");
  }
}
