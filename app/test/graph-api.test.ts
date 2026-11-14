// Map routes over plain HTTP/SSE: /api/map, /api/graph, positions and node delete, on both servers.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import type { GraphSnapshot } from "../shared/graph/contracts.ts";
import type { GlobalEvent, KnowledgeMap } from "../shared/protocol.ts";
import { startServer, type TestServer } from "./harness.ts";

type GraphEvent = Extract<GlobalEvent, { type: "graph" }>;

const lesson = {
  metadata: { lessonNumber: 1, slug: "t", title: "T", topic: "T", dateCreated: "", dateModified: "", version: "1" },
  nodes: [
    { nodeId: "alpha", title: "Alfa", tier: "axiom", content: "x" },
    { nodeId: "beta", title: "Beta", tier: "derivation", content: "x", dependencies: ["alpha", "ghost"] },
    { nodeId: "gamma", title: "Gamma", tier: "derivation", content: "x" },
  ],
};

const node = (id: string, name: string, messageId: string) => ({
  id,
  name,
  statement: `${name} vale.`,
  aliases: [],
  createdAt: "2026-01-01T00:00:00Z",
  provenance: [{ chatId: "demo/0001-t", messageId }],
});

const chain = (id: string, premises: string[], conclusion: string) => ({
  id,
  premises,
  conclusion,
  createdAt: "2026-01-01T00:00:00Z",
  provenance: [{ chatId: "demo/0001-t", messageId: "" }],
});

/** Raw files under the data folder (never through a server module). */
function seed(data: string): void {
  const write = (rel: string, text: string) => {
    const file = path.join(data, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  };
  write(
    "courses/demo/lessons/0001-t.html",
    `<html><body><script type="application/json" id="lesson-data">${JSON.stringify(lesson)}</script></body></html>`,
  );
  const progress = { kind: "check", lessonSlug: "t", nodeRefs: ["beta"], correct: false, at: "2026-01-02T00:00:00Z" };
  write("courses/demo/progress.jsonl", JSON.stringify(progress) + "\n");
  const nodes = [node("n_a", "A", "alpha"), node("n_b", "B", "beta"), node("n_c", "C", "gamma")];
  write("graph/nodes.jsonl", nodes.map((n) => JSON.stringify(n) + "\n").join("") + "{broken\n");
  const chains = [chain("arg_1", ["n_a", "n_b"], "n_c"), chain("arg_2", ["n_a"], "n_b")];
  write("graph/chains.jsonl", chains.map((c) => JSON.stringify(c) + "\n").join(""));
}

/** A fresh repo and data folder per test: ledgers and layouts never reach another test. */
async function withServer(fn: (server: TestServer, data: string) => Promise<void>): Promise<void> {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "graph-api-repo-"));
  const data = fs.mkdtempSync(path.join(os.tmpdir(), "graph-api-data-"));
  let server: TestServer | undefined;
  try {
    seed(data);
    server = await startServer({ repoRoot: repo, dataRoot: data });
    await fn(server, data);
  } finally {
    await server?.close();
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(data, { recursive: true, force: true, maxRetries: 3 });
  }
}

/** Same poll as integration.test.ts. */
async function waitFor<T>(fn: () => T | undefined | false, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Minimal SSE client: `ready` resolves when the headers arrive, `close` aborts the stream. */
function sse(url: string) {
  const events: GlobalEvent[] = [];
  const ac = new AbortController();
  const ready = fetch(url, { signal: ac.signal });
  void ready
    .then(async (res) => {
      const dec = new TextDecoder();
      let buf = "";
      for await (const chunk of res.body as unknown as AsyncIterable<Uint8Array>) {
        buf += dec.decode(chunk, { stream: true });
        let i: number;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, i);
          buf = buf.slice(i + 2);
          const data = block.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join("");
          if (data) events.push(JSON.parse(data));
        }
      }
    })
    .catch(() => {});
  return { events, ready, close: () => ac.abort() };
}

function post(server: TestServer, pathname: string, body: string): Promise<Response> {
  return fetch(`${server.url}${pathname}`, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body,
  });
}

