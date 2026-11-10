// JobManager + roles: real createServer, HTTP /mcp/job/:id, fake-claude scripted through the job's first message.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import type { JobRecord } from "../shared/protocol.ts";
import { createServer, type RunningServer } from "../server/main.ts";
import { JobManager } from "../server/jobs/index.ts";
import { checkDemo, checkSvg } from "../server/jobs/sanitize.ts";
import { browserAvailable, closeBrowser } from "../server/jobs/render.ts";
import { demoRole } from "../server/jobs/roles/demo.ts";
import { visualRole } from "../server/jobs/roles/visual.ts";
import { researchRole } from "../server/jobs/roles/research.ts";
import { toolsFor } from "../server/tools/index.ts";
import type { JobResult } from "../shared/protocol.ts";
import type { JobContext } from "../server/types.ts";
import { fakeContext, makeTempRepo } from "./helpers.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9";
const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");
export const script = (steps: unknown[]) => "FAKE:job:" + Buffer.from(JSON.stringify(steps)).toString("base64");
const BRIEF = "Vue 3.5 rende reattivo un ref leggendo .value dentro un effect: la dipendenza viene registrata al momento della lettura. Fonte primaria: la documentazione ufficiale.";
const SUBMIT = { tool: "submit_research", input: { brief: BRIEF, sources: [{ title: "Vue docs", url: "https://vuejs.org/guide/essentials/reactivity-fundamentals.html" }] } };

