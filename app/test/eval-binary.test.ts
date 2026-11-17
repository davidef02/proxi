// `npm run eval` against the built `proxi` binary with the fake engine: binary mode only (npm run test:rust).
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { after, describe, it } from "node:test";
import { FAKE_CLAUDE, REPO_ROOT } from "./harness.ts";

const dirs: string[] = [];
after(() => {
  for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
});

/** A settings path under a fresh temp dir: the learner's real settings never reach a run. */
function missingSettings(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "eval-binary-"));
  dirs.push(dir);
  return path.join(dir, "settings.json");
}

function runEval(args: string[], extraEnv: NodeJS.ProcessEnv = {}) {
  return spawnSync(process.execPath, ["--import", "tsx", path.join(REPO_ROOT, "app/eval/run.ts"), ...args], {
    cwd: REPO_ROOT,
    encoding: "utf-8",
    timeout: 120_000,
    env: { ...process.env, EVAL_FAKE_ENGINE: FAKE_CLAUDE, ...extraEnv },
  });
}

describe("eval against the binary", () => {
  it("runs one session over HTTP and stops at the plan (AC1)", () => {
    const r = runEval(["lezione-singola-nuxt", "--engine", "claude", "--settings", missingSettings(), "--timeout", "2"]);
    const output = `${r.stdout}\n${r.stderr}`;
    assert.equal(r.status, 1, output);
    assert.match(r.stdout, /binary: /);
    assert.match(r.stdout, /session \S+/);
    assert.match(r.stdout, /· Ciao, sono il docente\./);
    assert.match(r.stdout, /nudge 6\/6/);
    assert.match(r.stdout, /sse events: [1-9]\d*/);
    assert.match(r.stdout, /closed \S+/);
    assert.match(r.stdout, /FAIL lezione-singola-nuxt/);
    assert.match(r.stdout, /did not reach plan/);
  });

  // #19 brought agy into the Rust backend: --engine now selects a real engine, so the run reaches the
  // binary and reports the binary's error (LEARNER_AGY_BIN points the agy engine at a path that is not there).
  it("reports the binary's error for an engine whose executable is missing (AC3)", () => {
    const r = runEval(["lezione-singola-nuxt", "--engine", "agy", "--settings", missingSettings(), "--timeout", "2"], {
      LEARNER_AGY_BIN: path.join(os.tmpdir(), "no-such-agy-bin.exe"),
    });
    assert.equal(r.status, 2, `${r.stdout}\n${r.stderr}`);
    assert.match(r.stdout, /ERROR lezione-singola-nuxt: .*agy/);
  });

  it("ends the scenario on the timeout (AC3)", () => {
    const r = runEval(["lezione-singola-nuxt", "--engine", "claude", "--settings", missingSettings(), "--timeout", "0.0001"]);
    assert.equal(r.status, 2, `${r.stdout}\n${r.stderr}`);
    assert.match(r.stdout, /ERROR lezione-singola-nuxt: timeout/);
  });
});
