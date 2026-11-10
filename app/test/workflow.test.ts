import { describe, it, after } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { handleMcp } from "../server/mcp.ts";
import { toolsFor } from "../server/tools/index.ts";
import { buildSystemPrompt } from "../server/context.ts";
import { mergeFindings, readSnapshot } from "../server/graph/store.ts";
import { emitGlobal } from "../server/events.ts";
import { fakeContext, makeTempRepo } from "./helpers.ts";
import { ANKI_OPT_IN_QUESTION, type ChatItem, type SessionConfig, type SessionState } from "../shared/protocol.ts";

const dirs: string[] = [];
after(async () => {
  await new Promise((r) => setTimeout(r, 500)); // fire-and-forget board refreshes
  for (const d of dirs) fs.rmSync(d, { recursive: true, force: true, maxRetries: 3 });
});

const teach = (o: Partial<SessionConfig> = {}): SessionConfig => ({
  kind: "teach", course: "demo", model: "haiku", target: { mode: "planned", title: "Il primo passo" }, ankiOptIn: false, ...o,
});
const DAG = 'graph TD\n  N1["Nodo 1: la prima cosa è vera"] --> N2["Nodo 2: la seconda segue"]';
const FIN = { recordTitle: "Il primo passo: acquisito", summary: "s", evidence: "e", implications: "i" };

async function lesson(settings?: object) {
  const repo = await makeTempRepo();
  dirs.push(repo);
  if (settings) {
    fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
    fs.writeFileSync(path.join(repo, ".learner", "settings.json"), JSON.stringify(settings));
  }
  const config = teach();
  const ctx = fakeContext(repo, config, [{ approved: true }]);
  const tools = toolsFor(config, repo);
  const call = async (name: string, args: unknown) => {
    const res: any = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }, tools, ctx);
    return { isError: res.result.isError as boolean, text: res.result.content[0].text as string };
  };
  assert.equal((await call("propose_plan", { topic: "Demo topic", approach: "prosa", dag: DAG, goal: "capire" })).isError, false);
  await call("write_node", { planNode: "N1", node: { nodeId: "prima-cosa", title: "La prima cosa", tier: "unconditional-truth", content: "<p>vero</p>" } });
  return { repo, ctx, call };
}

describe("map trigger in finish_lesson", () => {
  it("starts one map job for the closed lesson, named after it", async () => {
    const { ctx, call } = await lesson();
    const r = await call("finish_lesson", FIN);
    assert.equal(r.isError, false, r.text);
    assert.equal(ctx.jobsStarted.length, 1);
    assert.equal(ctx.jobsStarted[0].kind, "map");
    assert.deepEqual(ctx.jobsStarted[0].input, { source: "demo/0001-il-primo-passo" });
    assert.equal(ctx.jobsStarted[0].job.title, "Mappa: Il primo passo");
    assert.equal((await call("finish_lesson", FIN)).isError, false); // refresh: the running job is not duplicated
    assert.equal(ctx.jobsStarted.length, 1);
    // in-flight map jobs are process-wide: finish this one so the next test can start its own
    emitGlobal({ type: "job", job: { ...ctx.jobsStarted[0].job, status: "done" } });
  });

  it("does nothing when autoMap is off", async () => {
    const { ctx, call } = await lesson({ autoMap: false });
    assert.equal((await call("finish_lesson", FIN)).isError, false);
    assert.equal(ctx.jobsStarted.length, 0);
  });

  it("a failing start never fails the close: it becomes a notice", async () => {
    const { ctx, call } = await lesson();
    ctx.jobs.start = () => {
      throw new Error("background jobs are not available");
    };
    const r = await call("finish_lesson", FIN);
    assert.equal(r.isError, false, r.text);
    const notice = ctx.items.find((i) => i.kind === "notice") as any;
    assert.match(notice.text, /not available/);
    assert.equal((ctx.items.at(-1) as any).kind, "finish");
  });
});

