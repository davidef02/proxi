import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { CLAUDE_MIN_VERSION, compareSemver, parseClaudeVersion, probeClaudeCli } from "../server/claude.ts";
import { createServer } from "../server/main.ts";

describe("Claude CLI version probe & compatibility (Issue #20)", () => {
  describe("parseClaudeVersion", () => {
    it("parses standard Claude Code version string", () => {
      const parsed = parseClaudeVersion("2.1.314 (Claude Code)\r\n");
      assert.deepEqual(parsed, {
        major: 2,
        minor: 1,
        patch: 314,
        raw: "2.1.314",
      });
    });

    it("parses fake-claude fixture version string", () => {
      const parsed = parseClaudeVersion("0.0.0 (fake-claude)\n");
      assert.deepEqual(parsed, {
        major: 0,
        minor: 0,
        patch: 0,
        raw: "0.0.0",
      });
    });

    it("parses clean semver string", () => {
      const parsed = parseClaudeVersion("2.1.259");
      assert.deepEqual(parsed, {
        major: 2,
        minor: 1,
        patch: 259,
        raw: "2.1.259",
      });
    });

    it("returns null for non-version strings", () => {
      assert.equal(parseClaudeVersion("unknown command"), null);
      assert.equal(parseClaudeVersion(""), null);
      assert.equal(parseClaudeVersion("claude version error"), null);
    });
  });

  describe("compareSemver", () => {
    const min = { major: 2, minor: 1, patch: 259 };

    it("returns positive when version is greater", () => {
      assert.ok(compareSemver({ major: 3, minor: 0, patch: 0 }, min) > 0);
      assert.ok(compareSemver({ major: 2, minor: 2, patch: 0 }, min) > 0);
      assert.ok(compareSemver({ major: 2, minor: 1, patch: 300 }, min) > 0);
    });

    it("returns 0 when version is equal", () => {
      assert.equal(compareSemver({ major: 2, minor: 1, patch: 259 }, min), 0);
    });

    it("returns negative when version is lower", () => {
      assert.ok(compareSemver({ major: 2, minor: 1, patch: 258 }, min) < 0);
      assert.ok(compareSemver({ major: 2, minor: 0, patch: 999 }, min) < 0);
      assert.ok(compareSemver({ major: 1, minor: 9, patch: 999 }, min) < 0);
    });
  });

  describe("probeClaudeCli", () => {
    it("reports not-found when binary cannot be located", async () => {
      const res = await probeClaudeCli({
        binPath: "C:\\path\\does\\not\\exist\\claude.exe",
      });
      assert.equal(res.status, "not-found");
      assert.equal(res.minVersion, CLAUDE_MIN_VERSION);
    });

    it("reports meets-minimum when installed version is supported", async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-probe-good-"));
      try {
        const script = path.join(tmp, "mock-claude.mjs");
        fs.writeFileSync(script, 'console.log("2.1.314 (Claude Code)");\n');
        const res = await probeClaudeCli({ binPath: script });
        assert.equal(res.status, "meets-minimum");
        assert.equal(res.version, "2.1.314");
        assert.equal(res.minVersion, CLAUDE_MIN_VERSION);
        assert.equal(res.binPath, script);
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    it("reports update-required when installed version is below minimum", async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-probe-old-"));
      try {
        const script = path.join(tmp, "mock-claude.mjs");
        fs.writeFileSync(script, 'console.log("2.0.100 (Claude Code)");\n');
        const res = await probeClaudeCli({ binPath: script });
        assert.equal(res.status, "update-required");
        assert.equal(res.version, "2.0.100");
        assert.equal(res.minVersion, CLAUDE_MIN_VERSION);
        assert.equal(res.binPath, script);
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    it("reports version-unchecked when probe output is unrecognized or process errors", async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-probe-fail-"));
      try {
        const script = path.join(tmp, "mock-claude.mjs");
        fs.writeFileSync(script, 'console.error("fatal crash"); process.exit(1);\n');
        const res = await probeClaudeCli({ binPath: script });
        assert.equal(res.status, "version-unchecked");
        assert.equal(res.minVersion, CLAUDE_MIN_VERSION);
        assert.ok(res.error);
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });
  });

  describe("GET /api/claude/status", () => {
    it("returns ClaudeStatusResponse from HTTP endpoint", async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "claude-status-srv-"));
      try {
        const mockBin = path.join(tmp, "fake-claude.mjs");
        fs.writeFileSync(mockBin, 'console.log("2.1.300 (Claude Code)");\n');
        const srv = await createServer({
          repoRoot: tmp,
          dataRoot: tmp,
          port: 0,
          claudeBin: mockBin,
        });
        try {
          const res = await fetch(`${srv.url}/api/claude/status`);
          assert.equal(res.status, 200);
          const body = (await res.json()) as any;
          assert.equal(body.status, "meets-minimum");
          assert.equal(body.version, "2.1.300");
          assert.equal(body.minVersion, CLAUDE_MIN_VERSION);
        } finally {
          await srv.close();
        }
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });
  });
});
