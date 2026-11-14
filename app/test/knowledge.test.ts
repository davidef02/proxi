import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { ankiCause, ankiStatus, appendProgress, purgeProgress, readProgress, reinforceList } from "../server/knowledge.ts";
import type { ProgressRecord } from "../shared/protocol.ts";

function makeRepo() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "know-"));
  const dir = path.join(root, "courses", "demo", "lessons");
  fs.mkdirSync(dir, { recursive: true });
  const doc = {
    metadata: { lessonNumber: 1, topic: "T", title: "T", slug: "t", dateCreated: "", dateModified: "", version: "1" },
    nodes: [
      { nodeId: "alpha", title: "A", tier: "axiom", content: "x" },
      { nodeId: "beta", title: "B", tier: "derivation", content: "x", dependencies: ["alpha", "ghost"] },
      { nodeId: "gamma", title: "C", tier: "derivation", content: "x" },
    ],
  };
  fs.writeFileSync(path.join(dir, "0001-t.html"), `<script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script>`);
  const prose = { ...doc, metadata: { ...doc.metadata, lessonNumber: 2, slug: "prose", title: "P" }, nodes: [] };
  fs.writeFileSync(path.join(dir, "0002-prose.html"), `<script type="application/json" id="lesson-data">${JSON.stringify(prose)}</script>`);
  return root;
}
const rec = (o: Partial<ProgressRecord>): ProgressRecord => ({ at: "2026-01-01T00:00:00Z", sessionId: "s", course: "demo", lessonSlug: "t", nodeRefs: [], kind: "check", ...o });
const ankiRec = (o: Partial<ProgressRecord>): ProgressRecord => rec({ kind: "review", source: "anki", sessionId: "anki", nodeRefs: ["demo/t#alpha"], ...o });

test("purgeProgress: (a) drops practice and exercise lines, keeps check and malformed lines byte-identical", () => {
  const root = makeRepo();
  const file = path.join(root, "courses", "demo", "progress.jsonl");
  const P = JSON.stringify({ kind: "practice", nodeRefs: ["demo/t#beta"], outcome: "solved" });
  const C = JSON.stringify({ kind: "check", nodeRefs: ["demo/t#alpha"], correct: true });
  const E = JSON.stringify({ kind: "exercise", nodeRefs: ["demo/t#gamma"], correct: true });
  fs.writeFileSync(file, P + "\n" + C + "\r\n" + E + "\n" + "not json\n", "utf-8");

  purgeProgress(root);
  assert.equal(fs.readFileSync(file, "utf-8"), C + "\r\n" + "not json\n");
});

test("purgeProgress: (b) second call leaves bytes equal and leaves no *.tmp", () => {
  const root = makeRepo();
  const file = path.join(root, "courses", "demo", "progress.jsonl");
  const P = JSON.stringify({ kind: "practice", nodeRefs: ["demo/t#beta"], outcome: "solved" });
  const C = JSON.stringify({ kind: "check", nodeRefs: ["demo/t#alpha"], correct: true });
  const E = JSON.stringify({ kind: "exercise", nodeRefs: ["demo/t#gamma"], correct: true });
  fs.writeFileSync(file, P + "\n" + C + "\r\n" + E + "\n" + "not json\n", "utf-8");

  purgeProgress(root);
  const bytesAfterFirst = fs.readFileSync(file);
  purgeProgress(root);
  const bytesAfterSecond = fs.readFileSync(file);
  assert.deepEqual(bytesAfterSecond, bytesAfterFirst);
  const demoFiles = fs.readdirSync(path.join(root, "courses", "demo"));
  assert.ok(!demoFiles.some((f) => f.endsWith(".tmp")));
});

test("purgeProgress: (c) file without a final newline keeps last kept segment unterminated", () => {
  const root = makeRepo();
  const file = path.join(root, "courses", "demo", "progress.jsonl");
  const P = JSON.stringify({ kind: "practice", nodeRefs: ["demo/t#beta"] });
  const C = JSON.stringify({ kind: "check", nodeRefs: ["demo/t#alpha"], correct: true });
  fs.writeFileSync(file, P + "\n" + C, "utf-8");

  purgeProgress(root);
  assert.equal(fs.readFileSync(file, "utf-8"), C);
});

