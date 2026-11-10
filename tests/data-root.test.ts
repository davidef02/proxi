import { describe, it, beforeEach, afterEach, mock } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  DATA_ROOT_ENV,
  dataRootProblem,
  defaultDataRoot,
  hasData,
  readDataRootPointer,
  resolveDataRoot,
  runCli,
  writeDataRootPointer,
} from "../extensions/data-root.ts";

let tmp: string;
let repo: string;
let home: string;

function write(file: string, content: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, content);
}

function pointerFile(root: string): string {
  return path.join(root, ".learner", "config.json");
}

beforeEach(() => {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "data-root-"));
  repo = path.join(tmp, "repo");
  home = path.join(tmp, "home");
  fs.mkdirSync(repo, { recursive: true });
  fs.mkdirSync(home, { recursive: true });
});

afterEach(() => {
  fs.rmSync(tmp, { recursive: true, force: true });
});

describe("resolveDataRoot", () => {
  it("env wins over the pointer, the pointer over the default", () => {
    const pointed = path.join(tmp, "pointed");
    const envRoot = path.join(tmp, "from-env");
    writeDataRootPointer(repo, pointed);

    assert.deepEqual(resolveDataRoot(repo, { [DATA_ROOT_ENV]: envRoot }, home), {
      dataRoot: envRoot,
      source: "env",
    });
    assert.deepEqual(resolveDataRoot(repo, {}, home), { dataRoot: pointed, source: "config" });

    fs.rmSync(pointerFile(repo));
    assert.deepEqual(resolveDataRoot(repo, {}, home), {
      dataRoot: defaultDataRoot(home),
      source: "default",
    });
  });

  it("an empty or whitespace env value is ignored", () => {
    const r = resolveDataRoot(repo, { [DATA_ROOT_ENV]: "   " }, home);
    assert.equal(r.source, "default");
  });

  it("a relative env path is resolved to an absolute path", () => {
    const r = resolveDataRoot(repo, { [DATA_ROOT_ENV]: "rel-data" }, home);
    assert.equal(r.source, "env");
    assert.equal(r.dataRoot, path.resolve("rel-data"));
    assert.ok(path.isAbsolute(r.dataRoot));
  });

  it("a repo that still holds courses/ or curriculum.json resolves to the default, not the repo", () => {
    fs.mkdirSync(path.join(repo, "courses"));
    write(path.join(repo, "curriculum.json"), "{}");
    assert.equal(hasData(repo), true);
    assert.deepEqual(resolveDataRoot(repo, {}, home), {
      dataRoot: defaultDataRoot(home),
      source: "default",
    });
  });

  it("the default is <home>/Documents/proxi-data", () => {
    assert.equal(defaultDataRoot(home), path.join(home, "Documents", "proxi-data"));
  });

  it("never creates anything", () => {
    resolveDataRoot(repo, {}, home);
    assert.equal(fs.existsSync(defaultDataRoot(home)), false);
    assert.equal(fs.existsSync(path.join(repo, ".learner")), false);
  });
});

describe("data root pointer", () => {
  it("returns undefined for a missing, invalid or relative pointer", () => {
    assert.equal(readDataRootPointer(repo), undefined);
    write(pointerFile(repo), "{not json");
    assert.equal(readDataRootPointer(repo), undefined);
    write(pointerFile(repo), JSON.stringify({ dataRoot: "relative/path" }));
    assert.equal(readDataRootPointer(repo), undefined);
    write(pointerFile(repo), JSON.stringify({ dataRoot: "  " }));
    assert.equal(readDataRootPointer(repo), undefined);
  });

  it("merges into an existing config.json, keeping other keys", () => {
    write(pointerFile(repo), JSON.stringify({ other: 1, dataRoot: "/old" }));
    const target = path.join(tmp, "data");
    writeDataRootPointer(repo, target);
    const parsed = JSON.parse(fs.readFileSync(pointerFile(repo), "utf8"));
    assert.deepEqual(parsed, { other: 1, dataRoot: path.resolve(target) });
    assert.equal(readDataRootPointer(repo), path.resolve(target));
  });

  it("overwrites an invalid config.json", () => {
    write(pointerFile(repo), "garbage");
    const target = path.join(tmp, "data");
    writeDataRootPointer(repo, target);
    assert.equal(readDataRootPointer(repo), path.resolve(target));
  });
});

describe("dataRootProblem", () => {
  it("rejects a relative path", () => {
    assert.equal(
      dataRootProblem(repo, "relative/data"),
      "La cartella dei dati deve essere un percorso assoluto."
    );
  });

  it("rejects a folder inside the repo", () => {
    assert.match(dataRootProblem(repo, path.join(repo, "data")) ?? "", /non può stare dentro/);
    assert.match(dataRootProblem(repo, path.join(repo, "data")) ?? "", /dentro il codice di Proxi/);
  });

  it("rejects the repo itself and a folder that contains the repo", () => {
    assert.match(dataRootProblem(repo, repo) ?? "", /non può stare dentro/);
    assert.match(dataRootProblem(repo, tmp) ?? "", /non può stare dentro/);
  });

  it("accepts a sibling folder", () => {
    assert.equal(dataRootProblem(repo, path.join(tmp, "data")), undefined);
  });
});

describe("runCli", () => {
  let savedEnv: string | undefined;

  beforeEach(() => {
    savedEnv = process.env[DATA_ROOT_ENV];
    process.env[DATA_ROOT_ENV] = path.join(tmp, "env-data");
  });

  afterEach(() => {
    if (savedEnv === undefined) delete process.env[DATA_ROOT_ENV];
    else process.env[DATA_ROOT_ENV] = savedEnv;
  });

  it("where returns 0", async () => {
    assert.equal(await runCli(["where"]), 0);
  });

  it("an unknown command returns 1", async () => {
    assert.equal(await runCli(["nope"]), 1);
  });

  it("migrate is gone: help does not list it and the command returns 1", async () => {
    const log = mock.method(console, "log");
    const error = mock.method(console, "error");
    try {
      assert.equal(await runCli(["help"]), 0);
      const help = log.mock.calls.map((c) => c.arguments.join(" ")).join("\n");
      assert.ok(!help.includes("migrate"));
      assert.ok(help.includes("proxi-data"));
      assert.equal(await runCli(["migrate"]), 1);
    } finally {
      log.mock.restore();
      error.mock.restore();
    }
  });
});

describe("the code repo does not track learner data", () => {
  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

  it(".gitignore ignores every data root, anchored to the repo root", () => {
    const lines = fs.readFileSync(path.join(repoRoot, ".gitignore"), "utf8").split(/\r?\n/).map((l) => l.trim());
    for (const entry of ["/courses/", "/graph/", "/decks/", "/curriculum.json", "/index.html"]) assert.ok(lines.includes(entry), entry);
  });

  it("git tracks no learner data, and the app test fixture is not ignored", () => {
    let tracked: string;
    try {
      tracked = execFileSync("git", ["ls-files", "courses", "graph", "decks", "curriculum.json", "index.html"], { cwd: repoRoot, encoding: "utf8" });
    } catch {
      return; // no git (e.g. a source tarball): nothing to check
    }
    assert.equal(tracked.trim(), "");
    const fixture = spawnSync("git", ["check-ignore", "-q", "app/test/fixtures/repo/courses/demo/MISSION.md"], { cwd: repoRoot });
    assert.equal(fixture.status, 1, "the fixture course must not be ignored");
  });
});
