// GET /api/claude/status over HTTP only (no probe internals), so it runs against the Node server and the proxi binary.
// The body is compared as text: the JSON key order is part of the contract.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { CLAUDE_MIN_VERSION } from "../shared/protocol.ts";
import { startServer } from "./harness.ts";

describe("GET /api/claude/status", () => {
  const dirs: string[] = [];

  function tempDir(prefix: string): string {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
    dirs.push(dir);
    return dir;
  }

  /** A mock claude script, used as the configured bin of the server. */
  function mock(name: string, source: string): string {
    const file = path.join(tempDir("claude-status-bin-"), name);
    fs.writeFileSync(file, source);
    return file;
  }

  /** Starts a server on the bin and returns the raw response body. */
  async function statusOf(bin: string): Promise<string> {
    const repo = tempDir("claude-status-repo-");
    const srv = await startServer({ repoRoot: repo, dataRoot: repo, claude: { cmd: bin } });
    try {
      const res = await fetch(`${srv.url}/api/claude/status`);
      assert.equal(res.status, 200);
      return await res.text();
    } finally {
      await srv.close();
    }
  }

  after(() => {
    for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  });

  it("meets-minimum", async () => {
    const script = mock("mock-claude.mjs", 'console.log("2.1.300 (Claude Code)");\n');
    assert.equal(
      await statusOf(script),
      JSON.stringify({ status: "meets-minimum", version: "2.1.300", minVersion: CLAUDE_MIN_VERSION, binPath: script }),
    );
  });

  it("update-required", async () => {
    const script = mock("mock-claude.mjs", 'console.log("2.0.100 (Claude Code)");\n');
    assert.equal(
      await statusOf(script),
      JSON.stringify({ status: "update-required", version: "2.0.100", minVersion: CLAUDE_MIN_VERSION, binPath: script }),
    );
  });

  it("unrecognised output is version-unchecked", async () => {
    const script = mock("mock-claude.mjs", 'console.log("hello");\n');
    assert.equal(
      await statusOf(script),
      JSON.stringify({
        status: "version-unchecked",
        minVersion: CLAUDE_MIN_VERSION,
        binPath: script,
        error: "Output non riconosciuto: hello",
      }),
    );
  });

  it("a missing binary is not-found", async () => {
    const missing = path.join(tempDir("claude-status-missing-"), "missing-claude.exe");
    assert.equal(
      await statusOf(missing),
      JSON.stringify({
        status: "not-found",
        minVersion: CLAUDE_MIN_VERSION,
        binPath: missing,
        error: `spawn ${missing} ENOENT`,
      }),
    );
  });
});
