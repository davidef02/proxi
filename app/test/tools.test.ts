import { describe, it, after } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import { handleMcp } from "../server/mcp.ts";
import { gradeQuiz } from "../server/tools/quiz.ts";
import { toolsFor } from "../server/tools/index.ts";
import { writeNodeSchema } from "../server/tools/node.ts";
import { resolveDataRoot } from "../../extensions/data-root.ts";
import { parseLessonDocument } from "../../extensions/html-lesson.ts";
import { deleteNode } from "../server/graph/store.ts";
import { fakeContext, makeTempRepo } from "./helpers.ts";
import type { SessionConfig } from "../shared/protocol.ts";

const dirs: string[] = [];
after(async () => {
  await new Promise((r) => setTimeout(r, 500)); // let fire-and-forget board refreshes finish
  for (const d of dirs) fs.rmSync(d, { recursive: true, force: true, maxRetries: 3 });
});

const teach = (o: Partial<SessionConfig> = {}): SessionConfig => ({
  kind: "teach", course: "demo", model: "haiku", target: { mode: "planned", title: "Il primo passo" }, ankiOptIn: false, ...o,
});
const DAG = 'graph TD\n  N1["Nodo 1: la prima cosa è vera"] --> N2["Nodo 2: la seconda segue"]';

async function setup(config: SessionConfig, answers: any[] = []) {
  const repo = await makeTempRepo();
  dirs.push(repo);
  const ctx = fakeContext(repo, config, answers);
  const tools = toolsFor(config, repo);
  const call = async (name: string, args: unknown) => {
    const res: any = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }, tools, ctx);
    return { isError: res.result.isError as boolean, text: res.result.content[0].text as string };
  };
  return { repo, ctx, tools, call };
}

const quizIn = (o: any = {}) => ({
  kind: "probe", question: "Q?", options: [{ label: "A" }, { label: "B" }, { label: "C", value: "c" }], explanation: "because", correctAnswer: "B", ...o,
});
const planIn = (o: any = {}) => ({ topic: "Demo topic", approach: "prosa", dag: DAG, goal: "capire", ...o });
const nodeIn = (o: any = {}) => ({
  planNode: "N1",
  node: { nodeId: "prima-cosa", title: "La prima cosa", tier: "unconditional-truth", content: "<p>vero</p>", ...o },
});

async function approvedLesson(config = teach(), extra: any[] = []) {
  const s = await setup(config, [{ approved: true }, ...extra]);
  const r = await s.call("propose_plan", planIn());
  assert.equal(r.isError, false, r.text);
  return s;
}

describe("toolsFor", () => {
  const names = (c: SessionConfig, repo: string) => toolsFor(c, repo).map((t) => t.name);
  it("selects by kind, Anki opt-in and exercises package", async () => {
    const repo = await makeTempRepo();
    dirs.push(repo);
    assert.deepEqual(names(teach(), repo), ["quiz", "ask", "propose_plan", "write_node", "research", "visual", "demo", "write_exercise", "run_exercise", "finish_lesson", "read_lesson", "search_concepts", "open_concept", "browse_concepts"]);
    assert.ok(names(teach({ ankiOptIn: true }), repo).includes("sync_anki"));
    assert.deepEqual(names({ kind: "plan", course: "demo", model: "haiku" }, repo), ["quiz", "ask", "propose_curriculum", "research", "read_lesson", "search_concepts", "open_concept", "browse_concepts"]);
    assert.deepEqual(names({ kind: "practice", course: "demo", model: "haiku" }, repo), ["quiz", "ask", "problem", "conclude", "research", "visual", "write_exercise", "run_exercise", "read_lesson", "search_concepts", "open_concept", "browse_concepts"]);
    fs.rmSync(path.join(repo, "courses/demo/exercises/package.json"));
    assert.ok(!names(teach(), repo).includes("run_exercise"));
    assert.deepEqual(names({ kind: "practice", course: "demo", model: "haiku" }, repo), ["quiz", "ask", "problem", "conclude", "research", "visual", "read_lesson", "search_concepts", "open_concept", "browse_concepts"]);
  });
});

describe("gradeQuiz", () => {
  const q: any = { kind: "check", question: "", options: [], explanation: "", correctAnswer: ["a", "b"], multiSelect: true };
  it("exact set only, dontKnow never correct", () => {
    assert.equal(gradeQuiz(q, { selected: ["b", "a"], dontKnow: false }).correct, true);
    assert.equal(gradeQuiz(q, { selected: ["a"], dontKnow: false }).correct, false);
    assert.equal(gradeQuiz(q, { selected: ["a", "b", "c"], dontKnow: false }).correct, false);
    assert.equal(gradeQuiz(q, { selected: [], dontKnow: true }).correct, false);
  });
});

describe("quiz", () => {
  it("grades, logs progress, returns labels", async () => {
    const { ctx, call } = await setup(teach(), [{ selected: ["c"], dontKnow: false, confidence: 80, note: "boh" }]);
    const r = await call("quiz", quizIn());
    assert.equal(r.isError, false);
    assert.deepEqual(JSON.parse(r.text), { correct: false, dontKnow: false, selected: ["C"], confidence: 80, note: "boh", correctAnswer: ["B"] });
    assert.equal(ctx.requests[0].kind, "quiz");
    assert.deepEqual([...ctx.requests[0].displayOrder].sort(), [0, 1, 2]);
    assert.equal(ctx.progress[0].kind, "probe");
    assert.equal(ctx.progress[0].correct, false);
  });

  it("correct answer omits correctAnswer", async () => {
    const { call } = await setup(teach(), [{ selected: ["B"], dontKnow: false, confidence: 90 }]);
    const out = JSON.parse((await call("quiz", quizIn())).text);
    assert.equal(out.correct, true);
    assert.equal("correctAnswer" in out, false);
  });

  it("includes thinkingWhileReading when provided by the learner", async () => {
    const { call } = await setup(teach(), [{ selected: ["B"], dontKnow: false, thinkingWhileReading: "Pensavo fosse A ma poi B" }]);
    const out = JSON.parse((await call("quiz", quizIn())).text);
    assert.equal(out.thinkingWhileReading, "Pensavo fosse A ma poi B");
  });

  it("dontKnow is reported and not correct", async () => {
    const { call } = await setup(teach(), [{ selected: [], dontKnow: true }]);
    const out = JSON.parse((await call("quiz", quizIn())).text);
    assert.deepEqual([out.correct, out.dontKnow, out.selected], [false, true, []]);
  });

  it("rejects bad quizzes before asking the learner", async () => {
    const { ctx, call } = await setup(teach());
    for (const bad of [
      quizIn({ correctAnswer: "zzz" }),
      quizIn({ options: [{ label: "A" }, { label: "A" }] }),
      quizIn({ correctAnswer: ["A", "B"] }),
      quizIn({ kind: "check" }),
      quizIn({ options: [{ label: "A" }] }),
      quizIn({ misconceptions: [{ value: "zz", misconception: "m" }] }),
    ]) {
      const r = await call("quiz", bad);
      assert.equal(r.isError, true, JSON.stringify(bad));
    }
    assert.equal(ctx.requests.length, 0);
  });

  it("nodeId must be in the approved plan; plan statuses follow checks", async () => {
    const { ctx, call } = await approvedLesson(teach(), [{ selected: ["A"], dontKnow: false }]);
    assert.equal(ctx.getPlan()!.nodeStatus.N1, "current");
    const bad = await call("quiz", quizIn({ kind: "check", nodeId: "N7" }));
    assert.equal(bad.isError, true);
    assert.match(bad.text, /not in the approved plan/);
    const ok = await call("quiz", quizIn({ kind: "check", nodeId: "N1" }));
    assert.equal(ok.isError, false);
    assert.deepEqual(ctx.progress[0].nodeRefs, ["N1"]);
    assert.equal(ctx.getPlan()!.nodeStatus.N1, "missed");
  });

  it("cancelled is an error result", async () => {
    const { call } = await setup(teach(), [new Error("cancelled")]);
    const r = await call("quiz", quizIn());
    assert.equal(r.isError, true);
    assert.match(r.text, /dismissed/);
  });
});

