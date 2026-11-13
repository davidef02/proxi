// GET/POST /api/settings over plain HTTP, identical against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { startServer, type TestServer } from "./harness.ts";

// Built at runtime: a literal `sk-ant…` key would trip the secret preflight (L12/L13).
const key = ["sk", "-ant-api03-", "harness0123456789"].join("");
const mask = `${key.slice(0, 7)}…${key.slice(-4)}`;

/** A fresh repo and a separate data folder per test: the settings file leaks between tests otherwise. */
async function withServer(fn: (server: TestServer, repo: string, data: string) => Promise<void>): Promise<void> {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "api-settings-repo-"));
  const data = fs.mkdtempSync(path.join(os.tmpdir(), "api-settings-data-"));
  let server: TestServer | undefined;
  try {
    server = await startServer({ repoRoot: repo, dataRoot: data });
    await fn(server, repo, data);
  } finally {
    await server?.close();
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(data, { recursive: true, force: true, maxRetries: 3 });
  }
}

function post(server: TestServer, body: unknown): Promise<Response> {
  return fetch(`${server.url}/api/settings`, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

/** Raw request: lets the test choose Host and send un-normalised bodies. */
function raw(port: number, p: string, o: { method?: string; headers?: Record<string, string> } = {}): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: "127.0.0.1", port, path: p, method: o.method ?? "GET", headers: o.headers }, (res) => {
      let body = "";
      res.on("data", (d) => (body += d));
      res.on("end", () => resolve({ status: res.statusCode!, body }));
    });
    req.on("error", reject);
    req.end(o.method === "POST" ? "{}" : undefined);
  });
}

describe("GET/POST /api/settings", () => {
  it("serves the defaults and the absolute roots", async () => {
    await withServer(async (server, repo, data) => {
      const res = await fetch(`${server.url}/api/settings`);
      assert.equal(res.status, 200);
      const body = (await res.json()) as Record<string, any>;
      assert.equal(body.provider, "claude");
      assert.equal(body.anthropicApiKey, "");
      assert.equal(body.repoRoot, path.resolve(repo));
      assert.equal(body.dataRoot, path.resolve(data));
    });
  });

  it("stores the API key and only ever returns the mask", async () => {
    await withServer(async (server, repo) => {
      const res = await post(server, { anthropicApiKey: key });
      assert.equal(res.status, 200);
      const text = await res.text();
      assert.equal((JSON.parse(text) as Record<string, any>).anthropicApiKey, mask);
      assert.ok(!text.includes(key));

      const saved = JSON.parse(fs.readFileSync(path.join(repo, ".learner", "settings.json"), "utf-8"));
      assert.equal(saved.anthropicApiKey, key);

      const get = await fetch(`${server.url}/api/settings`);
      assert.equal(((await get.json()) as Record<string, any>).anthropicApiKey, mask);
    });
  });

  it("rejects an invalid or unknown body with a JSON error", async () => {
    await withServer(async (server) => {
      const invalid = await post(server, "{");
      assert.equal(invalid.status, 400);
      assert.deepEqual(await invalid.json(), { error: "Invalid JSON body" });

      const unknown = await post(server, { nope: 1 });
      assert.equal(unknown.status, 400);
      assert.equal(typeof ((await unknown.json()) as Record<string, any>).error, "string");
    });
  });

  it("keeps the Host and x-learner-client guards", async () => {
    await withServer(async (server) => {
      const foreign = await raw(server.port, "/api/settings", { headers: { host: `evil.example:${server.port}` } });
      assert.equal(foreign.status, 403);
      assert.deepEqual(JSON.parse(foreign.body), { error: "Forbidden host" });

      const noClient = await raw(server.port, "/api/settings", { method: "POST" });
      assert.equal(noClient.status, 403);
      assert.deepEqual(JSON.parse(noClient.body), { error: "Missing x-learner-client header" });
    });
  });
});
