import { afterEach, beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  ALLOWED_EMAIL,
  exportTree,
  plantFakeSecret,
  runGitleaks,
  runTrufflehog,
  scanDirectory,
  validateReviewReports,
} from "../scripts/preflight-secrets.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const NEW_FILES = [
  "scripts/preflight-secrets.mjs",
  "scripts/preflight-secrets.d.mts",
  "tests/preflight-secrets.test.ts",
];

// fixtures are assembled at runtime: this file is scanned by the pre-flight it tests
const ENV_FILE = "." + "env";
const WIN_PATH = ["C:", "\\", "Users", "\\", "alice"].join("");
const MAC_PATH = ["", "Users", "bob"].join("/");
const LINUX_PATH = ["", "home", "carol"].join("/");
const DISALLOWED_EMAIL = ["leak", "private-domain.invalid"].join("@");
const GITHUB_PAT = ["ghp", "_", "x".repeat(36)].join("");
const SYNTHETIC_SK = ["sk", "-ant-api03-real-secret-key-12345"].join("");
const SYNTHETIC_MASKED_SK = ["sk", "-ant-…7890"].join("");
const SYNTHETIC_GHO = ["gho", "_SECRET_TOKEN_123"].join("");

let tmp: string;

beforeEach(() => {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "preflight-test-"));
});

afterEach(() => {
  fs.rmSync(tmp, { recursive: true, force: true });
});

function write(relPath: string, content: string): string {
  const file = path.join(tmp, relPath);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, content);
  return file;
}

function reviewFile(name: string, overrides: Record<string, unknown> = {}): string {
  const file = path.join(tmp, name);
  fs.writeFileSync(
    file,
    JSON.stringify({
      treeHash: "tree-abc",
      provider: "alpha",
      model: "model-1",
      filesReviewed: 3,
      verdict: "no findings",
      findings: [],
      ...overrides,
    }),
  );
  return file;
}

function installed(bin: string, args: string[]): boolean {
  const result = spawnSync(bin, args, { encoding: "utf8" });
  return !result.error && result.status === 0;
}

describe("exportTree and scanDirectory", () => {
  it("export HEAD and find nothing in the tracked tree", (t) => {
    const probe = spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" });
    if (probe.error || probe.status !== 0) return t.skip("git not available");

    const destDir = path.join(tmp, "export");
    const exported = exportTree(repoRoot, destDir);
    assert.equal(exported.destDir, destDir);
    const headTree = spawnSync("git", ["rev-parse", "HEAD^{tree}"], { cwd: repoRoot, encoding: "utf8" }).stdout.trim();
    assert.equal(exported.treeHash, headTree);

    // not committed yet: copy the new files into the export so they get scanned too
    for (const relPath of NEW_FILES) {
      const target = path.join(destDir, relPath);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.copyFileSync(path.join(repoRoot, relPath), target);
    }

    const result = scanDirectory(destDir, exported.treeHash);
    assert.deepEqual(result.findings, []);
    assert.equal(result.ok, true);
    assert.equal(result.exitCode, 0);
  });

  it("flags env files by basename", () => {
    write(ENV_FILE, "A=1\n");
    write(`${ENV_FILE}.production`, "B=2\n");
    write(`nested/${ENV_FILE}`, "C=3\n");

    const result = scanDirectory(tmp);
    assert.deepEqual(
      result.findings.filter((hit) => hit.category === "env-file").map((hit) => hit.file),
      [ENV_FILE, `${ENV_FILE}.production`, `nested/${ENV_FILE}`],
    );
    assert.equal(result.counts.envFiles, 3);
    assert.equal(result.ok, false);
    assert.equal(result.exitCode, 1);
  });

  it("flags personal paths but not the ones a .gitignore names", () => {
    write("docs/win.md", `path: ${WIN_PATH}\n`);
    write("docs/mac.md", `path: ${MAC_PATH}\n`);
    write("docs/linux.md", `path: ${LINUX_PATH}\n`);
    write(".gitignore", `${WIN_PATH}\n${MAC_PATH}\n${LINUX_PATH}\n`);

    const result = scanDirectory(tmp);
    assert.deepEqual(
      result.findings.filter((hit) => hit.category === "personal-path").map((hit) => hit.file),
      ["docs/linux.md", "docs/mac.md", "docs/win.md"],
    );
    assert.equal(result.counts.personalPaths, 3);
  });

  it("allows the noreply and example.com emails only", () => {
    write("ok.md", `${ALLOWED_EMAIL}\nno-reply@example.com\n`);
    write("bad.md", `contact ${DISALLOWED_EMAIL}\n`);

    const result = scanDirectory(tmp);
    assert.deepEqual(
      result.findings.filter((hit) => hit.category === "email").map((hit) => hit.match),
      [DISALLOWED_EMAIL],
    );
    assert.equal(result.counts.emails, 1);
  });

  it("flags real key prefixes and ignores synthetic tokens, mask-image and the scanner config", () => {
    write("src/real.ts", `const token = "${GITHUB_PAT}";\n`);
    write("src/ui.tsx", 'class="[mask-image:x]"\n<li class="task-list-item">item</li>\n');
    write("app/test/synthetic.test.ts", `${SYNTHETIC_SK}\n${SYNTHETIC_MASKED_SK}\n${SYNTHETIC_GHO}\n`);
    write(".gitleaks.toml", `${["sk", "-ant-anything"].join("")}\n`);

    const result = scanDirectory(tmp);
    assert.deepEqual(
      result.findings.filter((hit) => hit.category === "key-pattern").map((hit) => hit.file),
      ["src/real.ts"],
    );
    assert.equal(result.counts.keyPatterns, 1);
  });
});

