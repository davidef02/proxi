import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

const GONE = [
  "app/server",
  "app/desktop/main.mjs",
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
    const retired = [
      "lesson",
      "anki",
      "home",
      "data",
      "embed-bench",
      "app",
      "app:serve",
      "test",
      "test:watch",
      "typecheck",
      "golden:capture",
      "desktop",
      "desktop:install",
      "desktop:uninstall",
      "desktop:smoke",
    ];
    const present = retired.filter((s) => s in (pkg.scripts ?? {}));
    assert.deepEqual(present, []);
  });

  it("no electron in the manifests", () => {
    const pkg = JSON.parse(fs.readFileSync(path.join(repoRoot, "package.json"), "utf8"));
    const lock = JSON.parse(fs.readFileSync(path.join(repoRoot, "package-lock.json"), "utf8"));

    assert.equal(pkg.dependencies?.electron, undefined);
    assert.equal(pkg.devDependencies?.electron, undefined);
    assert.equal(lock.packages?.[""]?.devDependencies?.electron, undefined);

    const desktopScripts = Object.keys(pkg.scripts ?? {}).filter(
      (s) => /^desktop(:|$)/.test(s),
    );
    assert.deepEqual(desktopScripts, []);

    assert.equal(lock.packages?.["node_modules/electron"], undefined);
    const electronPackages = Object.keys(lock.packages ?? {}).filter((k) => k.includes("@electron"));
    assert.deepEqual(electronPackages, []);
  });

  it("no stale shell references in code/config", (t) => {
    const r1 = spawnSync(
      "git",
      [
        "grep",
        "-n",
        "app/desktop",
        "--",
        "package.json",
        "package-lock.json",
        "src-tauri/tauri.conf.json",
        "scripts",
        ".github",
        ":(exclude).github/workflows/ci.yml",
        ":(exclude)app/test/cutover.test.ts",
      ],
      { cwd: repoRoot, encoding: "utf8" },
    );
    if (r1.error) return t.skip(`git not available: ${r1.error.message}`);
    const matches = (r1.stdout ?? "")
      .split("\n")
      .map((l) => l.trim())
      .filter((l) => l && !l.includes("bench:baseline"));
    if (matches.length > 0) assert.fail(`files still referencing app/desktop:\n${matches.join("\n")}`);

    const ciYaml = fs.readFileSync(path.join(repoRoot, ".github/workflows/ci.yml"), "utf8");
    const nonGuardLines = ciYaml
      .split("\n")
      .filter(
        (line) =>
          line.includes("app/desktop") &&
          !line.includes("git ls-files -- app/desktop") &&
          !line.includes("app/desktop is back"),
      );
    assert.deepEqual(nonGuardLines, []);

    const r2 = spawnSync(
      "git",
      [
        "grep",
        "-l",
        "-E",
        "desktop:install|npm run desktop\\b",
        "--",
        "AGENTS.md",
        "README.md",
        "app/README.md",
        "docs",
        ":(exclude)app/test/cutover.test.ts",
      ],
      { cwd: repoRoot, encoding: "utf8" },
    );
    if (r2.error) return t.skip(`git not available: ${r2.error.message}`);
    if (r2.status === 0) assert.fail(`files still referencing desktop:install or npm run desktop:\n${r2.stdout.trim()}`);
    if (r2.status !== 1) assert.fail(r2.stderr);
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
