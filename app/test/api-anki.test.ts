// GET /api/anki, POST /api/anki/review and /api/anki/import over plain HTTP, identical against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { MockAnki } from "../../tests/mock-anki.ts";
import { startServer, type TestServer } from "./harness.ts";

const FIXTURE = path.join(import.meta.dirname, "fixtures", "anki", "import-progress.jsonl");
const SEED_LINE =
  '{"at":"2023-11-14T00:00:00.000Z","sessionId":"s1","course":"demo","lessonSlug":"t","nodeRefs":["demo/t#alpha"],"kind":"check","correct":true}\n';
/** Both servers may report a refused or timed-out AnkiConnect. */
const OFFLINE_ERRORS = ["fetch failed", "AnkiConnect request timed out"];

function lessonDoc(topic: string, lessonNumber: number, title: string, slug: string, nodeIds: string[]) {
  return {
    metadata: { lessonNumber, topic, title, slug, dateCreated: "", dateModified: "", version: "1" },
    nodes: nodeIds.map((nodeId) => ({ nodeId, title: nodeId, tier: "derivation", content: "x" })),
  };
}

/** The data root shared by every case: a planned course, a planned empty one and one only on disk. */
function seed(dataRoot: string): void {
  const write = (rel: string, text: string) => {
    const abs = path.join(dataRoot, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  };
  write(
    "curriculum.json",
    JSON.stringify({
      chapters: [
        { title: "Demo", slug: "demo", lessons: [{ title: "Lesson T", slug: "t", plannedNodes: 2 }, { title: "Lesson U", slug: "u", plannedNodes: 1 }] },
        { title: "Vuoto", slug: "vuoto", lessons: [] },
      ],
    }),
  );
  const island = (doc: unknown) => `<script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script>`;
  write("courses/demo/lessons/0001-t.html", island(lessonDoc("Demo", 1, "Lesson T", "t", ["alpha", "dup"])));
  write("courses/demo/lessons/0002-u.html", island(lessonDoc("Demo", 2, "Lesson U", "u", ["dup"])));
  write("courses/orfano/lessons/0001-o.html", island(lessonDoc("Orfano", 1, "O", "o", ["x"])));
  write("courses/demo/progress.jsonl", SEED_LINE);
}

/** A fresh repo and data root, one server, cleaned up even when the assertions throw. */
async function withServer(ankiUrl: string, fn: (server: TestServer, dataRoot: string) => Promise<void>): Promise<void> {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "api-anki-repo-"));
  const dataRoot = fs.mkdtempSync(path.join(os.tmpdir(), "api-anki-data-"));
  seed(dataRoot);
  let server: TestServer | undefined;
  try {
    server = await startServer({ repoRoot, dataRoot, fakes: { anki: ankiUrl } });
    await fn(server, dataRoot);
  } finally {
    await server?.close();
    fs.rmSync(repoRoot, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(dataRoot, { recursive: true, force: true, maxRetries: 3 });
  }
}

/** One open Anki the status and review cases share: Proxi, its two children and a Default deck. */
function openAnki(): MockAnki {
  const mock = new MockAnki();
  mock.deckNamesAndIds = { Proxi: 1, "Proxi::Demo": 2, "Proxi::Altro": 3, Default: 4 };
  mock.deckStats = {
    "1": { deck_id: 1, name: "Proxi", new_count: 5, learn_count: 2, review_count: 4, total_in_deck: 0 },
    "2": { deck_id: 2, name: "Demo", new_count: 5, learn_count: 2, review_count: 4, total_in_deck: 10 },
    "3": { deck_id: 3, name: "Altro", new_count: 1, learn_count: 0, review_count: 0, total_in_deck: 3 },
  };
  return mock;
}

/** The six cards of the Node golden: two skipped, four reviews written. */
function importAnki(): MockAnki {
  const mock = new MockAnki();
  mock.cards = [
    { cardId: 1001, note: 2001, deckName: "Proxi::Demo", tags: ["node::alpha"], interval: 10, lapses: 0, reviews: [{ id: 1700000002000, ease: 1 }, { id: 1700000001000, ease: 3 }] },
    { cardId: 1002, note: 2002, deckName: "Proxi::Demo", tags: ["NODE::dup", "lesson::u"], interval: -600, lapses: 2, reviews: [{ id: 1700000003000, ease: 4 }, { id: 1700000004000, ease: 0 }] },
    { cardId: 1003, note: 2003, deckName: "Proxi::Demo", tags: ["node::dup"], interval: 1, lapses: 0, reviews: [{ id: 1700000005000, ease: 3 }] },
    { cardId: 1004, note: 2004, deckName: "Proxi::Demo", tags: [], interval: 1, lapses: 0, reviews: [{ id: 1700000006000, ease: 3 }] },
    { cardId: 1005, note: 2005, deckName: "Proxi::Demo::Sub", tags: ["node::alpha", "lesson::t"], interval: 1, lapses: 0, reviews: [{ id: 1700000000500, ease: 2 }] },
    { cardId: 1006, note: 2006, deckName: "Altro::Demo", tags: ["node::alpha"], interval: 1, lapses: 0, reviews: [{ id: 1700000007000, ease: 3 }] },
  ];
  return mock;
}

