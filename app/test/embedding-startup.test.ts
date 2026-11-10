import assert from "node:assert/strict";
import * as fs from "node:fs/promises";
import path from "node:path";
import { after, describe, it } from "node:test";
import { createServer } from "../server/main.ts";
import { makeTempRepo } from "./helpers.ts";

const roots: string[] = [];
after(async () => {
  for (const root of roots) await fs.rm(root, { recursive: true, force: true });
});

describe("embedding model at startup", () => {
  it("asks for the model once, after listen, without blocking close", async () => {
    const root = await makeTempRepo();
    roots.push(root);
    const calls: string[] = [];
    const srv = await createServer({
      repoRoot: root,
      dataRoot: root,
      port: 0,
      openBrowser: false,
      checkUpdateOnStart: false,
      fetchModelOnStart: true,
      fetchModel: async (r) => {
        calls.push(r);
      },
    });
    assert.deepEqual(calls, [path.resolve(root)]);
    await srv.close();
  });

  it("close() resolves while the model fetch is still pending", async () => {
    const root = await makeTempRepo();
    roots.push(root);
    const srv = await createServer({
      repoRoot: root,
      dataRoot: root,
      port: 0,
      openBrowser: false,
      checkUpdateOnStart: false,
      fetchModelOnStart: true,
      fetchModel: () => new Promise(() => {}),
    });
    const closed = await Promise.race([
      srv.close().then(() => true),
      new Promise<boolean>((r) => setTimeout(() => r(false), 5000)),
    ]);
    assert.equal(closed, true);
  });

  it("does not fetch on a temp repo by default", async () => {
    const root = await makeTempRepo();
    roots.push(root);
    const calls: string[] = [];
    const srv = await createServer({
      repoRoot: root,
      dataRoot: root,
      port: 0,
      openBrowser: false,
      checkUpdateOnStart: false,
      fetchModel: async (r) => {
        calls.push(r);
      },
    });
    assert.deepEqual(calls, []);
    await srv.close();
  });
});
