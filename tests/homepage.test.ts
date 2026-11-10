import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFileSync } from "node:child_process";
import {
  fetchAnkiStatus,
  fetchDeckStatus,
  generateHomepage,
  renderHomepage,
  scanLessons,
  type AnkiStatus,
  type LessonSummary
} from "../extensions/homepage.ts";
import { matchCurriculum, planChapter, readCurriculum } from "../extensions/chapter.ts";

/** Writes a lesson artifact the way the retrofit and `lesson append` both do: data island last. */
function writeLesson(
  baseDir: string,
  course: string,
  lessonNumber: number,
  slug: string,
  title: string,
  tiers: string[] = []
): void {
  const dir = path.join(baseDir, "courses", course, "lessons");
  fs.mkdirSync(dir, { recursive: true });
  const doc = {
    metadata: {
      lessonNumber,
      topic: course,
      title,
      slug,
      dateCreated: "2026-09-01T00:00:00.000Z",
      dateModified: "2026-09-01T00:00:00.000Z",
      version: "1.0.0"
    },
    nodes: tiers.map((tier, i) => ({
      nodeId: "n" + i,
      title: "Nodo " + i,
      tier,
      content: "..."
    }))
  };
  const filename = String(lessonNumber).padStart(4, "0") + "-" + slug + ".html";
  fs.writeFileSync(
    path.join(dir, filename),
    "<html><body><h1>" +
      title +
      '</h1>\n<script type="application/json" id="lesson-data">\n' +
      JSON.stringify(doc, null, 2) +
      "\n</script>\n</body></html>",
    "utf-8"
  );
}