describe("ask", () => {
  it("maps values to labels and passes other", async () => {
    const { call } = await setup(teach(), [{ selected: ["v"], other: "altro" }]);
    const r = await call("ask", { question: "?", options: [{ label: "Uno", value: "v" }, { label: "Due" }] });
    assert.deepEqual(JSON.parse(r.text), { selected: ["Uno"], other: "altro" });
  });
});

describe("propose_plan", () => {
  it("rejects a DAG with bad labels without asking", async () => {
    const { ctx, call } = await setup(teach());
    const r = await call("propose_plan", planIn({ dag: 'graph TD\nN1["Primo"]' }));
    assert.equal(r.isError, true);
    assert.match(r.text, /line 2/);
    assert.equal(ctx.requests.length, 0);
  });

  it("free lessons need a title", async () => {
    const { call } = await setup(teach({ target: { mode: "free" } }));
    assert.match((await call("propose_plan", planIn())).text, /needs a title/);
  });

  it("approval creates the lesson with the planned title verbatim and sets the plan", async () => {
    const { repo, ctx, call } = await setup(teach(), [{ approved: true }]);
    const r = await call("propose_plan", planIn({ title: "Ignorato" }));
    const out = JSON.parse(r.text);
    assert.equal(out.approved, true);
    assert.equal(out.lessonPath, "courses/demo/lessons/0001-il-primo-passo.html");
    assert.ok(fs.existsSync(path.join(repo, out.lessonPath)));
    assert.equal(ctx.getLessonPath(), out.lessonPath);
    assert.deepEqual(ctx.events[0], { type: "lesson", lessonPath: out.lessonPath });
    assert.deepEqual(ctx.getPlan()!.nodeStatus, { N1: "current", N2: "todo" });
    assert.equal(ctx.getPlan()!.approved, true);
  });

  it("free lesson uses the proposed title", async () => {
    const { ctx, call } = await setup(teach({ target: { mode: "free" } }), [{ approved: true }]);
    await call("propose_plan", planIn({ title: "Un idea libera" }));
    assert.equal(ctx.getLessonPath(), "courses/demo/lessons/0001-un-idea-libera.html");
  });

  it("rejection returns feedback and creates nothing", async () => {
    const { repo, ctx, call } = await setup(teach(), [{ approved: false, feedback: "più corto" }]);
    const r = await call("propose_plan", planIn());
    assert.deepEqual(JSON.parse(r.text), { approved: false, feedback: "più corto" });
    assert.equal(ctx.getLessonPath(), undefined);
    assert.deepEqual(fs.readdirSync(path.join(repo, "courses/demo/lessons")).filter((f) => f.endsWith(".html")), []);
  });

  it("existing mode attaches without creating", async () => {
    const first = await approvedLesson();
    const lp = first.ctx.getLessonPath()!;
    const cfg = teach({ target: { mode: "existing", lessonPath: lp } });
    const ctx2 = fakeContext(first.repo, cfg, [{ approved: true }]);
    const res: any = await handleMcp(
      { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "propose_plan", arguments: planIn() } },
      toolsFor(cfg, first.repo), ctx2);
    assert.equal(JSON.parse(res.result.content[0].text).lessonPath, lp);
    assert.equal(fs.readdirSync(path.join(first.repo, "courses/demo/lessons")).filter((f) => f.endsWith(".html")).length, 1);
  });
});

