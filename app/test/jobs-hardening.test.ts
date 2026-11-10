// Review fixes: sanitizers, CSP-enforced demos, job deadline / map serialisation / pruning, slim payloads, settings,
// pending start, lost-result re-queue, global events, /files sandbox.
import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import { z } from "zod";
import type { ChatItem, GlobalEvent, SessionState } from "../shared/protocol.ts";
import { createServer, type RunningServer } from "../server/main.ts";
import { JobManager } from "../server/jobs/index.ts";
import { checkDemo, checkSvg } from "../server/jobs/sanitize.ts";
import { browserAvailable, closeBrowser, renderSvgPng, runDemoPage } from "../server/jobs/render.ts";
import { subscribeGlobal } from "../server/events.ts";
import { readSettings, writeSettings } from "../server/settings.ts";
import { effortFor } from "../server/engine.ts";
import { makeTempRepo } from "./helpers.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9";
const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");
const script = (steps: unknown[]) => "FAKE:job:" + Buffer.from(JSON.stringify(steps)).toString("base64");
const SUBMIT = { tool: "submit_research", input: { brief: "Vue 3.5 rende reattivo un ref leggendo .value dentro un effect: la dipendenza viene registrata alla lettura.", sources: [{ title: "Vue docs", url: "https://vuejs.org/" }] } };

async function waitFor<T>(fn: () => T | undefined | false, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}
const rm = (d: string) => fs.rmSync(d, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });

const hasBrowser = await browserAvailable();
await closeBrowser();
const skipBrowser = hasBrowser ? false : "no browser available";

describe("sanitizers: bypasses and false rejections", () => {
  it("svg: text content that looks like an attribute is fine", () => {
    assert.deepEqual(checkSvg('<svg viewBox="0 0 10 10"><text x="1" y="1">data = 5 e src = 3</text><rect fill="#fff" style="fill:url(#g)"/></svg>'), []);
  });
  const bad: [string, string][] = [
    ["tab-split javascript (entity)", '<svg><a href="java&#9;script:alert(1)"><text>x</text></a></svg>'],
    ["newline-split javascript (entity)", '<svg><a xlink:href="java&#10;script:alert(1)"><text>x</text></a></svg>'],
    ["background attr", '<svg><rect background="http://evil.example/x"/></svg>'],
    ["svg root then table background", '<svg/><table background="http://evil.example/x"><tr><td>x</td></tr></table>'],
    ["image element", '<svg><image href="data:image/png;base64,AAAA"/></svg>'],
    ["css escape url", '<svg><rect style="fill:u\\72l(http://evil.example/x)"/></svg>'],
    ["style element escape", '<svg><style>rect{fill:\\75rl(http://evil.example/x)}</style></svg>'],
    ["image-set", '<svg><rect style="fill:image-set(\'http://evil.example/x\' 1x)"/></svg>'],
    ["srcset", '<svg><rect srcset="http://evil.example/x"/></svg>'],
  ];
  for (const [n, svg] of bad) it(`svg rejects: ${n}`, () => assert.ok(checkSvg(svg).length > 0, svg));
  it("demo hint stays quiet on legit text and attributes", () => {
    assert.deepEqual(checkDemo('<!doctype html><p>data = "x" e src = 3</p><a href="#a">ok</a><script>const s = "https://x"</script>'), []);
  });
});

