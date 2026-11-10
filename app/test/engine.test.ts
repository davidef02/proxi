// Engine contract: the same cases through both adapters, each driving its fake (fake-claude, fake-agy).
import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import type { SessionStatus } from "../shared/protocol.ts";
import { spawnEngine } from "../server/engine.ts";
import { JobManager } from "../server/jobs/index.ts";
import { Turns } from "../server/turns.ts";
import type { EngineEvent, EngineHandle, EngineLaunch, Provider } from "../server/types.ts";
import { makeTempRepo, memoryJobs } from "./helpers.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const BINS: Record<Provider, { cmd: string; args: string[] }> = {
  claude: { cmd: process.execPath, args: [path.join(here, "fake-claude.mjs")] },
  agy: { cmd: process.execPath, args: [path.join(here, "fake-agy.mjs")] },
  opencode: { cmd: process.execPath, args: [path.join(here, "fake-opencode.mjs")] },
};
const RESUME_FAIL_ENV = { claude: "FAKE_CLAUDE_RESUME_FAIL", agy: "FAKE_AGY_RESUME_FAIL", opencode: "FAKE_OPENCODE_RESUME_FAIL" } as const;
const b64 = (o: unknown) => Buffer.from(JSON.stringify(o)).toString("base64");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "engine-contract-"));
after(() => fs.rmSync(tmp, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }));

/** Minimal MCP endpoint: one tool "echo"; records the token each call presented. */
let mcpUrl = "";
const tokens: (string | undefined)[] = [];
const server = http.createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    const m = JSON.parse(body);
    tokens.push(req.headers["x-learner-token"] as string | undefined);
    if (m.id === undefined) return void res.writeHead(202).end();
    res.setHeader("content-type", "application/json");
    const result =
      m.method === "initialize" ? { protocolVersion: m.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "app", version: "1" } }
      : m.method === "tools/list" ? { tools: [{ name: "echo", inputSchema: { type: "object" }, _meta: { "antigravity/alwaysLoad": true } }] }
      : { content: [{ type: "text", text: "eco " + JSON.stringify(m.params.arguments) }] };
    res.end(JSON.stringify({ jsonrpc: "2.0", id: m.id, result }));
  });
});
before(async () => {
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  mcpUrl = `http://127.0.0.1:${(server.address() as any).port}/mcp/x`;
});
after(() => server.close());

function launch(provider: Provider, resume?: string): EngineLaunch {
  return {
    provider, bin: BINS[provider], repoRoot: tmp, scratchDir: fs.mkdtempSync(path.join(tmp, provider + "-")), model: "m",
    systemPrompt: "Sei il docente.", grant: ["read"], mcp: { name: "app", url: mcpUrl, token: "tok" }, addDirs: [], resume,
  };
}

function start(l: EngineLaunch) {
  const events: EngineEvent[] = [];
  let exits = 0;
  const waiters: (() => void)[] = [];
  const poke = () => waiters.splice(0).forEach((w) => w());
  const handle: EngineHandle = spawnEngine(l, {
    onEvent: (e) => (events.push(e), poke()),
    onStderr: () => {},
    onExit: () => (exits++, poke()),
  });
  const until = (pred: () => boolean, ms = 10000) =>
    new Promise<void>((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("timeout; events=" + JSON.stringify(events.map((e) => e.type)))), ms);
      const check = () => (pred() ? (clearTimeout(t), resolve()) : waiters.push(check));
      check();
    });
  const count = (type: EngineEvent["type"]) => events.filter((e) => e.type === type).length;
  const token = () => (events.filter((e) => e.type === "init").at(-1) as any)?.sessionId as string | undefined;
  return { handle, events, until, count, token, exits: () => exits };
}

const alive = (pid: number) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};

