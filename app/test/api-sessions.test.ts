// The session API over HTTP and SSE only (no manager internals), so it runs against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import type { ServerEvent, SessionState } from "../shared/protocol.ts";
import { fakeClaude, startServer, type TestServer } from "./harness.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // closed port: never reach the real Anki

const SEED = {
  chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }],
};

function tempDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

/** A data folder with one course and one planned lesson. */
function seedData(): string {
  const data = tempDir("api-sessions-data-");
  fs.writeFileSync(path.join(data, "curriculum.json"), JSON.stringify(SEED));
  fs.mkdirSync(path.join(data, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(data, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(data, "courses", "demo", "NOTES.md"), "# Notes\n");
  return data;
}

async function waitFor<T>(fn: () => T | undefined | false | Promise<T | undefined | false>, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Windows: a killed binary leaves its engine child holding the folder for a moment; retry instead of failing the teardown. */
async function removeTree(dir: string): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 1 });
      return;
    } catch (error) {
      if (attempt === 40) throw error;
      await new Promise((r) => setTimeout(r, 250));
    }
  }
}

/** Minimal SSE client: parses `data:` lines from a fetch stream into ServerEvents. */
function sse(url: string) {
  const events: ServerEvent[] = [];
  const ac = new AbortController();
  void (async () => {
    const res = await fetch(url, { signal: ac.signal });
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
  })().catch(() => {});
  return { events, close: () => ac.abort() };
}

