import { describe, it, after } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import { handleMcp } from "../server/mcp.ts";
import { toolsFor } from "../server/tools/index.ts";
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
