import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { locateClaudeBinary } from "../server/claude.ts";

describe("locateClaudeBinary (Issue #19)", () => {
  it("respects LEARNER_CLAUDE_BIN override over any PATH or file check", () => {
    const override = "C:\\custom\\claude-custom.exe";
    const result = locateClaudeBinary({ env: { LEARNER_CLAUDE_BIN: override } });
    assert.equal(result, override);
  });

  it("resolves claude.exe from PATH on Windows", () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-bin-test-"));
    try {
      const target = path.join(tmp, "claude.exe");
      fs.writeFileSync(target, "");
      const res = locateClaudeBinary({
        platform: "win32",
        env: { PATH: tmp },
      });
      assert.equal(res, path.resolve(target));
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("strictly ignores .cmd, .bat, and extensionless files on Windows", () => {
    const dirShim = fs.mkdtempSync(path.join(os.tmpdir(), "claude-shim-"));
    const dirExe = fs.mkdtempSync(path.join(os.tmpdir(), "claude-exe-"));
    try {
      // Put claude.cmd and claude in dirShim
      fs.writeFileSync(path.join(dirShim, "claude.cmd"), "@echo off");
      fs.writeFileSync(path.join(dirShim, "claude.bat"), "@echo off");
      fs.writeFileSync(path.join(dirShim, "claude"), "echo test");
      // Put claude.exe in dirExe
      const target = path.join(dirExe, "claude.exe");
      fs.writeFileSync(target, "");

      // dirShim comes first in PATH
      const res = locateClaudeBinary({
        platform: "win32",
        env: { PATH: `${dirShim};${dirExe}` },
      });
      assert.equal(res, path.resolve(target));
    } finally {
      fs.rmSync(dirShim, { recursive: true, force: true });
      fs.rmSync(dirExe, { recursive: true, force: true });
    }
  });

  it("falls back to ~/.local/bin/claude.exe when PATH lacks claude", () => {
    const fakeHome = fs.mkdtempSync(path.join(os.tmpdir(), "claude-home-"));
    try {
      const localBin = path.join(fakeHome, ".local", "bin");
      fs.mkdirSync(localBin, { recursive: true });
      const target = path.join(localBin, "claude.exe");
      fs.writeFileSync(target, "");

      const res = locateClaudeBinary({
        platform: "win32",
        homeDir: fakeHome,
        env: { PATH: "" },
      });
      assert.equal(res, path.resolve(target));
    } finally {
      fs.rmSync(fakeHome, { recursive: true, force: true });
    }
  });

  it("falls back to ~/.claude/local/claude.exe when ~/.local/bin is missing", () => {
    const fakeHome = fs.mkdtempSync(path.join(os.tmpdir(), "claude-home-"));
    try {
      const localClaude = path.join(fakeHome, ".claude", "local");
      fs.mkdirSync(localClaude, { recursive: true });
      const target = path.join(localClaude, "claude.exe");
      fs.writeFileSync(target, "");

      const res = locateClaudeBinary({
        platform: "win32",
        homeDir: fakeHome,
        env: { PATH: "" },
      });
      assert.equal(res, path.resolve(target));
    } finally {
      fs.rmSync(fakeHome, { recursive: true, force: true });
    }
  });

  it("handles case-insensitive Path env and quoted paths", () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-bin-test-"));
    try {
      const target = path.join(tmp, "claude.exe");
      fs.writeFileSync(target, "");
      const res = locateClaudeBinary({
        platform: "win32",
        env: { Path: `"${tmp}"` },
      });
      assert.equal(res, path.resolve(target));
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("resolves extensionless claude on non-Windows platforms", () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-bin-test-"));
    try {
      const target = path.join(tmp, "claude");
      fs.writeFileSync(target, "");
      const res = locateClaudeBinary({
        platform: "linux",
        env: { PATH: tmp },
      });
      assert.equal(res, path.resolve(target));
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("returns undefined when claude cannot be found anywhere", () => {
    const res = locateClaudeBinary({
      platform: "win32",
      homeDir: "C:\\nonexistent\\home",
      env: { PATH: "C:\\nonexistent\\path" },
    });
    assert.equal(res, undefined);
  });
});
