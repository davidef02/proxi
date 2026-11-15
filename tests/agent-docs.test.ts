import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

describe("agent docs", () => {
  it("agent-facing docs use proxi, not npm run lesson|anki|home|data", (t) => {
    // assembled at runtime so this file's own source never matches
    const pattern = ["npm run ", "(lesson|anki|home|data)", "\\b"].join("");
    const r = spawnSync(
      "git",
      [
        "grep",
        "-n",
        "-I",
        "-E",
        pattern,
        "--",
        "AGENTS.md",
        "CLAUDE.md",
        "CONTEXT.md",
        "README.md",
        "app/README.md",
        "docs/desktop.md",
        "skills",
        "docs/agents",
        "agents"
      ],
      { cwd: repoRoot, encoding: "utf8" }
    );
    if (r.error) return t.skip(`git not available: ${r.error.message}`);

    if (r.status === 0) {
      assert.fail(`agent docs still call npm run lesson|anki|home|data:\n${r.stdout.trim()}`);
    }
    if (r.status !== 1) assert.fail(r.stderr);
  });
});
