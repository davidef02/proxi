import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { buildSystemPrompt, openingMessage } from "../server/context.ts";
import { initLesson, appendNode } from "../../extensions/html-lesson.ts";
import type { SessionConfig } from "../shared/protocol.ts";

const REAL = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const TITLE = "Il titolo pianificato della lezione due";

async function makeRepo(opts: { bare?: boolean } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-"));
  fs.mkdirSync(path.join(root, "assets"), { recursive: true });
  fs.copyFileSync(path.join(REAL, "assets", "lesson-template.html"), path.join(root, "assets", "lesson-template.html"));
  fs.mkdirSync(path.join(root, "docs", "agents"), { recursive: true });
  for (const f of ["pedagogy.md", "anki-cards.md"]) fs.copyFileSync(path.join(REAL, "docs", "agents", f), path.join(root, "docs", "agents", f));
  const c = path.join(root, "courses", "demo");
  fs.mkdirSync(path.join(c, "learning-records"), { recursive: true });
  fs.mkdirSync(path.join(c, "lessons"), { recursive: true });
  fs.writeFileSync(
    path.join(root, "curriculum.json"),
    JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Lezione uno", slug: "lezione-uno", plannedNodes: 3 }, { title: TITLE, slug: "il-titolo-pianificato-della-lezione-due", plannedNodes: 4 }] }] })
  );
  if (!opts.bare) {
    fs.writeFileSync(path.join(c, "MISSION.md"), "# Mission\nMISSION-MARKER");
    fs.writeFileSync(path.join(c, "NOTES.md"), "# Notes\nNOTES-MARKER");
    for (const n of [1, 2, 3, 4]) fs.writeFileSync(path.join(c, "learning-records", `000${n}-x${n}.md`), `# rec ${n}\nREC-${n}`);
  }
  const dag = 'graph TD\n  N1["Nodo 1: Prima cosa"] --> N2["Nodo 2: Seconda cosa"]\n  N2 --> N3["Nodo 3: Terza cosa"]';
  const { filePath } = await initLesson({ topic: "Demo topic", title: "Lezione uno", dagMermaid: dag, targetGoal: "GOAL-MARKER", baseDir: root, lessonsDir: path.join(c, "lessons") });
  await appendNode({
    lessonPath: filePath,
    baseDir: root,
    node: {
      nodeId: "prima-cosa",
      title: "Prima cosa",
      tier: "unconditional-truth",
      content: "<p>x</p>",
      ankiParams: {
        targetQuestion: "Che cos'è la prima cosa?",
        targetFact: "È la prima cosa fondamentale.",
      },
    },
  });
  const rel = path.relative(root, filePath).replace(/\\/g, "/");
  return { root, rel };
}

const teachPlanned = (ankiOptIn: boolean): SessionConfig => ({ kind: "teach", course: "demo", model: "sonnet", target: { mode: "planned", title: TITLE }, ankiOptIn });
const teachStandalone = (): SessionConfig => ({ kind: "teach", course: "lezioni-singole", model: "sonnet", target: { mode: "free" }, ankiOptIn: false });
const planDemo = (): SessionConfig => ({ kind: "plan", course: "demo", model: "sonnet", chapterTitle: "Demo" });
const practiceCfg = (extra: Partial<SessionConfig> = {}): SessionConfig => ({ kind: "practice", course: "demo", model: "sonnet", practiceNodes: [], ...extra });

