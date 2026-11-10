import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

// upstream text, hashed offline so the test never downloads anything
const AGPL_SHA256 = "0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0";
const SENTENCE =
  "Heavily inspired by [LearnHarness](https://github.com/amosblomqvist/learnharness) and Matt Pocock's " +
  "[/teach skill](https://github.com/mattpocock/skills/blob/main/skills/productivity/teach/SKILL.md), " +
  "Proxi tries to get the best of both worlds and glue it to Anki spaced repetition, so what you learn " +
  "actually stays.";

function read(file: string): string {
  return fs.readFileSync(path.join(repoRoot, file), "utf8").replace(/\r\n/g, "\n");
}

describe("public repo meta", () => {
  it("LICENSE is the verbatim AGPL-3.0 text", () => {
    const text = read("LICENSE");
    assert.equal(Buffer.byteLength(text), 34523);
    assert.equal(createHash("sha256").update(text).digest("hex"), AGPL_SHA256);
  });

  it("NOTICE credits LearnHarness and Matt Pocock", () => {
    const text = read("NOTICE");
    const credits = [
      "LearnHarness",
      "Amos Blomqvist",
      "https://github.com/amosblomqvist/learnharness",
      "Modified by Davide Fantauzzi in 2026",
      "Matt Pocock",
      "https://github.com/mattpocock/skills/blob/main/skills/productivity/teach/SKILL.md"
    ];
    for (const credit of credits) {
      assert.ok(text.includes(credit), `NOTICE is missing: ${credit}`);
    }
  });

  it("package.json declares AGPL-3.0-only", () => {
    assert.equal(JSON.parse(read("package.json")).license, "AGPL-3.0-only");
  });

  it("security policy and issue/PR templates exist", () => {
    const files = [
      "SECURITY.md",
      ".github/ISSUE_TEMPLATE/bug.md",
      ".github/ISSUE_TEMPLATE/feature.md",
      ".github/ISSUE_TEMPLATE/config.yml",
      ".github/pull_request_template.md"
    ];
    for (const file of files) {
      assert.ok(fs.existsSync(path.join(repoRoot, file)), `missing ${file}`);
    }
    assert.match(read(".github/ISSUE_TEMPLATE/config.yml"), /^blank_issues_enabled: false$/m);
  });

  it("README has the sentence, presentation, screenshot and four sections, under 60 lines", () => {
    const text = read("README.md");
    const lines = text.replace(/\n$/, "").split("\n");
    assert.ok(lines.length < 60, `README has ${lines.length} lines`);

    let inFence = false;
    const headings: string[] = [];
    for (const line of lines) {
      if (line.startsWith("```")) {
        inFence = !inFence;
      } else if (!inFence && /^#{1,6}\s/.test(line)) {
        headings.push(line);
      }
    }
    assert.deepEqual(headings, ["## Install", "## Requirements", "## Build", "## License"]);

    const firstHeading = lines.findIndex((line) => /^#{1,6}\s/.test(line));
    const blocks = lines
      .slice(0, firstHeading)
      .join("\n")
      .split(/\n\s*\n/)
      .map((block) => block.trim())
      .filter(Boolean);
    assert.equal(blocks.length, 3, "preamble should be sentence, presentation and screenshot");
    assert.equal(blocks[0], SENTENCE);
    const presentation = blocks[1].split("\n");
    assert.ok(presentation.length >= 1 && presentation.length <= 3, `${presentation.length} presentation lines`);
    assert.match(blocks[1], /Anki/);
    assert.match(blocks[2], /^!\[[^\]]*\]\(docs\/screenshot\.png\)$/);

    const png = fs.readFileSync(path.join(repoRoot, "docs/screenshot.png"));
    assert.deepEqual([...png.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  });

  it("no internal docs or visual-tools are tracked", (t) => {
    const r = spawnSync("git", ["ls-files", "docs/issues", "docs/plan", "docs/research", "extensions/visual-tools"], {
      cwd: repoRoot,
      encoding: "utf8"
    });
    if (r.error) return t.skip(`git not available: ${r.error.message}`);
    if (r.status !== 0) assert.fail(r.stderr);

    const files = r.stdout.split(/\r?\n/).filter(Boolean);
    assert.deepEqual(files, [], `internal files tracked:\n${files.join("\n")}`);
  });

  it("no tracked file links to internal docs", (t) => {
    // assembled at runtime so this file's own source never matches
    const pattern = ["docs/(plan|research)", "/|", "PLAN", "-[0-9]"].join("");
    // .gitignore names those paths to ignore them; ignore rules are not links
    const r = spawnSync("git", ["grep", "-l", "-I", "-E", pattern, "--", ".", ":!.gitignore"], {
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

    assert.deepEqual(files, [], `links to internal docs in:\n${files.join("\n")}`);
  });

  it("no personal paths in tracked files", (t) => {
    // same trick: split pieces keep this file from matching itself
    const pattern = ["C:", "\\\\", "Users", "\\\\", "|/", "Users/|/", "home/[a-z]"].join("");
    const r = spawnSync("git", ["grep", "-l", "-I", "-E", pattern], {
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

    assert.deepEqual(files, [], `personal paths in:\n${files.join("\n")}`);
  });
});
