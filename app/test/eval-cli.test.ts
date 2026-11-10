// The eval CLI's guards: bad input exits 2 before any engine run (outside CI, this test costs nothing).
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const CLI = path.join(REPO, "app", "eval", "run.ts");

const run = (...args: string[]) =>
  spawnSync(process.execPath, ["--import", "tsx", CLI, ...args], { cwd: REPO, encoding: "utf-8" });

test("eval CLI: an unknown scenario lists the valid names and exits 2", () => {
  const r = run("nope");
  assert.equal(r.status, 2);
  assert.match(r.stderr, /unknown scenario: nope/);
  for (const name of ["lezione-singola-nuxt", "lezione-singola-nuxt-vuoto", "plan-nuxt", "plan-nuxt-vuoto"]) {
    assert.ok(r.stderr.includes(name), `the list names ${name}`);
  }
});

test("eval CLI: an unknown engine and a non-positive timeout exit 2", () => {
  const engine = run("lezione-singola-nuxt", "--engine", "gpt");
  assert.equal(engine.status, 2);
  assert.match(engine.stderr, /--engine must be one of: claude, agy, opencode/);

  const timeout = run("lezione-singola-nuxt", "--timeout", "0");
  assert.equal(timeout.status, 2);
  assert.match(timeout.stderr, /--timeout must be a positive number/);
});
