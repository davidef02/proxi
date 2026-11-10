import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { describe, it } from "node:test";
import { MockAnki } from "../../tests/mock-anki.ts";
import { importAnkiReviews } from "../server/anki-import.ts";
import { buildKnowledgeMap, readProgress } from "../server/knowledge.ts";

function makeTestRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "anki-import-test-"));
  fs.writeFileSync(
    path.join(root, "curriculum.json"),
    JSON.stringify({
      chapters: [
        {
          title: "Demo",
          slug: "demo",
          lessons: [
            { title: "Lesson T", slug: "t", plannedNodes: 5 },
            { title: "Lesson U", slug: "u", plannedNodes: 1 },
          ],
        },
      ],
    }),
  );
  const lessonsDir = path.join(root, "courses", "demo", "lessons");
  fs.mkdirSync(lessonsDir, { recursive: true });
  fs.writeFileSync(path.join(root, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(root, "courses", "demo", "NOTES.md"), "# Notes\n");

  const docT = {
    metadata: {
      lessonNumber: 1,
      topic: "Demo",
      title: "Lesson T",
      slug: "t",
      dateCreated: "",
      dateModified: "",
      version: "1",
    },
    nodes: [
      { nodeId: "alpha", title: "A", tier: "axiom", content: "x" },
      { nodeId: "beta", title: "B", tier: "derivation", content: "x" },
      { nodeId: "gamma", title: "C", tier: "derivation", content: "x" },
      { nodeId: "delta", title: "D", tier: "derivation", content: "x" },
      { nodeId: "dup", title: "Dup", tier: "derivation", content: "x" },
    ],
  };
  fs.writeFileSync(
    path.join(lessonsDir, "0001-t.html"),
    `<script type="application/json" id="lesson-data">${JSON.stringify(docT)}</script>`,
  );

  const docU = {
    metadata: {
      lessonNumber: 2,
      topic: "Demo",
      title: "Lesson U",
      slug: "u",
      dateCreated: "",
      dateModified: "",
      version: "1",
    },
    nodes: [{ nodeId: "dup", title: "Dup", tier: "derivation", content: "x" }],
  };
  fs.writeFileSync(
    path.join(lessonsDir, "0002-u.html"),
    `<script type="application/json" id="lesson-data">${JSON.stringify(docU)}</script>`,
  );

  return root;
}