describe("write_node", () => {
  it("requires a lesson", async () => {
    const { call } = await setup(teach());
    const r = await call("write_node", nodeIn());
    assert.equal(r.isError, true);
    assert.match(r.text, /No lesson artifact yet/);
  });

  it("writes, upserts, emits, updates the plan", async () => {
    const { repo, ctx, call } = await approvedLesson();
    const r = await call("write_node", nodeIn({ quiz: { question: "Q", options: ["a", "b"], correctIndex: 2, correctAnswer: "b", explanation: "e", misconceptions: [{ optionIndex: 1, misconception: "m" }] } }));
    assert.deepEqual(JSON.parse(r.text), { ok: true, nodeCount: 1 });
    const html = fs.readFileSync(path.join(repo, ctx.getLessonPath()!), "utf-8");
    assert.match(html, /id="node-prima-cosa"/);
    const emitted = ctx.items.at(-1) as any;
    assert.deepEqual({ ...emitted, node: undefined }, { id: "node:prima-cosa", kind: "node", nodeId: "prima-cosa", planNode: "N1", title: "La prima cosa", lessonPath: ctx.getLessonPath(), node: undefined });
    assert.equal(emitted.node.nodeId, "prima-cosa");
    assert.equal(emitted.node.quiz.correctAnswer, "b");
    assert.deepEqual(ctx.events.at(-1), { type: "lesson", lessonPath: ctx.getLessonPath(), nodeId: "prima-cosa" });
    assert.deepEqual(ctx.getPlan()!.nodeStatus, { N1: "done", N2: "current" });
    const again = await call("write_node", nodeIn({ title: "Nuovo titolo" }));
    assert.equal(JSON.parse(again.text).nodeCount, 1);
  });

  it("stores anchors to existing concepts in the data island", async () => {
    const { repo, ctx, call } = await approvedLesson();
    const anchors = [{ id: "n_fx_a", name: "Primo passo" }];
    const r = await call("write_node", nodeIn({ anchors }));
    assert.equal(r.isError, false, r.text);
    const doc = parseLessonDocument(fs.readFileSync(path.join(repo, ctx.getLessonPath()!), "utf-8"));
    assert.deepEqual(doc!.nodes[0].anchors, anchors);
  });

  it("reminds to anchor a node whose prose names a live concept", async () => {
    const { call } = await approvedLesson();
    const r = await call("write_node", nodeIn({ content: "<p>Riprendiamo il primo passo di ieri.</p>" }));
    assert.equal(r.isError, false, r.text);
    assert.match(JSON.parse(r.text).notes[0], /anchors missing: the prose names Primo passo \(n_fx_a\)/);
    const anchored = await call("write_node", nodeIn({ content: "<p>Riprendiamo il primo passo di ieri.</p>", anchors: [{ id: "n_fx_a", name: "Primo passo" }] }));
    assert.equal(JSON.parse(anchored.text).notes, undefined);
    const dropped = await call("write_node", nodeIn({ content: "<p>senza ancoraggio</p>" }));
    assert.match(JSON.parse(dropped.text).notes[0], /anchors dropped/);
    const unnamed = await call("write_node", nodeIn({ content: "<p>un ref senza il nome del concetto</p>", anchors: [{ id: "n_fx_a", name: "Primo passo" }] }));
    assert.match(JSON.parse(unnamed.text).notes[0], /anchors not named in the prose: Primo passo \(n_fx_a\)/);
  });

  it("checks anchors against the stored concept name, normalised like the prose", async () => {
    const { repo, call } = await approvedLesson();
    fs.appendFileSync(
      path.join(repo, "graph", "nodes.jsonl"),
      JSON.stringify({ id: "n_fx_snake", name: "use_state", statement: "Uno stato che si riusa.", aliases: [], createdAt: "2026-11-01T00:00:00.000Z", provenance: [{ chatId: "demo/0002-il-secondo-passo", messageId: "m1" }] }) + "\n",
    );
    // the tutor's name differs from the stored one: the prose names the stored concept, so no note
    const stored = await call("write_node", nodeIn({ content: "<p>Riprendiamo il Primo passo.</p>", anchors: [{ id: "n_fx_a", name: "Direzione" }] }));
    assert.equal(JSON.parse(stored.text).notes, undefined);
    // the note names the stored concept, not the name the tutor sent
    const unnamed = await call("write_node", nodeIn({ content: "<p>niente</p>", anchors: [{ id: "n_fx_a", name: "Direzione" }] }));
    assert.match(JSON.parse(unnamed.text).notes[0], /anchors not named in the prose: Primo passo \(n_fx_a\)/);
    assert.doesNotMatch(JSON.parse(unnamed.text).notes[0], /Direzione/);
    // an underscore in the name survives the same normalisation as the prose
    const snake = await call("write_node", nodeIn({ content: "<p>Qui conta <code>use_state</code>.</p>", anchors: [{ id: "n_fx_snake", name: "use_state" }] }));
    assert.equal(JSON.parse(snake.text).notes, undefined);
  });

  it("flags prose that names the tutor's name instead of the concept's stored name", async () => {
    const { call } = await approvedLesson();
    const r = await call("write_node", nodeIn({ content: "<p>Solo direzione.</p>", anchors: [{ id: "n_fx_a", name: "Direzione" }] }));
    assert.match(JSON.parse(r.text).notes[0], /anchors not named in the prose: Primo passo \(n_fx_a\)/);
    assert.doesNotMatch(JSON.parse(r.text).notes[0], /Direzione/);
  });

  it("normalises emphasis and extra spaces in a stored concept name on both feedback paths", async () => {
    const { repo, call } = await approvedLesson();
    const name = "Props  e `emit`";
    fs.appendFileSync(
      path.join(repo, "graph", "nodes.jsonl"),
      JSON.stringify({ id: "n_fx_emit", name, statement: "Le props passano i dati, emit li risale.", aliases: [], createdAt: "2026-11-01T00:00:00.000Z", provenance: [{ chatId: "demo/0002-il-secondo-passo", messageId: "m1" }] }) + "\n",
    );
    // anchored: the prose names the stored concept once markup and double spaces are normalised
    const anchored = await call("write_node", nodeIn({ content: "<p>Le props e emit di Vue.</p>", anchors: [{ id: "n_fx_emit", name }] }));
    assert.equal(JSON.parse(anchored.text).notes, undefined);
    // unanchored: the missing-anchor note still recognises the stored name through its markup
    const missing = await call("write_node", nodeIn({ content: "<p>Le props e emit di Vue.</p>" }));
    assert.ok(JSON.parse(missing.text).notes[0].includes(`anchors missing: the prose names ${name} (n_fx_emit)`), JSON.parse(missing.text).notes[0]);
  });

  it("trims surrounding spaces in a stored concept name before comparing", async () => {
    const first = await approvedLesson();
    const padded = (id: string, name: string) =>
      JSON.stringify({ id, name, statement: "Un concetto noto.", aliases: [], createdAt: "2026-11-01T00:00:00.000Z", provenance: [{ chatId: "demo/0000-seed", messageId: "m1" }] }) + "\n";
    fs.appendFileSync(path.join(first.repo, "graph", "nodes.jsonl"), padded("n_fx_pad", "Primo passo "));
    // anchored: the stored name's trailing space must not hide it from the prose
    const anchored = await first.call("write_node", nodeIn({ content: "<p>Riprendiamo il Primo passo.</p>", anchors: [{ id: "n_fx_pad", name: "Primo passo " }] }));
    assert.equal(JSON.parse(anchored.text).notes, undefined);
    // unanchored: a three-letter concept padded to four characters is still too short to be suggested
    const { repo, call } = await approvedLesson();
    fs.appendFileSync(path.join(repo, "graph", "nodes.jsonl"), padded("n_fx_ref", "Ref "));
    const short = await call("write_node", nodeIn({ content: "<p>una Ref </p>" }));
    assert.equal(JSON.parse(short.text).notes, undefined);
  });

  it("trims surrounding spaces in the anchor's own name for a deleted concept", async () => {
    const { repo, call } = await approvedLesson();
    const anchors = [{ id: "n_fx_a", name: "  Primo passo " }];
    assert.equal((await call("write_node", nodeIn({ anchors, content: "<p>Riprendiamo il Primo passo.</p>" }))).isError, false);
    await deleteNode(repo, "n_fx_a");
    // the concept is gone, so the fallback name is the only text: its leading/trailing spaces must not hide it
    const kept = await call("write_node", nodeIn({ anchors, content: "<p>Di nuovo il Primo passo.</p>" }));
    assert.equal(JSON.parse(kept.text).notes, undefined);
  });

  it("falls back to the anchor's own name for a deleted concept when asking for the prose", async () => {
    const { repo, ctx, call } = await approvedLesson();
    const anchors = [{ id: "n_fx_a", name: "Primo passo" }];
    assert.equal((await call("write_node", nodeIn({ anchors, content: "<p>Riprendiamo il Primo passo.</p>" }))).isError, false);
    await deleteNode(repo, "n_fx_a");
    // the concept is gone: its stored name is not looked up, the anchor's own name is the text
    const kept = await call("write_node", nodeIn({ anchors, content: "<p>Di nuovo il Primo passo.</p>" }));
    assert.equal(JSON.parse(kept.text).notes, undefined);
    const unnamed = await call("write_node", nodeIn({ anchors, content: "<p>niente</p>" }));
    assert.match(JSON.parse(unnamed.text).notes[0], /anchors not named in the prose: Primo passo \(n_fx_a\)/);
    const doc = parseLessonDocument(fs.readFileSync(path.join(repo, ctx.getLessonPath()!), "utf-8"));
    assert.deepEqual(doc!.nodes[0].anchors, anchors);
  });

  it("suggests live concepts at most three times, through markup and in any field", async () => {
    const { call } = await approvedLesson();
    const r = await call("write_node", nodeIn({ content: "<p>PRIMO <strong>PASSO</strong>, secondo passo, direzione, progresso.</p>" }));
    const note = JSON.parse(r.text).notes[0];
    assert.match(note, /anchors missing: the prose names Primo passo \(n_fx_a\)/);
    assert.equal(note.match(/\(n_fx_[a-d]\)/g)!.length, 3);

    const inSteps = await call("write_node", nodeIn({
      content: "<p>niente da vedere qui</p>",
      steps: [{ stepId: "s1", title: "Un passo", content: "Qui conta il Secondo passo." }],
    }));
    assert.match(JSON.parse(inSteps.text).notes[0], /anchors missing: the prose names Secondo passo \(n_fx_b\)/);
  });

  it("does not suggest a deleted concept, a short name or a name inside a longer word", async () => {
    const { repo, call } = await approvedLesson();
    fs.appendFileSync(
      path.join(repo, "graph", "nodes.jsonl"),
      JSON.stringify({ id: "n_fx_ref", name: "Ref", statement: "Un ref è una scatola.", aliases: [], createdAt: "2026-11-01T00:00:00.000Z", provenance: [{ chatId: "demo/0000-seed", messageId: "m1" }] }) + "\n",
    );
    await deleteNode(repo, "n_fx_a");
    const r = await call("write_node", nodeIn({ content: "<p>un primo passo, stiamo direzionando, una Ref</p>" }));
    assert.equal(r.isError, false, r.text);
    assert.equal(JSON.parse(r.text).notes, undefined);
  });

  it("a lesson without anchors still parses and takes an anchored rewrite", async () => {
    const { repo, ctx, call } = await approvedLesson();
    const file = path.join(repo, ctx.getLessonPath()!);
    assert.equal((await call("write_node", nodeIn({ content: "<p>senza ancore</p>" }))).isError, false);
    assert.equal(parseLessonDocument(fs.readFileSync(file, "utf-8"))!.nodes[0].anchors, undefined);
    const anchors = [{ id: "n_fx_b", name: "Secondo passo" }];
    assert.equal((await call("write_node", nodeIn({ anchors, content: "<p>con un'ancora</p>" }))).isError, false);
    assert.deepEqual(parseLessonDocument(fs.readFileSync(file, "utf-8"))!.nodes[0].anchors, anchors);
  });

  it("rejects a malformed anchor id or an empty name before checking the map", async () => {
    const { call } = await approvedLesson();
    for (const anchor of [{ id: "vue_ref", name: "Ref" }, { id: "n_fx_a", name: "" }]) {
      const r = await call("write_node", nodeIn({ anchors: [anchor] }));
      assert.equal(r.isError, true, JSON.stringify(anchor));
      assert.match(r.text, /^Invalid input/);
    }
  });

  it("rejects an unknown anchor id, naming it", async () => {
    const { repo, ctx, call } = await approvedLesson();
    const r = await call("write_node", nodeIn({ anchors: [{ id: "n_nope", name: "X" }] }));
    assert.equal(r.isError, true);
    assert.match(r.text, /n_nope/);
    const doc = parseLessonDocument(fs.readFileSync(path.join(repo, ctx.getLessonPath()!), "utf-8"));
    assert.equal(doc!.nodes.length, 0);
  });

  it("names every unknown anchor id and keeps the known ones out of the error", async () => {
    const { repo, ctx, call } = await approvedLesson();
    const r = await call("write_node", nodeIn({
      anchors: [
        { id: "n_fx_a", name: "Primo passo" },
        { id: "n_nope1", name: "X" },
        { id: "n_nope2", name: "Y" },
      ],
    }));
    assert.equal(r.isError, true);
    assert.match(r.text, /n_nope1, n_nope2/);
    assert.doesNotMatch(r.text, /n_fx_a/);
    const doc = parseLessonDocument(fs.readFileSync(path.join(repo, ctx.getLessonPath()!), "utf-8"));
    assert.equal(doc!.nodes.length, 0);
  });

  it("rewrite keeps an anchor whose concept was deleted", async () => {
    const { repo, ctx, call } = await approvedLesson();
    const anchors = [{ id: "n_fx_a", name: "Primo passo" }];
    assert.equal((await call("write_node", nodeIn({ anchors }))).isError, false);
    await deleteNode(repo, "n_fx_a");
    const r = await call("write_node", nodeIn({ anchors, content: "<p>di nuovo</p>" }));
    assert.equal(r.isError, false, r.text);
    const doc = parseLessonDocument(fs.readFileSync(path.join(repo, ctx.getLessonPath()!), "utf-8"));
    assert.deepEqual(doc!.nodes[0].anchors, anchors);
    const bad = await call("write_node", nodeIn({ anchors: [...anchors, { id: "n_nope", name: "X" }], content: "<p>terzo</p>" }));
    assert.equal(bad.isError, true);
    assert.match(bad.text, /n_nope/);
    assert.doesNotMatch(bad.text, /n_fx_a/);
  });

  it("strips ankiParams without opt-in and says so; keeps them with opt-in", async () => {
    const anki = { targetQuestion: "Q?", targetFact: "F" };
    const no = await approvedLesson();
    const r = await no.call("write_node", nodeIn({ ankiParams: anki }));
    assert.match(r.text, /ankiParams removed/);
    assert.doesNotMatch(fs.readFileSync(path.join(no.repo, no.ctx.getLessonPath()!), "utf-8"), /targetQuestion/);
    const yes = await approvedLesson(teach({ ankiOptIn: true }), [{ selected: ["A"], dontKnow: false }, { selected: ["B"], dontKnow: false }]);
    // no check yet, then a missed check: stripped, with the instruction on how to keep them
    const r1 = await yes.call("write_node", nodeIn({ ankiParams: anki }));
    assert.match(r1.text, /kept after the check passes/);
    await yes.call("quiz", quizIn({ kind: "check", nodeId: "N1" }));
    assert.match((await yes.call("write_node", nodeIn({ ankiParams: anki }))).text, /kept after the check passes/);
    // passed check: kept
    await yes.call("quiz", quizIn({ kind: "check", nodeId: "N1" }));
    const r2 = await yes.call("write_node", nodeIn({ ankiParams: anki }));
    assert.doesNotMatch(r2.text, /removed/);
    assert.match(fs.readFileSync(path.join(yes.repo, yes.ctx.getLessonPath()!), "utf-8"), /targetQuestion/);
  });

  it("hardens the payload", async () => {
    const { call } = await approvedLesson(teach({ ankiOptIn: true }));
    const q = (o: any) => nodeIn({ quiz: { question: "Q", options: ["a", "b"], correctIndex: 2, correctAnswer: "b", explanation: "e", ...o } });
    const bads = [
      q({ correctIndex: undefined }),
      q({ correctAnswer: undefined }),
      q({ correctAnswer: "a" }),
      nodeIn({ ankiParams: { targetQuestion: "Q?" } }),
      nodeIn({ ankiParams: { targetFact: "F" } }),
      nodeIn({ ankiParams: { targetQuestion: "Q?", targetFact: "F", suggestedCloze: "{{c1::x}}" } }),
      nodeIn({ ankiParams: { suggestedCloze: "no cloze here" } }),
      nodeIn({ content: "<p>x</p><script>alert(1)</script>" }),
      nodeIn({ motivation: "<iframe src=x></iframe>" }),
      nodeIn({ connection: '<img src=x onerror="alert(1)">' }),
      nodeIn({ content: '<a href="javascript:alert(1)">x</a>' }),
      q({ explanation: "<script>x</script>" }),
      nodeIn({ visuals: [{ type: "svg-inline", content: '<svg onload="x()"></svg>' }] }),
    ];
    for (const b of bads) {
      const r = await call("write_node", b);
      assert.equal(r.isError, true, JSON.stringify(b));
      assert.match(r.text, /^Invalid input/);
    }
    assert.equal((await call("write_node", nodeIn({ ankiParams: { suggestedCloze: "Il {{c2::ref}} è una scatola" }, content: "<p>una <code>&lt;button&gt;</code> e un <strong>on</strong> click</p>" }))).isError, false);
  });

  it("every real node in the learner's lessons passes the schema (skipped without courses, e.g. CI)", (t) => {
    const root = resolveDataRoot().dataRoot;
    if (!fs.existsSync(path.join(root, "courses"))) return t.skip("no courses in the data folder");
    let checked = 0;
    for (const c of fs.readdirSync(path.join(root, "courses"), { withFileTypes: true })) {
      const dir = path.join(root, "courses", c.name, "lessons");
      if (!c.isDirectory() || !fs.existsSync(dir)) continue;
      for (const f of fs.readdirSync(dir).filter((x) => x.endsWith(".html"))) {
        const m = /<script type="application\/json" id="lesson-data">([\s\S]*?)<\/script>/.exec(fs.readFileSync(path.join(dir, f), "utf-8"));
        if (!m) continue;
        (JSON.parse(m[1]).nodes ?? []).forEach((node: any, i: number) => {
          const a = node.ankiParams; // legacy fact-only cards (no question) never produced a card; new writes must give both
          if (a && !a.targetQuestion !== !a.targetFact) delete node.ankiParams;
          if (a?.targetQuestion && a.suggestedCloze) delete a.suggestedCloze; // legacy: the engine keeps only the question card
          const r = writeNodeSchema.safeParse({ planNode: `N${i + 1}`, node });
          assert.ok(r.success, `${c.name}/${f} ${node.nodeId}: ${JSON.stringify(r.error?.issues)}`);
          checked++;
        });
      }
    }
    assert.ok(checked > 0);
  });

  it("supports steps and derives node.content when missing or empty", async () => {
    const { repo, ctx, call } = await approvedLesson();
    const nodePayload = {
      nodeId: "steps-node",
      title: "Nodo con step",
      tier: "derivation",
      steps: [
        { stepId: "s1", title: "P1", content: "Primo passo di spiegazione." },
        { stepId: "s2", title: "P2", content: "Secondo passo di spiegazione." }
      ]
    };
    const r = await call("write_node", { planNode: "N1", node: nodePayload });
    assert.equal(r.isError, false);
    const out = JSON.parse(r.text);
    assert.equal(out.ok, true);
    assert.equal(out.nodeCount, 1);
    const emitted = ctx.items.at(-1) as any;
    assert.equal(emitted.node.content, "Primo passo di spiegazione.\n\nSecondo passo di spiegazione.");
    assert.equal(emitted.node.steps.length, 2);
  });

  it("rejects unsafe HTML inside step content", async () => {
    const { call } = await approvedLesson();
    const badNode = {
      nodeId: "bad-step-node",
      title: "Nodo cattivo",
      tier: "derivation",
      steps: [
        { stepId: "s1", content: "Valido" },
        { stepId: "s2", content: "<script>alert(1)</script>" }
      ]
    };
    const r = await call("write_node", { planNode: "N1", node: badNode });
    assert.equal(r.isError, true);
    assert.match(r.text, /unsafe HTML/i);
  });

  it("warns when planNode is out of order", async () => {
    const { call } = await approvedLesson();
    const r = await call("write_node", { ...nodeIn(), planNode: "N2" });
    assert.match(r.text, /warning/);
    assert.equal(r.isError, false);
  });

  it("validates the payload", async () => {
    const { call } = await approvedLesson();
    for (const bad of [
      nodeIn({ nodeId: "Bad Id" }),
      nodeIn({ tier: "weird" }),
      nodeIn({ content: "" }),
      nodeIn({ quiz: { question: "Q", options: ["a", "b"], correctIndex: 0, explanation: "e" } }),
      nodeIn({ quiz: { question: "Q", options: ["a", "b"], correctIndex: 3, explanation: "e" } }),
      nodeIn({ quiz: { question: "Q", options: ["a", "b"], correctIndex: 1, explanation: "e", misconceptions: [{ optionIndex: 3, misconception: "m" }] } }),
      { ...nodeIn(), planNode: "3" },
    ]) {
      const r = await call("write_node", bad);
      assert.equal(r.isError, true, JSON.stringify(bad));
      assert.match(r.text, /^Invalid input/);
    }
  });
});

