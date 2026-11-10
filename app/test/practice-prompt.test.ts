import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const PROMPT = fs.readFileSync(path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../prompts/practice.md"), "utf8");

test("practice prompt lists the tools of a practice session", () => {
  for (const tool of ["mcp__app__problem", "mcp__app__conclude", "mcp__app__write_exercise", "mcp__app__run_exercise"]) {
    assert.ok(PROMPT.includes(tool), `missing ${tool}`);
  }
});

test("practice prompt follows the Opening line of the session block", () => {
  assert.ok(PROMPT.includes("## Opening"), "missing the Opening section");
  assert.ok(PROMPT.includes("`Opening:`"), "the Opening section must point at the session block's `Opening:` line");
});