function post(server: TestServer, p: string, body: unknown): Promise<Response> {
  return fetch(server.url + p, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

function actions(mock: MockAnki) {
  return mock.requests.map((r) => ({ action: r.action, version: r.version, params: r.params }));
}

function progressBytes(dataRoot: string): Buffer {
  return fs.readFileSync(path.join(dataRoot, "courses", "demo", "progress.jsonl"));
}

async function withMock(mock: MockAnki, fn: (mock: MockAnki, url: string) => Promise<void>): Promise<void> {
  const url = await mock.start();
  try {
    await fn(mock, url);
  } finally {
    await mock.stop();
  }
}

describe("GET /api/anki", () => {
  it("sums the Proxi deck and badges every course", async () => {
    await withMock(openAnki(), async (mock, url) => {
      await withServer(url, async (server) => {
        const res = await fetch(`${server.url}/api/anki`);
        assert.equal(res.status, 200);
        assert.equal(
          await res.text(),
          '{"online":true,"due":4,"newCards":5,"learning":2,"total":13,"byCourse":{"demo":{"due":4,"learning":2,"newCards":5},"vuoto":{"due":0,"learning":0,"newCards":0},"orfano":{"due":0,"learning":0,"newCards":0}}}',
        );
        assert.deepEqual(actions(mock), [
          { action: "deckNamesAndIds", version: 6, params: undefined },
          { action: "getDeckStats", version: 6, params: { decks: ["Proxi", "Proxi::Demo", "Proxi::Altro"] } },
        ]);
      });
    });
  });

  it("answers one course's deck and rejects an invalid course", async () => {
    await withMock(openAnki(), async (_mock, url) => {
      await withServer(url, async (server) => {
        const demo = await fetch(`${server.url}/api/anki?course=demo`);
        assert.equal(demo.status, 200);
        assert.equal(await demo.text(), '{"online":true,"due":4,"newCards":5,"learning":2,"total":10,"deck":"Proxi::Demo","deckExists":true}');

        const vuoto = await fetch(`${server.url}/api/anki?course=vuoto`);
        assert.equal(vuoto.status, 200);
        assert.equal(await vuoto.text(), '{"online":true,"due":0,"newCards":0,"learning":0,"total":0,"deck":"Proxi::Vuoto","deckExists":false}');

        const bad = await fetch(`${server.url}/api/anki?course=Bad`);
        assert.equal(bad.status, 400);
        assert.equal(await bad.text(), '{"error":"Corso non valido"}');
      });
    });
  });

  it("answers zeros and asks only for deck names when there is no Proxi deck", async () => {
    const mock = new MockAnki();
    mock.deckNamesAndIds = { Default: 1 };
    mock.deckStats = null;
    await withMock(mock, async (mock, url) => {
      await withServer(url, async (server) => {
        const res = await fetch(`${server.url}/api/anki`);
        assert.equal(res.status, 200);
        assert.equal(
          await res.text(),
          '{"online":true,"due":0,"newCards":0,"learning":0,"total":0,"byCourse":{"demo":{"due":0,"learning":0,"newCards":0},"vuoto":{"due":0,"learning":0,"newCards":0},"orfano":{"due":0,"learning":0,"newCards":0}}}',
        );
        assert.deepEqual(actions(mock), [{ action: "deckNamesAndIds", version: 6, params: undefined }]);
      });
    });
  });

  it("answers online:false for status, import and review when AnkiConnect is closed", async () => {
    await withServer("http://127.0.0.1:9", async (server, dataRoot) => {
      const all = await fetch(`${server.url}/api/anki`);
      assert.equal(all.status, 200);
      const allBody = (await all.json()) as Record<string, unknown>;
      assert.deepEqual(Object.keys(allBody), ["online", "error"]);
      assert.equal(allBody.online, false);
      assert.ok(OFFLINE_ERRORS.includes(allBody.error as string), String(allBody.error));

      const one = await fetch(`${server.url}/api/anki?course=demo`);
      assert.equal(one.status, 200);
      const oneBody = (await one.json()) as Record<string, unknown>;
      assert.deepEqual(Object.keys(oneBody), ["online", "error", "deck"]);
      assert.equal(oneBody.online, false);
      assert.equal(oneBody.deck, "Proxi::Demo");
      assert.ok(OFFLINE_ERRORS.includes(oneBody.error as string), String(oneBody.error));

      const imported = await post(server, "/api/anki/import", { course: "demo" });
      assert.equal(imported.status, 200);
      const importBody = (await imported.json()) as Record<string, unknown>;
      assert.deepEqual(Object.keys(importBody), ["online", "imported", "skipped", "error"]);
      assert.deepEqual(
        { online: importBody.online, imported: importBody.imported, skipped: importBody.skipped },
        { online: false, imported: 0, skipped: 0 },
      );
      assert.ok(OFFLINE_ERRORS.includes(importBody.error as string), String(importBody.error));
      assert.deepEqual(progressBytes(dataRoot), Buffer.from(SEED_LINE));

      const review = await post(server, "/api/anki/review", { course: "demo" });
      assert.equal(review.status, 502);
      assert.equal(typeof ((await review.json()) as { error: unknown }).error, "string");
    });
  });
});

describe("POST /api/anki/review", () => {
  it("opens the course deck in Anki", async () => {
    await withMock(openAnki(), async (mock, url) => {
      await withServer(url, async (server) => {
        const res = await post(server, "/api/anki/review", { course: "demo" });
        assert.equal(res.status, 200);
        assert.equal(await res.text(), '{"ok":true}');
        assert.deepEqual(actions(mock), [
          { action: "deckNamesAndIds", version: 6, params: undefined },
          { action: "getDeckStats", version: 6, params: { decks: ["Proxi::Demo"] } },
          { action: "guiDeckReview", version: 6, params: { name: "Proxi::Demo" } },
        ]);
      });
    });
  });

  it("answers 404 for a course without a deck", async () => {
    await withMock(openAnki(), async (mock, url) => {
      await withServer(url, async (server) => {
        const res = await post(server, "/api/anki/review", { course: "vuoto" });
        assert.equal(res.status, 404);
        assert.equal(await res.text(), '{"error":"Nessun mazzo Anki per questo corso"}');
        assert.ok(!mock.requests.some((r) => r.action === "guiDeckReview"));
      });
    });
  });

  it("maps an Anki error to 502", async () => {
    const mock = openAnki();
    mock.guiDeckReviewError = "boom";
    await withMock(mock, async (_mock, url) => {
      await withServer(url, async (server) => {
        const res = await post(server, "/api/anki/review", { course: "demo" });
        assert.equal(res.status, 502);
        assert.equal(await res.text(), '{"error":"boom"}');
      });
    });
  });

  it("guards the body and the client header", async () => {
    await withMock(openAnki(), async (_mock, url) => {
      await withServer(url, async (server) => {
        const bad = await post(server, "/api/anki/review", { course: "Bad" });
        assert.equal(bad.status, 400);
        assert.equal(typeof ((await bad.json()) as { error: unknown }).error, "string");

        const broken = await post(server, "/api/anki/review", "{");
        assert.equal(broken.status, 400);
        assert.equal(await broken.text(), '{"error":"Invalid JSON body"}');

        const noHeader = await fetch(`${server.url}/api/anki/review`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ course: "demo" }),
        });
        assert.equal(noHeader.status, 403);
      });
    });
  });
});

