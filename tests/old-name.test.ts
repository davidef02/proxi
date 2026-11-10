import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
// assembled at runtime so this file never contains the retired name itself
const pattern = ["ia", "teacher"].join("[-_]?");

describe("old app name", () => {
  it("appears in no tracked file except docs/ROADMAP.md", (t) => {
    const r = spawnSync("git", ["grep", "-l", "-i", "-E", pattern], {
      cwd: repoRoot,
      encoding: "utf8"
    });
    if (r.error) return t.skip(`git not available: ${r.error.message}`);

    let files: string[];
    if (r.status === 1) {
      files = [];
    } else if (r.status === 0) {
      files = r.stdout.split(/\r?\n/).filter(Boolean);
    } else {
      assert.fail(r.stderr);
    }

    const unexpected = files.filter((f) => f !== "docs/ROADMAP.md");
    assert.deepEqual(unexpected, [], `old app name found in:\n${unexpected.join("\n")}`);
  });
});