describe("headless rendering enforces the network policy", { skip: skipBrowser }, () => {
  it("demo: obfuscated fetch, sendBeacon, Image, WebSocket and CSS background all fail the run", async () => {
    try {
      for (const body of [
        "<script>fetch('ht'+'tps://example.invalid/a').catch(()=>{})</script>",
        "<script>navigator.sendBeacon('ht'+'tps://example.invalid/b','x')</script>",
        "<script>new Image().src='ht'+'tps://example.invalid/c'</script>",
        "<script>try{new WebSocket('ws://127.0.0.1:9/d')}catch(e){console.error(String(e))}</script>",
        "<style>body{background:image-set('http://example.invalid/e' 1x)}</style><p>x</p>",
      ]) {
        const r = await runDemoPage(`<!doctype html><body>${body}</body>`, 400);
        assert.ok(r.errors.length > 0, body);
      }
    } finally {
      await closeBrowser();
    }
  });

  it("svg render performs no network request", async () => {
    let hits = 0;
    const srv = http.createServer((_q, res) => (hits++, res.end("x")));
    await new Promise<void>((r) => srv.listen(0, "127.0.0.1", r));
    const port = (srv.address() as any).port;
    try {
      await renderSvgPng(`<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><image href="http://127.0.0.1:${port}/x.png" width="20" height="20"/></svg>`);
      assert.equal(hits, 0);
    } finally {
      srv.close();
      await closeBrowser();
    }
  });
});

describe("JobManager: deadline, map serialisation, pruning, pids", () => {
  let root: string;
  before(async () => (root = await makeTempRepo()));
  after(async () => {
    await new Promise((r) => setTimeout(r, 300));
    rm(root);
  });
  const mgr = (o: object = {}) => new JobManager({ repoRoot: root, getPort: () => 1, token: "t", bins: { claude: { cmd: process.execPath, args: [FAKE] }, agy: { cmd: "agy" }, opencode: { cmd: "opencode" } }, onSessionJob() {}, ...o });

  it("a hung job is settled with 'tempo scaduto' and frees its slot; the pid stays tracked until close", async () => {
    const m = mgr({ jobTimeoutMs: 400, maxConcurrent: 1 });
    try {
      const a = m.start("research", { question: "a " + script([{ sleep: 20000 }]) }, { title: "a" });
      const b = m.start("research", { question: "b " + script([{ sleep: 20000 }]) }, { title: "b" });
      await waitFor(() => m.livePids().length === 1);
      await waitFor(() => m.get(a.id)!.status === "error");
      assert.equal(m.get(a.id)!.error, "tempo scaduto");
      await waitFor(() => m.get(b.id)!.status === "error"); // b took the freed slot, then timed out too
      await waitFor(() => m.livePids().length === 0);
    } finally {
      await m.shutdown();
    }
  });

  it("map jobs run one at a time, beside the 2-slot pool", async () => {
    const m = mgr();
    try {
      m.register({
        kind: "map", grant: [], maxTurns: 3, systemPrompt: async () => "x",
        firstMessage: (i: any) => i.hint, tools: () => [{ name: "done", description: "d", schema: z.object({}), run: async () => ({ text: "ok" }) }],
      });
      const sleepy = (ms: number) => ({ hint: script([{ sleep: ms }]) });
      const m1 = m.start("map", sleepy(1500), { title: "m1" });
      const m2 = m.start("map", sleepy(100), { title: "m2" });
      const r1 = m.start("research", { question: "r " + script([{ sleep: 1500 }]) }, { title: "r1" });
      await waitFor(() => m.get(m1.id)!.activity !== "in coda" && m.get(r1.id)!.activity !== "in coda");
      assert.equal(m.get(m2.id)!.activity, "in coda", "second map waits for the first");
      m.cancel(m1.id);
      await waitFor(() => m.get(m2.id)!.activity !== "in coda");
    } finally {
      await m.shutdown();
    }
  });

  it("startup prunes settled records older than 30 days", () => {
    const dir = path.join(root, ".learner", "jobs");
    fs.mkdirSync(dir, { recursive: true });
    const old = { id: "old-1", kind: "research", title: "t", model: "haiku", status: "done", activity: "", createdAt: "2020-01-01T00:00:00Z", updatedAt: "2020-01-01T00:00:00Z", costUsd: 0 };
    fs.writeFileSync(path.join(dir, "old-1.json"), JSON.stringify(old));
    const fresh = { ...old, id: "new-1", updatedAt: new Date().toISOString() };
    fs.writeFileSync(path.join(dir, "new-1.json"), JSON.stringify(fresh));
    const m = mgr();
    assert.equal(m.get("old-1"), undefined);
    assert.ok(!fs.existsSync(path.join(dir, "old-1.json")));
    assert.ok(m.get("new-1"));
  });
});

