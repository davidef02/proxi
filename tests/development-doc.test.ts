import { describe, it } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const docPath = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "docs/agents/development.md");
const heading = "## The `proxi` binary";

function readDoc(): string {
  return fs.readFileSync(docPath, "utf8");
}

function binarySection(): string {
  const lines = readDoc().split("\n");
  const start = lines.findIndex((line) => line.trimEnd() === heading);
  assert.notEqual(start, -1, "missing section " + heading);
  const end = lines.findIndex((line, i) => i > start && line.startsWith("## "));
  return lines.slice(start, end === -1 ? undefined : end).join("\n");
}

describe("development doc: the proxi binary", () => {
  it("names the release build", () => {
    assert.ok(
      binarySection().includes("cargo build --release --locked"),
      "missing cargo build --release --locked",
    );
  });

  it("names the Rust tests", () => {
    assert.ok(binarySection().includes("cargo test --locked"), "missing cargo test --locked");
  });

  it("names the build output folder", () => {
    assert.ok(binarySection().includes("target/release"), "missing target/release");
  });

  it("names the installer and the version check", () => {
    const section = binarySection();
    assert.ok(section.includes("npm run desktop:install"), "missing npm run desktop:install");
    assert.ok(section.includes("proxi --version"), "missing proxi --version");
  });
});
