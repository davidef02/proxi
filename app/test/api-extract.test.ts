// Graph sources and extract over plain HTTP, on both servers.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, it } from "node:test";
import { fakeClaude, removeTree, startServer, type TestServer } from "./harness.ts";

const NODE = {
  nodeId: "ref-scatola",
  title: "Il ref",
  tier: "axiom",
  content: "<p>Un <code>ref</code> è una scatola. FAKE:slow:300</p>",
  quiz: { question: "Che cosa è un ref?", options: ["a", "una scatola"], correctIndex: 2, explanation: "e" },
};

function island(nodes: unknown[], title: string, lessonNumber: number): string {
  const doc = { metadata: { lessonNumber, topic: "t", title, slug: title.toLowerCase().replaceAll(" ", "-"), dateCreated: "", dateModified: "", version: "1" }, nodes };
  return `<html><body><script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script></body></html>`;
}

describe("graph extract routes over HTTP", () => {
  const servers: TestServer[] = [];
  const dirs: string[] = [];

  afterEach(async () => {
    for (const server of servers.splice(0)) await server.close();
    for (const dir of dirs.splice(0)) await removeTree(dir);
  });

  async function setup(): Promise<{ server: TestServer }> {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "api-extract-"));
    const repo = path.join(root, "repo");
    const data = path.join(root, "data");
    fs.mkdirSync(repo);
    fs.mkdirSync(path.join(data, "courses", "demo", "lessons"), { recursive: true });
    fs.writeFileSync(path.join(data, "courses", "demo", "lessons", "0001-il-ref.html"), island([NODE], "Il ref è una scatola", 1));
    fs.writeFileSync(path.join(data, "courses", "demo", "lessons", "0002-prosa.html"), "<html><head><title>Lezione in prosa</title></head><body><h1>Titolo</h1><p>Le props scendono. FAKE:slow:300</p></body></html>");
    const server = await startServer({ repoRoot: repo, dataRoot: data, claude: fakeClaude() });
    dirs.push(root);
    servers.push(server);
    return { server };
  }

  const post = (server: TestServer, bodyOrPath: unknown, body: unknown = {}) => {
    const route = typeof bodyOrPath === "string" && bodyOrPath.startsWith("/") ? bodyOrPath : "/api/graph/extract";
    const value = route === "/api/graph/extract" ? bodyOrPath : body;
    return fetch(`${server.url}${route}`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-learner-client": "1" },
      body: typeof value === "string" ? value : JSON.stringify(value),
    });
  };

  it("GET sources lists every lesson, uncovered", async () => {
    const { server } = await setup();
    const response = await fetch(`${server.url}/api/graph/sources`);
    assert.equal(response.status, 200);
    const sources = await response.json();
    const expected = [
      { id: "demo/0001-il-ref", course: "demo", lessonTitle: "Il ref è una scatola", lessonNumber: 1, lessonPath: "courses/demo/lessons/0001-il-ref.html", covered: false, stale: false, isolated: 0 },
      { id: "demo/0002-prosa", course: "demo", lessonTitle: "Lezione in prosa", lessonNumber: 2, lessonPath: "courses/demo/lessons/0002-prosa.html", covered: false, stale: false, isolated: 0 },
    ];
    assert.deepStrictEqual(sources, expected);
    assert.equal(JSON.stringify(sources), JSON.stringify(expected));
  });

  it("unknown sources answer 400 and start nothing", async () => {
    const { server } = await setup();
    for (const [body, error] of [
      [{ sources: ["demo/nope"] }, "unknown source: demo/nope"],
      [{ sources: ["demo/0001-il-ref", "demo/a", "demo/b"] }, "unknown source: demo/a, demo/b"],
    ] as const) {
      const response = await post(server, body);
      assert.equal(response.status, 400);
      assert.deepEqual(await response.json(), { error });
    }
    assert.deepEqual(await (await fetch(`${server.url}/api/jobs`)).json(), []);
  });

  it("extract starts one map job per pending lesson, skips in-flight ones, forces listed ones", async () => {
    const { server } = await setup();
    const first = await post(server, {});
    assert.equal(first.status, 200);
    const started = (await first.json()).jobs;
    assert.equal(started.length, 2);
    assert.deepEqual(started.map((job: { kind: string; status: string; title: string }) => [job.kind, job.status, job.title]), [
      ["map", "running", "Mappa: Il ref è una scatola"],
      ["map", "running", "Mappa: Lezione in prosa"],
    ]);
    assert.deepEqual(await (await post(server, {})).json(), { jobs: [] });
    assert.deepEqual(await (await post(server, { sources: ["demo/0001-il-ref"] })).json(), { jobs: [] });
    for (const job of started) await post(server, `/api/jobs/${job.id}/cancel`);
    const forced = await post(server, { sources: ["demo/0001-il-ref"] });
    const forcedJobs = (await forced.json()).jobs;
    assert.equal(forcedJobs.length, 1);
    await post(server, `/api/jobs/${forcedJobs[0].id}/cancel`);
    const invalid = await post(server, "{");
    assert.equal(invalid.status, 200);
    const invalidJobs = (await invalid.json()).jobs;
    assert.equal(invalidJobs.length, 2);
    for (const job of invalidJobs) await post(server, `/api/jobs/${job.id}/cancel`);
  });
});
