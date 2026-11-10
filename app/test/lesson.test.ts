// The Lesson-artifact module on a fixture repo: path rule, one parse with its record, status, plan write, write seam.
import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { docRel, readDocBody, readReferenceDocument, listLessons, lessonRel, lessonAbs, coursesChanged, readLesson, readLessonHtml, revisePlan, closingRecord } from "../server/lesson.ts";
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

test("path rule: accepts courses/<course>/lessons/<file>.html only, no traversal", () => {
  assert.equal(lessonRel("courses/demo/lessons/0001-a.html"), "courses/demo/lessons/0001-a.html");
  assert.equal(lessonRel("courses\\demo\\lessons\\0001-a.html"), "courses/demo/lessons/0001-a.html");
  for (const bad of ["../x/lessons/0001-a.html", "courses/../lessons/0001-a.html", "courses/demo/lessons/../../x.html", "courses/demo/reference/a.html", "courses/demo/lessons/a.md", "courses/demo/lessons/sub/a.html", "lessons/0001-a.html", "/etc/passwd", ""])
    assert.equal(lessonRel(bad), null, bad);
  assert.throws(() => lessonAbs("/r", "courses/demo/lessons/../x.html"), /Bad lesson path/);
  assert.equal(lessonAbs("/r", "courses/demo/lessons/0001-a.html"), path.resolve("/r", "courses/demo/lessons/0001-a.html"));
});

test("readLesson: parsed once with its record; missing and island-less are null, a malformed island throws", () => {
  const { root, w } = repo();
  w("courses/demo/lessons/0001-a.html", lessonHtml(1, "a", 1, DAG));
  w("courses/demo/lessons/0002-prosa.html", "<html>solo prosa</html>");
  w("courses/demo/lessons/0003-rotta.html", '<script type="application/json" id="lesson-data">{nope</script>');
  w("courses/demo/learning-records/0009-x.md", "# x\n\n<!-- lesson: a -->\n");
  const l = readLesson(root, "courses/demo/lessons/0001-a.html")!;
  assert.equal(l.summary.slug, "a");
  assert.equal(l.doc.nodes.length, 1);
  assert.equal(l.dagNodes, 3);
  assert.equal(l.pendingNodes, 2);
  assert.equal(l.record, "courses/demo/learning-records/0009-x.md");
  assert.equal(readLesson(root, "courses/demo/lessons/0099-none.html"), null);
  assert.equal(readLesson(root, "courses/demo/lessons/0002-prosa.html"), null);
  assert.match(readLessonHtml(root, "courses/demo/lessons/0002-prosa.html")!, /prosa/);
  assert.equal(readLessonHtml(root, "courses/demo/lessons/0099-none.html"), null);
  assert.throws(() => readLesson(root, "courses/demo/lessons/0003-rotta.html"), /Malformed/);
  assert.throws(() => readLesson(root, "../x.html"), /Bad lesson path/);
  assert.deepEqual(listLessons(root).map((x) => x.summary.slug), ["a"]); // listing skips the unreadable and the island-less
});

test("status: taught / inProgress / closed, one closed rule (marker first, number only for marker-less records)", () => {
  const { root, w } = repo();
  const st = (slug: string) => listLessons(root).find((l) => l.summary.slug === slug)!.status;
  w("courses/demo/lessons/0001-a.html", lessonHtml(1, "a", 1, DAG));
  w("courses/demo/lessons/0002-b.html", lessonHtml(2, "b", 3, DAG));
  w("courses/demo/lessons/0003-c.html", lessonHtml(3, "c", 1, DAG));
  assert.equal(st("a"), "inProgress");
  assert.equal(st("b"), "taught");
  // A record carrying another lesson's marker closes that lesson, never the one sharing its number.
  w("courses/demo/learning-records/0003-x.md", "# x\n\n<!-- lesson: a -->\n");
  assert.equal(st("a"), "closed");
  assert.equal(st("c"), "inProgress");
  // The empty marker (plan/practice record) names no lesson: it closes nothing, whatever its number.
  w("courses/demo/learning-records/0002-y.md", "# y\n\n<!-- lesson: -->\n");
  assert.equal(st("b"), "taught");
  // Legacy: a record with no marker at all closes the lesson with its number.
  w("courses/demo/learning-records/0001-z.md", "# z\n");
  assert.equal(closingRecord(root, "demo", "a", 1), "courses/demo/learning-records/0003-x.md"); // its marker wins over the legacy number
  w("courses/demo/learning-records/0003-x.md", "# x\n");
  assert.equal(st("c"), "closed");
});

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

test("docRel / readDocBody: lessons and reference sheets only, scripts and styles stripped", () => {
  assert.equal(docRel("courses/c/reference/x.html"), "courses/c/reference/x.html");
  assert.equal(docRel("courses/c/lessons/0001-a.html"), "courses/c/lessons/0001-a.html");
  assert.equal(docRel("courses/c/exercises/x.html"), null);
  assert.equal(docRel("courses/c/reference/../../x.html"), null);
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "doc-"));
  fs.mkdirSync(path.join(root, "courses/c/reference"), { recursive: true });
  fs.writeFileSync(path.join(root, "courses/c/reference/x.html"), "<html><head><title>T</title><style>a{}</style><link rel=x></head><body><script>1</script><main><h2>Ciao</h2></main></body></html>");
  assert.deepEqual(readDocBody(root, "courses/c/reference/x.html"), { title: "T", html: "<h2>Ciao</h2>" });
  assert.equal(readDocBody(root, "courses/c/reference/nope.html"), null);
});

test("readReferenceDocument: parses #reference-data island and falls back to sibling lesson", () => {
  const { root, w } = repo();
  // Case 1: reference HTML with #reference-data island
  const refDoc = {
    title: "Sintesi A",
    topic: "A",
    slug: "a",
    nodes: [{ nodeId: "n1", number: 1, title: "Nodo 1", tier: "unconditional-truth", summary: "Fatto 1" }],
    formulas: [],
    misconceptions: [],
  };
  w("courses/c/reference/a.html", `<html><body><script type="application/json" id="reference-data">${JSON.stringify(refDoc)}</script></body></html>`);
  const parsed = readReferenceDocument(root, "courses/c/reference/a.html");
  assert.equal(parsed?.title, "Sintesi A");
  assert.equal(parsed?.nodes.length, 1);
  assert.equal(parsed?.nodes[0].summary, "Fatto 1");

  // Case 2: reference file missing island or not compiled yet -> falls back to lesson artifact
  w("courses/c/lessons/0002-b.html", lessonHtml(2, "b", 2, DAG));
  w("courses/c/learning-records/0002-b.md", "# Rec\n\n## Evidence\n\n- Learner slip A\n\n<!-- lesson: b -->\n");
  const fallback = readReferenceDocument(root, "courses/c/reference/b.html");
  assert.equal(fallback?.title, "b");
  assert.equal(fallback?.nodes.length, 2);
  assert.equal(fallback?.initialDag, DAG);
  assert.ok(fallback?.misconceptions.some(m => m.description.includes("Learner slip A")));

  // Case 3: Invalid path or missing lesson
  assert.equal(readReferenceDocument(root, "courses/c/reference/nope.html"), null);
  assert.equal(readReferenceDocument(root, "courses/c/lessons/b.html"), null);
});
