// The engines' model lists: the static fallback and the fakes' lists. A separate file because Node caches
// each list for the whole process (the harness starts every Node server in-process), while the binary caches
// per server; the opencode list is still a fresh cache here.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { AGY_MODELS_FALLBACK } from "../server/agy.ts";
import { fakeAgy, fakeOpencode, startServer, type StartOptions, type TestServer } from "./harness.ts";

const dirs: string[] = [];
function tempDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  dirs.push(dir);
  return dir;
}

async function withServer<T>(
  bins: Pick<StartOptions, "agy" | "opencode" | "claude">,
  fn: (srv: TestServer) => Promise<T>,
): Promise<T> {
  const repo = tempDir("api-engine-models-repo-");
  const srv = await startServer({ repoRoot: repo, dataRoot: repo, ...bins });
  try {
    return await fn(srv);
  } finally {
    await srv.close();
  }
}

const get = async (url: string): Promise<string> => {
  const res = await fetch(url);
  assert.equal(res.status, 200);
  return await res.text();
};

describe("engine model lists", () => {
  after(() => {
    for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  });

  it("a missing agy falls back to the static list", async () => {
    const missing = path.join(tempDir("api-engine-models-agy-"), "missing-agy");
    await withServer({ agy: { cmd: missing } }, async (srv) => {
      assert.equal(await get(srv.url + "/api/agy/models"), JSON.stringify(AGY_MODELS_FALLBACK));
    });
  });

  it("the fakes list their models", async () => {
    await withServer({ agy: fakeAgy(), opencode: fakeOpencode() }, async (srv) => {
      // Node already cached the fallback above for the whole process: only a per-server cache can list the
      // fake's own models here, which is what the binary does.
      const agy = await get(srv.url + "/api/agy/models");
      assert.equal(agy, JSON.stringify(srv.mode === "binary" ? ["gemini-3.8-flash-high", "claude-sonnet-4-6"] : AGY_MODELS_FALLBACK));
      assert.equal(
        await get(srv.url + "/api/opencode/models"),
        JSON.stringify(["omniroute/auto/best-coding", "omniroute/auto/fast", "google/gemini-2.5-pro", "opencode/big-pickle"]),
      );
    });
  });
});