describe("api sessions over HTTP", () => {
  let root: string;
  let data: string;
  let server: TestServer;
  let teachId: string;

  const start = () => startServer({ repoRoot: root, dataRoot: data, claude: fakeClaude() });
  const post = (p: string, body: unknown = {}) =>
    fetch(server.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const get = async (id: string): Promise<SessionState> => {
    const r = await fetch(`${server.url}/api/sessions/${id}`);
    assert.equal(r.status, 200);
    return (await r.json()) as SessionState;
  };
  const idle = (id: string) => waitFor(async () => (await get(id)).status === "idle");

  before(async () => {
    root = tempDir("api-sessions-repo-");
    data = seedData();
    server = await start();
  });
  after(async () => {
    await server.close();
    await removeTree(root);
    await removeTree(data);
  });

  it("guards and validation", async () => {
    const noHeader = await fetch(server.url + "/api/sessions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    });
    assert.equal(noHeader.status, 403);

    const badJson = await fetch(server.url + "/api/sessions", {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "application/json" },
      body: "{",
    });
    assert.equal(badJson.status, 400);
    assert.equal(typeof ((await badJson.json()) as any).error, "string");

    const invalid = [
      { kind: "teach", course: "Bad Name" },
      { kind: "teach", course: "nope" },
      { kind: "teach", course: "demo", model: "haiku", ankiOptIn: false, target: { mode: "planned", title: "Prima Lezione" } },
    ];
    for (const body of invalid) {
      const r = await post("/api/sessions", body);
      assert.equal(r.status, 400, JSON.stringify(body));
      assert.equal(typeof ((await r.json()) as any).error, "string", JSON.stringify(body));
    }

    for (const p of ["/api/sessions/nope", "/api/sessions/nope/events"]) {
      const r = await fetch(server.url + p);
      assert.equal(r.status, 404, p);
      assert.equal(typeof ((await r.json()) as any).error, "string", p);
    }

    // The body is validated before the id lookup (Node's textBody runs before manager.message).
    for (const body of [{}, { text: "" }, { text: "x".repeat(20001) }]) {
      const r = await post("/api/sessions/nope/message", body);
      assert.equal(r.status, 400, JSON.stringify(body));
      assert.equal(typeof ((await r.json()) as any).error, "string", JSON.stringify(body));
    }
    const unknown = await post("/api/sessions/nope/message", { text: "ciao" });
    assert.equal(unknown.status, 404);
    assert.equal(typeof ((await unknown.json()) as any).error, "string");
  });

  it("creates a planned teach session", async () => {
    const res = await post("/api/sessions", {
      kind: "teach",
      course: "demo",
      model: "haiku",
      ankiOptIn: false,
      target: { mode: "planned", title: "Prima lezione" },
    });
    assert.equal(res.status, 200);
    teachId = ((await res.json()) as { id: string }).id;
    assert.equal(typeof teachId, "string");
    await idle(teachId);

    const state = await get(teachId);
    assert.equal(state.title, "Prima lezione");
    assert.equal(state.items[0].kind, "learner");
    assert.equal((state.items[0] as any).text, 'Iniziamo la lezione "Prima lezione".');
    const teacher = state.items.find((i) => i.kind === "teacher") as any;
    assert.equal(teacher.text, "Ciao, sono il docente.");
    assert.equal(teacher.streaming, false);
    assert.equal(state.usage.turns, 1);
    assert.equal(state.retryable, false);
    assert.equal(state.pendingRequestId, null);
  });

  it("sends a snapshot first, then the live events of the next turn", async () => {
    const stream = sse(`${server.url}/api/sessions/${teachId}/events`);
    try {
      await waitFor(() => stream.events[0]?.type === "snapshot");
      const snapshot = stream.events[0] as any;
      assert.equal(snapshot.state.id, teachId);
      assert.ok(snapshot.state.items.some((i: any) => i.kind === "teacher" && i.text === "Ciao, sono il docente."));

      const before = stream.events.length;
      assert.equal((await post(`/api/sessions/${teachId}/message`, { text: "Ciao" })).status, 200);
      await waitFor(() => stream.events.slice(before).some((e) => e.type === "status" && (e as any).status === "idle"));

      const live = stream.events.slice(before).map((e) => e as any);
      assert.deepEqual(
        live.map((e) => (e.type === "item" ? `item:${e.item.kind}` : e.type)),
        ["item:learner", "status", "item:teacher", "delta", "delta", "delta", "item:teacher", "usage", "status"],
      );
      assert.equal(live[0].item.text, "Ciao");
      assert.equal(live[1].status, "running");
      assert.equal(live[2].item.text, "");
      assert.equal(live[2].item.streaming, true);
      assert.equal(live.slice(3, 6).map((e) => e.text).join(""), "Ciao, sono il docente.");
      assert.equal(live[6].item.text, "Ciao, sono il docente.");
      assert.equal(live[6].item.streaming, false);
      assert.equal(live[7].usage.turns, 2);
      assert.equal(live[8].status, "idle");
      assert.equal(live.filter((e) => e.type === "snapshot").length, 0, "no second snapshot");
    } finally {
      stream.close();
    }
  });

  it("refuses a second message while the teacher is busy", async () => {
    assert.equal((await post(`/api/sessions/${teachId}/message`, { text: "FAKE:slow:10" })).status, 200);
    const busy = await post(`/api/sessions/${teachId}/message`, { text: "ancora" });
    assert.equal(busy.status, 409);
    assert.equal(typeof ((await busy.json()) as any).error, "string");
    await idle(teachId);
  });

  it("an engine crash leaves a retryable error, and the next message resumes", async () => {
    const stream = sse(`${server.url}/api/sessions/${teachId}/events`);
    try {
      await waitFor(() => stream.events[0]?.type === "snapshot");
      const turnsBefore = (await get(teachId)).usage.turns;
      assert.equal((await post(`/api/sessions/${teachId}/message`, { text: "FAKE:crash" })).status, 200);
      const crashed = await waitFor(async () => {
        const s = await get(teachId);
        return s.status === "error" ? s : undefined;
      });
      assert.equal(crashed.retryable, true);
      assert.ok(crashed.lastError?.startsWith("teacher process exited (1)"), crashed.lastError);
      await waitFor(() =>
        stream.events.some((e) => e.type === "status" && (e as any).status === "error" && (e as any).retryable === true),
      );

      assert.equal((await post(`/api/sessions/${teachId}/message`, { text: "ancora" })).status, 200);
      await idle(teachId);
      assert.ok((await get(teachId)).usage.turns > turnsBefore);
    } finally {
      stream.close();
    }
  });

  it("lists summaries newest first, without internal state", async () => {
    const created = await post("/api/sessions", { kind: "practice", course: "demo", model: "haiku" });
    assert.equal(created.status, 200);
    const practiceId = ((await created.json()) as { id: string }).id;
    await idle(practiceId);

    const res = await fetch(server.url + "/api/sessions");
    assert.equal(res.status, 200);
    const list = (await res.json()) as any[];
    assert.deepEqual(list.map((s) => s.id), [practiceId, teachId]);
    for (const s of list)
      for (const key of ["items", "plan", "exercises", "pendingRequestId"]) assert.ok(!(key in s), `${s.id} leaks ${key}`);
  });

  it("persists state.json and loads it on the next server", async () => {
    const dir = path.join(root, ".learner", "sessions", teachId);
    const file = path.join(dir, "state.json");
    const before = await get(teachId);
    // The save is debounced (300 ms): wait for the file to reach the live session, since a killed
    // binary cannot flush on close like the Node server does.
    const saved = await waitFor(() => {
      try {
        const raw = JSON.parse(fs.readFileSync(file, "utf-8"));
        return raw.state?.status === "idle" && JSON.stringify(raw.state.items) === JSON.stringify(before.items) ? raw : undefined;
      } catch {
        return undefined; // not written yet
      }
    }, 3000);
    const keys = Object.keys(saved);
    assert.deepEqual(keys.slice(0, 3), ["state", "provider", "resume"]);
    assert.equal(keys.at(-1), "canRetry");
    assert.ok(fs.existsSync(path.join(dir, "mcp.json")));
    assert.ok(fs.existsSync(path.join(dir, "system.md")));

    await server.close();
    server = await start();
    const loaded = await get(teachId);
    assert.equal(loaded.status, "idle");
    assert.deepEqual(loaded.items, before.items);

    assert.equal((await post(`/api/sessions/${teachId}/message`, { text: "di nuovo" })).status, 200);
    await idle(teachId);
  });

  it("hands the engine the auth and data-root environment", async () => {
    const repo = tempDir("api-sessions-env-repo-");
    const envData = seedData();
    const envFile = path.join(repo, "env.json");
    const ambient = ["sk", "ambient"].join("-");
    const key = ["sk", "configured"].join("-");
    const prev = process.env.ANTHROPIC_API_KEY;
    process.env.ANTHROPIC_API_KEY = ambient;
    const srv = await startServer({ repoRoot: repo, dataRoot: envData, claude: fakeClaude(["--env-out", envFile]) });
    const postTo = (p: string, body: unknown = {}) =>
      fetch(srv.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
    const statusOf = (id: string) => fetch(`${srv.url}/api/sessions/${id}`).then((r) => r.json() as Promise<SessionState>);
    try {
      const first = await postTo("/api/sessions", {
        kind: "teach",
        course: "demo",
        model: "haiku",
        ankiOptIn: false,
        target: { mode: "planned", title: "Prima lezione" },
      });
      assert.equal(first.status, 200);
      const id = ((await first.json()) as { id: string }).id;
      await waitFor(async () => (await statusOf(id)).status === "idle");
      const env = JSON.parse(fs.readFileSync(envFile, "utf-8"));
      assert.equal(env.ANTHROPIC_API_KEY, null, "subscription strips the ambient key");
      assert.equal(env.MCP_TOOL_TIMEOUT, "86400000");
      assert.equal(env.LEARNER_DATA_ROOT, path.resolve(envData));

      const settings = await postTo("/api/settings", { claudeAuth: "apiKey", anthropicApiKey: `  ${key}  ` });
      assert.equal(settings.status, 200);
      const second = await postTo("/api/sessions", { kind: "practice", course: "demo", model: "haiku" });
      const id2 = ((await second.json()) as { id: string }).id;
      await waitFor(async () => (await statusOf(id2)).status === "idle");
      const env2 = JSON.parse(fs.readFileSync(envFile, "utf-8"));
      assert.equal(env2.ANTHROPIC_API_KEY, key, "the api key reaches the engine trimmed");
    } finally {
      await srv.close();
      if (prev === undefined) delete process.env.ANTHROPIC_API_KEY;
      else process.env.ANTHROPIC_API_KEY = prev;
      await removeTree(repo);
      await removeTree(envData);
    }
  });
});
