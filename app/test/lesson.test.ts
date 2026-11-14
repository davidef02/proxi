// The Lesson-artifact module on a fixture repo: path rule, one parse with its record, status, plan write, write seam.
import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { listLessons, coursesChanged, readLesson, revisePlan } from "../server/lesson.ts";
import { subscribeGlobal } from "../server/events.ts";
import { readLessonText } from "../server/graph/extract.ts";
import { finishLessonTool } from "../server/tools/finish.ts";
import { toolsFor } from "../server/tools/index.ts";
import { fakeContext } from "./helpers.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:1"; // never reach a real Anki from a test

const DAG = "graph TD\nN1[a] --> N2[b]\nN2 --> N3[c]";

function lessonHtml(num: number, slug: string, nodes: number, dag = "", quiz?: object) {
  const doc = {
    metadata: { lessonNumber: num, topic: slug, title: slug, slug, dateCreated: "", dateModified: "", version: "1", initialDag: dag, targetGoal: "g" },
    nodes: Array.from({ length: nodes }, (_, i) => ({ nodeId: `n${i + 1}`, title: `Nodo ${i + 1}`, tier: "axiom", content: "x", ...(quiz ? { quiz } : {}) })),
  };
  return `<html><body><script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script></body></html>`;
}

function repo() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "lesson-"));
  const w = (p: string, c: string) => {
    fs.mkdirSync(path.dirname(path.join(root, p)), { recursive: true });
    fs.writeFileSync(path.join(root, p), c);
  };
  return { root, w };
}

test("a plan/practice record never makes a future lesson look closed", async () => {
  const { root, w } = repo();
  w("courses/demo/learning-records/0001-first.md", "# first\n\n<!-- lesson: first -->\n");
  const ctx = fakeContext(root, { kind: "practice", course: "demo", model: "haiku", practiceNodes: [] } as any);
  await finishLessonTool.run({ recordTitle: "Pratica", summary: "s", evidence: "e", implications: "i" }, ctx);
  const rec = fs.readdirSync(path.join(root, "courses/demo/learning-records")).find((f) => f.startsWith("0002-"))!;
  assert.match(fs.readFileSync(path.join(root, "courses/demo/learning-records", rec), "utf-8"), /<!-- lesson: -->\s*$/);
  w("courses/demo/lessons/0002-next.html", lessonHtml(2, "next", 1, DAG));
  assert.equal(listLessons(root)[0].status, "inProgress");
});

test("revisePlan writes DAG and goal through html-lesson and reports whether it changed anything", async () => {
  const { root, w } = repo();
  fs.cpSync(path.resolve(import.meta.dirname, "../../assets"), path.join(root, "assets"), { recursive: true });
  w("courses/demo/lessons/0001-a.html", lessonHtml(1, "a", 1, DAG));
  const rel = "courses/demo/lessons/0001-a.html";
  assert.equal(await revisePlan(root, rel, { dag: DAG, goal: "g" }), false);
  const dag2 = DAG + "\nN3 --> N4[d]";
  assert.equal(await revisePlan(root, rel, { dag: dag2, goal: "nuovo" }), true);
  const l = readLesson(root, rel)!;
  assert.equal(l.doc.metadata.initialDag, dag2);
  assert.equal(l.doc.metadata.targetGoal, "nuovo");
  assert.equal(l.doc.nodes.length, 1);
  assert.match(fs.readFileSync(path.join(root, rel), "utf-8"), /nuovo|N4\[d\]/); // the rendered page follows the island
});

test("extract reads quiz.correctIndex as 1-based", () => {
  const { root, w } = repo();
  w("courses/demo/lessons/0001-a.html", lessonHtml(1, "a", 1, "", { question: "Q?", options: ["uno", "due", "tre"], correctIndex: 2, explanation: "e" }));
  assert.match(readLessonText(root, "demo/0001-a")!.text, /Risposta corretta: due/);
});

test("write seam: write tools are flagged, and coursesChanged raises one debounced 'courses' event", async () => {
  const writes = toolsFor({ kind: "teach", course: "demo", model: "haiku", ankiOptIn: true } as any, "/nonexistent").filter((t) => t.writes).map((t) => t.name).sort();
  assert.deepEqual(writes, ["finish_lesson", "propose_plan", "write_node"]);
  const { root } = repo();
  const seen: string[] = [];
  const off = subscribeGlobal((e) => seen.push(e.type));
  coursesChanged(root);
  coursesChanged(root);
  assert.deepEqual(seen, []);
  await new Promise((r) => setTimeout(r, 500));
  off();
  assert.deepEqual(seen, ["courses"]);
});
