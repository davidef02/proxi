import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

const GONE = [
  "app/server",
  "extensions/cli.ts",
  "extensions/embed-bench.ts",
  "extensions/ask-user-question.ts",
  "extensions/quiz.ts",
  "extensions/md-log.ts",
  "app/test/capture-golden.ts",
  "app/test/capture-golden-home.ts",
  "app/test/capture-golden.test.ts",
  "app/test/capture-lesson-golden.ts",
  "app/test/capture-anki-golden.ts",
  "app/test/capture-jobs-golden.ts",
  "app/test/capture-mcp-golden.ts",
  "app/test/capture-mcp-tools-golden.ts",
  "app/test/capture-parity-golden.ts",
];

describe("cutover to the proxi binary", () => {
  it("leaves no Node server, CLI entry point or capture script on disk", () => {
    const stillThere = GONE.filter((p) => fs.existsSync(path.join(repoRoot, p)));
    assert.deepEqual(stillThere, []);
  });

  it("drops the retired npm scripts", () => {
    const pkg = JSON.parse(fs.readFileSync(path.join(repoRoot, "package.json"), "utf8"));
    const retired = ["lesson", "anki", "home", "data", "embed-bench", "app", "app:serve", "test", "test:watch", "typecheck", "golden:capture"];
    const present = retired.filter((s) => s in (pkg.scripts ?? {}));
    assert.deepEqual(present, []);
  });

  it("keeps no reference to app/server in code", (t) => {
    const r = spawnSync("git", ["grep", "-l", "app/server", "--", "app", "extensions", "tests", "scripts", ":(exclude)app/test/cutover.test.ts"], {
      cwd: repoRoot,
      encoding: "utf8",
    });
    if (r.error) return t.skip(`git not available: ${r.error.message}`);
    if (r.status === 0) assert.fail(`files still referencing app/server:\n${r.stdout.trim()}`);
    if (r.status !== 1) assert.fail(r.stderr);
  });
});