async function waitFor<T>(fn: () => T | undefined | false, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

const hasBrowser = await browserAvailable();
await closeBrowser();
const skipBrowser = hasBrowser ? false : "no browser available";

describe("JobManager with fake-claude", () => {
  let srv: RunningServer;
  let root: string;
  const jobs = () => srv.jobs;
  const ended = (id: string) => waitFor(() => (["done", "error", "cancelled"].includes(jobs().get(id)!.status) ? jobs().get(id)! : false));
  const research = (question: string, title = "Ricerca: t") => jobs().start("research", { question }, { title });

  before(async () => {
    root = await makeTempRepo();
    srv = await createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, claudeBin: process.execPath, claudeBinArgs: [FAKE], addDirs: [] });
  });
  after(async () => {
    await srv.close();
    await new Promise((r) => setTimeout(r, 500)); // let killed processes release their cwd (Windows)
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  });

  it("research job: tool call finishes it, cost + result recorded, record persisted, listed over HTTP", async () => {
    const j = research("cos'è un ref? " + script([SUBMIT]));
    assert.equal(j.status, "running");
    const done = await ended(j.id);
    assert.equal(done.status, "done");
    assert.equal(done.result?.kind, "research");
    assert.match((done.result as any).brief, /Vue 3\.5/);
    assert.equal((done.result as any).sources[0].title, "Vue docs");
    assert.ok(done.costUsd > 0);
    assert.equal(done.activity, "");
    const saved = JSON.parse(fs.readFileSync(path.join(root, ".learner", "jobs", `${j.id}.json`), "utf-8")) as JobRecord;
    assert.equal(saved.status, "done");
    assert.ok(fs.existsSync(path.join(root, ".learner", "jobs", j.id, "system.md")));
    const mcp = JSON.parse(fs.readFileSync(path.join(root, ".learner", "jobs", j.id, "mcp.json"), "utf-8"));
    assert.equal(mcp.mcpServers.job.url, `http://127.0.0.1:${srv.port}/mcp/job/${j.id}`);
    assert.ok(mcp.mcpServers.job.headers["x-learner-token"]);
    const list = (await (await fetch(srv.url + "/api/jobs")).json()) as JobRecord[];
    assert.ok(list.some((x) => x.id === j.id && x.status === "done"));
  });

  it("system prompt, max-turns and tool allow-list reach the process", async () => {
    // fake-claude logs argv to stderr, which the manager does not keep: check the written files + role shape instead
    for (const [role, turns, grant] of [[researchRole, 20, ["web"]], [visualRole, 14, []], [demoRole, 24, []]] as const) {
      assert.equal(role.maxTurns, turns);
      assert.deepEqual(role.grant, [...grant]);
      assert.ok((await role.systemPrompt({} as any, root)).length > 200);
    }
  });

  it("result without submit becomes an error; failed tool input is reported to the model and does not finish", async () => {
    const a = await ended(research("niente " + "FAKE:hello").id);
    assert.equal(a.status, "error");
    assert.match(a.error!, /terminato senza consegnare/);
    const b = await ended(research("input sbagliato " + script([{ tool: "submit_research", input: { brief: "corto", sources: [] } }])).id);
    assert.equal(b.status, "error");
    assert.equal(b.result, undefined);
  });

  it("cancel stops a running job; no leftover pid", async () => {
    const j = research("lento FAKE:slow");
    await waitFor(() => jobs().livePids().length > 0);
    jobs().cancel(j.id);
    assert.equal(jobs().get(j.id)!.status, "cancelled");
    await waitFor(() => jobs().livePids().length === 0);
    const r = await fetch(srv.url + `/api/jobs/${j.id}/cancel`, { method: "POST", headers: { "x-learner-client": "1" } });
    assert.equal(r.status, 200);
    assert.equal((await fetch(srv.url + "/api/jobs/nope/cancel", { method: "POST", headers: { "x-learner-client": "1" } })).status, 404);
  });

  it("at most 2 run at once; the third waits 'in coda' and runs afterwards", async () => {
    const steps = script([{ sleep: 700 }, SUBMIT]);
    const [a, b, c] = [research("a " + steps), research("b " + steps), research("c " + steps)];
    assert.equal(jobs().get(c.id)!.activity, "in coda");
    await waitFor(() => jobs().livePids().length === 2);
    assert.equal(jobs().get(c.id)!.activity, "in coda");
    for (const j of [a, b, c]) assert.equal((await ended(j.id)).status, "done");
  });

  it("/mcp/job/:id: token required, unknown job 404, finished job 409; tools/list serves the role's tools", async () => {
    const j = research("x FAKE:slow");
    await waitFor(() => jobs().livePids().length > 0);
    const call = (id: string, headers: Record<string, string>) =>
      fetch(`${srv.url}/mcp/job/${id}`, { method: "POST", headers: { "content-type": "application/json", ...headers }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }) });
    assert.equal((await call(j.id, {})).status, 401);
    assert.equal((await call("nope", { "x-learner-token": srv.token })).status, 404);
    const ok = await call(j.id, { "x-learner-token": srv.token });
    assert.deepEqual(((await ok.json()) as any).result.tools.map((t: any) => t.name), ["submit_research"]);
    jobs().cancel(j.id);
    assert.equal((await call(j.id, { "x-learner-token": srv.token })).status, 409);
  });

  it("unknown kind throws; records left running by a previous run become errors on startup", () => {
    assert.throws(() => jobs().start("nope" as any, {}, { title: "x" }), /sconosciuto/);
    const dir = path.join(root, ".learner", "jobs");
    const rec: JobRecord = { id: "stale-1", kind: "demo", title: "Demo: x", model: "haiku", status: "running", activity: "prova la demo", createdAt: "2026-01-01T00:00:00Z", updatedAt: "2026-01-01T00:00:00Z", costUsd: 0 };
    fs.writeFileSync(path.join(dir, "stale-1.json"), JSON.stringify(rec));
    const m = new JobManager({ repoRoot: root, getPort: () => 1, token: "t", bins: { claude: { cmd: "claude" }, agy: { cmd: "agy" }, opencode: { cmd: "opencode" } }, onSessionJob() {} });
    const got = m.get("stale-1")!;
    assert.equal(got.status, "error");
    assert.equal(got.error, "interrotto alla chiusura dell'app");
    assert.equal(JSON.parse(fs.readFileSync(path.join(dir, "stale-1.json"), "utf-8")).status, "error");
  });

  it("activity follows tool uses and the job's own setActivity", async () => {
    const seen: string[] = [];
    const un = (await import("../server/events.ts")).subscribeGlobal((e) => e.type === "job" && seen.push(e.job.activity));
    await ended(research("att " + script([{ tool: "submit_research", input: SUBMIT.input }])).id);
    un();
    assert.ok(seen.includes("submit_research"), JSON.stringify(seen));
    assert.ok(seen.includes("consegna la ricerca"), JSON.stringify(seen));
  });
});

