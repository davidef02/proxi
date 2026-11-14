import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const GOLDEN = "app/test/fixtures/anki/import-progress.jsonl";

describe("Anki routes ported to Rust", () => {
  it("keeps the import golden LF-only and marked -text in git", (t) => {
    const bytes = fs.readFileSync(path.join(repoRoot, GOLDEN));
    assert.ok(!bytes.includes(0x0d), "the golden must have no CR bytes on disk");

    const r = spawnSync("git", ["check-attr", "text", GOLDEN], { cwd: repoRoot, encoding: "utf8" });
    if (r.error) return t.skip(`git not available: ${r.error.message}`);
    assert.equal(r.status, 0, r.stderr);
    assert.match(r.stdout, /: text: unset$/m);
  });

  it("leaves no TypeScript copy of the ported import tests", () => {
    // D10: the unit tests moved to crates/proxi/tests/anki_import.rs, with no duplicate left to maintain.
    assert.equal(fs.existsSync(path.join(repoRoot, "app/test/anki-import.test.ts")), false);
  });
});