describe("standalone lessons (lezioni-singole)", () => {
  // slugify folds the apostrophe away: "L'entropia" -> "lentropia"
  const SOURCE = "lezioni-singole/0001-lentropia-misura-il-numero-di-microstati";
  const LESSON_PATH = `courses/${SOURCE.replace("lezioni-singole/", "lezioni-singole/lessons/")}.html`;
  const TSV = "decks/lentropia-misura-il-numero-di-microstati.tsv";

  it("maps a closed free lesson into the global store and exports Proxi::Lezioni singole", async () => {
    const repo = await makeTempRepo();
    dirs.push(repo);
    const config: SessionConfig = { kind: "teach", course: "lezioni-singole", model: "haiku", target: { mode: "free" }, ankiOptIn: true };
    const ctx = fakeContext(repo, config, [{ approved: true }, { selected: ["Il numero di microstati"], dontKnow: false, confidence: 85 }]);
    const tools = toolsFor(config, repo);
    const call = async (name: string, args: unknown) => {
      const res: any = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }, tools, ctx);
      return { isError: res.result.isError as boolean, text: res.result.content[0].text as string };
    };
    const prev = process.env.LEARNER_ANKI_URL;
    process.env.LEARNER_ANKI_URL = "http://127.0.0.1:1";
    try {
      const plan = await call("propose_plan", { topic: "Entropia", title: "L'entropia misura il numero di microstati", approach: "prosa", dag: DAG, goal: "capire l'entropia" });
      assert.equal(plan.isError, false, plan.text);
      assert.equal(ctx.getLessonPath(), LESSON_PATH);

      const quiz = await call("quiz", {
        kind: "check",
        nodeId: "N1",
        question: "Che cosa misura l'entropia?",
        options: [{ label: "Il numero di microstati" }, { label: "La temperatura assoluta" }],
        correctAnswer: "Il numero di microstati",
        explanation: "Conta i microstati compatibili con il macrostato.",
      });
      assert.equal(quiz.isError, false, quiz.text);
      assert.equal(JSON.parse(quiz.text).correct, true);

      const node = await call("write_node", {
        planNode: "N1",
        node: {
          nodeId: "entropia",
          title: "L'entropia conta i microstati",
          tier: "unconditional-truth",
          content: "<p>Più microstati sono compatibili con il macrostato, maggiore è l'entropia.</p>",
          ankiParams: { targetQuestion: "Che cosa misura l'entropia?", targetFact: "Il numero di microstati compatibili con il macrostato." },
        },
      });
      assert.equal(node.isError, false, node.text);

      const fin = await call("finish_lesson", FIN);
      assert.equal(fin.isError, false, fin.text);
      assert.equal(ctx.jobsStarted.length, 1);
      assert.equal(ctx.jobsStarted[0].kind, "map");
      assert.deepEqual(ctx.jobsStarted[0].input, { source: SOURCE });

      const tsv = fs.readFileSync(path.join(repo, TSV), "utf-8");
      assert.match(tsv, /^#deck:Proxi::Lezioni singole$/m);
      assert.match(tsv, /Che cosa misura l'entropia\?/);

      await mergeFindings(repo, SOURCE, { propositions: [{ id: "p1", name: "Entropia", statement: "L'entropia misura i microstati." }], chains: [] });
      const snap = await readSnapshot(repo);
      assert.ok(
        snap.nodes.some((n) => n.provenance.some((p) => p.chatId === SOURCE)),
        "the standalone lesson is recorded in the global graph store",
      );

      // in-flight map jobs are process-wide: settle this one so later tests can start their own
      emitGlobal({ type: "job", job: { ...ctx.jobsStarted[0].job, status: "done" } });
    } finally {
      if (prev === undefined) delete process.env.LEARNER_ANKI_URL;
      else process.env.LEARNER_ANKI_URL = prev;
    }
  });
});