describe("teacher tools", () => {
  it("toolsFor wiring: teach = research+visual+demo; plan = research; practice = research+visual", () => {
    const names = (kind: string) => toolsFor({ kind, course: "demo", ankiOptIn: false } as any, "/nonexistent").map((t) => t.name);
    const teach = names("teach");
    for (const n of ["research", "visual", "demo"]) assert.ok(teach.includes(n as any), n);
    assert.ok(names("plan").includes("research" as any) && !names("plan").includes("visual" as any));
    assert.ok(names("practice").includes("visual" as any) && !names("practice").includes("demo" as any));
  });

  it("research / visual / demo tools start a job stamped with a title and return an ack with the id", async () => {
    const root = await makeTempRepo();
    try {
      const ctx = fakeContext(root, { kind: "teach", course: "demo", ankiOptIn: false } as any);
      const run = (name: string, input: unknown) => toolsFor(ctx.config, root).find((t) => t.name === name)!.run(input as any, ctx);
      const r = await run("research", { question: "Quale versione di Vue?", context: "lezione 1" });
      assert.match(r.text, /^Avviato in background \(job job-1\)/);
      await run("visual", { brief: "Un albero con tre nodi" });
      await run("demo", { title: "Slider", brief: "Uno slider che cambia un numero" });
      assert.deepEqual(ctx.jobsStarted.map((j) => j.kind), ["research", "visual", "demo"]);
      assert.equal(ctx.jobsStarted[0].job.title, "Ricerca: Quale versione di Vue?");
      assert.equal(ctx.jobsStarted[2].job.title, "Demo: Slider");
      assert.equal((await run("visual", { brief: "" })).isError ?? false, false); // schema is enforced by MCP, not by run()
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("sanitizers (trust boundary)", () => {
  const ok = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><defs><marker id="a"><path d="M0 0"/></marker></defs><path marker-end="url(#a)" d="M0 0L5 5" stroke="#eee"/><text x="1" y="1">x &#8594; y</text></svg>';
  it("accepts a plain svg (xml prolog, local url/href ok)", () => {
    assert.deepEqual(checkSvg(ok), []);
    assert.deepEqual(checkSvg('<?xml version="1.0"?>\n' + ok), []);
    assert.deepEqual(checkSvg('<svg><use href="#a"/><rect fill="url(#g)"/></svg>'), []);
  });
  const bad: [string, string][] = [
    ["script", '<svg><script>alert(1)</script></svg>'],
    ["SCRIPT upper", '<svg><SCRIPT >x</SCRIPT></svg>'],
    ["foreignObject", '<svg><foreignObject><div/></foreignObject></svg>'],
    ["onload", '<svg onload="x()"></svg>'],
    ["onclick after quote", '<svg><rect fill="red"onclick=x()/></svg>'],
    ["javascript href", '<svg><a href="javascript:alert(1)"><text>x</text></a></svg>'],
    ["entity-obfuscated javascript", '<svg><a href="&#106;avascript:alert(1)"><text>x</text></a></svg>'],
    ["whitespace javascript", '<svg><a href="java\tscript:alert(1)"><text>x</text></a></svg>'],
    ["external href", '<svg><image href="https://evil.example/x.png"/></svg>'],
    ["xlink external", '<svg><image xlink:href="http://evil.example/x.png"/></svg>'],
    ["protocol-relative", '<svg><image href="//evil.example/x.png"/></svg>'],
    ["external url()", '<svg><rect style="fill:url(https://evil.example/x)"/></svg>'],
    ["@import", '<svg><style>@import "x.css";</style></svg>'],
    ["not svg root", '<div><svg></svg></div>'],
    ["html root", '<html><svg></svg></html>'],
    ["trailing junk", '<svg></svg><script>1</script>'],
    ["doctype", '<!DOCTYPE svg [<!ENTITY x "y">]><svg>&x;</svg>'],
    ["iframe", '<svg><iframe src="x"/></svg>'],
    ["animate href", '<svg><a><animate attributeName="href" values="javascript:alert(1)"/></a></svg>'],
    ["empty", ""],
  ];
  for (const [name, svg] of bad) it(`svg rejects: ${name}`, () => assert.ok(checkSvg(svg).length > 0, svg));

  it("demo html: accepts self-contained, rejects external network resources", () => {
    const page = '<!doctype html><html><body><input type="range" id="r"><output id="o"></output><script>document.getElementById("r").oninput=e=>{o.textContent=e.target.value}; const u = "see https://x"</script><style>body{background:url(data:image/png;base64,AAAA)}</style></body></html>';
    assert.deepEqual(checkDemo(page), []);
    for (const html of [
      '<script src="https://cdn.example/x.js"></script>',
      '<script src=//cdn.example/x.js></script>',
      '<link rel="stylesheet" href="http://x/y.css">',
      '<img src="https://x/y.png">',
      '<style>@import url("https://x/y.css");</style>',
      '<div style="background:url(https://x/y.png)"></div>',
      '<script>fetch("https://api.example/x")</script>',
      "<script>fetch(`//api.example/x`)</script>",
      '<script>const x=new XMLHttpRequest();x.open("GET","https://x/y")</script>',
      '<script>new WebSocket("wss://x/y")</script>',
      '<script>img.src="http://x/y.png"</script>',
      '<iframe src="about:blank"></iframe>',
      '<meta http-equiv="refresh" content="0;url=http://x">',
      "",
    ]) assert.ok(checkDemo(html).length > 0, html);
  });
});

function jobCtx(): JobContext & { finished?: JobResult; activity: string[] } {
  const c: any = { repoRoot: "/r", jobId: "t1", activity: [], setActivity: (t: string) => c.activity.push(t), finish: (r: JobResult) => (c.finished = r) };
  return c;
}
const tool = (role: { tools(i: any): any[] }, name: string, input: any = {}) => role.tools(input).find((t) => t.name === name)!;

describe("research / visual / demo job tools", () => {
  it("submit_research finishes with brief + sources; rejects non-http sources", async () => {
    const c = jobCtx();
    const t = tool(researchRole, "submit_research");
    const bad = await t.run({ brief: BRIEF, sources: [{ title: "x", url: "ftp://x/y" }] }, c);
    assert.equal(bad.isError, true);
    assert.equal(c.finished, undefined);
    await t.run(SUBMIT.input, c);
    assert.equal((c.finished as JobResult | undefined)?.kind, "research");
    assert.equal(t.schema.safeParse({ brief: BRIEF, sources: [] }).success, false, "at least one source");
  });

  it("write_svg rejects hostile svg with a clear error and keeps no draft; submit_visual needs a rendered draft", async () => {
    const c = jobCtx();
    const w = await tool(visualRole, "write_svg").run({ svg: '<svg onload="x()"></svg>' }, c);
    assert.equal(w.isError, true);
    assert.match(w.text, /rifiutato/);
    assert.equal((await tool(visualRole, "submit_visual").run({}, c)).isError, true);
    await tool(visualRole, "write_svg").run({ svg: '<svg xmlns="http://www.w3.org/2000/svg"><circle r="4"/></svg>' }, c);
    const s = await tool(visualRole, "submit_visual").run({ caption: "c" }, c);
    assert.equal(s.isError, true, "draft not rendered yet");
    assert.match(s.text, /render_svg/);
    assert.equal(c.finished, undefined);
  });

  it("write_demo rejects remote resources; submit_demo refuses without a passing run_demo", async () => {
    const c = jobCtx();
    const w = await tool(demoRole, "write_demo").run({ html: '<script src="https://cdn.example/x.js"></script>' }, c);
    assert.equal(w.isError, true);
    assert.match(w.text, /rete esterna/);
    await tool(demoRole, "write_demo").run({ html: "<p>ciao</p>" }, c);
    const s = await tool(demoRole, "submit_demo", { title: "T" }).run({}, c);
    assert.equal(s.isError, true);
    assert.match(s.text, /run_demo/);
    assert.equal(c.finished, undefined);
  });

  it("visual: write -> render (image) -> submit delivers the svg", { skip: skipBrowser }, async () => {
    try {
      const c = jobCtx();
      const svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 60" width="240" height="120"><rect x="5" y="5" width="110" height="50" fill="none" stroke="#e6e6e6"/><text x="20" y="35" fill="#e6e6e6">ref</text></svg>';
      await tool(visualRole, "write_svg").run({ svg }, c);
      const r = await tool(visualRole, "render_svg").run({}, c);
      assert.equal(r.isError, undefined);
      assert.equal(r.images?.[0].mimeType, "image/png");
      assert.ok(Buffer.from(r.images![0].data, "base64").subarray(1, 4).toString() === "PNG");
      await tool(visualRole, "submit_visual").run({ caption: "Un riquadro." }, c);
      assert.deepEqual(c.finished, { kind: "visual", svg, caption: "Un riquadro." });
      // a new write_svg invalidates the render
      await tool(visualRole, "write_svg").run({ svg }, c);
      assert.equal((await tool(visualRole, "submit_visual").run({}, c)).isError, true);
    } finally {
      await closeBrowser();
    }
  });

  it("demo: errors fail run_demo and block submit; a clean run passes and delivers; network is blocked", { skip: skipBrowser }, async () => {
    try {
      const c = jobCtx();
      const t = (n: string, i: any = {}) => tool(demoRole, n, { title: "Slider" }).run(i, c);
      await t("write_demo", { html: "<p id=a>x</p><script>throw new Error('boom')</script>" });
      const f = await t("run_demo");
      assert.equal(f.isError, true);
      assert.match(f.text, /boom/);
      assert.equal(f.images?.length, 1);
      assert.equal((await t("submit_demo")).isError, true);
      await t("write_demo", { html: "<p id=a>x</p><script>console.error('kaputt')</script>" });
      assert.match((await t("run_demo")).text, /kaputt/);
      await t("write_demo", { html: "<!doctype html><body><input type=range id=r><output id=o>0</output><script>r.oninput=()=>o.textContent=r.value</script></body>" });
      const p = await t("run_demo");
      assert.equal(p.isError, undefined);
      assert.match(p.text, /PASSATO/);
      await t("submit_demo", { notes: "Muovi lo slider." });
      assert.equal((c.finished as JobResult | undefined)?.kind, "demo");
      assert.equal((c.finished as any).title, "Slider");
      assert.equal((c.finished as any).notes, "Muovi lo slider.");
      // an in-page request to a remote host is blocked and reported (defence in depth behind write_demo's check)
      await t("write_demo", { html: "<script>const u=['https:','//example.invalid/x'].join(''); fetch(u).catch(()=>{})</script>" });
      const net = await t("run_demo");
      assert.equal(net.isError, true);
      assert.match(net.text, /bloccata|Content Security Policy/);
    } finally {
      await closeBrowser();
    }
  });

  it("full visual job through MCP (fake-claude drives write_svg, render_svg, submit_visual)", { skip: skipBrowser }, async () => {
    const root = await makeTempRepo();
    const srv = await createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, claudeBin: process.execPath, claudeBinArgs: [FAKE], addDirs: [] });
    try {
      const svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 40"><circle cx="20" cy="20" r="10" stroke="#eee" fill="none"/></svg>';
      const j = srv.jobs.start("visual", { brief: "un cerchio " + script([{ tool: "submit_visual", input: {} }, { tool: "write_svg", input: { svg } }, { tool: "render_svg" }, { tool: "submit_visual", input: { caption: "Cerchio." } }]) }, { title: "Figura: cerchio" });
      const done = await waitFor(() => (srv.jobs.get(j.id)!.status !== "running" ? srv.jobs.get(j.id)! : false), 30000);
      assert.equal(done.status, "done", done.error);
      assert.deepEqual(done.result, { kind: "visual", svg, caption: "Cerchio." });
    } finally {
      await srv.close();
      fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
    }
  });
});
