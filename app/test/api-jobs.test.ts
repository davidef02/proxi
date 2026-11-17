// Job routes and the job MCP endpoint over plain HTTP, on both servers.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, it } from "node:test";
import { SESSIONS_DATA_SEED, waitForStatus } from "./golden-scripts.ts";
import { fakeClaude, removeTree, startServer, type TestServer } from "./harness.ts";

const BRIEF = "Vue 3.5 rende reattivo un ref leggendo .value dentro un effect: la dipendenza viene registrata al momento della lettura. Fonte primaria: la documentazione ufficiale.";
const SUBMIT = { tool: "submit_research", input: { brief: BRIEF, sources: [{ title: "Vue docs", url: "https://vuejs.org/guide/essentials/reactivity-fundamentals.html" }] } };

function encoded(value: unknown): string {
  return Buffer.from(JSON.stringify(value)).toString("base64");
}

function researchMessage(question: string): string {
  return `FAKE:tool:research:${encoded({ question })}`;
}

function jobScript(steps: unknown[]): string {
  return `FAKE:job:${encoded(steps)}`;
}

async function waitFor<T>(fn: () => T | undefined | false | Promise<T | undefined | false>, ms = 15_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

describe("job routes over HTTP", () => {
  const servers: TestServer[] = [];
  const dirs: string[] = [];

  afterEach(async () => {
    for (const server of servers.splice(0)) await server.close();
    for (const dir of dirs.splice(0)) await removeTree(dir);
  });

  async function setup(): Promise<{ repo: string; data: string; server: TestServer }> {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "api-jobs-"));
    const repo = path.join(root, "repo");
    const data = path.join(root, "data");
    fs.mkdirSync(repo);
    fs.mkdirSync(data);
    fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
    fs.writeFileSync(path.join(repo, ".learner", "settings.json"), '{ "autoMap": false }\n');
    for (const [rel, text] of Object.entries(SESSIONS_DATA_SEED)) {
      const file = path.join(data, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, text);
    }
    const server = await startServer({ repoRoot: repo, dataRoot: data, claude: fakeClaude() });
    dirs.push(root);
    servers.push(server);
    return { repo, data, server };
  }

  async function post(base: string, route: string, body: unknown): Promise<Response> {
    return fetch(base + route, {
      method: "POST",
      headers: { "content-type": "application/json", "x-learner-client": "1" },
      body: JSON.stringify(body),
    });
  }

  async function onlyJob(base: string): Promise<Record<string, any>> {
    return waitFor(async () => {
      const response = await fetch(`${base}/api/jobs`);
      const jobs = (await response.json()) as Record<string, any>[];
      return jobs.length === 1 ? jobs[0] : undefined;
    });
  }

  async function jobUntil(base: string, id: string, predicate: (job: Record<string, any>) => boolean): Promise<Record<string, any>> {
    return waitFor(async () => {
      const response = await fetch(`${base}/api/jobs/${id}`);
      const job = (await response.json()) as Record<string, any>;
      return predicate(job) ? job : undefined;
    });
  }

  async function jobToken(repo: string, id: string): Promise<string> {
    return waitFor(() => {
      try {
        const mcp = JSON.parse(fs.readFileSync(path.join(repo, ".learner", "jobs", id, "mcp.json"), "utf8"));
        return mcp.mcpServers.job.headers["x-learner-token"];
      } catch {
        return undefined;
      }
    });
  }

  async function createTeach(server: TestServer): Promise<string> {
    const response = await post(server.url, "/api/sessions", {
      kind: "teach", course: "demo", model: "haiku", ankiOptIn: false, target: { mode: "planned", title: "Prima lezione" },
    });
    assert.equal(response.status, 200);
    const id = ((await response.json()) as { id: string }).id;
    await waitForStatus(server.url, id, "idle");
    return id;
  }

  it("research job: done with cost and result, persisted; list is slim, GET has the result", async () => {
    const { repo, server } = await setup();
    const session = await createTeach(server);
    const response = await post(server.url, `/api/sessions/${session}/message`, { text: researchMessage("Cos'è un ref? " + jobScript([SUBMIT])) });
    assert.equal(response.status, 200);
    const listed = await onlyJob(server.url);
    const job = await jobUntil(server.url, listed.id, (value) => value.status === "done" && value.delivered === true);
    assert.equal(job.result.kind, "research");
    assert.match(job.result.brief, /Vue 3\.5/);
    assert.equal(job.result.sources[0].title, "Vue docs");
    assert.ok(job.costUsd > 0);
    assert.equal(job.activity, "");
    const saved = JSON.parse(fs.readFileSync(path.join(repo, ".learner", "jobs", `${job.id}.json`), "utf8"));
    assert.equal(saved.status, "done");
    assert.ok(fs.existsSync(path.join(repo, ".learner", "jobs", job.id, "system.md")));
    const mcp = JSON.parse(fs.readFileSync(path.join(repo, ".learner", "jobs", job.id, "mcp.json"), "utf8"));
    assert.equal(mcp.mcpServers.job.url, `${server.url}/mcp/job/${job.id}`);
    const slim = (await (await fetch(`${server.url}/api/jobs`)).json() as Record<string, any>[]).find((value) => value.id === job.id)!;
    assert.equal(slim.status, "done");
    assert.equal("result" in slim, false);
    const unknown = await fetch(`${server.url}/api/jobs/nope`);
    assert.equal(unknown.status, 404);
    assert.deepEqual(await unknown.json(), { error: "Job not found" });
  });

  it("job endpoint: 401 without token, 404 unknown, tools/list, 409 once settled; cancel answers", async () => {
    const { repo, server } = await setup();
    const session = await createTeach(server);
    await post(server.url, `/api/sessions/${session}/message`, { text: researchMessage("lento FAKE:slow:300") });
    const job = await onlyJob(server.url);
    const token = await jobToken(repo, job.id);
    const mcp = (id: string, suppliedToken?: string) => fetch(`${server.url}/mcp/job/${id}`, {
      method: "POST",
      headers: { "content-type": "application/json", ...(suppliedToken ? { "x-learner-token": suppliedToken } : {}) },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
    });
    assert.equal((await mcp(job.id)).status, 401);
    const unknown = await mcp("nope", token);
    assert.equal(unknown.status, 404);
    assert.deepEqual(await unknown.json(), { error: "Job not found" });
    await jobUntil(server.url, job.id, (value) => value.status === "running");
    const tools = await mcp(job.id, token);
    assert.deepEqual((await tools.json()).result.tools.map((tool: { name: string }) => tool.name), ["submit_research"]);
    const cancelled = await post(server.url, `/api/jobs/${job.id}/cancel`, {});
    assert.equal(cancelled.status, 200);
    assert.deepEqual(await cancelled.json(), { ok: true });
    const settled = await jobUntil(server.url, job.id, (value) => value.status === "cancelled");
    assert.equal("error" in settled, false);
    const refused = await mcp(job.id, token);
    assert.equal(refused.status, 409);
    assert.deepEqual(await refused.json(), { error: "Job is not running" });
    assert.equal((await post(server.url, `/api/jobs/${job.id}/cancel`, {})).status, 200);
    assert.equal((await post(server.url, "/api/jobs/nope/cancel", {})).status, 404);
  });
});
