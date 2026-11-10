import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { listCourses, listStandaloneLessons, STANDALONE_COURSE_SLUG } from "../server/courses.ts";
import { hasData } from "../../extensions/data-root.ts";

function lessonHtml(num: number, slug: string, title: string, nodes: number, dag = "") {
  const doc = {
    metadata: { lessonNumber: num, topic: title, title, slug, dateCreated: "", dateModified: "", version: "1", initialDag: dag },
    nodes: Array.from({ length: nodes }, (_, i) => ({ nodeId: `n${i + 1}`, title: `Nodo ${i + 1}`, tier: "axiom", content: "x" })),
  };
  return `<html><script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script></html>`;
}

test("listCourses on a temp repo: planned, orphans, flags, inProgress", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "courses-"));
  const w = (p: string, c: string) => {
    fs.mkdirSync(path.dirname(path.join(root, p)), { recursive: true });
    fs.writeFileSync(path.join(root, p), c);
  };
  w("curriculum.json", JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [
    { title: "Prima", slug: "prima", plannedNodes: 2 },
    { title: "Seconda", slug: "seconda" },
  ] }] }));
  w("courses/demo/MISSION.md", "# M\n\n## Why\n\nImparare **Vue** `ref`\nin due righe.\n\nSecondo paragrafo.\n");
  w("courses/demo/exercises/package.json", "{}");
  w("courses/demo/lessons/0001-prima.html", lessonHtml(1, "prima", "Prima", 1, "graph TD\nN1[a] --> N2[b]\nN2 --> N3[c]"));
  w("courses/demo/reference/prima.html", "<html/>");
  w("courses/demo/lessons/0002-extra.html", lessonHtml(2, "extra", "Extra", 1));
  w("courses/loose/lessons/0001-solo.html", lessonHtml(1, "solo", "Solo", 1));

  const res = await listCourses(root);
  assert.deepEqual(res.map((c) => c.slug), ["demo", "loose"]);
  const demo = res[0];
  assert.equal(demo.hasMission, true);
  assert.equal(demo.hasExercises, true);
  assert.equal(demo.destination, "Imparare Vue ref in due righe.");
  assert.equal(res[1].destination, undefined);
  assert.equal(demo.lessons.length, 2);
  const [prima, seconda] = demo.lessons;
  assert.equal(prima.lessonPath, "courses/demo/lessons/0001-prima.html");
  assert.equal(prima.nodeCount, 1);
  assert.equal(prima.inProgress, true);
  assert.equal(prima.referencePath, "courses/demo/reference/prima.html");
  assert.equal(prima.plannedNodes, 2);
  assert.equal(seconda.lessonPath, undefined);
  assert.equal(demo.orphans.length, 1);
  assert.equal(demo.orphans[0].slug, "extra");
  assert.equal(demo.orphans[0].inProgress, false);
  assert.equal(res[1].lessons.length, 0);
  // A learning record closes the lesson even with DAG nodes left (by number, or by the slug marker).
  w("courses/demo/learning-records/0001-x.md", "# x\n");
  assert.equal((await listCourses(root))[0].lessons[0].inProgress, false);
  fs.rmSync(path.join(root, "courses/demo/learning-records/0001-x.md"));
  w("courses/demo/learning-records/0007-y.md", "# y\n\n<!-- lesson: prima -->\n");
  assert.equal((await listCourses(root))[0].lessons[0].inProgress, false);
  assert.equal(res[1].orphans[0].slug, "solo");
  assert.equal(res[1].hasMission, false);
  fs.rmSync(root, { recursive: true, force: true });
});

test("listCourses auto-initializes courses/lezioni-singole/ with MISSION.md when data exists, leaves an empty root untouched, and excludes lezioni-singole from the structured list; listStandaloneLessons returns its lessons", async () => {
  const emptyRoot = fs.mkdtempSync(path.join(os.tmpdir(), "courses-empty-"));
  await listCourses(emptyRoot);
  assert.equal(hasData(emptyRoot), false, "listing an untouched data folder must not create courses/");
  assert.deepEqual(await listStandaloneLessons(emptyRoot), []);
  assert.equal(hasData(emptyRoot), false);
  fs.rmSync(emptyRoot, { recursive: true, force: true });

  const root = fs.mkdtempSync(path.join(os.tmpdir(), "courses-"));
  const w = (p: string, c: string) => {
    fs.mkdirSync(path.dirname(path.join(root, p)), { recursive: true });
    fs.writeFileSync(path.join(root, p), c);
  };
  w("curriculum.json", JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [] }] }));
  w("courses/demo/MISSION.md", "# M\n\n## Why\n\nImparare Vue.\n");

  const courses = await listCourses(root);
  const standaloneDir = path.join(root, "courses", STANDALONE_COURSE_SLUG);
  assert.equal(fs.existsSync(path.join(standaloneDir, "MISSION.md")), true);
  assert.deepEqual(courses.map((c) => c.slug), ["demo"], "lezioni-singole is not a structured course");

  // create-only: an existing MISSION.md (the learner's own words) is never overwritten
  w(`courses/${STANDALONE_COURSE_SLUG}/MISSION.md`, "# Mio contenitore\n\n## Why\n\nMarker.\n");
  await listCourses(root);
  assert.match(fs.readFileSync(path.join(standaloneDir, "MISSION.md"), "utf-8"), /Marker\./);

  w(
    `courses/${STANDALONE_COURSE_SLUG}/lessons/0001-trasformata-di-fourier.html`,
    lessonHtml(1, "trasformata-di-fourier", "La trasformata di Fourier scompone un segnale", 2),
  );
  w(`courses/${STANDALONE_COURSE_SLUG}/reference/trasformata-di-fourier.html`, "<html/>");
  w(`courses/${STANDALONE_COURSE_SLUG}/learning-records/0001-chiusa.md`, "# chiusa\n\n<!-- lesson: trasformata-di-fourier -->\n");
  w(
    `courses/${STANDALONE_COURSE_SLUG}/lessons/0002-entropia.html`,
    lessonHtml(2, "entropia", "L'entropia di Shannon misura la sorpresa media", 0, "graph TD\nN1[a] --> N2[b]"),
  );
  // even a chapter in the plan must not pull the container into the structured list
  w(
    "curriculum.json",
    JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [] }, { title: "Lezioni singole", slug: STANDALONE_COURSE_SLUG, lessons: [] }] }),
  );
  assert.deepEqual((await listCourses(root)).map((c) => c.slug), ["demo"]);

  const standalone = await listStandaloneLessons(root);
  assert.deepEqual(standalone.map((l) => l.slug), ["trasformata-di-fourier", "entropia"]);
  const [fourier, entropy] = standalone;
  assert.equal(fourier.lessonPath, `courses/${STANDALONE_COURSE_SLUG}/lessons/0001-trasformata-di-fourier.html`);
  assert.equal(fourier.title, "La trasformata di Fourier scompone un segnale");
  assert.equal(fourier.referencePath, `courses/${STANDALONE_COURSE_SLUG}/reference/trasformata-di-fourier.html`);
  assert.equal(fourier.inProgress, false);
  assert.equal(entropy.inProgress, true);
  fs.rmSync(root, { recursive: true, force: true });
});
