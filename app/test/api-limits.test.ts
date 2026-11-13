// GET /api/limits and the switches that disable it, identical against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { startServer, type TestServer } from "./harness.ts";

/** What `.learner/limits.json` holds after a claude turn reported the 5-hour window. */
const seeded = { fiveHour: { utilization: 0.42, resetsAt: 1700000000 }, at: "2026-11-12T10:00:00.000Z" };

function seedLimits(repo: string): void {
  const dir = path.join(repo, ".learner");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "limits.json"), JSON.stringify(seeded));
}

async function withServer(
  opts: { seed?: (repo: string) => void },
  fn: (server: TestServer, repo: string, data: string) => Promise<void>,
): Promise<void> {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "api-limits-repo-"));
  const data = fs.mkdtempSync(path.join(os.tmpdir(), "api-limits-data-"));
  let server: TestServer | undefined;
  try {
    // Before start: both servers cache the first read for the process' lifetime.
    opts.seed?.(repo);
    server = await startServer({ repoRoot: repo, dataRoot: data });
    await fn(server, repo, data);
  } finally {
    await server?.close();
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(data, { recursive: true, force: true, maxRetries: 3 });
  }
}

function saveSettings(server: TestServer, patch: unknown): Promise<Response> {
  return fetch(`${server.url}/api/settings`, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: JSON.stringify(patch),
  });
}

async function getLimits(server: TestServer): Promise<unknown> {
  const res = await fetch(`${server.url}/api/limits`);
  assert.equal(res.status, 200);
  return res.json();
}

describe("GET /api/limits", () => {
  it("is null when no claude turn has reported limits yet", async () => {
    await withServer({}, async (server) => {
      assert.equal(await getLimits(server), null);
    });
  });

  it("returns the seeded limits until the provider disables them", async () => {
    await withServer({ seed: seedLimits }, async (server) => {
      assert.deepEqual(await getLimits(server), seeded);

      assert.equal((await saveSettings(server, { provider: "agy" })).status, 200);
      assert.equal(await getLimits(server), null);
    });
  });

  it("is null once claudeAuth switches to apiKey", async () => {
    await withServer({ seed: seedLimits }, async (server) => {
      assert.equal((await saveSettings(server, { claudeAuth: "apiKey" })).status, 200);
      assert.equal(await getLimits(server), null);
    });
  });
});