for (const provider of ["claude", "agy", "opencode"] as const) {
  describe(`engine contract: ${provider}`, () => {
    let token = "";

    it("a turn: init issues a resume token, text streams, one successful result", async () => {
      const s = start(launch(provider));
      s.handle.send("ciao");
      await s.until(() => s.count("result") === 1);
      token = s.token()!;
      assert.ok(token, "init carries the token");
      assert.ok(s.count("text-delta") > 0 && s.events.some((e) => e.type === "assistant" && e.texts.length));
      assert.equal((s.events.find((e) => e.type === "result") as any).isError, false);
      await s.handle.stop();
    });

    it("an MCP tool call reaches the endpoint with the token and comes back as mcp__app__<tool> + its result", async () => {
      const s = start(launch(provider));
      const before = tokens.length;
      s.handle.send("FAKE:tool:echo:" + b64({ a: 1 }));
      await s.until(() => s.count("result") === 1);
      const use = s.events.flatMap((e) => (e.type === "assistant" ? e.toolUses : []))[0];
      assert.equal(use.name, "mcp__app__echo");
      const tr = s.events.find((e) => e.type === "tool-result") as any;
      assert.equal(tr.toolUseId, use.id);
      assert.match(tr.text, /eco \{"a":1\}/);
      assert.ok(tokens.length > before && tokens.slice(before).every((t) => t === "tok"));
      await s.handle.stop();
    });

    it("interrupt ends the turn with an error result; the handle answers the next turn on the same conversation", async () => {
      const s = start(launch(provider));
      s.handle.send("FAKE:slow");
      await s.until(() => s.count("text-delta") > 0);
      const first = s.token();
      s.handle.interrupt();
      await s.until(() => s.count("result") === 1);
      assert.equal((s.events.find((e) => e.type === "result") as any).isError, true);
      s.handle.send("ancora");
      await s.until(() => s.count("result") === 2);
      assert.equal((s.events.filter((e) => e.type === "result")[1] as any).isError, false);
      assert.equal(s.token(), first, "same conversation after the interrupt");
      assert.equal(s.exits(), 0);
      await s.handle.stop();
    });

    it("a new handle given the token resumes that conversation", async () => {
      const s = start(launch(provider, token));
      s.handle.send("ci sei?");
      await s.until(() => s.count("result") === 1);
      assert.equal(s.token(), token);
      await s.handle.stop();
    });

    it("a refused token: resume-failed, then onExit once", async () => {
      process.env[RESUME_FAIL_ENV[provider]] = "1";
      try {
        const s = start(launch(provider, "sconosciuto"));
        s.handle.send("x");
        await s.until(() => s.exits() === 1);
        assert.deepEqual(s.events.map((e) => e.type), ["resume-failed"]);
      } finally {
        delete process.env[RESUME_FAIL_ENV[provider]];
      }
    });

    it("stop(): onExit exactly once and the process is gone, also after an interrupt", async () => {
      for (const interrupted of [false, true]) {
        const s = start(launch(provider));
        s.handle.send(interrupted ? "FAKE:slow" : "ciao");
        await s.until(() => (interrupted ? s.count("text-delta") > 0 : s.count("result") === 1));
        const pid = s.handle.pid!;
        if (interrupted) s.handle.interrupt();
        await s.handle.stop();
        await s.until(() => s.exits() === 1);
        await new Promise((r) => setTimeout(r, 100));
        assert.equal(s.exits(), 1);
        assert.equal(alive(pid), false);
      }
    });

    it("the turn module relaunches fresh after a refused resume and the re-sent turn is answered", async () => {
      process.env[RESUME_FAIL_ENV[provider]] = "1";
      const h = { status: "idle" as SessionStatus, notices: [] as string[] };
      const turns = new Turns(
        {
          spawn: async (resume, cb) => spawnEngine(launch(provider, resume), cb),
          event: () => {},
          status: () => h.status,
          setStatus: (st) => void (h.status = st),
          notice: (_l, t) => void h.notices.push(t),
          learner: () => {},
          pending: () => 0,
          cancelPending: () => {},
          jobs: memoryJobs(),
        },
        "sconosciuto",
      );
      try {
        await turns.send("dopo il crash", ["dopo il crash"]);
        const end = Date.now() + 10000;
        while (h.status !== "idle" && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
        assert.equal(h.status, "idle");
        assert.match(h.notices.join(" "), /Ripresa della conversazione non riuscita/);
        assert.ok(turns.resume && turns.resume !== "sconosciuto", "a fresh token was issued");
      } finally {
        delete process.env[RESUME_FAIL_ENV[provider]];
        await turns.stop();
      }
    });
  });
}

describe("JobManager on agy: no pid outlives its job", () => {
  it("a cancelled agy job leaves no tracked pid (the exit is reported after stop)", async () => {
    const root = await makeTempRepo();
    fs.mkdirSync(path.join(root, ".learner"), { recursive: true });
    fs.writeFileSync(path.join(root, ".learner", "settings.json"), JSON.stringify({ provider: "agy" }));
    const m = new JobManager({ repoRoot: root, getPort: () => 1, token: "t", bins: BINS, onSessionJob() {} });
    try {
      const j = m.start("research", { question: "q FAKE:slow" }, { title: "r" });
      const end = Date.now() + 10000;
      while (m.livePids().length === 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
      const [pid] = m.livePids();
      assert.ok(pid);
      m.cancel(j.id);
      while (m.livePids().length > 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
      assert.deepEqual(m.livePids(), []);
      assert.equal(alive(pid), false);
    } finally {
      await m.shutdown();
      fs.rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    }
  });
});

describe("JobManager on opencode: no pid outlives its job", () => {
  it("a cancelled opencode job leaves no tracked pid (the exit is reported after stop)", async () => {
    const root = await makeTempRepo();
    fs.mkdirSync(path.join(root, ".learner"), { recursive: true });
    fs.writeFileSync(path.join(root, ".learner", "settings.json"), JSON.stringify({ provider: "opencode" }));
    const m = new JobManager({ repoRoot: root, getPort: () => 1, token: "t", bins: BINS, onSessionJob() {} });
    try {
      const j = m.start("research", { question: "q FAKE:slow" }, { title: "r" });
      const end = Date.now() + 10000;
      while (m.livePids().length === 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
      const [pid] = m.livePids();
      assert.ok(pid);
      m.cancel(j.id);
      while (m.livePids().length > 0 && Date.now() < end) await new Promise((r) => setTimeout(r, 25));
      assert.deepEqual(m.livePids(), []);
      assert.equal(alive(pid), false);
    } finally {
      await m.shutdown();
      fs.rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    }
  });
});