describe("read_lesson", () => {
  it("returns the nodes as compact text, filtered by nodeIds; rejects paths outside a course's lessons", async () => {
    const s = await approvedLesson(teach(), [{ selected: ["b"], dontKnow: false }]);
    const w = await s.call("write_node", nodeIn({ content: "<p>vero <b>sempre</b></p>", codeBlocks: [{ language: "ts", code: "const x = ref(0)" }] }));
    assert.equal(w.isError, false, w.text);
    const rel = s.ctx.getLessonPath()!;
    const all = await s.call("read_lesson", { lessonPath: rel });
    assert.equal(all.isError, false, all.text);
    assert.match(all.text, /## prima-cosa: La prima cosa/);
    assert.match(all.text, /vero sempre/);
    assert.match(all.text, /const x = ref\(0\)/);
    assert.doesNotMatch(all.text, /<p>|<style|<script/);
    const none = await s.call("read_lesson", { lessonPath: rel, nodeIds: ["altro"] });
    assert.match(none.text, /Nodi non trovati: altro/);
    assert.equal((await s.call("read_lesson", { lessonPath: "../secret.html" })).isError, true);
  });
});

describe("concept tools", () => {
  it("are listed by tools/list for every session kind", async () => {
    const repo = await makeTempRepo();
    dirs.push(repo);
    const configs: SessionConfig[] = [
      teach(),
      teach({ course: "lezioni-singole", target: { mode: "free" } }),
      { kind: "plan", course: "demo", model: "haiku", chapterTitle: "Demo" },
      { kind: "practice", course: "demo", model: "haiku" },
    ];
    for (const c of configs) {
      const res: any = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/list" }, toolsFor(c, repo), fakeContext(repo, c, []));
      const names: string[] = res.result.tools.map((t: any) => String(t.name).replace(/^mcp__app__/, ""));
      for (const n of ["search_concepts", "open_concept", "browse_concepts"]) assert.ok(names.includes(n), `${n} missing for ${c.kind} ${c.course}`);
    }
  });

  it("search_concepts finds the fixture concept; open_concept rejects unknown ids", async () => {
    const { call } = await setup(teach());
    const found = await call("search_concepts", { query: "passo" });
    assert.equal(found.isError, false, found.text);
    assert.match(found.text, /n_fx_a/);

    const opened = await call("open_concept", { id: "n_fx_a" });
    assert.equal(opened.isError, false, opened.text);
    assert.ok(opened.text.includes("Statement: Ogni percorso comincia da un primo passo concreto."));

    const browsed = await call("browse_concepts", { course: "demo" });
    assert.equal(browsed.isError, false, browsed.text);
    assert.ok(browsed.text.startsWith("Lessons of Demo"), browsed.text);

    const nope = await call("open_concept", { id: "nope" });
    assert.equal(nope.isError, true);
    assert.match(nope.text, /Unknown concept id "nope"/);
  });
});

describe("sync_anki", () => {
  it("only offered with opt-in; offline falls back to the TSV", async () => {
    process.env.LEARNER_ANKI_URL = "http://127.0.0.1:1";
    try {
      const s = await approvedLesson(teach({ ankiOptIn: true }), [{ selected: ["B"], dontKnow: false }]);
      await s.call("quiz", quizIn({ kind: "check", nodeId: "N1" }));
      await s.call("write_node", nodeIn({ ankiParams: { targetQuestion: "Q?", targetFact: "F" } }));
      const r = await s.call("sync_anki", {});
      assert.equal(r.isError, false);
      const summary = JSON.parse(r.text).summary;
      assert.match(summary, /offline|incomplete/i);
      assert.match(summary, /decks\/.*\.tsv/);
      assert.equal((s.ctx.items.at(-1) as any).kind, "notice");
    } finally {
      delete process.env.LEARNER_ANKI_URL;
    }
  });

  it("is refused without opt-in even if called directly", async () => {
    const s = await setup(teach({ ankiOptIn: true }));
    const noOpt = fakeContext(s.repo, teach({ ankiOptIn: false }));
    const res: any = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "sync_anki", arguments: {} } }, s.tools, noOpt);
    assert.equal(res.result.isError, true);
  });
});