describe("anki-import", () => {
  it("AC2: instability rules derive shaky/shaky/solid/solid on alpha/beta/gamma/delta", async () => {
    const root = makeTestRepo();
    const mock = new MockAnki();
    mock.cards = [
      {
        deckName: "Proxi::Demo",
        tags: ["node::alpha"],
        interval: 10,
        lapses: 0,
        reviews: [
          { id: 1700000001000, ease: 3 },
          { id: 1700000002000, ease: 1 },
        ],
      },
      {
        deckName: "Proxi::Demo",
        tags: ["node::beta"],
        interval: 10,
        lapses: 3,
        reviews: [
          { id: 1700000003000, ease: 3 },
          { id: 1700000004000, ease: 3 },
        ],
      },
      {
        deckName: "Proxi::Demo",
        tags: ["node::gamma"],
        interval: 30,
        lapses: 3,
        reviews: [
          { id: 1700000005000, ease: 3 },
          { id: 1700000006000, ease: 3 },
        ],
      },
      {
        deckName: "Proxi::Demo",
        tags: ["node::delta"],
        interval: 5,
        lapses: 0,
        reviews: [
          { id: 1700000007000, ease: 3 },
          { id: 1700000008000, ease: 2 },
        ],
      },
    ];

    const url = await mock.start();
    try {
      const res = await importAnkiReviews(root, "demo", { ankiUrl: url });
      assert.equal(res.online, true);
      assert.equal(res.imported, 8);
      assert.equal(res.skipped, 0);
      assert.equal(mock.requests.find((r) => r.action === "findCards")?.params?.query, 'deck:"Proxi::Demo"');

      const map = await buildKnowledgeMap(root);
      const byNode = Object.fromEntries(map.nodes.map((n) => [n.nodeId, n]));
      assert.equal(byNode.alpha.status, "shaky");
      assert.equal(byNode.beta.status, "shaky");
      assert.equal(byNode.gamma.status, "solid");
      assert.equal(byNode.delta.status, "solid");
    } finally {
      await mock.stop();
    }
  });

  it("AC4: running import twice produces imported: 0 on second run and byte-identical progress.jsonl", async () => {
    const root = makeTestRepo();
    const mock = new MockAnki();
    mock.cards = [
      {
        deckName: "Proxi::Demo",
        tags: ["node::alpha"],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000001000, ease: 3 }],
      },
    ];

    const url = await mock.start();
    try {
      const r1 = await importAnkiReviews(root, "demo", { ankiUrl: url });
      assert.equal(r1.online, true);
      assert.equal(r1.imported, 1);

      const progressFile = path.join(root, "courses", "demo", "progress.jsonl");
      const bytes1 = fs.readFileSync(progressFile);

      const r2 = await importAnkiReviews(root, "demo", { ankiUrl: url });
      assert.equal(r2.online, true);
      assert.equal(r2.imported, 0);

      const bytes2 = fs.readFileSync(progressFile);
      assert.deepEqual(bytes1, bytes2);
    } finally {
      await mock.stop();
    }
  });

  it("AC5: skips cards without node tag, unknown nodeId, or ambiguous nodeId, and resolves lesson tag", async () => {
    const root = makeTestRepo();
    const mock = new MockAnki();
    mock.cards = [
      {
        deckName: "Proxi::Demo",
        tags: [],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000001000, ease: 3 }],
      },
      {
        deckName: "Proxi::Demo",
        tags: ["node::nope"],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000002000, ease: 3 }],
      },
      {
        deckName: "Proxi::Demo",
        tags: ["node::dup"],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000003000, ease: 3 }],
      },
      {
        deckName: "Proxi::Demo",
        tags: ["node::alpha"],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000004000, ease: 3 }],
      },
      {
        deckName: "Proxi::Demo",
        tags: ["node::dup", "lesson::u"],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000005000, ease: 3 }],
      },
    ];

    const url = await mock.start();
    try {
      const res = await importAnkiReviews(root, "demo", { ankiUrl: url });
      assert.equal(res.online, true);
      assert.equal(res.skipped, 3);
      assert.equal(res.imported, 2);

      const progress = await readProgress(root, "demo");
      assert.equal(progress.length, 2);

      const dupRec = progress.find((r) => r.reviewId === 1700000005000);
      assert.ok(dupRec);
      assert.deepEqual(dupRec.nodeRefs, ["demo/u#dup"]);
      assert.equal(dupRec.lessonSlug, "u");

      const alphaRec = progress.find((r) => r.reviewId === 1700000004000);
      assert.ok(alphaRec);
      assert.deepEqual(alphaRec.nodeRefs, ["demo/t#alpha"]);
      assert.equal(alphaRec.lessonSlug, "t");
    } finally {
      await mock.stop();
    }
  });

  it("ignores reviews of cards in decks outside Proxi", async () => {
    const root = makeTestRepo();
    const mock = new MockAnki();
    mock.cards = [
      {
        deckName: "Altro::Demo",
        tags: ["node::alpha"],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000001000, ease: 3 }],
      },
    ];

    const url = await mock.start();
    try {
      const res = await importAnkiReviews(root, "demo", { ankiUrl: url });
      assert.equal(res.online, true);
      assert.equal(res.imported, 0);
      assert.equal(mock.requests.find((r) => r.action === "findCards")?.params?.query, 'deck:"Proxi::Demo"');
      assert.equal(fs.existsSync(path.join(root, "courses", "demo", "progress.jsonl")), false);
    } finally {
      await mock.stop();
    }
  });

  it("Dead port: resolves online:false, imported:0, skipped:0, and creates no progress.jsonl", async () => {
    const root = makeTestRepo();
    const res = await importAnkiReviews(root, "demo", { ankiUrl: "http://127.0.0.1:9" });
    assert.equal(res.online, false);
    assert.equal(res.imported, 0);
    assert.equal(res.skipped, 0);
    assert.ok(typeof res.error === "string");
    assert.ok(!fs.existsSync(path.join(root, "courses", "demo", "progress.jsonl")));
  });

  it("Concurrency: two concurrent calls return the same running promise and write lines once", async () => {
    const root = makeTestRepo();
    const mock = new MockAnki();
    mock.cards = [
      {
        deckName: "Proxi::Demo",
        tags: ["node::alpha"],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000001000, ease: 3 }],
      },
    ];

    const url = await mock.start();
    try {
      const [r1, r2] = await Promise.all([
        importAnkiReviews(root, "demo", { ankiUrl: url }),
        importAnkiReviews(root, "demo", { ankiUrl: url }),
      ]);
      assert.equal(r1, r2);
      assert.equal(r1.online, true);
      assert.equal(r1.imported, 1);

      const progressFile = path.join(root, "courses", "demo", "progress.jsonl");
      const lines = fs.readFileSync(progressFile, "utf-8").trim().split("\n");
      assert.equal(lines.length, 1);
    } finally {
      await mock.stop();
    }
  });
});
