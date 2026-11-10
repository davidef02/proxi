// End to end on the server side: real createServer + HTTP API + SSE client + fake-claude, on a temp repo copy.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import type { ChatItem, ServerEvent, SessionState } from "../shared/protocol.ts";
import { createServer, type RunningServer } from "../server/main.ts";
import { makeTempRepo } from "./helpers.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // closed port: never reach the real Anki
const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");
const b64 = (o: unknown) => Buffer.from(JSON.stringify(o)).toString("base64");
const DAG = 'graph TD\n  N1["Nodo 1: la prima cosa è vera"] --> N2["Nodo 2: la seconda ne segue"]';
const PLAN = { topic: "Demo topic", approach: "Prosa.", dag: DAG, goal: "capire la prima cosa" };
const NODE = { planNode: "N1", node: { nodeId: "prima-cosa", title: "La prima cosa", tier: "unconditional-truth", content: "<p>vero</p>" } };
const QUIZ = {
  kind: "check", nodeId: "N1", question: "Q?", options: [{ label: "A" }, { label: "B" }], explanation: "perché", correctAnswer: "B",
};

async function waitFor<T>(fn: () => T | undefined | false, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
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

describe("integration: server + SSE + fake-claude", () => {
  let srv: RunningServer;
  let root: string;
  const api = (p: string, body?: unknown) =>
    fetch(srv.url + p, body === undefined ? undefined : { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const state = (id: string) => srv.manager.get(id) as SessionState;
  const items = <K extends ChatItem["kind"]>(id: string, kind: K) => state(id).items.filter((i): i is Extract<ChatItem, { kind: K }> => i.kind === kind);
  const create = async (title: string) => {
    const r = await api("/api/sessions", { kind: "teach", course: "demo", model: "haiku", target: { mode: "planned", title }, ankiOptIn: false });
    assert.equal(r.status, 200);
    return ((await r.json()) as { id: string }).id;
  };
  const say = async (id: string, text: string) => assert.equal((await api(`/api/sessions/${id}/message`, { text })).status, 200);
  const idle = (id: string) => waitFor(() => state(id).status === "idle");

  before(async () => {
    root = await makeTempRepo();
    srv = await createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, claudeBin: process.execPath, claudeBinArgs: [FAKE], addDirs: [] });
  });
  after(async () => {
    await srv.close();
    await new Promise((r) => setTimeout(r, 500)); // fire-and-forget board refreshes
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
  });

  it("full teach flow: plan approval, node, quiz, map, files", async () => {
    const id = await create("Il primo passo");
    const stream = sse(`${srv.url}/api/sessions/${id}/events`);
    try {
      const ev = stream.events;
      await waitFor(() => ev[0]?.type === "snapshot");
      await idle(id);
      assert.equal(state(id).items[0].kind, "learner");
      assert.match((state(id).items[0] as any).text, /Il primo passo/);
      assert.ok(items(id, "teacher").some((t) => t.text.includes("docente")));

      // propose_plan: pending plan card over SSE, then approve
      await say(id, "FAKE:tool:propose_plan:" + b64(PLAN));
      const planItem = await waitFor(() => ev.find((e) => e.type === "item" && e.item.kind === "plan") as any);
      // the first turn may finish before the SSE subscription lands (it then arrives in the
      // snapshot); this turn starts after the snapshot, so its text must stream as deltas
      assert.ok(ev.some((e) => e.type === "delta"));
      const requestId = planItem.item.requestId as string;
      await waitFor(() => ev.some((e) => e.type === "pending" && e.requestId === requestId));
      assert.equal(state(id).status, "waiting");
      assert.equal((await api(`/api/sessions/${id}/answer`, { requestId, kind: "plan", answer: { approved: true } })).status, 200);
      await idle(id);
      const lessonsDir = path.join(root, "courses", "demo", "lessons");
      const file = fs.readdirSync(lessonsDir).find((f) => f.endsWith("-il-primo-passo.html"));
      assert.ok(file, "artifact named after the planned title");
      const lessonPath = `courses/demo/lessons/${file}`;
      assert.equal(state(id).lessonPath, lessonPath);
      assert.ok(ev.some((e) => e.type === "plan" && e.plan.approved));
      assert.ok(fs.readFileSync(path.join(lessonsDir, file!), "utf-8").includes("Il primo passo"));

      // write_node: node item + lesson event, island contains the node
      await say(id, "FAKE:tool:write_node:" + b64(NODE));
      await idle(id);
      assert.equal(items(id, "node")[0]?.nodeId, "prima-cosa");
      assert.ok(ev.some((e) => e.type === "lesson" && e.nodeId === "prima-cosa"));
      assert.ok(fs.readFileSync(path.join(lessonsDir, file!), "utf-8").includes("prima-cosa"));
      assert.equal(state(id).plan?.nodeStatus.N1, "done");

      // quiz: answer correct -> progress record -> map
      await say(id, "FAKE:tool:quiz:" + b64(QUIZ));
      const quiz = await waitFor(() => items(id, "quiz").find((q) => !q.result));
      assert.equal((await api(`/api/sessions/${id}/answer`, { requestId: quiz.requestId, kind: "quiz", answer: { selected: ["B"], dontKnow: false, confidence: 80 } })).status, 200);
      await idle(id);
      assert.equal(items(id, "quiz")[0].result?.correct, true);
      assert.ok(items(id, "teacher").at(-1)!.text.includes('"correct":true'), "fake-claude received the graded result");
      const rec = fs.readFileSync(path.join(root, "courses", "demo", "progress.jsonl"), "utf-8").trim().split("\n").map((l) => JSON.parse(l));
      assert.ok(rec.some((r) => r.kind === "check" && r.correct === true && r.nodeRefs.includes("N1") && r.lessonSlug === "il-primo-passo"));
      const map = (await (await api("/api/map")).json()) as { nodes: { nodeId: string; status: string }[] };
      assert.equal(map.nodes.find((n) => n.nodeId === "prima-cosa")?.status, "solid");

      const f = await api("/files/" + lessonPath);
      assert.equal(f.status, 200);
      assert.ok((await f.text()).includes("prima-cosa"));

      // tool chips carry the raw tool names the UI maps to labels
      assert.ok(items(id, "tool").some((t) => t.tool === "mcp__app__write_node"));
    } finally {
      stream.close();
    }
  });

  it("resumes an existing lesson with its plan restored", async () => {
    const lessonsDir = path.join(root, "courses", "demo", "lessons");
    const file = fs.readdirSync(lessonsDir).find((f) => f.endsWith("-il-primo-passo.html"))!;
    const r = await api("/api/sessions", { kind: "teach", course: "demo", model: "haiku", target: { mode: "existing", lessonPath: `courses/demo/lessons/${file}` }, ankiOptIn: false });
    const { id } = (await r.json()) as { id: string };
    await idle(id);
    const s = state(id);
    assert.equal(s.lessonPath, `courses/demo/lessons/${file}`);
    assert.equal(s.plan?.approved, true);
    assert.equal(s.plan?.nodeStatus.N1, "done");
    assert.match((s.items[0] as any).text, /Riprendiamo la lezione "Il primo passo"/);
    // quiz nodeId validation works without a new propose_plan: N2 is accepted, N9 is not
    await say(id, "FAKE:tool:quiz:" + b64({ ...QUIZ, nodeId: "N9" }));
    await idle(id);
    assert.match(items(id, "teacher").at(-1)!.text, /Invalid quiz/);
    await say(id, "FAKE:tool:quiz:" + b64({ ...QUIZ, nodeId: "N2" }));
    await waitFor(() => items(id, "quiz").find((q) => !q.result));
    await api(`/api/sessions/${id}/interrupt`, {});
    await idle(id);
  });

  it("interrupt during a slow turn returns to idle", async () => {
    const id = await create("Il secondo passo");
    await idle(id);
    await say(id, "FAKE:slow");
    await waitFor(() => state(id).status === "running" && items(id, "teacher").some((t) => t.text.includes("parola0")));
    await api(`/api/sessions/${id}/interrupt`, {});
    await idle(id);
    assert.equal(state(id).lastError, undefined);
  });

  it("an externally killed process gives error, then the next message respawns with --resume", async () => {
    const id = await create("Il secondo passo");
    await idle(id);
    const sess = (srv.manager as any).sessions.get(id);
    assert.ok(!sess.turns.stderr.join("\n").includes("--resume"));
    process.kill(sess.turns.pid);
    await waitFor(() => state(id).status === "error");
    assert.match(state(id).lastError ?? "", /teacher process exited/);
    await say(id, "ciao ancora");
    await idle(id);
    assert.ok(sess.turns.stderr.join("\n").includes("--resume"), "respawned with --resume");
    assert.equal(items(id, "learner").length, 2);
    assert.equal(items(id, "teacher").length, 2, "one teacher item per turn, ids unique across respawns");
  });

  it("one teacher item per text block (thinking block first), not streaming once finalised", async () => {
    const id = await create("Il secondo passo");
    await idle(id);
    const t = items(id, "teacher");
    assert.equal(t.length, 1);
    assert.equal(t[0].text, "Ciao, sono il docente.");
    assert.equal(t[0].streaming, false);
  });

  it("concept tools: search_concepts through the fake engine", async () => {
    // "Il primo passo" was taught above, so create returns 400 for it
    const id = await create("Il secondo passo");
    await idle(id);
    await say(id, "FAKE:tool:search_concepts:" + b64({ query: "passo" }));
    await idle(id);
    const text = items(id, "teacher").at(-1)!.text;
    assert.ok(text.includes("n_fx_a"), "the fake engine echoes the tool result");
    assert.ok(text.includes("Primo passo"), "the result line carries the concept name");
  });

  it("resume after a missed check restores the node as missed", async () => {
    const lessonsDir = path.join(root, "courses", "demo", "lessons");
    const file = fs.readdirSync(lessonsDir).find((f) => f.endsWith("-il-primo-passo.html"))!;
    const target = { mode: "existing", lessonPath: `courses/demo/lessons/${file}` };
    const mk = async () => ((await (await api("/api/sessions", { kind: "teach", course: "demo", model: "haiku", target, ankiOptIn: false })).json()) as { id: string }).id;
    const id = await mk();
    await idle(id);
    await say(id, "FAKE:tool:quiz:" + b64(QUIZ));
    const quiz = await waitFor(() => items(id, "quiz").find((q) => !q.result));
    await api(`/api/sessions/${id}/answer`, { requestId: quiz.requestId, kind: "quiz", answer: { selected: ["A"], dontKnow: false, confidence: 80 } });
    await idle(id);
    const id2 = await mk();
    assert.equal(state(id2).plan?.nodeStatus.N1, "missed");
  });

  it("rejects re-teaching a taught planned title and continuing a prose lesson", async () => {
    const a = await api("/api/sessions", { kind: "teach", course: "demo", model: "haiku", target: { mode: "planned", title: "Il primo passo" }, ankiOptIn: false });
    assert.equal(a.status, 400);
    assert.match(((await a.json()) as any).error, /Continua/);
    const prose = path.join(root, "courses", "demo", "lessons", "0099-prosa.html");
    const island = { metadata: { title: "Prosa", topic: "t" }, nodes: [] };
    fs.writeFileSync(prose, `<html><script type="application/json" id="lesson-data">${JSON.stringify(island)}</script></html>`);
    const b = await api("/api/sessions", { kind: "teach", course: "demo", model: "haiku", target: { mode: "existing", lessonPath: "courses/demo/lessons/0099-prosa.html" }, ankiOptIn: false });
    assert.equal(b.status, 400);
    assert.match(((await b.json()) as any).error, /prosa/);
  });

  it("a --resume that dies before init falls back to a fresh session and re-sends the message", async () => {
    const id = await create("Il secondo passo");
    await idle(id);
    const sess = (srv.manager as any).sessions.get(id);
    const oldId = sess.turns.resume;
    process.kill(sess.turns.pid);
    await waitFor(() => state(id).status === "error");
    process.env.FAKE_CLAUDE_RESUME_FAIL = "1";
    try {
      await say(id, "dopo il crash");
      await waitFor(() => items(id, "notice").some((n) => /Ripresa della conversazione non riuscita/.test(n.text)));
      await idle(id);
    } finally {
      delete process.env.FAKE_CLAUDE_RESUME_FAIL;
    }
    assert.notEqual(sess.turns.resume, oldId);
    assert.equal(items(id, "learner").length, 2, "learner message not duplicated");
    assert.equal(items(id, "teacher").length, 2, "the re-sent message got its reply");
  });

  it("/files: bad escapes give 400, HTML gets CSP and nosniff", async () => {
    assert.equal((await api("/files/%E0%A4%A")).status, 400);
    assert.equal((await api("/%E0%A4%A")).status, 400);
    const file = fs.readdirSync(path.join(root, "courses", "demo", "lessons")).find((f) => f.endsWith("-il-primo-passo.html"))!;
    const r = await api("/files/courses/demo/lessons/" + file);
    assert.equal(r.headers.get("x-content-type-options"), "nosniff");
    assert.match(r.headers.get("content-security-policy") ?? "", /frame-ancestors 'self'/);
  });

  it("supersedes older pending quiz when a new quiz arrives; answering does not bring back old quiz", async () => {
    const lessonsDir = path.join(root, "courses", "demo", "lessons");
    const file = fs.readdirSync(lessonsDir).find((f) => f.endsWith("-il-primo-passo.html"))!;
    const target = { mode: "existing", lessonPath: `courses/demo/lessons/${file}` };
    const id = ((await (await api("/api/sessions", { kind: "teach", course: "demo", model: "haiku", target, ankiOptIn: false })).json()) as { id: string }).id;
    await idle(id);

    const ctx = (srv.manager as any).getContext(id);
    let p1Err: any;
    const p1 = ctx.request({ kind: "quiz", input: QUIZ, displayOrder: [0, 1, 2, 3] }).catch((e: any) => { p1Err = e; });
    const quiz1 = await waitFor(() => items(id, "quiz").find((q) => q.requestId && !q.result));
    assert.equal(state(id).pendingRequestId, quiz1.requestId);

    // Second quiz arrives (e.g. engine retry or replacement while quiz1 was pending)
    const QUIZ2 = { ...QUIZ, question: "Second question?" };
    let p2Answer: any;
    const p2 = ctx.request({ kind: "quiz", input: QUIZ2, displayOrder: [0, 1, 2, 3] }).then((a: any) => { p2Answer = a; });
    const quiz2 = await waitFor(() => items(id, "quiz").find((q) => q.requestId && q.input.question === "Second question?"));
    assert.equal(state(id).pendingRequestId, quiz2.requestId);

    // Quiz 1 was cancelled/superseded
    await p1;
    assert.equal(p1Err?.message, "cancelled");
    const q1Item = items(id, "quiz").find((q) => q.requestId === quiz1.requestId);
    assert.equal(q1Item?.cancelled, true);

    // Answering quiz 2 settles it
    const res = await api(`/api/sessions/${id}/answer`, { requestId: quiz2.requestId, kind: "quiz", answer: { selected: ["B"], dontKnow: false, confidence: 90 } });
    assert.equal(res.status, 200);
    await p2;
    assert.deepEqual(p2Answer, { selected: ["B"], dontKnow: false, confidence: 90 });

    // The session has NO pending request left — quiz 1 never reopens
    assert.equal(state(id).pendingRequestId, null);
  });
});