test("purgeProgress: (d) missing courses dir or missing progress.jsonl does not throw", () => {
  const root = makeRepo();
  assert.doesNotThrow(() => purgeProgress(root));

  const emptyRoot = fs.mkdtempSync(path.join(os.tmpdir(), "empty-"));
  assert.doesNotThrow(() => purgeProgress(emptyRoot));
});

test("purgeProgress: (e) file with only practice/exercise lines becomes empty", () => {
  const root = makeRepo();
  const file = path.join(root, "courses", "demo", "progress.jsonl");
  const P = JSON.stringify({ kind: "practice", nodeRefs: ["demo/t#beta"], outcome: "solved" });
  const E = JSON.stringify({ kind: "exercise", nodeRefs: ["demo/t#gamma"], correct: true });
  fs.writeFileSync(file, P + "\n" + E + "\n", "utf-8");

  purgeProgress(root);
  assert.equal(fs.readFileSync(file, "utf-8"), "");
});

test("purgeProgress: (f) keeps blank lines and custom JSON objects intact", () => {
  const root = makeRepo();
  const file = path.join(root, "courses", "demo", "progress.jsonl");
  const P = JSON.stringify({ kind: "practice", nodeRefs: ["demo/t#beta"] });
  const custom = JSON.stringify({ kind: "custom", data: 123 });
  fs.writeFileSync(file, P + "\n\n" + custom + "\n", "utf-8");

  purgeProgress(root);
  assert.equal(fs.readFileSync(file, "utf-8"), "\n" + custom + "\n");
});

test("(f) readProgress returns review lines, and purgeProgress keeps a review line byte-identical", async () => {
  const root = makeRepo();
  const file = path.join(root, "courses", "demo", "progress.jsonl");
  const R = JSON.stringify(ankiRec({ cardId: 101, reviewId: 2001, ease: 3, interval: 30, lapses: 0 }));
  const C = JSON.stringify(rec({ nodeRefs: ["demo/t#alpha"], correct: true }));
  const P = JSON.stringify({ kind: "practice", nodeRefs: ["demo/t#beta"], outcome: "solved" });
  fs.writeFileSync(file, R + "\n" + C + "\r\n" + P + "\n", "utf-8");

  const progress = await readProgress(root, "demo");
  assert.equal(progress.length, 2);
  assert.equal(progress[0].kind, "review");
  assert.equal(progress[0].reviewId, 2001);

  purgeProgress(root);
  assert.equal(fs.readFileSync(file, "utf-8"), R + "\n" + C + "\r\n");
});

test("ankiCause: Again, lapses, and a recovered card", () => {
  assert.equal(ankiCause([ankiRec({ ease: 1 })])?.cause, "Anki: Again");
  assert.equal(ankiCause([ankiRec({ ease: 3, lapses: 3, interval: 5 })])?.cause, "Anki: 3 lapses");
  assert.equal(
    ankiCause([
      ankiRec({ cardId: 1, ease: 1, at: "2026-01-01T00:00:00Z", reviewId: 100 }),
      ankiRec({ cardId: 1, ease: 3, lapses: 0, interval: 30, at: "2026-01-02T00:00:00Z", reviewId: 101 }),
    ]),
    undefined,
  );
});

test("ankiCause is defined iff ankiStatus is shaky", () => {
  const cases: Record<string, ProgressRecord[]> = {
    empty: [],
    again: [ankiRec({ ease: 1 })],
    "lapses shaky": [ankiRec({ ease: 3, lapses: 3, interval: 10 })],
    "mature lapses": [ankiRec({ ease: 3, lapses: 3, interval: 30 })],
    recovered: [
      ankiRec({ cardId: 1, ease: 1, at: "2026-01-01T00:00:00Z", reviewId: 100 }),
      ankiRec({ cardId: 1, ease: 3, lapses: 0, interval: 30, at: "2026-01-02T00:00:00Z", reviewId: 101 }),
    ],
    "two cards, one shaky": [ankiRec({ cardId: 1, ease: 1 }), ankiRec({ cardId: 2, ease: 3, lapses: 0, interval: 30 })],
  };
  for (const [name, rs] of Object.entries(cases)) {
    assert.equal(ankiCause(rs) !== undefined, ankiStatus(rs) === "shaky", name);
  }
});