describe("exercise tools", () => {
  const ex = (o: any = {}) => ({ name: "01-demo", instructions: "Fai **questo**.", files: [{ path: "Esercizio.vue", content: "<template/>" }, { path: "Esercizio.test.js", content: "// t" }], ...o });

  it("writes files and instructions, updates the session list", async () => {
    const { repo, ctx, call } = await setup(teach());
    const r = await call("write_exercise", ex());
    assert.equal(r.isError, false, r.text);
    const out = JSON.parse(r.text);
    assert.equal(out.dir, "courses/demo/exercises/01-demo");
    assert.match(out.note, /editor dell'app/);
    const dir = path.join(repo, out.dir);
    assert.equal(fs.readFileSync(path.join(dir, "Esercizio.vue"), "utf-8"), "<template/>");
    assert.match(fs.readFileSync(path.join(dir, "ISTRUZIONI.md"), "utf-8"), /Fai \*\*questo\*\*/);
    assert.equal(ctx.getExercises()[0].name, "01-demo");
    assert.match(ctx.getExercises()[0].instructions!, /questo/);
    assert.equal((ctx.items.at(-1) as any).kind, "exercise");
  });

  it("rejects bad names and paths and writes nothing", async () => {
    const { repo, call } = await setup(teach());
    const bads = [
      ex({ name: "demo" }), ex({ name: "1-x" }), ex({ name: "../01-x" }),
      ex({ files: [{ path: "../evil.js", content: "" }] }),
      ex({ files: [{ path: "/abs.js", content: "" }] }),
      ex({ files: [{ path: "C:/x.js", content: "" }] }),
      ex({ files: [{ path: "a\\b.js", content: "" }] }),
      ex({ files: [{ path: "sub/../../x.js", content: "" }] }),
      ex({ files: [{ path: "run.sh", content: "" }] }),
      ex({ files: [{ path: "ISTRUZIONI.md", content: "" }] }),
      ex({ files: [] }),
    ];
    for (const b of bads) assert.equal((await call("write_exercise", b)).isError, true, JSON.stringify(b));
    assert.equal(fs.existsSync(path.join(repo, "courses/demo/exercises/01-demo")), false);
    assert.equal(fs.existsSync(path.join(repo, "courses/evil.js")), false);
  });

  it("allows nested relative paths", async () => {
    const { repo, call } = await setup(teach());
    assert.equal((await call("write_exercise", ex({ files: [{ path: "sub/dir/x.ts", content: "1" }] }))).isError, false);
    assert.ok(fs.existsSync(path.join(repo, "courses/demo/exercises/01-demo/sub/dir/x.ts")));
  });

  it("run_exercise validates the name", async () => {
    const { call } = await setup(teach());
    assert.equal((await call("run_exercise", { name: "../x" })).isError, true);
  });

  it("run_exercise records no progress (AC1)", async () => {
    const { repo, ctx, call } = await setup(teach());
    const progressFile = path.join(repo, "courses/demo/progress.jsonl");
    fs.mkdirSync(path.dirname(progressFile), { recursive: true });
    const seed = '{"kind":"check","nodeRefs":["demo/seed"],"correct":true}\n';
    fs.writeFileSync(progressFile, seed, "utf-8");
    const initialBytes = fs.readFileSync(progressFile);

    await call("run_exercise", { name: "01-missing" });
    assert.deepEqual(ctx.progress, []);
    assert.deepEqual(fs.readFileSync(progressFile), initialBytes);
  });
});

describe("finish_lesson", () => {
  const fin = (o: any = {}) => ({ recordTitle: "Il primo passo: acquisito", summary: "Sa fare X.", evidence: "Quiz ok.", implications: "Procedere.", ...o });

  it("needs a lesson in teach sessions", async () => {
    const { call } = await setup(teach());
    assert.equal((await call("finish_lesson", fin())).isError, true);
  });

  it("compiles the reference, numbers the record, appends NOTES", async () => {
    const { repo, ctx, call } = await approvedLesson();
    await call("write_node", nodeIn());
    const learning = path.join(repo, "courses/demo/learning-records");
    fs.mkdirSync(learning, { recursive: true });
    fs.writeFileSync(path.join(learning, "0007-old.md"), "x");
    const r = await call("finish_lesson", fin({ notesAddendum: "Gli piacciono gli esempi." }));
    assert.equal(r.isError, false, r.text);
    const out = JSON.parse(r.text);
    assert.equal(out.recordPath, "courses/demo/learning-records/0001-il-primo-passo-acquisito.md");
    assert.equal(out.referencePath, "courses/demo/reference/il-primo-passo.html");
    assert.equal(out.notesUpdated, true);
    assert.equal(out.anki, undefined);
    assert.ok(fs.existsSync(path.join(repo, out.referencePath)));
    assert.equal(
      fs.readFileSync(path.join(repo, out.recordPath), "utf-8"),
      "# Il primo passo: acquisito\n\nSa fare X.\n\n## Evidence\n\nQuiz ok.\n\n## Implications\n\nProcedere.\n\n<!-- lesson: il-primo-passo -->\n");
    assert.equal(fs.readFileSync(path.join(repo, "courses/demo/NOTES.md"), "utf-8").replace(/\r\n/g, "\n"), "# Notes\n\nPrefers short answers.\n\nGli piacciono gli esempi.\n");
    assert.equal((ctx.items.at(-1) as any).kind, "finish");
  });
});

describe("finish_lesson idempotency", () => {
  it("a second close only refreshes the reference and dedupes the NOTES addendum", async () => {
    const { repo, call } = await approvedLesson();
    await call("write_node", nodeIn());
    const fin = { recordTitle: "Il primo passo: acquisito", summary: "s", evidence: "e", implications: "i", notesAddendum: "Nota unica." };
    assert.equal((await call("finish_lesson", fin)).isError, false);
    const again = await call("finish_lesson", fin);
    assert.equal(again.isError, false);
    assert.match(again.text, /already closed: reference refreshed/);
    assert.match(again.text, /0001-/);
    assert.equal(fs.readdirSync(path.join(repo, "courses/demo/learning-records")).length, 1);
    assert.equal(fs.readFileSync(path.join(repo, "courses/demo/NOTES.md"), "utf-8").match(/Nota unica/g)!.length, 1);
  });
});

describe("finish_lesson numbering", () => {
  it("falls back to max+1 when the lesson number is taken", async () => {
    const { repo, call } = await approvedLesson();
    await call("write_node", nodeIn());
    const learning = path.join(repo, "courses/demo/learning-records");
    fs.mkdirSync(learning, { recursive: true });
    fs.writeFileSync(path.join(learning, "0001-other.md"), "x\n<!-- lesson: other -->\n");
    const r = await call("finish_lesson", { recordTitle: "T", summary: "s", evidence: "e", implications: "i" });
    assert.equal(JSON.parse(r.text).recordPath, "courses/demo/learning-records/0002-t.md");
  });
});

describe("write_exercise protects the learner's work", () => {
  const ex = (o: any = {}) => ({ name: "01-demo", instructions: "i", files: [{ path: "Esercizio.vue", content: "v1" }], ...o });
  it("rewrites app-written files, refuses learner-edited ones", async () => {
    const { repo, call } = await setup(teach());
    const f = path.join(repo, "courses/demo/exercises/01-demo/Esercizio.vue");
    assert.equal((await call("write_exercise", ex())).isError, false);
    assert.equal((await call("write_exercise", ex({ files: [{ path: "Esercizio.vue", content: "v2" }] }))).isError, false);
    assert.equal(fs.readFileSync(f, "utf-8"), "v2");
    fs.writeFileSync(f, "il lavoro del learner");
    const r = await call("write_exercise", ex({ files: [{ path: "Esercizio.vue", content: "v3" }, { path: "Altro.js", content: "x" }] }));
    assert.equal(r.isError, true);
    assert.match(r.text, /Esercizio\.vue è stato modificato dal learner/);
    assert.equal(fs.readFileSync(f, "utf-8"), "il lavoro del learner");
    assert.equal(fs.existsSync(path.join(repo, "courses/demo/exercises/01-demo/Altro.js")), false);
  });
  it("CRLF checkout of an app-written file is not a learner edit", async () => {
    const { repo, call } = await setup(teach());
    const f = path.join(repo, "courses/demo/exercises/01-demo/Esercizio.vue");
    assert.equal((await call("write_exercise", ex({ files: [{ path: "Esercizio.vue", content: "a\nb\n" }] }))).isError, false);
    fs.writeFileSync(f, "a\r\nb\r\n");
    const r = await call("write_exercise", ex({ files: [{ path: "Esercizio.vue", content: "c\n" }] }));
    assert.equal(r.isError, false, r.text);
    assert.equal(fs.readFileSync(f, "utf-8"), "c\n");
    fs.writeFileSync(f, "mio");
    assert.match((await call("write_exercise", ex())).text, /nuovo nome di esercizio \(NN-/);
  });
  it("does not clobber a pre-existing file it never wrote", async () => {
    const { repo, call } = await setup(teach());
    const d = path.join(repo, "courses/demo/exercises/01-demo");
    fs.mkdirSync(d, { recursive: true });
    fs.writeFileSync(path.join(d, "Esercizio.vue"), "mio");
    assert.equal((await call("write_exercise", ex())).isError, true);
  });
});

describe("propose_plan on an existing lesson", () => {
  it("an approved revised DAG rewrites the island and the artifact", async () => {
    const first = await approvedLesson();
    await first.call("write_node", nodeIn());
    const rel = first.ctx.getLessonPath()!;
    const cfg = teach({ target: { mode: "existing", lessonPath: rel } });
    const ctx = fakeContext(first.repo, cfg, [{ approved: true }]);
    ctx.setLessonPath(rel);
    const dag3 = DAG + '\n  N2 --> N3["Nodo 3: la terza completa"]';
    const res: any = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "propose_plan", arguments: planIn({ dag: dag3, goal: "nuovo goal" }) } }, toolsFor(cfg, first.repo), ctx);
    assert.equal(res.result.isError, false, res.result.content[0].text);
    const html = fs.readFileSync(path.join(first.repo, rel), "utf-8");
    assert.match(html, /Nodo 3: la terza completa/);
    assert.match(html, /"targetGoal": "nuovo goal"/);
    assert.match(html, /id="node-prima-cosa"/);
    assert.deepEqual(Object.keys(ctx.getPlan()!.nodeStatus), ["N1", "N2", "N3"]);
    assert.equal(ctx.getPlan()!.nodeStatus.N3, "todo");
  });
});