describe("Homepage: status board over chapter -> lesson -> node", () => {
  let sandboxDir: string;

  beforeEach(() => {
    sandboxDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-home-test-"));
  });

  afterEach(() => {
    if (sandboxDir && fs.existsSync(sandboxDir)) {
      fs.rmSync(sandboxDir, { recursive: true, force: true });
    }
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 1: The plan — the only home for a lesson not yet taught
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 1: curriculum.json", () => {
    it("reads an empty curriculum when the file does not exist", () => {
      assert.deepEqual(readCurriculum({ baseDir: sandboxDir }).chapters, []);
    });

    it("slugifies chapter and lesson titles, and takes an explicit course directory", () => {
      const curriculum = planChapter({
        baseDir: sandboxDir,
        chapter: "App State & Caching",
        slug: "app-cache",
        lessons: [
          { title: "Architetture di cache", plannedNodes: 3 },
          { title: "Probabilità condizionata" }
        ]
      });

      const chapter = curriculum.chapters[0];
      assert.equal(chapter.slug, "app-cache", "explicit --slug wins over the title");
      assert.deepEqual(
        chapter.lessons.map((l) => l.slug),
        ["architetture-di-cache", "probabilita-condizionata"],
        "accents fold to ASCII"
      );
      assert.equal(chapter.lessons[0].plannedNodes, 3);
      assert.deepEqual(readCurriculum({ baseDir: sandboxDir }), curriculum);
    });

    it("accepts an explicit lesson slug for a file that predates the title-derived rule", () => {
      const curriculum = planChapter({
        baseDir: sandboxDir,
        chapter: "Docker",
        lessons: [{ title: "Image e container: la distinzione che regge tutto", slug: "image-e-container" }]
      });
      assert.equal(curriculum.chapters[0].lessons[0].slug, "image-e-container");
    });

    it("replaces the lessons of a re-planned chapter and leaves other chapters intact", () => {
      planChapter({ baseDir: sandboxDir, chapter: "Docker", lessons: [{ title: "Image" }] });
      planChapter({ baseDir: sandboxDir, chapter: "Nginx", lessons: [{ title: "Proxy" }] });
      const curriculum = planChapter({
        baseDir: sandboxDir,
        chapter: "Docker",
        lessons: [{ title: "Image" }, { title: "Compose" }]
      });

      assert.deepEqual(curriculum.chapters.map((c) => c.slug), ["docker", "nginx"]);
      assert.deepEqual(curriculum.chapters[0].lessons.map((l) => l.slug), ["image", "compose"]);
      assert.deepEqual(curriculum.chapters[1].lessons.map((l) => l.slug), ["proxy"]);
    });

    it("refuses two planned lessons that slug alike", () => {
      assert.throws(
        () => planChapter({
          baseDir: sandboxDir,
          chapter: "Docker",
          lessons: [{ title: "Image e container" }, { title: "Image, e container!" }]
        }),
        /share the slug "image-e-container"/
      );
    });

    it("refuses a curriculum whose chapters are not an array, instead of erasing it", () => {
      fs.writeFileSync(
        path.join(sandboxDir, "curriculum.json"),
        JSON.stringify({ chapters: { docker: {} } })
      );
      assert.throws(
        () => planChapter({ baseDir: sandboxDir, chapter: "New", lessons: [{ title: "A" }] }),
        /"chapters" must be an array/
      );
      // The point of throwing: the previous plan is still on disk.
      assert.match(fs.readFileSync(path.join(sandboxDir, "curriculum.json"), "utf-8"), /docker/);
    });

    it("reads a chapter with no lessons key as an empty chapter", () => {
      fs.writeFileSync(
        path.join(sandboxDir, "curriculum.json"),
        JSON.stringify({ chapters: [{ title: "X", slug: "x" }] })
      );
      const match = matchCurriculum(readCurriculum({ baseDir: sandboxDir }), []);
      assert.equal(match.chapters[0].lessons.length, 0);
      assert.equal(match.chapters[0].taught, 0);
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 2: "Taught" is derived from the course directories, never stored
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 2: scanning taught lessons", () => {
    it("walks every course, keeping per-course numbering and node tiers", () => {
      writeLesson(sandboxDir, "docker", 1, "image-e-container", "Image e container", [
        "unconditional-truth",
        "derivation"
      ]);
      writeLesson(sandboxDir, "app-cache", 1, "cache-in-memory", "Cache in memory");

      const lessons = scanLessons({ baseDir: sandboxDir });
      assert.equal(lessons.length, 2);

      const docker = lessons.find((l) => l.course === "docker")!;
      assert.equal(docker.lessonNumber, 1);
      assert.equal(docker.slug, "image-e-container");
      assert.equal(docker.nodeCount, 2);
      assert.deepEqual(docker.tiers, ["unconditional-truth", "derivation"]);

      const cache = lessons.find((l) => l.course === "app-cache")!;
      assert.equal(cache.lessonNumber, 1, "numbering restarts per course");
      assert.equal(cache.nodeCount, 0, "a hand-authored lesson has no captured nodes");
    });

    it("skips an HTML file with no data island instead of guessing at it", () => {
      const dir = path.join(sandboxDir, "courses", "docker", "lessons");
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, "0001-prosa.html"), "<html><body><h1>Prosa</h1></body></html>");
      assert.deepEqual(scanLessons({ baseDir: sandboxDir }), []);
    });

    it("reports a corrupted island and keeps scanning the other lessons", () => {
      writeLesson(sandboxDir, "docker", 1, "image-e-container", "Image e container", ["axiom"]);
      const dir = path.join(sandboxDir, "courses", "docker", "lessons");
      fs.writeFileSync(
        path.join(dir, "0002-rotta.html"),
        '<html><script type="application/json" id="lesson-data">{ "metadata": ' + "</scr" + "ipt></html>"
      );

      const warnings: string[] = [];
      const realWarn = console.warn;
      console.warn = (msg: string) => warnings.push(msg);
      try {
        const lessons = scanLessons({ baseDir: sandboxDir });
        assert.equal(lessons.length, 1, "the healthy lesson still shows up");
        assert.equal(lessons[0].slug, "image-e-container");
      } finally {
        console.warn = realWarn;
      }
      assert.equal(warnings.length, 1);
      assert.match(warnings[0], /0002-rotta.html/);
    });

    it("keeps a slug-shadowed lesson visible as an orphan", () => {
      writeLesson(sandboxDir, "docker", 1, "compose", "Compose, prima versione");
      writeLesson(sandboxDir, "docker", 2, "compose", "Compose, seconda versione");
      const curriculum = planChapter({
        baseDir: sandboxDir,
        chapter: "Docker",
        lessons: [{ title: "Compose", slug: "compose" }]
      });

      const match = matchCurriculum(curriculum, scanLessons({ baseDir: sandboxDir }));
      assert.equal(match.chapters[0].taught, 1);
      // The one the plan did not claim must still surface somewhere, not vanish from the board.
      assert.equal(match.orphans.length, 1);
      assert.equal(match.orphans[0].lessonNumber, 2);
    });

    it("returns an empty list when no course exists yet", () => {
      assert.deepEqual(scanLessons({ baseDir: sandboxDir }), []);
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 3: The join — plan x course directories x Anki
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 3: the join", () => {
    const lesson = (over: Partial<LessonSummary> = {}): LessonSummary => ({
      course: "docker",
      lessonNumber: 1,
      slug: "image-e-container",
      title: "Image e container",
      filename: "0001-image-e-container.html",
      nodeCount: 2,
      tiers: ["unconditional-truth", "derivation"],
      ...over
    });

    it("links a taught lesson to its course directory and never links a plan entry", () => {
      const html = renderHomepage({
        match: matchCurriculum(
          {
            chapters: [
              {
                title: "Docker",
                slug: "docker",
                lessons: [
                  { title: "Image e container", slug: "image-e-container" },
                  { title: "Healthcheck", slug: "healthcheck", plannedNodes: 4 }
                ]
              }
            ]
          },
          [lesson()]
        ),
        anki: null
      });

      assert.match(html, /href="courses\/docker\/lessons\/0001-image-e-container\.html"/);
      assert.match(html, /1\/2/);
      assert.match(html, /Healthcheck/);
      assert.doesNotMatch(html, /href="courses\/[^"]*healthcheck/);
      assert.match(html, /4 nodi previsti/);
      assert.match(html, /href="courses\/docker\/"/, "chapter links to its course directory");
    });

    it("does not let one course claim another's plan entry with the same slug", () => {
      const match = matchCurriculum(
        {
          chapters: [
            { title: "Docker", slug: "docker", lessons: [{ title: "Basi", slug: "basi" }] },
            { title: "Nginx", slug: "nginx", lessons: [{ title: "Basi", slug: "basi" }] }
          ]
        },
        [lesson({ course: "docker", slug: "basi", filename: "0001-basi.html" })]
      );

      assert.equal(match.chapters[0].taught, 1);
      assert.equal(match.chapters[1].taught, 0, "nginx/basi is still to do");
      assert.equal(match.orphans.length, 0);
    });

    it("puts a taught lesson no plan entry claims under its own section", () => {
      const match = matchCurriculum({ chapters: [] }, [lesson({ slug: "fuori-piano" })]);
      assert.equal(match.orphans.length, 1);
      const html = renderHomepage({ match, anki: null });
      assert.match(html, /Fuori dal piano/);
      assert.match(html, /href="courses\/docker\/lessons\/0001-image-e-container\.html"/);
    });

    it("titles the page Proxi", () => {
      const match = matchCurriculum({ chapters: [] }, [lesson({ slug: "fuori-piano" })]);
      const html = renderHomepage({ match, anki: null });
      assert.match(html, /<title>Proxi<\/title>/);
      assert.match(html, /<h1>Proxi<\/h1>/);
    });

    it("reads a hand-authored lesson as prose, not as zero nodes", () => {
      const html = renderHomepage({
        match: matchCurriculum({ chapters: [] }, [lesson({ nodeCount: 0, tiers: [] })]),
        anki: null
      });
      assert.match(html, />prosa</);
      assert.doesNotMatch(html, />0 nodi</, "the row reads prosa, not a node count of zero");
    });

    it("renders Anki counts when present and omits the strip entirely when absent", () => {
      const anki: AnkiStatus = {
        due: 23,
        newCards: 12,
        learning: 5,
        total: 148,
        deckCount: 3,
        generatedAt: "2026-09-01T09:14:00.000Z"
      };
      const match = matchCurriculum({ chapters: [] }, []);
      assert.match(renderHomepage({ match, anki }), /da ripassare/);
      assert.match(renderHomepage({ match, anki }), />23</);
      assert.doesNotMatch(renderHomepage({ match, anki: null }), /da ripassare/);
    });

    it("escapes titles coming from the plan and from lesson metadata", () => {
      const html = renderHomepage({
        match: matchCurriculum(
          {
            chapters: [
              { title: "<script>alert(1)</script>", slug: "x", lessons: [{ title: "a & b", slug: "a-b" }] }
            ]
          },
          []
        ),
        anki: null
      });
      assert.doesNotMatch(html, /<script>alert\(1\)<\/script>/);
      assert.match(html, /a &amp; b/);
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 4: Generation — and the explicit opt-out from Anki
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 4: generation", () => {
    it("writes index.html without contacting AnkiConnect when noAnki is set", async () => {
      writeLesson(sandboxDir, "docker", 1, "image-e-container", "Image e container", ["axiom"]);
      planChapter({
        baseDir: sandboxDir,
        chapter: "Docker",
        lessons: [{ title: "Image e container" }]
      });

      const res = await generateHomepage({ baseDir: sandboxDir, noAnki: true });

      assert.equal(res.filePath, path.join(sandboxDir, "index.html"));
      assert.equal(res.ankiOnline, false);
      assert.equal(res.chapterCount, 1);
      assert.equal(res.lessonCount, 1);
      assert.equal(res.orphanCount, 0);
      assert.match(
        fs.readFileSync(res.filePath, "utf-8"),
        /href="courses\/docker\/lessons\/0001-image-e-container\.html"/
      );
    });

    it("refuses to generate when AnkiConnect is unreachable and --no-anki was not passed", async () => {
      await assert.rejects(
        () => generateHomepage({ baseDir: sandboxDir, ankiUrl: "http://127.0.0.1:9", timeoutMs: 300 }),
        /--no-anki/
      );
      assert.equal(fs.existsSync(path.join(sandboxDir, "index.html")), false);
    });

    it("is driveable from the CLI", () => {
      const script = path.resolve("extensions/homepage.ts");
      const curriculum = path.join(sandboxDir, "curriculum.json");
      const out = path.join(sandboxDir, "index.html");
      writeLesson(sandboxDir, "docker", 1, "image-e-container", "Image e container", ["axiom"]);

      execFileSync(
        process.execPath,
        ["--import", "tsx", script, "plan",
          "--chapter", "Docker",
          "--lessons-json", '[{"title":"Image e container"},{"title":"Healthcheck","plannedNodes":4}]',
          "--curriculum", curriculum],
        { stdio: "pipe", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
      );
      execFileSync(
        process.execPath,
        ["--import", "tsx", script, "--no-anki",
          "--curriculum", curriculum,
          "--dir", path.join(sandboxDir, "courses"),
          "--out", out],
        { stdio: "pipe", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
      );

      const html = fs.readFileSync(out, "utf-8");
      assert.match(html, /Healthcheck/);
      assert.match(html, /4 nodi previsti/);
      assert.match(html, /1\/2/);
    });
  });
});

describe("fetchAnkiStatus", () => {
  it("counts due/new/learning from the root deck only (Anki already rolls subdecks into it)", async () => {
    const http = await import("node:http");
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        const result =
          action === "deckNamesAndIds"
            ? { Altro: 9, "Proxi": 1, "Proxi::Vue": 2, "Proxi::Vue::Proxi": 3 }
            : {
                "3": { deck_id: 3, name: "Proxi", new_count: 0, learn_count: 0, review_count: 0, total_in_deck: 1 },
                "1": { deck_id: 1, name: "Proxi", new_count: 2, learn_count: 1, review_count: 3, total_in_deck: 0 },
                "2": { deck_id: 2, name: "Vue", new_count: 2, learn_count: 1, review_count: 3, total_in_deck: 7 },
              };
        res.end(JSON.stringify({ result, error: null }));
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    const emptyDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-anki-test-"));
    try {
      const { status } = await fetchAnkiStatus({ ankiUrl: `http://127.0.0.1:${port}`, baseDir: emptyDir });
      assert.deepEqual(
        { ...status, generatedAt: "" },
        { due: 3, newCards: 2, learning: 1, total: 8, deckCount: 3, generatedAt: "", byCourse: {} }
      );
    } finally {
      server.close();
      fs.rmSync(emptyDir, { recursive: true, force: true });
    }
  });

  it("maps one getDeckStats batch to byCourse, zero-filling courses with no deck", async () => {
    const http = await import("node:http");
    const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-anki-test-"));
    planChapter({ baseDir, chapter: "Vue", lessons: [{ title: "Il template" }] });
    planChapter({ baseDir, chapter: "Rust", lessons: [{ title: "Ownership" }] });

    let getDeckStatsCalls = 0;
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { "Proxi::Vue": 2 }, error: null }));
        } else if (action === "getDeckStats") {
          getDeckStatsCalls++;
          res.end(JSON.stringify({
            result: {
              "2": { deck_id: 2, name: "Vue", new_count: 2, learn_count: 1, review_count: 3, total_in_deck: 9 },
            },
            error: null,
          }));
        } else {
          res.end(JSON.stringify({ result: null, error: "unknown action" }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status } = await fetchAnkiStatus({ ankiUrl: `http://127.0.0.1:${port}`, baseDir });
      assert.deepEqual(status?.byCourse, {
        vue: { due: 3, learning: 1, newCards: 2 },
        rust: { due: 0, learning: 0, newCards: 0 },
      });
      assert.equal(getDeckStatsCalls, 1, "one batch for every course, never one call per course");
    } finally {
      server.close();
      fs.rmSync(baseDir, { recursive: true, force: true });
    }
  });

  it("zero-fills every planned course when Anki has no Proxi deck at all", async () => {
    const http = await import("node:http");
    const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-anki-test-"));
    planChapter({ baseDir, chapter: "Vue", lessons: [{ title: "Il template" }] });
    planChapter({ baseDir, chapter: "Rust", lessons: [{ title: "Ownership" }] });

    let getDeckStatsCalls = 0;
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { Altro: 9 }, error: null }));
        } else {
          getDeckStatsCalls++;
          res.end(JSON.stringify({ result: {}, error: null }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status } = await fetchAnkiStatus({ ankiUrl: `http://127.0.0.1:${port}`, baseDir });
      assert.equal(status?.deckCount, 0);
      // A course whose deck does not exist yet still gets an entry, so the sidebar can hide its badge.
      assert.deepEqual(status?.byCourse, {
        vue: { due: 0, learning: 0, newCards: 0 },
        rust: { due: 0, learning: 0, newCards: 0 },
      });
      assert.equal(getDeckStatsCalls, 0, "no Proxi deck: nothing to ask getDeckStats about");
    } finally {
      server.close();
      fs.rmSync(baseDir, { recursive: true, force: true });
    }
  });

  it("ignores decks under any other root", async () => {
    const http = await import("node:http");
    const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-anki-test-"));
    let statsDecks: string[] | undefined;
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action, params } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { Proxi: 1, "Proxi::Demo": 2, "Altro::Demo": 3 }, error: null }));
        } else if (action === "getDeckStats") {
          statsDecks = params.decks;
          res.end(JSON.stringify({ result: {}, error: null }));
        } else {
          res.end(JSON.stringify({ result: null, error: "unknown action" }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      await fetchAnkiStatus({ ankiUrl: `http://127.0.0.1:${port}`, baseDir });
      assert.deepEqual(statsDecks, ["Proxi", "Proxi::Demo"]);
    } finally {
      server.close();
      fs.rmSync(baseDir, { recursive: true, force: true });
    }
  });

  it("includes a course that exists only on disk, not in the plan", async () => {
    const http = await import("node:http");
    const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-anki-test-"));
    planChapter({ baseDir, chapter: "Vue", lessons: [{ title: "Il template" }] });
    writeLesson(baseDir, "go", 1, "goroutine", "Goroutine");

    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { "Proxi::Vue": 2, "Proxi::go": 3 }, error: null }));
        } else {
          res.end(JSON.stringify({
            result: {
              "2": { deck_id: 2, name: "Vue", new_count: 2, learn_count: 1, review_count: 3, total_in_deck: 9 },
              "3": { deck_id: 3, name: "go", new_count: 7, learn_count: 0, review_count: 0, total_in_deck: 7 },
            },
            error: null,
          }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status } = await fetchAnkiStatus({ ankiUrl: `http://127.0.0.1:${port}`, baseDir });
      assert.deepEqual(status?.byCourse, {
        vue: { due: 3, learning: 1, newCards: 2 },
        // No curriculum chapter: the deck falls back to the folder slug, and it is not dropped from the map.
        go: { due: 0, learning: 0, newCards: 7 },
      });
    } finally {
      server.close();
      fs.rmSync(baseDir, { recursive: true, force: true });
    }
  });

  it("still badges the courses on disk when the plan is unreadable", async () => {
    const http = await import("node:http");
    const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-anki-test-"));
    // A broken plan must cost the badges of planned-only courses, not the whole status board.
    fs.writeFileSync(path.join(baseDir, "curriculum.json"), "{ not json", "utf-8");
    writeLesson(baseDir, "go", 1, "goroutine", "Goroutine");

    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { "Proxi::go": 3 }, error: null }));
        } else {
          res.end(JSON.stringify({
            result: {
              "3": { deck_id: 3, name: "go", new_count: 4, learn_count: 1, review_count: 2, total_in_deck: 7 },
            },
            error: null,
          }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status } = await fetchAnkiStatus({ ankiUrl: `http://127.0.0.1:${port}`, baseDir });
      assert.deepEqual(status?.byCourse, { go: { due: 2, learning: 1, newCards: 4 } });
    } finally {
      server.close();
      fs.rmSync(baseDir, { recursive: true, force: true });
    }
  });

  it("reads courses from an explicit coursesDir", async () => {
    const http = await import("node:http");
    const baseDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-anki-test-"));
    planChapter({ baseDir, chapter: "Vue", lessons: [{ title: "Il template" }] });
    // The taught course lives outside <baseDir>/courses: only the coursesDir option can find it.
    writeLesson(path.join(baseDir, "alt"), "go", 1, "goroutine", "Goroutine");

    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { "Proxi::Vue": 2, "Proxi::go": 3 }, error: null }));
        } else {
          res.end(JSON.stringify({
            result: {
              "2": { deck_id: 2, name: "Vue", new_count: 2, learn_count: 1, review_count: 3, total_in_deck: 9 },
              "3": { deck_id: 3, name: "go", new_count: 7, learn_count: 0, review_count: 0, total_in_deck: 7 },
            },
            error: null,
          }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status } = await fetchAnkiStatus({
        ankiUrl: `http://127.0.0.1:${port}`,
        baseDir,
        coursesDir: path.join(baseDir, "alt", "courses"),
      });
      assert.deepEqual(status?.byCourse, {
        vue: { due: 3, learning: 1, newCards: 2 },
        go: { due: 0, learning: 0, newCards: 7 },
      });
    } finally {
      server.close();
      fs.rmSync(baseDir, { recursive: true, force: true });
    }
  });
});

