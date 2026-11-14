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
  });

  it("the shortcut installer builds proxi and edits the user PATH through user-path.mjs", () => {
    const installer = fs.readFileSync(path.join(repoRoot, "app", "desktop", "install-shortcuts.mjs"), "utf8");
    assert.ok(installer.includes("./user-path.mjs"));
    assert.ok(installer.includes('"target", "release"'));
    assert.ok(installer.includes("--locked"));
  });
});