describe("validateReviewReports", () => {
  it("accepts two clean reviews from different providers", () => {
    const first = reviewFile("review-a.json", { provider: "alpha" });
    const second = reviewFile("review-b.json", { provider: "beta" });

    const result = validateReviewReports([first, second], "tree-abc");
    assert.equal(result.ok, true);
    assert.deepEqual(result.errors, []);
  });

  it("rejects the same provider, a wrong tree, findings and a missing file", () => {
    const first = reviewFile("review-a.json", { provider: "alpha" });
    const sameProvider = reviewFile("review-b.json", { provider: "alpha" });
    assert.equal(validateReviewReports([first, sameProvider], "tree-abc").ok, false);

    const wrongTree = reviewFile("review-c.json", { provider: "beta", treeHash: "tree-other" });
    assert.equal(validateReviewReports([first, wrongTree], "tree-abc").ok, false);

    const withFindings = reviewFile("review-d.json", {
      provider: "beta",
      verdict: "findings",
      findings: [{ file: "x" }],
    });
    assert.equal(validateReviewReports([first, withFindings], "tree-abc").ok, false);

    const result = validateReviewReports([first, path.join(tmp, "missing.json")], "tree-abc");
    assert.equal(result.ok, false);
    assert.match(result.errors.join("\n"), /missing\.json/);
  });
});

describe("plantFakeSecret", () => {
  it("trips the grep for a key, a personal path and an email", () => {
    const scratch = path.join(tmp, "scratch");
    const file = plantFakeSecret(scratch);
    assert.equal(fs.existsSync(file), true);

    const result = scanDirectory(scratch);
    assert.equal(result.ok, false);
    assert.equal(result.exitCode, 1);
    const categories = new Set(result.findings.map((hit) => hit.category));
    for (const category of ["key-pattern", "personal-path", "email"] as const) {
      assert.ok(categories.has(category), `missing ${category}`);
    }
  });

  it("trips gitleaks when installed", (t) => {
    if (!installed("gitleaks", ["version"])) return t.skip("gitleaks not on PATH");
    const scratch = path.join(tmp, "scratch");
    plantFakeSecret(scratch);

    const result = runGitleaks(
      scratch,
      path.join(tmp, "reports", "gitleaks.json"),
      "",
      path.join(repoRoot, ".gitleaks.toml"),
    );
    assert.equal(result.ok, false);
    assert.ok(result.findingsCount > 0, `found ${result.findingsCount}`);
  });

  it("trips trufflehog when installed", (t) => {
    if (!installed("trufflehog", ["--version"])) return t.skip("trufflehog not on PATH");
    const scratch = path.join(tmp, "scratch");
    plantFakeSecret(scratch);

    const result = runTrufflehog(scratch, path.join(tmp, "reports", "trufflehog.json"));
    assert.equal(result.ok, false);
    assert.ok(result.findingsCount > 0, `found ${result.findingsCount}`);
  });
});