describe("practicePropositions", () => {
  it("the practice prompt lists the selected propositions with their lessons", async () => {
    const repo = await makeTempRepo();
    dirs.push(repo);
    await mergeFindings(repo, "demo/0001-il-ref", { propositions: [{ id: "p1", name: "Ref scatola", statement: "Un ref è un contenitore reattivo." }], chains: [] });
    const digest = await (await import("../server/graph/store.ts")).graphDigest(repo);
    const id = /^(n_\S+)/m.exec(digest)![1];
    const cfg: SessionConfig = { kind: "practice", course: "demo", model: "haiku", practicePropositions: [id, "n_missing"] };
    const p = await buildSystemPrompt({ config: cfg, repoRoot: repo, sessionId: "s", today: "2026-01-01" });
    assert.match(p, /Ref scatola \| Un ref è un contenitore reattivo\. \| lessons: /);
    assert.match(p, /n_missing \(not found in the map\)/);
  });
});

// ───────────── the Anki question inside the session ─────────────

let mod: typeof import("../server/main.ts") | undefined;
try {
  mod = await import("../server/main.ts");
} catch {
  /* other agents' modules may be mid-edit */
}
const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");

async function waitFor<T>(fn: () => T | undefined | false, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

describe("in-session Anki question", { skip: mod ? false : "main.ts not loadable" }, () => {
  it("asks first with no claude process, then launches with the stored answer", async () => {
    const root = await makeTempRepo();
    dirs.push(root);
    const srv = await mod!.createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, claudeBin: process.execPath, claudeBinArgs: [FAKE], addDirs: [] });
    const post = (p: string, body: unknown = {}) =>
      fetch(srv.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
    const state = (id: string) => srv.manager.get(id) as SessionState;
    try {
      const res = await post("/api/sessions", { kind: "teach", course: "demo", target: { mode: "planned", title: "Il primo passo" } });
      assert.equal(res.status, 200);
      const { id } = (await res.json()) as { id: string };
      let s = state(id);
      assert.equal(s.status, "waiting");
      assert.equal(s.config.ankiOptIn, undefined);
      assert.deepEqual(srv.manager.livePids(), []);
      const ask = s.items.find((i): i is Extract<ChatItem, { kind: "ask" }> => i.kind === "ask")!;
      assert.equal(ask.input.question, ANKI_OPT_IN_QUESTION);
      assert.deepEqual(ask.input.options, [{ label: "Sì", value: "si" }, { label: "No", value: "no" }]);
      assert.equal(s.pendingRequestId, ask.requestId);
      assert.equal((await post(`/api/sessions/${id}/message`, { text: "ciao" })).status, 409);

      assert.equal((await post(`/api/sessions/${id}/answer`, { requestId: ask.requestId, kind: "ask", answer: { selected: ["si"] } })).status, 200);
      await waitFor(() => state(id).status === "idle");
      s = state(id);
      assert.equal(s.config.ankiOptIn, true);
      assert.ok(s.items.some((i) => i.kind === "learner"));
      assert.equal(srv.manager.livePids().length, 1);

      // "No" is stored as false
      const id2 = ((await (await post("/api/sessions", { kind: "teach", course: "demo", target: { mode: "free" } })).json()) as { id: string }).id;
      const ask2 = state(id2).items.find((i) => i.kind === "ask") as Extract<ChatItem, { kind: "ask" }>;
      await post(`/api/sessions/${id2}/answer`, { requestId: ask2.requestId, kind: "ask", answer: { selected: ["no"] } });
      await waitFor(() => state(id2).status === "idle");
      assert.equal(state(id2).config.ankiOptIn, false);

      // closing while the question is open leaves a closed session and no process
      const id3 = ((await (await post("/api/sessions", { kind: "teach", course: "demo", target: { mode: "free" } })).json()) as { id: string }).id;
      await post(`/api/sessions/${id3}/close`);
      await new Promise((r) => setTimeout(r, 100));
      assert.equal(state(id3).status, "closed");
    } finally {
      await srv.close();
    }
  });
});
