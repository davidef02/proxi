import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { listConflicts, listTrash, resolveConflict, restoreTrash } from "../server/sync/store.ts";

const DAY = 24 * 60 * 60 * 1000;
const NOW = 1_700_000_000_000;

function setup() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-store-"));
  const data = path.join(tmp, "data");
  const trashDir = path.join(tmp, "trash");
  const conflictsDir = path.join(tmp, "conflicts");
  fs.mkdirSync(data, { recursive: true });
  const put = (root: string, rel: string, body: string) => {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), body);
  };
  const cleanup = () => fs.rmSync(tmp, { recursive: true, force: true });
  return { tmp, data, trashDir, conflictsDir, put, cleanup };
}

test("listTrash lists newest first; restoreTrash moves a file back and refuses to overwrite", async () => {
  const t = setup();
  try {
    t.put(t.trashDir, `${NOW - DAY}/courses/a/x.md`, "restore me");
    t.put(t.trashDir, `${NOW}/graph/nodes.jsonl`, "{}");
    const list = listTrash(t.trashDir);
    assert.deepEqual(list.map((e) => e.id), [`${NOW}/graph/nodes.jsonl`, `${NOW - DAY}/courses/a/x.md`]);
    assert.deepEqual(list[1], {
      id: `${NOW - DAY}/courses/a/x.md`,
      rel: "courses/a/x.md",
      deletedAt: new Date(NOW - DAY).toISOString(),
      size: "restore me".length,
    });

    await restoreTrash(t.trashDir, t.data, `${NOW - DAY}/courses/a/x.md`);
    assert.equal(fs.readFileSync(path.join(t.data, "courses/a/x.md"), "utf-8"), "restore me");
    assert.equal(fs.existsSync(path.join(t.trashDir, `${NOW - DAY}`)), false);

    t.put(t.trashDir, `${NOW}/courses/a/x.md`, "again");
    await assert.rejects(restoreTrash(t.trashDir, t.data, `${NOW}/courses/a/x.md`), /Esiste già un file con questo nome: courses\/a\/x\.md/);
    assert.equal(fs.readFileSync(path.join(t.data, "courses/a/x.md"), "utf-8"), "restore me");
  } finally {
    t.cleanup();
  }
});

test("resolveConflict: local/remote write the copy back only when the copy is the chosen side", async () => {
  const t = setup();
  try {
    // copy = local side (locale): "local" keeps it, "remote" discards it
    t.put(t.data, "courses/a/x.md", "current");
    t.put(t.conflictsDir, `courses/a/x.md.locale-${NOW}`, "local copy");
    await resolveConflict(t.conflictsDir, t.data, `courses/a/x.md.locale-${NOW}`, "local");
    assert.equal(fs.readFileSync(path.join(t.data, "courses/a/x.md"), "utf-8"), "local copy");
    assert.equal(fs.existsSync(path.join(t.conflictsDir, "courses")), false);

    t.put(t.data, "courses/a/y.md", "current");
    t.put(t.conflictsDir, `courses/a/y.md.locale-${NOW}`, "discard me");
    await resolveConflict(t.conflictsDir, t.data, `courses/a/y.md.locale-${NOW}`, "remote");
    assert.equal(fs.readFileSync(path.join(t.data, "courses/a/y.md"), "utf-8"), "current");

    // copy = remote side (remoto): "remote" keeps it, "local" discards it
    t.put(t.data, "decks/d.tsv", "current");
    t.put(t.conflictsDir, `decks/d.tsv.remoto-${NOW}`, "remote copy");
    await resolveConflict(t.conflictsDir, t.data, `decks/d.tsv.remoto-${NOW}`, "remote");
    assert.equal(fs.readFileSync(path.join(t.data, "decks/d.tsv"), "utf-8"), "remote copy");

    t.put(t.data, "decks/e.tsv", "current");
    t.put(t.conflictsDir, `decks/e.tsv.remoto-${NOW}`, "discard me");
    await resolveConflict(t.conflictsDir, t.data, `decks/e.tsv.remoto-${NOW}`, "local");
    assert.equal(fs.readFileSync(path.join(t.data, "decks/e.tsv"), "utf-8"), "current");
    assert.deepEqual(listConflicts(t.conflictsDir), []);
  } finally {
    t.cleanup();
  }
});

test("resolveConflict both moves the copy into conflitti/ inside the data folder", async () => {
  const t = setup();
  try {
    const id = `courses/a/x.md.remoto-${NOW}`;
    t.put(t.data, "courses/a/x.md", "current");
    t.put(t.conflictsDir, id, "kept copy");
    await resolveConflict(t.conflictsDir, t.data, id, "both");
    assert.equal(fs.readFileSync(path.join(t.data, "conflitti", id), "utf-8"), "kept copy");
    assert.equal(fs.readFileSync(path.join(t.data, "courses/a/x.md"), "utf-8"), "current");
    assert.deepEqual(listConflicts(t.conflictsDir), []);
  } finally {
    t.cleanup();
  }
});

test("traversal ids are rejected by restore and resolve", async () => {
  const t = setup();
  try {
    t.put(t.conflictsDir, `ok.md.locale-${NOW}`, "x");
    await assert.rejects(resolveConflict(t.conflictsDir, t.data, `../ok.md.locale-${NOW}`, "local"), /Conflitto non valido/);
    await assert.rejects(resolveConflict(t.conflictsDir, t.data, `courses/../../x.md.locale-${NOW}`, "local"), /Conflitto non valido/);
    await assert.rejects(resolveConflict(t.conflictsDir, t.data, `ok.md.locale-${NOW}`, "nope" as "local"), /Scelta non valida/);
    await assert.rejects(restoreTrash(t.trashDir, t.data, `${NOW}/../../x`), /Identificatore non valido/);
    await assert.rejects(restoreTrash(t.trashDir, t.data, `${NOW}/courses/../../x`), /Identificatore non valido/);
    await assert.rejects(restoreTrash(t.trashDir, t.data, "../x"), /Identificatore non valido/);
    assert.equal(fs.existsSync(path.join(t.tmp, "x")), false);
    assert.equal(fs.existsSync(path.join(t.conflictsDir, `ok.md.locale-${NOW}`)), true);
  } finally {
    t.cleanup();
  }
});
