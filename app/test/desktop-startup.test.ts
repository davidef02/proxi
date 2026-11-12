// Pure startup helpers of the desktop shell: the UI staleness rule and the initial route, plus a source check that
// main.mjs actually uses them.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const here = import.meta.dirname;
const startup = () => import(pathToFileURL(path.join(here, "../desktop/startup.mjs")).href);

const OLD = new Date("2026-01-01T10:00:00Z");
const NEW = new Date("2026-01-01T12:00:00Z");

/** Writes a file with an explicit mtime; NTFS keeps sub-second values, so the ordering is stable. */
function write(file: string, mtime: Date): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, "x");
  fs.utimesSync(file, mtime, mtime);
}

/** Runs `fn` against a fresh temp dir and always removes it. */
async function withTemp(fn: (root: string) => void): Promise<void> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "desktop-startup-"));
  try {
    fn(root);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

test("uiIsStale: missing dist/index.html is stale", async () => {
  const { uiIsStale } = await startup();
  await withTemp((root) => {
    assert.equal(uiIsStale(root), true);
  });
});

test("uiIsStale: a build newer than every source is fresh", async () => {
  const { uiIsStale } = await startup();
  await withTemp((root) => {
    write(path.join(root, "web", "dist", "index.html"), NEW);
    write(path.join(root, "web", "index.html"), OLD);
    write(path.join(root, "web", "src", "a", "b.ts"), OLD);
    write(path.join(root, "shared", "x.mjs"), OLD);
    assert.equal(uiIsStale(root), false);
  });
});

test("uiIsStale: a nested file under web/src newer than the build is stale", async () => {
  const { uiIsStale } = await startup();
  await withTemp((root) => {
    write(path.join(root, "web", "dist", "index.html"), OLD);
    write(path.join(root, "web", "src", "a", "b.ts"), NEW);
    assert.equal(uiIsStale(root), true);
  });
});

test("uiIsStale: a shared file newer than the build is stale", async () => {
  const { uiIsStale } = await startup();
  await withTemp((root) => {
    write(path.join(root, "web", "dist", "index.html"), OLD);
    write(path.join(root, "shared", "x.mjs"), NEW);
    assert.equal(uiIsStale(root), true);
  });
});

test("uiIsStale: a missing shared directory is skipped", async () => {
  const { uiIsStale } = await startup();
  await withTemp((root) => {
    write(path.join(root, "web", "dist", "index.html"), NEW);
    write(path.join(root, "web", "src", "a", "b.ts"), OLD);
    assert.equal(uiIsStale(root), false);
  });
});

test("startUrl: a safe route is appended without doubling the slash", async () => {
  const { startUrl } = await startup();
  assert.equal(startUrl("http://127.0.0.1:5", "#/c/demo"), "http://127.0.0.1:5/#/c/demo");
  assert.equal(startUrl("http://127.0.0.1:5/", "#/c/demo"), "http://127.0.0.1:5/#/c/demo");
  assert.equal(startUrl("http://127.0.0.1:5//", "#/c/demo"), "http://127.0.0.1:5/#/c/demo");
});

test("startUrl: anything else keeps the base", async () => {
  const { startUrl } = await startup();
  for (const bad of [undefined, "", "#/c/demo?x=1", "javascript:alert(1)", "#/c/<b>", "/c/demo"]) {
    assert.equal(startUrl("http://127.0.0.1:5", bad), "http://127.0.0.1:5", JSON.stringify(bad));
  }
});

test("desktop main: uses the startup helpers", () => {
  const main = fs.readFileSync(path.join(here, "../desktop/main.mjs"), "utf8");
  assert.match(main, /startUrl\(srv\.url, env\.LEARNER_OPEN_HASH\)/);
  assert.match(main, /uiIsStale\(appRoot\)/);
  assert.ok(!main.includes("function uiIsStale"));
});