describe("fetchDeckStatus", () => {
  it("returns counts and exists true for an existing deck", async () => {
    const http = await import("node:http");
    let getDeckStatsCalls = 0;
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { "Proxi::Vue": 42 }, error: null }));
        } else if (action === "getDeckStats") {
          getDeckStatsCalls++;
          res.end(JSON.stringify({
            result: {
              "42": { deck_id: 42, name: "Vue", new_count: 5, learn_count: 2, review_count: 8, total_in_deck: 20 },
            },
            error: null,
          }));
        } else {
          res.end(JSON.stringify({ result: null, error: "unknown action" }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status, error } = await fetchDeckStatus("Proxi::Vue", { ankiUrl: `http://127.0.0.1:${port}` });
      assert.equal(error, undefined);
      assert.deepEqual(status, { due: 8, newCards: 5, learning: 2, total: 20, exists: true });
      assert.equal(getDeckStatsCalls, 1);
    } finally {
      server.close();
    }
  });

  it("returns zeros and exists false when deck is missing, and getDeckStats is never called", async () => {
    const http = await import("node:http");
    let getDeckStatsCalls = 0;
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { "Proxi::Other": 1 }, error: null }));
        } else if (action === "getDeckStats") {
          getDeckStatsCalls++;
          res.end(JSON.stringify({ result: {}, error: null }));
        } else {
          res.end(JSON.stringify({ result: null, error: "unknown action" }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status, error } = await fetchDeckStatus("Proxi::Vue", { ankiUrl: `http://127.0.0.1:${port}` });
      assert.equal(error, undefined);
      assert.deepEqual(status, { due: 0, newCards: 0, learning: 0, total: 0, exists: false });
      assert.equal(getDeckStatsCalls, 0);
    } finally {
      server.close();
    }
  });

  it("returns error when port is closed", async () => {
    const { status, error } = await fetchDeckStatus("Proxi::Vue", { ankiUrl: "http://127.0.0.1:1", timeoutMs: 200 });
    assert.equal(status, undefined);
    assert.ok(typeof error === "string" && error.length > 0);
  });
});
