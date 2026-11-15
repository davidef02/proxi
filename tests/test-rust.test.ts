import { afterEach, beforeEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { RUST_PORTED, missingFiles, rustTestArgs, rustTestEnv } from "../scripts/test-rust.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

let tmp: string;

beforeEach(() => {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "test-rust-"));
});

afterEach(() => {
  fs.rmSync(tmp, { recursive: true, force: true });
});

describe("rustTestEnv", () => {
  it("forces PROXI_TEST_SERVER to binary and keeps every other key", () => {
    assert.deepEqual(rustTestEnv({ PATH: "p", FOO: "1" }), {
      PATH: "p",
      FOO: "1",
      PROXI_TEST_SERVER: "binary",
    });
  });

  it("overrides an inherited mode without mutating the input", () => {
    const base = { PROXI_TEST_SERVER: "node" };
    assert.equal(rustTestEnv(base).PROXI_TEST_SERVER, "binary");
    assert.equal(base.PROXI_TEST_SERVER, "node");
  });

  it("defaults to process.env and carries PATH", () => {
    const env = rustTestEnv();
    assert.equal(env.PROXI_TEST_SERVER, "binary");
    const pathKey = Object.keys(process.env).find((key) => key.toUpperCase() === "PATH");
    assert.ok(pathKey, "process.env has no PATH");
    assert.equal(env[pathKey], process.env[pathKey]);
  });
});

describe("rustTestArgs", () => {
  it("runs the ported files through tsx", () => {
    assert.deepEqual(rustTestArgs(), ["--import", "tsx", "--test", ...RUST_PORTED]);
  });
});

describe("RUST_PORTED", () => {
  it("lists only app test files, harness included", () => {
    for (const file of RUST_PORTED) {
      assert.match(file, /^app\/test\/.+\.test\.ts$/);
    }
    assert.ok(RUST_PORTED.includes("app/test/harness.test.ts"));
  });

  it("runs the Anki routes against the spawned binary", () => {
    assert.ok(RUST_PORTED.includes("app/test/api-anki.test.ts"));
  });

  it("includes the graph API test ported to Rust", () => {
    assert.ok(RUST_PORTED.includes("app/test/graph-api.test.ts"));
  });

  it("runs the exercises routes against the spawned binary", () => {
    assert.ok(RUST_PORTED.includes("app/test/api-exercises.test.ts"));
  });
});

describe("missingFiles", () => {
  it("returns the entries that do not exist, in order", () => {
    fs.writeFileSync(path.join(tmp, "a.test.ts"), "");
    assert.deepEqual(missingFiles(tmp, ["a.test.ts", "b.test.ts"]), ["b.test.ts"]);
  });

  it("finds every ported file in the repo", () => {
    assert.deepEqual(missingFiles(repoRoot), []);
  });
});