test("reinforceList: only Anki-shaky nodes, most recent first, honours the limit", async () => {
  assert.deepEqual(await reinforceList(makeRepo(), "demo"), []);

  const root = makeRepo();
  await appendProgress(root, ankiRec({ cardId: 1, nodeRefs: ["demo/t#alpha"], ease: 1, at: "2026-01-01T00:00:00Z" }));
  await appendProgress(root, rec({ nodeRefs: ["demo/t#beta"], correct: true, confidence: 20, at: "2026-01-05T00:00:00Z" })); // shaky check only
  await appendProgress(root, rec({ nodeRefs: ["demo/t#gamma"], correct: false, at: "2026-01-06T00:00:00Z" })); // missed check

  const list = await reinforceList(root, "demo");
  assert.deepEqual(list.map((e) => e.title), ["A"]);
  assert.equal(list[0].lessonNumber, 1);
  assert.equal(list[0].cause, "Anki: Again");
  assert.equal(list[0].key, "demo/t#alpha");

  // older shaky review on B: A stays first, limit trims the tail
  await appendProgress(root, ankiRec({ cardId: 2, nodeRefs: ["demo/t#beta"], ease: 1, at: "2025-12-01T00:00:00Z" }));
  assert.deepEqual((await reinforceList(root, "demo")).map((e) => e.title), ["A", "B"]);
  assert.deepEqual((await reinforceList(root, "demo", 1)).map((e) => e.title), ["A"]);
});

test("ankiCause picks the newest shaky card, reviewId breaking ties", () => {
  // the later shaky card supplies the cause
  assert.deepEqual(
    ankiCause([
      ankiRec({ cardId: 1, ease: 1, at: "2026-01-01T00:00:00Z", reviewId: 100 }),
      ankiRec({ cardId: 2, ease: 3, lapses: 4, interval: 5, at: "2026-02-01T00:00:00Z", reviewId: 200 }),
    ]),
    { cause: "Anki: 4 lapses", at: "2026-02-01T00:00:00Z" },
  );

  // same `at`: the higher reviewId wins
  assert.equal(
    ankiCause([
      ankiRec({ cardId: 1, ease: 1, at: "2026-02-01T00:00:00Z", reviewId: 100 }),
      ankiRec({ cardId: 2, ease: 3, lapses: 3, interval: 5, at: "2026-02-01T00:00:00Z", reviewId: 200 }),
    ])?.cause,
    "Anki: 3 lapses",
  );
});

test("reinforceList keeps lesson order when evidence shares the same `at`", async () => {
  const root = makeRepo();
  const at = "2026-01-01T00:00:00Z";
  await appendProgress(root, ankiRec({ cardId: 1, nodeRefs: ["demo/t#beta"], ease: 1, at }));
  await appendProgress(root, ankiRec({ cardId: 2, nodeRefs: ["demo/t#alpha"], ease: 1, at }));

  assert.deepEqual((await reinforceList(root, "demo")).map((e) => e.title), ["A", "B"]);
});

test("reinforceList sorts by the review that supplies each cause", async () => {
  const root = makeRepo();
  // A: shaky card at 01-01 plus a newer passing card at 03-01 -> still sorted by 01-01
  await appendProgress(root, ankiRec({ cardId: 1, nodeRefs: ["demo/t#alpha"], ease: 1, at: "2026-01-01T00:00:00Z" }));
  await appendProgress(root, ankiRec({ cardId: 2, nodeRefs: ["demo/t#alpha"], ease: 3, lapses: 0, interval: 30, at: "2026-03-01T00:00:00Z" }));
  await appendProgress(root, ankiRec({ cardId: 3, nodeRefs: ["demo/t#beta"], ease: 1, at: "2026-02-01T00:00:00Z" }));

  const list = await reinforceList(root, "demo");
  assert.deepEqual(list.map((e) => e.title), ["B", "A"]);
  assert.equal(list[1].at, "2026-01-01T00:00:00Z");
  assert.equal(list[1].cause, "Anki: Again");
});