describe("POST /api/anki/import", () => {
  it("appends the reviews in reviewId order, byte-identical to the Node golden", async () => {
    await withMock(importAnki(), async (mock, url) => {
      await withServer(url, async (server, dataRoot) => {
        const res = await post(server, "/api/anki/import", { course: "demo" });
        assert.equal(res.status, 200);
        assert.equal(await res.text(), '{"online":true,"imported":4,"skipped":2}');
        assert.deepEqual(progressBytes(dataRoot), fs.readFileSync(FIXTURE));
        assert.equal(mock.requests.find((r) => r.action === "findCards")?.params?.query, 'deck:"Proxi::Demo"');
      });
    });
  });

  it("adds no line on a second import", async () => {
    await withMock(importAnki(), async (_mock, url) => {
      await withServer(url, async (server, dataRoot) => {
        const first = await post(server, "/api/anki/import", { course: "demo" });
        assert.equal(first.status, 200);
        assert.equal(await first.text(), '{"online":true,"imported":4,"skipped":2}');
        const before = progressBytes(dataRoot);

        const second = await post(server, "/api/anki/import", { course: "demo" });
        assert.equal(second.status, 200);
        assert.equal(await second.text(), '{"online":true,"imported":0,"skipped":2}');
        assert.deepEqual(progressBytes(dataRoot), before);
      });
    });
  });

  it("writes each review once when two imports overlap", async () => {
    await withMock(importAnki(), async (_mock, url) => {
      await withServer(url, async (server, dataRoot) => {
        const [a, b] = await Promise.all([post(server, "/api/anki/import", { course: "demo" }), post(server, "/api/anki/import", { course: "demo" })]);
        assert.equal(a.status, 200);
        assert.equal(b.status, 200);
        for (const res of [a, b]) {
          const body = (await res.json()) as { online: boolean; imported: number; skipped: number };
          assert.equal(body.online, true);
          assert.equal(body.skipped, 2);
          assert.ok(body.imported === 0 || body.imported === 4, `imported ${body.imported}`);
        }
        assert.deepEqual(progressBytes(dataRoot), fs.readFileSync(FIXTURE));
      });
    });
  });
});
