import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { describe, it } from "node:test";
import {
  psQuote,
  readUserPath,
  withPathEntry,
  withoutPathEntry,
  writeUserPath,
} from "../desktop/user-path.mjs";

const repoRoot = path.resolve(import.meta.dirname, "../..");
const dir = "C:\\repo\\target\\release";

// The source of one top-level `if (...) {` block of the installer, up to its closing brace.
function block(text: string, header: string): string {
  const source = text.replace(/\r\n/g, "\n");
  const start = source.indexOf(header);
  assert.notEqual(start, -1, "missing " + header);
  const lines = source.slice(start).split("\n");
  let depth = 0;
  for (let i = 0; i < lines.length; i++) {
    for (const ch of lines[i]) {
      if (ch === "{") depth += 1;
      if (ch === "}") depth -= 1;
    }
    if (depth === 0 && i > 0) return lines.slice(0, i + 1).join("\n");
  }
  assert.fail("unbalanced block after " + header);
}

describe("user PATH", () => {
  it("adds the bin folder to an empty PATH", () => {
    assert.deepEqual(withPathEntry("", dir), { value: dir, changed: true });
  });

  it("appends the bin folder after the existing entries", () => {
    assert.deepEqual(withPathEntry("C:\\a;C:\\b", dir), { value: "C:\\a;C:\\b;" + dir, changed: true });
  });

  it("leaves the PATH verbatim when the bin folder is already there in another case or with a trailing slash", () => {
    const current = "c:\\REPO\\target\\release\\";
    assert.deepEqual(withPathEntry(current, dir), { value: current, changed: false });
  });

  it("keeps %VAR% entries verbatim and drops empty segments", () => {
    assert.deepEqual(withPathEntry("A;;%USERPROFILE%\\bin;", dir), {
      value: "A;%USERPROFILE%\\bin;" + dir,
      changed: true,
    });
  });

  it("removes every variant of the bin folder and keeps the others in order", () => {
    const current = ["C:\\a", dir, dir + "\\", "C:\\b"].join(";");
    assert.deepEqual(withoutPathEntry(current, dir), { value: "C:\\a;C:\\b", changed: true });
  });

  it("leaves the PATH verbatim when the bin folder is absent", () => {
    const current = "C:\\a;C:\\b";
    assert.deepEqual(withoutPathEntry(current, dir), { value: current, changed: false });
  });

  it("quotes a path for PowerShell, doubling single quotes", () => {
    assert.equal(psQuote("C:\\O'Brien"), "'C:\\O''Brien'");
  });

  it("writes the PATH as an ExpandString and broadcasts the change", () => {
    let script = "";
    const ps = (s: string) => {
      script = s;
      return "";
    };
    writeUserPath(ps, "C:\\O'Brien");
    assert.ok(script.includes("ExpandString"), script);
    assert.ok(script.includes("SetEnvironmentVariable('PROXI_PATH_REFRESH'"), script);
    assert.ok(script.includes("'C:\\O''Brien'"), script);
  });

  it("reads the raw user PATH without expanding %VAR%", () => {
    let script = "";
    const ps = (s: string) => {
      script = s;
      return "C:\\a;%USERPROFILE%\\bin";
    };
    assert.equal(readUserPath(ps), "C:\\a;%USERPROFILE%\\bin");
    assert.match(script, /DoNotExpandEnvironmentNames/);
    assert.match(script, /HKCU:\\Environment/);
    assert.ok(script.includes("[Console]::OutputEncoding=[Text.Encoding]::UTF8"), script);
  });

  it("refuses to write back a PATH decoded with the wrong code page", () => {
    let called = false;
    const ps = (_s: string) => {
      called = true;
      return "";
    };
    assert.throws(() => writeUserPath(ps, "C:\\Citt\uFFFD\\bin"), /U\+FFFD/);
    assert.equal(called, false, "must not touch the registry");
  });

  it("the shortcut installer builds proxi and edits the user PATH through user-path.mjs", () => {
    const installer = fs.readFileSync(path.join(repoRoot, "app", "desktop", "install-shortcuts.mjs"), "utf8");
    assert.ok(installer.includes("./user-path.mjs"));
    assert.ok(installer.includes('"target", "release"'));
    assert.ok(installer.includes("--locked"));
  });

  it("the installer adds the bin folder on install, drops it on uninstall, and edits no PATH with --dir", () => {
    const installer = fs.readFileSync(path.join(repoRoot, "app", "desktop", "install-shortcuts.mjs"), "utf8");
    assert.ok(installer.includes("const userPath = !dirArg;"), "--dir must skip the PATH step");
    const uninstall = block(installer, "if (uninstall) {");
    assert.ok(uninstall.includes("withoutPathEntry(readUserPath(ps), binDir)"), "uninstall must read the user PATH");
    assert.ok(uninstall.includes("writeUserPath(ps, r.value)"), "uninstall must write the shortened PATH");
    const install = block(installer, "if (userPath) {\n  const r = withPathEntry");
    assert.ok(install.includes("withPathEntry(readUserPath(ps), binDir)"), "install must read and extend the user PATH");
    assert.ok(install.includes("writeUserPath(ps, r.value)"), "install must write the extended PATH");
    assert.ok(install.includes("is already on the user PATH"), "a second install must not duplicate the entry");
  });
});
