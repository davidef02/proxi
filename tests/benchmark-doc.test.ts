import { describe, it } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const docPath = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "docs/benchmark.md");

function readDoc(): string {
  return fs.readFileSync(docPath, "utf8");
}

describe("benchmark doc", () => {
  it("names the RAM metric", () => {
    const doc = readDoc();
    assert.match(doc, /WorkingSetPrivate/);
    assert.match(doc, /Private Working Set/);
  });

  it("names the settle time", () => {
    assert.match(readDoc(), /\b10 s\b/);
  });

  it("names the interactive moment", () => {
    const doc = readDoc();
    assert.ok(doc.includes("next-action"), "missing next-action");
    assert.ok(doc.includes("#/c/demo"), "missing #/c/demo");
  });

  it("names the rerun command", () => {
    assert.ok(readDoc().includes("npm run bench:baseline"), "missing npm run bench:baseline");
  });
});