describe("GET /api/map", () => {
  it("lists the lesson nodes with status, attempts and dependency edges", async () => {
    await withServer(async (server) => {
      const res = await fetch(`${server.url}/api/map`);
      assert.equal(res.status, 200);
      const map = (await res.json()) as KnowledgeMap;
      assert.deepEqual(map.nodes.map((n) => n.key), ["demo/t#alpha", "demo/t#beta", "demo/t#gamma"]);
      const alpha = map.nodes.find((n) => n.key === "demo/t#alpha")!;
      const beta = map.nodes.find((n) => n.key === "demo/t#beta")!;
      assert.equal(alpha.status, "taught");
      assert.equal(beta.status, "missed");
      assert.equal(beta.attempts, 1);
      assert.deepEqual(map.edges, [{ from: "demo/t#alpha", to: "demo/t#beta" }]);
      assert.equal(alpha.lessonPath, "courses/demo/lessons/0001-t.html");
    });
  });
});

describe("GET /api/graph", () => {
  it("returns the snapshot with an implication node, statuses and a settled layout", async () => {
    await withServer(async (server, data) => {
      const res = await fetch(`${server.url}/api/graph`);
      assert.equal(res.status, 200);
      const snap = (await res.json()) as GraphSnapshot;
      assert.deepEqual(snap.propositions.map((n) => n.id), ["n_a", "n_b", "n_c"]);
      const imp = snap.nodes.find((n) => n.id === "arg_1");
      assert.equal(imp?.name, "⟹");
      assert.equal(snap.propositions.find((n) => n.id === "n_b")!.status, "missed");
      assert.ok(snap.nodes.every((n) => snap.positions?.[n.id] !== undefined));

      const layout = path.join(data, "graph", "layout.json");
      assert.ok(fs.existsSync(layout));
      assert.deepEqual(Object.keys(JSON.parse(fs.readFileSync(layout, "utf8"))), Object.keys(snap.positions!));
    });
  });
});

describe("POST /api/graph/positions", () => {
  it("merges known ids into layout.json and rejects a missing positions object", async () => {
    await withServer(async (server, data) => {
      const res = await post(server, "/api/graph/positions", JSON.stringify({ positions: { n_a: { x: 5, y: 6 }, ghost: { x: 1, y: 2 } } }));
      assert.equal(res.status, 200);
      assert.deepEqual(await res.json(), { ok: true });

      const layout = fs.readFileSync(path.join(data, "graph", "layout.json"), "utf8");
      assert.ok(layout.includes('"n_a":{"x":5,"y":6}'), layout);
      assert.ok(!layout.includes("ghost"), layout);
      assert.ok(!layout.endsWith("\n"));

      for (const body of ["{}", "{"]) {
        const bad = await post(server, "/api/graph/positions", body);
        assert.equal(bad.status, 400);
        assert.deepEqual(await bad.json(), { error: "positions mancante" });
      }
    });
  });
});

describe("POST /api/graph/nodes/:id/delete", () => {
  it("removes the node, cascades its chains, emits a graph event and rejects bad ids", async () => {
    await withServer(async (server, data) => {
      const stream = sse(`${server.url}/api/events`);
      try {
        assert.equal((await stream.ready).status, 200);

        const res = await post(server, "/api/graph/nodes/n_b/delete", "{}");
        assert.equal(res.status, 200);
        assert.deepEqual(await res.json(), { ok: true });
        const event = await waitFor(() => stream.events.find((e): e is GraphEvent => e.type === "graph" && typeof e.revision === "number"));
        assert.equal(typeof event.revision, "number");
      } finally {
        stream.close();
      }

      const nodes = fs.readFileSync(path.join(data, "graph", "nodes.jsonl"), "utf8");
      assert.ok(!nodes.includes('"n_b"'), nodes);
      assert.ok(nodes.includes("{broken"), nodes);
      const chains = fs.readFileSync(path.join(data, "graph", "chains.jsonl"), "utf8");
      assert.ok(!chains.includes("arg_1"), chains);
      assert.ok(!chains.includes("arg_2"), chains);

      const snap = (await (await fetch(`${server.url}/api/graph`)).json()) as GraphSnapshot;
      assert.ok(!snap.propositions.some((n) => n.id === "n_b"));
      assert.ok(!snap.nodes.some((n) => n.id === "n_b"));

      const bad = await post(server, "/api/graph/nodes/x_1/delete", "{}");
      assert.equal(bad.status, 400);
      assert.deepEqual(await bad.json(), { error: "id non valido" });
    });
  });
});