describe("server: payloads, settings, files, events, pending start, lost results", () => {
  let srv: RunningServer;
  let root: string;
  const opts = () => ({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, claudeBin: process.execPath, claudeBinArgs: [FAKE], addDirs: [] });
  const post = (p: string, body: unknown = {}) =>
    fetch(srv.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const state = (id: string) => srv.manager.get(id) as SessionState;
  const sessionFile = (id: string) => path.join(root, ".learner", "sessions", id, "state.json");

  before(async () => {
    root = await makeTempRepo();
    fs.mkdirSync(path.join(root, "courses", "demo", "assets"), { recursive: true });
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-x.html"), "<!doctype html><p id=a>x</p>");
    fs.writeFileSync(path.join(root, "courses", "demo", "assets", "a.css"), "p{}");
    srv = await createServer(opts());
  });
  after(async () => {
    await srv.close();
    await new Promise((r) => setTimeout(r, 500));
    rm(root);
  });

  it("GET /api/jobs and the global job event carry no result; GET /api/jobs/:id has it", async () => {
    const seen: GlobalEvent[] = [];
    const off = subscribeGlobal((e) => seen.push(e));
    const j = srv.jobs.start("research", { question: "q " + script([SUBMIT]) }, { title: "Ricerca: q" });
    await waitFor(() => srv.jobs.get(j.id)!.status === "done");
    off();
    assert.ok(seen.some((e) => e.type === "job" && e.job.id === j.id && e.job.status === "done"));
    assert.ok(seen.every((e) => e.type !== "job" || e.job.result === undefined));
    const list = (await (await fetch(srv.url + "/api/jobs")).json()) as any[];
    assert.equal(list.find((x) => x.id === j.id).result, undefined);
    const full = (await (await fetch(srv.url + "/api/jobs/" + j.id)).json()) as any;
    assert.equal(full.result.kind, "research");
    assert.equal((await fetch(srv.url + "/api/jobs/nope")).status, 404);
  });

  it("settings: unknown/bad keys are stripped without resetting the rest; write is atomic; repoRoot is reported", async () => {
    const f = path.join(root, ".learner", "settings.json");
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, JSON.stringify({ teacherModel: "haiku", mapModel: "not-a-model", legacy: 1, autoMap: false }));
    const s = readSettings(root);
    assert.equal(s.teacherModel, "haiku");
    assert.equal(s.autoMap, false);
    assert.notEqual(s.mapModel as string, "not-a-model");
    writeSettings(root, { subagentModel: "haiku" });
    assert.deepEqual(fs.readdirSync(path.dirname(f)).filter((n) => n.endsWith(".tmp")), []);
    assert.equal(readSettings(root).teacherModel, "haiku");
    writeSettings(root, { effort: { teacher: "high" } });
    assert.equal(readSettings(root).effort.teacher, "high");
    assert.equal(readSettings(root).effort.subagent, "default");
    writeSettings(root, { effort: { subagent: "low" } });
    assert.equal(readSettings(root).effort.teacher, "high");
    assert.equal(readSettings(root).effort.subagent, "low");
    assert.equal(effortFor(readSettings(root), "claude", "teacher"), "high");
    assert.equal(effortFor(readSettings(root), "claude", "subagent"), "low");
    assert.equal(effortFor(readSettings(root), "claude", "map"), undefined);
    assert.equal(effortFor(readSettings(root), "agy", "teacher"), undefined);
    const r = (await (await fetch(srv.url + "/api/settings")).json()) as any;
    assert.equal(r.repoRoot, path.resolve(root));
    assert.equal(r.teacherModel, "haiku");
  });

  it("/files: html is sandboxed, static assets carry CORS", async () => {
    const h = await fetch(srv.url + "/files/courses/demo/lessons/0001-x.html");
    assert.match(h.headers.get("content-security-policy")!, /^sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox; frame-ancestors 'self'$/);
    const c = await fetch(srv.url + "/files/courses/demo/assets/a.css");
    assert.equal(c.headers.get("access-control-allow-origin"), "*");
  });

  it("events: sessions on create/status", async () => {
    const seen: string[] = [];
    const off = subscribeGlobal((e) => seen.push(e.type));
    const res = await post("/api/sessions", { kind: "teach", course: "demo", model: "haiku", ankiOptIn: false, target: { mode: "free" } });
    const { id } = (await res.json()) as { id: string };
    await waitFor(() => state(id).status === "idle");
    await waitFor(() => seen.includes("sessions"));
    off();
  });

  it("Anki card dismissed: next message runs the opening with his text, no 'Turno interrotto'; opt-in defaults to false", async () => {
    const { id } = (await (await post("/api/sessions", { kind: "teach", course: "demo", target: { mode: "free" } })).json()) as { id: string };
    assert.equal(state(id).status, "waiting");
    await post(`/api/sessions/${id}/interrupt`);
    await waitFor(() => state(id).status === "idle");
    assert.ok(!state(id).items.some((i) => i.kind === "notice" && /interrotto/i.test(i.text)));
    assert.equal((await post(`/api/sessions/${id}/message`, { text: "partiamo da zero" })).status, 200);
    await waitFor(() => state(id).status === "idle" && state(id).usage.turns === 1);
    const learner = state(id).items.filter((i): i is Extract<ChatItem, { kind: "learner" }> => i.kind === "learner");
    assert.equal(learner.length, 2, "opening + his message");
    assert.equal(learner[1].text, "partiamo da zero");
    assert.equal(state(id).config.ankiOptIn, false);
  });

  it("app restarted while the Anki card is open: the card is asked again and answering starts the lesson", async () => {
    const { id } = (await (await post("/api/sessions", { kind: "teach", course: "demo", target: { mode: "free" } })).json()) as { id: string };
    await srv.close();
    srv = await createServer(opts());
    const s = state(id);
    assert.equal(s.status, "waiting");
    const asks = s.items.filter((i): i is Extract<ChatItem, { kind: "ask" }> => i.kind === "ask");
    assert.equal(asks.length, 1);
    assert.ok(!asks[0].cancelled);
    assert.equal((await post(`/api/sessions/${id}/answer`, { requestId: asks[0].requestId, kind: "ask", answer: { selected: ["no"] } })).status, 200);
    await waitFor(() => state(id).status === "idle" && state(id).usage.turns === 1);
  });

  it("a finished-but-undelivered result found on load travels with the next learner message", async () => {
    const { id } = (await (await post("/api/sessions", { kind: "teach", course: "demo", model: "haiku", ankiOptIn: false, target: { mode: "free" } })).json()) as { id: string };
    await waitFor(() => state(id).status === "idle");
    await srv.close();
    const jobId = "lost-1";
    const rec = {
      id: jobId, kind: "research", title: "Ricerca: q", sessionId: id, model: "haiku", status: "done", activity: "",
      createdAt: new Date().toISOString(), updatedAt: new Date().toISOString(), costUsd: 0,
      result: { kind: "research", brief: "BRIEF-PERSO", sources: [] },
    };
    fs.writeFileSync(path.join(root, ".learner", "jobs", jobId + ".json"), JSON.stringify(rec));
    const raw = JSON.parse(fs.readFileSync(sessionFile(id), "utf-8"));
    raw.state.items.push({ id: "job:" + jobId, at: rec.createdAt, kind: "job", job: rec });
    fs.writeFileSync(sessionFile(id), JSON.stringify(raw));
    srv = await createServer(opts());
    assert.equal(srv.jobs.get(jobId)!.delivered, undefined);
    assert.equal((await post(`/api/sessions/${id}/message`, { text: "ciao" })).status, 200);
    await waitFor(() => srv.jobs.get(jobId)!.delivered);
    await waitFor(() => state(id).status === "idle");
  });
});