describe("propose_curriculum re-planning", () => {
  const cfg: SessionConfig = { kind: "plan", course: "demo", model: "haiku", chapterTitle: "Demo" };
  const cur = (lessons: any[], o: any = {}) => ({ chapter: "Demo", slug: "demo", destination: "x", lessons, ...o });
  it("rejects a slug other than the session course", async () => {
    const { call } = await setup(cfg);
    assert.match((await call("propose_curriculum", cur([{ title: "A" }], { slug: "altro" }))).text, /must be this session's course/);
  });
  it("fails when a taught lesson would disappear; keeps legacy slugs", async () => {
    const { repo, call } = await setup(cfg, [{ approved: true }]);
    const cpath = path.join(repo, "curriculum.json");
    const c = JSON.parse(fs.readFileSync(cpath, "utf-8"));
    c.chapters[0].lessons[0] = { title: "Un titolo molto lungo che non produce lo slug", slug: "legacy", plannedNodes: 2 };
    fs.writeFileSync(cpath, JSON.stringify(c));
    fs.writeFileSync(path.join(repo, "courses/demo/lessons/0001-legacy.html"), "<html></html>");
    const bad = await call("propose_curriculum", cur([{ title: "Altro" }]));
    assert.equal(bad.isError, true);
    assert.match(bad.text, /drops already taught lessons.*Un titolo molto lungo/);
    const ok = await call("propose_curriculum", cur([{ title: "Un titolo molto lungo che non produce lo slug" }, { title: "Nuova" }]));
    assert.equal(ok.isError, false, ok.text);
    const ch = JSON.parse(fs.readFileSync(cpath, "utf-8")).chapters[0];
    assert.equal(ch.lessons[0].slug, "legacy");
    assert.equal(ch.lessons[1].slug, "nuova");
  });
});

describe("propose_curriculum", () => {
  const cfg: SessionConfig = { kind: "plan", course: "nuovo-corso", model: "haiku", chapterTitle: "Nuovo corso" };
  const cur = (o: any = {}) => ({ chapter: "Nuovo corso", slug: "nuovo-corso", destination: "Arrivare.", mission: "# Mission\n\n## Why\n\nx", lessons: [{ title: "Prima lezione", plannedNodes: 3 }, { title: "Seconda lezione" }], ...o });

  it("validates before asking", async () => {
    const { ctx, call } = await setup(cfg);
    for (const bad of [cur({ slug: "Bad Slug" }), cur({ mission: undefined }), cur({ lessons: [] }), cur({ lessons: [{ title: "Uno!" }, { title: "uno" }] })]) {
      assert.equal((await call("propose_curriculum", bad)).isError, true, JSON.stringify(bad));
    }
    assert.equal(ctx.requests.length, 0);
  });

  it("approval creates the course, MISSION.md and the plan", async () => {
    const { repo, call } = await setup(cfg, [{ approved: true }]);
    const r = await call("propose_curriculum", cur());
    assert.equal(r.isError, false, r.text);
    for (const d of ["lessons", "reference", "learning-records"]) assert.ok(fs.existsSync(path.join(repo, "courses/nuovo-corso", d)));
    assert.match(fs.readFileSync(path.join(repo, "courses/nuovo-corso/MISSION.md"), "utf-8"), /## Why/);
    const c = JSON.parse(fs.readFileSync(path.join(repo, "curriculum.json"), "utf-8"));
    const ch = c.chapters.find((x: any) => x.slug === "nuovo-corso");
    assert.deepEqual(ch.lessons, [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 3 }, { title: "Seconda lezione", slug: "seconda-lezione" }]);
    assert.equal(c.chapters.length, 2);
  });

  it("never overwrites an existing MISSION.md", async () => {
    const { repo, call } = await setup({ ...cfg, course: "demo" }, [{ approved: true }]);
    const r = await call("propose_curriculum", cur({ slug: "demo", chapter: "Demo" }));
    assert.match(r.text, /left untouched/);
    assert.match(fs.readFileSync(path.join(repo, "courses/demo/MISSION.md"), "utf-8"), /Test course/);
  });

  it("rejection writes nothing", async () => {
    const { repo, call } = await setup(cfg, [{ approved: false, feedback: "no" }]);
    assert.deepEqual(JSON.parse((await call("propose_curriculum", cur())).text), { approved: false, feedback: "no" });
    assert.equal(fs.existsSync(path.join(repo, "courses/nuovo-corso")), false);
  });
});

describe("problem / conclude", () => {
  const cfg: SessionConfig = { kind: "practice", course: "demo", model: "haiku" };
  const prob = { statement: "S", mode: "recall", nodeKeys: ["demo/x#n1"], modelSolution: "M" };
  const concl = { outcome: "solvedWithHints", verdict: "ok", rung: "R2", missClass: null, retest: false };

  it("one open problem at a time; conclude upserts and records nothing", async () => {
    const { repo, ctx, call } = await setup(cfg);
    const progressFile = path.join(repo, "courses/demo/progress.jsonl");
    fs.mkdirSync(path.dirname(progressFile), { recursive: true });
    const seed = '{"kind":"check","nodeRefs":["demo/seed"],"correct":true}\n';
    fs.writeFileSync(progressFile, seed, "utf-8");
    const initialBytes = fs.readFileSync(progressFile);

    assert.equal((await call("conclude", concl)).isError, true);
    assert.equal((await call("problem", prob)).isError, false);
    assert.equal((await call("problem", prob)).isError, true);
    assert.equal((await call("conclude", concl)).isError, false);
    assert.equal((ctx.items[1] as any).conclusion.outcome, "solvedWithHints");
    assert.equal((ctx.items[1] as any).id, "item-1");
    assert.deepEqual(ctx.progress, []);
    assert.deepEqual(fs.readFileSync(progressFile), initialBytes);
    assert.equal((await call("problem", prob)).isError, false);
  });
});

describe("write_exercise hints", () => {
  const ex = (o: any = {}) => ({ name: "01-demo", instructions: "i", files: [{ path: "Esercizio.vue", content: "v1" }], hints: ["uno", "due"], final: true, ...o });
  it("persists hints, keeps progress unless hints change", async () => {
    const { repo, ctx, call } = await setup(teach());
    const f = path.join(repo, "courses/demo/exercises/01-demo/SUGGERIMENTI.json");
    assert.equal((await call("write_exercise", ex())).isError, false);
    assert.deepEqual(JSON.parse(fs.readFileSync(f, "utf-8")), { hints: ["uno", "due"], final: true, revealed: 0 });
    assert.equal(ctx.getExercises()[0].final, true);
    fs.writeFileSync(f, JSON.stringify({ hints: ["uno", "due"], final: true, revealed: 1 }));
    await call("write_exercise", ex());
    assert.equal(JSON.parse(fs.readFileSync(f, "utf-8")).revealed, 1);
    await call("write_exercise", ex({ hints: ["uno", "tre"] }));
    assert.equal(JSON.parse(fs.readFileSync(f, "utf-8")).revealed, 0);
    assert.equal((await call("write_exercise", ex({ hints: ["1", "2", "3", "4", "5"] }))).isError, true);
  });
});
