import { test, after } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { courseIndexBlock } from "../server/courseindex.ts";
import { appendProgress } from "../server/knowledge.ts";
import { initLesson, appendNode } from "../../extensions/html-lesson.ts";

const REAL = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const dirs: string[] = [];
after(() => dirs.forEach((d) => fs.rmSync(d, { recursive: true, force: true })));

/** Demo has two planned lessons (one taught) and is solid on its only node; extra has one orphan lesson. */
async function makeRepo(): Promise<{ root: string; demo: string }> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "courseindex-"));
  dirs.push(root);
  fs.mkdirSync(path.join(root, "assets"), { recursive: true });
  fs.copyFileSync(path.join(REAL, "assets", "lesson-template.html"), path.join(root, "assets", "lesson-template.html"));
  fs.writeFileSync(
    path.join(root, "curriculum.json"),
    JSON.stringify({
      chapters: [
        {
          title: "Demo",
          slug: "demo",
          lessons: [
            { title: "Lezione uno", slug: "lezione-uno", plannedNodes: 1 },
            { title: "Lezione due", slug: "lezione-due", plannedNodes: 1 },
          ],
        },
      ],
    }),
  );

  const demo = await initLesson({
    topic: "Demo",
    title: "Lezione uno",
    baseDir: root,
    lessonsDir: path.join(root, "courses", "demo", "lessons"),
  });
  await appendNode({
    lessonPath: demo.filePath,
    baseDir: root,
    node: { nodeId: "prima-cosa", title: "Prima cosa", tier: "unconditional-truth", content: "<p>x</p>" },
  });

  const extra = await initLesson({
    topic: "Extra",
    title: "Lezione extra",
    baseDir: root,
    lessonsDir: path.join(root, "courses", "extra", "lessons"),
  });
  await appendNode({
    lessonPath: extra.filePath,
    baseDir: root,
    node: { nodeId: "extra-cosa", title: "Extra cosa", tier: "unconditional-truth", content: "<p>x</p>" },
  });

  await appendProgress(root, {
    at: "2026-01-01T00:00:00.000Z",
    sessionId: "s",
    course: "demo",
    lessonSlug: "lezione-uno",
    nodeRefs: ["demo/lezione-uno#prima-cosa"],
    kind: "check",
    correct: true,
    confidence: 90,
  });
  return { root, demo: demo.filePath };
}

const STANDALONE_LINE =
  "- Lezioni singole (courses/lezioni-singole) | lessons 0 (no plan) | solido 0, fragile 0, insegnato 0, mancato 0 | last lesson -";

test("one line per course: planned, orphan and Lezioni singole", async () => {
  const { root } = await makeRepo();
  const block = await courseIndexBlock(root);

  assert.ok(block.startsWith("# Course index\n"));
  assert.match(
    block,
    /^- Demo \(courses\/demo\) \| lessons 1\/2 \| solido 1, fragile 0, insegnato 0, mancato 0 \| last lesson \d{4}-\d{2}-\d{2}$/m,
  );
  assert.match(
    block,
    /^- extra \(courses\/extra\) \| lessons 1 \(no plan\) \| solido \d+, fragile \d+, insegnato \d+, mancato \d+ \| last lesson \d{4}-\d{2}-\d{2}$/m,
  );
  assert.ok(block.split("\n").includes(STANDALONE_LINE));

  // plan first, then orphan courses, Lezioni singole last
  const at = (needle: string) => block.split("\n").findIndex((l) => l.startsWith(needle));
  assert.ok(at("- Demo (") < at("- extra ("));
  assert.ok(at("- extra (") < at("- Lezioni singole ("));

  // no writes on read: the standalone course is never created
  assert.ok(!fs.existsSync(path.join(root, "courses", "lezioni-singole")));
});

test("empty graph note, gone once a proposition exists", async () => {
  const { root } = await makeRepo();
  let block = await courseIndexBlock(root);
  assert.ok(block.includes("The knowledge graph is empty: the concept tools return nothing yet."));

  fs.mkdirSync(path.join(root, "graph"), { recursive: true });
  fs.writeFileSync(
    path.join(root, "graph", "nodes.jsonl"),
    JSON.stringify({
      id: "n_fx",
      name: "Prima cosa",
      statement: "La prima cosa è così.",
      aliases: [],
      createdAt: "2026-01-01T00:00:00.000Z",
      provenance: [{ chatId: "demo/0001-lezione-uno", messageId: "prima-cosa" }],
    }) + "\n",
  );
  block = await courseIndexBlock(root);
  assert.ok(!block.includes("The knowledge graph is empty"));
});

test("a malformed curriculum.json does not break the block", async () => {
  const { root } = await makeRepo();
  fs.writeFileSync(path.join(root, "curriculum.json"), '{"chapters": 5}');

  const block = await courseIndexBlock(root);
  assert.ok(block.startsWith("# Course index\n"));
  assert.ok(block.split("\n").includes(STANDALONE_LINE));
  // both courses fall back to orphan rows
  assert.match(block, /^- demo \(courses\/demo\) \| lessons 1 \(no plan\) \|/m);
});

test("counts every status separately", async () => {
  const { root, demo } = await makeRepo();
  for (const nodeId of ["seconda", "terza", "quarta"]) {
    await appendNode({
      lessonPath: demo,
      baseDir: root,
      node: { nodeId, title: nodeId, tier: "derivation", content: "<p>x</p>" },
    });
  }
  const base = { sessionId: "s", course: "demo", lessonSlug: "lezione-uno", kind: "check" as const };
  // prima-cosa is already solid from makeRepo: add shaky, missed and an untouched (taught) node
  await appendProgress(root, { ...base, nodeRefs: ["demo/lezione-uno#seconda"], correct: true, confidence: 20, at: "2026-01-02T00:00:00.000Z" });
  await appendProgress(root, { ...base, nodeRefs: ["demo/lezione-uno#terza"], correct: false, at: "2026-01-03T00:00:00.000Z" });

  const block = await courseIndexBlock(root);
  assert.match(
    block,
    /^- Demo \(courses\/demo\) \| lessons 1\/2 \| solido 1, fragile 1, insegnato 1, mancato 1 \| last lesson \d{4}-\d{2}-\d{2}$/m,
  );
});

test("a lezioni-singole chapter in the plan is not duplicated", async () => {
  const { root } = await makeRepo();
  fs.writeFileSync(
    path.join(root, "curriculum.json"),
    JSON.stringify({
      chapters: [
        {
          title: "Demo",
          slug: "demo",
          lessons: [
            { title: "Lezione uno", slug: "lezione-uno", plannedNodes: 1 },
            { title: "Lezione due", slug: "lezione-due", plannedNodes: 1 },
          ],
        },
        { title: "Lezioni singole", slug: "lezioni-singole", lessons: [{ title: "Sola", slug: "sola", plannedNodes: 1 }] },
      ],
    }),
  );
  const sola = await initLesson({
    topic: "Sola",
    title: "Sola",
    baseDir: root,
    lessonsDir: path.join(root, "courses", "lezioni-singole", "lessons"),
  });
  await appendNode({
    lessonPath: sola.filePath,
    baseDir: root,
    node: { nodeId: "sola-cosa", title: "Sola cosa", tier: "unconditional-truth", content: "<p>x</p>" },
  });

  const block = await courseIndexBlock(root);
  const standaloneLines = block.split("\n").filter((l) => l.startsWith("- Lezioni singole (courses/lezioni-singole)"));
  assert.equal(standaloneLines.length, 1);
  assert.match(standaloneLines[0], / \| lessons 1 \(no plan\) \| /);
});
