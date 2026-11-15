// The eval CLI's guards: bad input exits 2 before any engine run (outside CI, this test costs nothing).
import { test, after } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { evalServeArgs, writeEvalSettings } from "../eval/harness.ts";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const CLI = path.join(REPO, "app", "eval", "run.ts");

const run = (...args: string[]) =>
  spawnSync(process.execPath, ["--import", "tsx", CLI, ...args], { cwd: REPO, encoding: "utf-8" });

const dirs: string[] = [];
after(() => dirs.forEach((d) => fs.rmSync(d, { recursive: true, force: true })));

const tempDir = (): string => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "eval-cli-"));
  dirs.push(dir);
  return dir;
};

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

test("eval CLI: a missing binary names the build command and exits 2", () => {
  const r = spawnSync(process.execPath, ["--import", "tsx", CLI, "lezione-singola-nuxt"], {
    cwd: REPO,
    encoding: "utf-8",
    env: { ...process.env, PROXI_BIN: path.join(os.tmpdir(), "no-such-proxi-bin") },
  });
  assert.equal(r.status, 2);
  assert.match(r.stderr, /cargo build --release --locked/);
  assert.ok(!r.stdout.includes("== lezione-singola-nuxt"), "no scenario runs");
});

test("writeEvalSettings: --engine patches provider and sync never runs", () => {
  const settingsFile = path.join(tempDir(), "settings.json");
  fs.writeFileSync(settingsFile, JSON.stringify({ provider: "agy", syncProvider: "github", teacherModel: "opus" }));
  const written = (root: string) => path.join(root, ".learner", "settings.json");
  const read = (root: string) => JSON.parse(fs.readFileSync(written(root), "utf-8"));

  const patched = tempDir();
  writeEvalSettings(patched, settingsFile, "claude");
  assert.deepEqual(read(patched), { provider: "claude", teacherModel: "opus" });

  const copied = tempDir();
  writeEvalSettings(copied, settingsFile);
  assert.deepEqual(read(copied), { provider: "agy", teacherModel: "opus" });

  const missing = path.join(tempDir(), "missing.json");
  const nothing = tempDir();
  writeEvalSettings(nothing, missing);
  assert.equal(fs.existsSync(written(nothing)), false, "no settings file means the defaults apply");

  const defaults = tempDir();
  writeEvalSettings(defaults, missing, "claude");
  assert.deepEqual(read(defaults), { provider: "claude" });
});

test("evalServeArgs: fake Anki, no update check, fake engine flags", () => {
  assert.deepEqual(evalServeArgs("/repo", "/data"), [
    "serve",
    "--port", "0",
    "--repo-root", "/repo",
    "--data-root", "/data",
    "--no-update-check",
    "--anki-url", "http://127.0.0.1:9",
  ]);
  assert.deepEqual(evalServeArgs("/repo", "/data", { cmd: "node", args: ["fake.mjs"] }), [
    "serve",
    "--port", "0",
    "--repo-root", "/repo",
    "--data-root", "/data",
    "--no-update-check",
    "--anki-url", "http://127.0.0.1:9",
    "--claude-bin", "node",
    "--claude-bin-arg", "fake.mjs",
  ]);
});

test("eval code imports no server module (AC2)", () => {
  const evalDir = path.join(REPO, "app", "eval");
  const serverDir = path.join(REPO, "app", "server") + path.sep;
  const sources = (dir: string): string[] =>
    fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
      const full = path.join(dir, e.name);
      return e.isDirectory() ? sources(full) : e.name.endsWith(".ts") ? [full] : [];
    });

  const specifiers = [
    /(?:import|export)[^'"]*?from\s+["'](\.[^"']+)["']/g,
    /import\(["'](\.[^"']+)["']\)/g,
  ];
  const importedBy = new Map<string, string>();
  const chain = (file: string): string => {
    const parts = [path.relative(REPO, file)];
    for (let at = importedBy.get(file); at; at = importedBy.get(at)) parts.unshift(path.relative(REPO, at));
    return parts.join(" -> ");
  };

  const visited = new Set<string>();
  const queue = sources(evalDir);
  const offenders: string[] = [];
  while (queue.length > 0) {
    const file = queue.pop()!;
    if (visited.has(file)) continue;
    visited.add(file);
    if (file.startsWith(serverDir)) {
      offenders.push(chain(file));
      continue;
    }
    const src = fs.readFileSync(file, "utf-8");
    for (const re of specifiers) {
      for (const match of src.matchAll(re)) {
        const target = path.resolve(path.dirname(file), match[1]);
        if (!fs.existsSync(target)) continue;
        if (!importedBy.has(target)) importedBy.set(target, file);
        queue.push(target);
      }
    }
  }

  assert.deepEqual(offenders, [], `eval code reaches the server through:\n${offenders.join("\n")}`);
});