test("teach planned: sections, verbatim title, Anki line, course files, last 3 records", async () => {
  const { root } = await makeRepo();
  const p = await buildSystemPrompt({ config: teachPlanned(true), repoRoot: root, sessionId: "s", today: "2026-10-27" });
  assert.match(p, /# Who you are/);
  assert.match(p, /# This is a teaching session/);
  assert.match(p, /# Course context/);
  assert.match(p, /# This session/);
  assert.ok(p.includes(`"${TITLE}"`));
  assert.match(p, /Anki opt-in: Sì/);
  assert.match(p, /Today: 2026-10-27/);
  assert.match(p, /<file path="courses\/demo\/MISSION\.md">/);
  assert.ok(p.includes("NOTES-MARKER"));
  assert.ok(!p.includes("REC-1") && p.includes("REC-2") && p.includes("REC-4"));
  assert.match(p, /\[ \] ----\s+Il titolo pianificato/);
  assert.ok(p.includes("anki-cards.md"));
});

test("Anki No omits the card reference", async () => {
  const { root } = await makeRepo();
  const p = await buildSystemPrompt({ config: teachPlanned(false), repoRoot: root });
  assert.match(p, /Anki opt-in: No/);
  assert.ok(!p.includes('path="docs/agents/anki-cards.md"'));
});

test("existing lesson lists captured nodes and the first missing plan node", async () => {
  const { root, rel } = await makeRepo();
  const config: SessionConfig = { kind: "teach", course: "demo", model: "sonnet", target: { mode: "existing", lessonPath: rel }, ankiOptIn: false };
  const p = await buildSystemPrompt({ config, repoRoot: root, lessonPath: rel, sessionId: "s", today: "2026-10-27" });
  assert.ok(p.includes("GOAL-MARKER"));
  assert.ok(p.includes('N1: nodeId "prima-cosa"'));
  assert.match(p, /Captured nodes \(1 of 3 planned\)/);
  assert.match(p, /First missing plan node: N2/);
  assert.ok(p.includes("Nodo 3: Terza cosa"));
});

test("missing files are tolerated", async () => {
  const { root } = await makeRepo({ bare: true });
  fs.rmSync(path.join(root, "docs"), { recursive: true });
  const p = await buildSystemPrompt({ config: { kind: "teach", course: "demo", model: "haiku", target: { mode: "free" }, ankiOptIn: false }, repoRoot: root });
  assert.match(p, /Target: free/);
  assert.ok(!p.includes("MISSION-MARKER"));
});

test("plan session on a new course and practice session", async () => {
  const { root } = await makeRepo();
  const plan = await buildSystemPrompt({ config: { kind: "plan", course: "nuovo", model: "sonnet", chapterTitle: "Nuovo corso" }, repoRoot: root });
  assert.match(plan, /# This is a planning session/);
  assert.match(plan, /is new: provide a full mission/);
  const prac = await buildSystemPrompt({ config: practiceCfg(), repoRoot: root });
  assert.match(prac, /# This is an exercise session \(Esercizi\)/);
  assert.match(prac, /Course nodes \(pass the key as problem\.nodeKeys\):/);
  assert.match(prac, /- key "demo\/lezione-uno#prima-cosa" \| Prima cosa \| status/);
  assert.ok(!prac.includes("Reference documents"));
});

test("openingMessage", () => {
  assert.equal(openingMessage(teachPlanned(true)), `Iniziamo la lezione "${TITLE}".`);
  assert.match(openingMessage({ kind: "plan", course: "c", model: "sonnet", chapterTitle: "X", note: "ciao" }), /^Pianifichiamo il corso "X"\. ciao$/);
  assert.match(openingMessage({ kind: "practice", course: "c", model: "sonnet" }), /^Facciamo esercizi\./);
});

test("reference preface states that app tools override the docs", async () => {
  const { root } = await makeRepo();
  const p = await buildSystemPrompt({ config: teachPlanned(false), repoRoot: root });
  assert.match(p, /the app tools and the `# This session` block override them/);
  assert.match(p, /mirror quiz in a node artifact is not/);
});

function ankiRec(
  nodeId: string,
  at: string,
  ease: number,
  extra: { lapses?: number; interval?: number; reviewId?: number; cardId?: number } = {},
) {
  return {
    at,
    sessionId: "anki",
    course: "demo",
    lessonSlug: "lezione-uno",
    nodeRefs: [`demo/lezione-uno#${nodeId}`],
    kind: "review",
    source: "anki",
    reviewId: extra.reviewId ?? 1,
    cardId: extra.cardId ?? 1,
    ease,
    lapses: extra.lapses ?? 0,
    interval: extra.interval ?? 0,
  };
}

function writeProgress(root: string, records: unknown[]): void {
  const dir = path.join(root, "courses", "demo");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "progress.jsonl"), records.map((r) => JSON.stringify(r)).join("\n") + "\n");
}

function graphLine(i: number, chatId: string): string {
  return JSON.stringify({
    id: `n_g${i}`,
    name: `Concetto ${i}`,
    statement: `La proposizione numero ${i} del grafo.`,
    aliases: [],
    createdAt: "2026-01-01T00:00:00.000Z",
    provenance: [{ chatId, messageId: "prima-cosa" }],
  });
}

test("every kind carries the course index", async () => {
  const { root } = await makeRepo();
  const build = (config: SessionConfig) => buildSystemPrompt({ config, repoRoot: root, sessionId: "s", today: "2026-10-27" });
  for (const config of [teachPlanned(false), teachStandalone(), planDemo(), practiceCfg()]) {
    const p = await build(config);
    assert.ok(p.includes("# Course index"), `${config.kind} has no course index`);
    assert.match(
      p,
      /^- Demo \(courses\/demo\) \| lessons \d+\/\d+ \| solido \d+, fragile \d+, insegnato \d+, mancato \d+ \| last lesson (\d{4}-\d{2}-\d{2}|-)$/m,
    );
    assert.ok(
      p.split("\n").some((l) => l.startsWith("- Lezioni singole (courses/lezioni-singole) | lessons ")),
      `${config.kind} has no Lezioni singole line`,
    );
  }

  // the practice paths that skip the opening question still get the index
  for (const extra of [{ practicePropositions: ["n_x"] }, { practiceNodes: ["missing#x"] }]) {
    const p = await build(practiceCfg(extra));
    assert.ok(p.includes("# Course index"));
  }
});

test("prompt length does not follow the graph size", async () => {
  const { root, rel } = await makeRepo();
  fs.mkdirSync(path.join(root, "graph"), { recursive: true });
  const chatId = `demo/${path.basename(rel).replace(/\.html$/, "")}`;
  const configs = [teachPlanned(true), planDemo(), practiceCfg()];
  const build = () => Promise.all(configs.map((config) => buildSystemPrompt({ config, repoRoot: root, sessionId: "s", today: "2026-10-27" })));
  const write = (n: number) =>
    fs.writeFileSync(path.join(root, "graph", "nodes.jsonl"), Array.from({ length: n }, (_, i) => graphLine(i, chatId)).join("\n") + "\n");

  write(42);
  const small = await build();
  write(2000);
  const big = await build();
  for (let i = 0; i < configs.length; i++) {
    assert.equal(big[i].length, small[i].length, `${configs[i].kind} prompt grew with the graph`);
  }
});

test("no removed concept list reaches the prompt", async () => {
  const { root } = await makeRepo();
  writeProgress(root, [ankiRec("prima-cosa", "2026-01-01T00:00:00.000Z", 1)]);
  const configs = [teachPlanned(true), teachStandalone(), planDemo(), practiceCfg()];
  const prompts: string[] = [];
  for (const config of configs) prompts.push(await buildSystemPrompt({ config, repoRoot: root, sessionId: "s", today: "2026-10-27" }));

  const removed = [
    "Cards already in this course",
    "Concepts to reinforce",
    "Weak in his Anki reviews",
    "No nodes selected: choose from the course map",
    "Anki cards of this course",
  ];
  for (const p of prompts) for (const text of removed) assert.ok(!p.includes(text), `"${text}" still in the prompt`);

  const prac = prompts[3];
  assert.ok(prac.includes("Opening: nothing picked on the map."));
  assert.ok(prac.includes("call mcp__app__browse_concepts on this course"));
  assert.ok(prac.includes("Course nodes (pass the key as problem.nodeKeys):"));
  assert.match(prac, /^- key "demo\//m);
});

test("practice: picked map items skip the opening question", async () => {
  const { root } = await makeRepo();
  let p = await buildSystemPrompt({ config: practiceCfg({ practicePropositions: ["n_x"] }), repoRoot: root });
  assert.ok(p.includes("Opening: he picked these on the map."));
  assert.ok(!p.includes("Opening: nothing picked on the map."));

  p = await buildSystemPrompt({ config: practiceCfg({ practiceNodes: ["demo/lezione-uno#prima-cosa"] }), repoRoot: root });
  assert.ok(p.includes("Opening: he picked these on the map."));
  assert.ok(p.includes('key "demo/lezione-uno#prima-cosa"'));
  assert.ok(!p.includes("Opening: nothing picked on the map."));

  // picked keys the map no longer has: still his picks, so still no opening question
  p = await buildSystemPrompt({ config: practiceCfg({ practiceNodes: ["demo/lezione-uno#sparito"] }), repoRoot: root });
  assert.ok(p.includes("not found in the map"));
  assert.ok(p.includes("Opening: he picked these on the map."));
  assert.ok(!p.includes("Opening: nothing picked on the map."));
});

test("Phase 1a rule and card rule in the teach prompt, concept tools in every prompt", () => {
  const teach = fs.readFileSync(path.join(REAL, "app", "prompts", "teach.md"), "utf-8");
  const practice = fs.readFileSync(path.join(REAL, "app", "prompts", "practice.md"), "utf-8");
  const plan = fs.readFileSync(path.join(REAL, "app", "prompts", "plan.md"), "utf-8");
  assert.ok(teach.includes("Before the Phase 1a probe, consult his knowledge"));
  assert.ok(teach.includes("Before authoring cards, call `mcp__app__search_concepts`"));
  assert.ok(practice.includes("Before your opening question, consult the concept tools"));
  assert.ok(plan.includes("Before probing, read the **Course index**"));
  for (const text of [teach, practice, plan]) {
    for (const tool of ["mcp__app__search_concepts", "mcp__app__open_concept", "mcp__app__browse_concepts"]) {
      assert.ok(text.includes(tool), `missing ${tool}`);
    }
  }
});

test("write_node payload documents anchors to known concepts", () => {
  const teach = fs.readFileSync(path.join(REAL, "app", "prompts", "teach.md"), "utf-8");
  const start = teach.indexOf("### `write_node` payload");
  assert.ok(start >= 0);
  const rest = teach.slice(start + 1);
  const end = rest.search(/\n#{2,3} /);
  const section = end < 0 ? rest : rest.slice(0, end);
  assert.match(section, /`anchors`/);
  assert.match(section, /n_…/);
  assert.match(section, /search_concepts/);
  assert.match(section, /Every node gets at least one anchor/);
  assert.match(section, /names each anchored concept with its stored name verbatim/);
  assert.match(section, /resend its `anchors`/);
});

test("the teach cadence anchors every node to the concept kept in Phase 1a", () => {
  const teach = fs.readFileSync(path.join(REAL, "app", "prompts", "teach.md"), "utf-8");
  assert.match(teach, /keep its `n_…` id for the node's `anchors`/);
  assert.match(teach, /Every node carries `anchors`/);
});

test("the concept search guidance asks for bare-name, one-concept-at-a-time queries", () => {
  const teach = fs.readFileSync(path.join(REAL, "app", "prompts", "teach.md"), "utf-8");

  const probe = teach.indexOf("Before the Phase 1a probe");
  assert.ok(probe >= 0);
  const probeEnd = teach.indexOf("**1b.", probe);
  assert.ok(probeEnd > probe);
  const probeSection = teach.slice(probe, probeEnd);
  assert.match(probeSection, /one concept at a time by its bare name/);
  assert.match(probeSection, /whole-phrase query hides the partial matches behind any record that holds every word/);

  const start = teach.indexOf("### `write_node` payload");
  const rest = teach.slice(start + 1);
  const end = rest.search(/\n#{2,3} /);
  const payload = end < 0 ? rest : rest.slice(0, end);
  assert.match(payload, /only if those searches return nothing relevant, omit `anchors`/);
});

test("map prompt tells the extractor to use the tutor's declared anchors", () => {
  const map = fs.readFileSync(path.join(REAL, "app", "prompts", "subagents", "map.md"), "utf-8");
  assert.match(map, /\*\*ancore dichiarate dal tutor\*\*/);
  assert.match(map, /tra le\s+premesse/);
  assert.match(map, /aggiunta in automatico/);
});
