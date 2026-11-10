import { test } from "node:test";
import assert from "node:assert/strict";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { inSyncSet, readManifest, syncPass, writeManifest, type SyncPassOptions } from "../server/sync/engine.ts";
import { RemoteMovedError, type Manifest, type RemoteStat, type SyncProvider, type SyncResult } from "../server/sync/types.ts";

const KEY = "mem:test";
const NOW = 1_700_000_000_000;
const sha1 = (b: Buffer) => crypto.createHash("sha1").update(b).digest("hex");

/** In-memory provider. Writes and removals are staged until commit(), like the GitHub provider. */
function memProvider(files: Record<string, { data: string; mtimeMs: number }> = {}) {
  const store = new Map<string, { data: Buffer; mtimeMs: number }>(
    Object.entries(files).map(([k, v]) => [k, { data: Buffer.from(v.data), mtimeMs: v.mtimeMs }]),
  );
  const staged = new Map<string, { data: Buffer; mtimeMs: number } | null>();
  const counters = { reads: 0, writes: 0, commits: 0 };
  const hooks = { movesLeft: 0, failWrite: new Set<string>() };
  const provider: SyncProvider = {
    key: KEY,
    async list() {
      staged.clear();
      const out = new Map<string, RemoteStat>();
      for (const [rel, f] of store) out.set(rel, { size: f.data.length, mtimeMs: f.mtimeMs, token: sha1(f.data) });
      return out;
    },
    async read(rel) {
      counters.reads++;
      const f = store.get(rel);
      if (!f) throw new Error(`missing ${rel}`);
      return f.data;
    },
    async write(rel, data, mtimeMs) {
      counters.writes++;
      if (hooks.failWrite.has(rel)) throw new Error("boom");
      staged.set(rel, { data, mtimeMs });
      return { size: data.length, mtimeMs, token: sha1(data) };
    },
    async remove(rel) {
      staged.set(rel, null);
    },
    async commit() {
      if (hooks.movesLeft > 0) {
        // Another PC pushed meanwhile: the store moves, our staged changes are lost.
        hooks.movesLeft--;
        store.set("curriculum.json", { data: Buffer.from("other pc"), mtimeMs: Date.now() + 1_000_000 });
        throw new RemoteMovedError("remote moved");
      }
      counters.commits++;
      for (const [rel, s] of staged) {
        if (s) store.set(rel, s);
        else store.delete(rel);
      }
      staged.clear();
    },
  };
  return { provider, store, counters, hooks };
}

const counts = (r: SyncResult) => ({ pushed: r.pushed, pulled: r.pulled, deleted: r.deleted, conflicts: r.conflicts, errors: r.errors });

function setup(remote: Record<string, { data: string; mtimeMs: number }> = {}) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-engine-"));
  const data = path.join(tmp, "data");
  const conflictsDir = path.join(tmp, "conflicts");
  const trashDir = path.join(tmp, "trash");
  const mem = memProvider(remote);
  let manifest: Manifest = { v: 2, key: KEY, files: {} };
  const write = (rel: string, body: string, ageS = 0) => {
    const f = path.join(data, rel);
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, body);
    const t = new Date(Date.now() - ageS * 1000);
    fs.utimesSync(f, t, t);
  };
  const read = (rel: string) => fs.readFileSync(path.join(data, rel), "utf-8");
  const pass = async (extra: Partial<SyncPassOptions> = {}) => {
    const out = await syncPass({ dataRoot: data, provider: mem.provider, manifest, conflictsDir, trashDir, now: () => NOW, ...extra });
    manifest = out.manifest;
    return out.result;
  };
  const cleanup = () => fs.rmSync(tmp, { recursive: true, force: true });
  return { tmp, data, conflictsDir, trashDir, mem, write, read, pass, cleanup, manifest: () => manifest };
}

test("syncPass: one-sided moves, skips node_modules/.learner/positions, second pass does nothing", async () => {
  const t = setup({ "graph/nodes.jsonl": { data: "{}\n", mtimeMs: Date.now() - 5_000 }, "graph/positions.json": { data: "{}", mtimeMs: Date.now() } });
  try {
    t.write("courses/a/lessons/0001-x.html", "L1");
    t.write("courses/a/exercises/node_modules/x.js", "nope");
    t.write(".learner/settings.json", "{}");
    assert.deepEqual(counts(await t.pass()), { pushed: 1, pulled: 1, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.mem.store.get("courses/a/lessons/0001-x.html")?.data.toString(), "L1");
    assert.equal(t.mem.store.has("courses/a/exercises/node_modules/x.js"), false);
    assert.equal(t.read("graph/nodes.jsonl"), "{}\n");
    assert.equal(fs.existsSync(path.join(t.data, "graph/positions.json")), false);

    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 0, deleted: 0, conflicts: 0, errors: [] });
    assert.deepEqual(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
  } finally {
    t.cleanup();
  }
});

test("syncPass: first meeting with a fresh clone → remote wins, the local copy is kept in conflicts", async () => {
  const t = setup({ "curriculum.json": { data: "drive-newer-work", mtimeMs: Date.now() - 3_600_000 } });
  try {
    t.write("curriculum.json", "git-version");
    const r = await t.pass();
    assert.equal(r.conflicts, 1);
    assert.equal(r.pulled, 1);
    assert.equal(t.read("curriculum.json"), "drive-newer-work");
    const kept = fs.readdirSync(t.conflictsDir);
    assert.equal(kept.length, 1);
    assert.match(kept[0], /^curriculum\.json\.locale-\d+$/);
    assert.equal(fs.readFileSync(path.join(t.conflictsDir, kept[0]), "utf-8"), "git-version");
  } finally {
    t.cleanup();
  }
});

test("syncPass: edits seconds apart are synced; only the changed side moves", async () => {
  const t = setup();
  try {
    t.write("courses/a/x.md", "v1", 10);
    await t.pass();
    t.write("courses/a/x.md", "v2", 9); // same size, 1 s later
    assert.deepEqual(counts(await t.pass()), { pushed: 1, pulled: 0, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.mem.store.get("courses/a/x.md")?.data.toString(), "v2");
    t.mem.store.set("courses/a/x.md", { data: Buffer.from("v3-from-other-pc"), mtimeMs: Date.now() - 100_000 });
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 1, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.read("courses/a/x.md"), "v3-from-other-pc");
  } finally {
    t.cleanup();
  }
});

test("syncPass: both changed → newer wins, loser kept; a failing file does not stop the pass", async () => {
  const t = setup();
  try {
    t.write("decks/d.tsv", "base", 100);
    t.write("decks/e.tsv", "e", 100);
    await t.pass();
    t.write("decks/d.tsv", "local-edit", 50);
    t.mem.store.set("decks/d.tsv", { data: Buffer.from("remote-edit-newer"), mtimeMs: Date.now() - 10_000 });
    t.write("decks/e.tsv", "e2", 5);
    t.mem.hooks.failWrite.add("decks/e.tsv");
    const r = await t.pass();
    assert.equal(r.conflicts, 1);
    assert.deepEqual(r.errors, ["decks/e.tsv: boom"]);
    assert.equal(t.read("decks/d.tsv"), "remote-edit-newer");
    const kept = fs.readdirSync(path.join(t.conflictsDir, "decks"));
    assert.equal(kept.length, 1);
    assert.equal(fs.readFileSync(path.join(t.conflictsDir, "decks", kept[0]), "utf-8"), "local-edit");
  } finally {
    t.cleanup();
  }
});

test("syncPass: a local delete removes the remote file in one commit", async () => {
  const t = setup();
  try {
    t.write("courses/a/x.md", "x");
    t.write("courses/a/y.md", "y");
    for (let i = 0; i < 4; i++) t.write(`courses/keep/k${i}.md`, "keep"); // 1 of 6 stays under the 30% guard
    await t.pass();
    fs.rmSync(path.join(t.data, "courses/a/x.md"));
    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 0, deleted: 1, conflicts: 0, errors: [] });
    assert.equal(t.mem.store.has("courses/a/x.md"), false);
    assert.equal(t.mem.counters.commits, 1);
  } finally {
    t.cleanup();
  }
});

test("syncPass: a remote delete moves the local file to trash and prunes its empty course dir", async () => {
  const t = setup();
  try {
    t.write("courses/gone/a.md", "bye");
    for (let i = 0; i < 4; i++) t.write(`courses/keep/k${i}.md`, "keep"); // 1 of 5 stays under the 30% guard
    await t.pass();
    t.mem.store.delete("courses/gone/a.md");
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 0, deleted: 1, conflicts: 0, errors: [] });
    assert.equal(fs.readFileSync(path.join(t.trashDir, String(NOW), "courses/gone/a.md"), "utf-8"), "bye");
    assert.equal(fs.existsSync(path.join(t.data, "courses/gone")), false);
    assert.equal(fs.existsSync(path.join(t.data, "courses")), true);
    assert.equal(fs.existsSync(path.join(t.data, "courses/keep/k0.md")), true);
  } finally {
    t.cleanup();
  }
});

test("syncPass: edit beats delete in both directions", async () => {
  const t = setup();
  try {
    // local edit, remote delete → the edit is pushed back
    t.write("courses/e.md", "one", 100);
    await t.pass();
    t.write("courses/e.md", "two", 0);
    t.mem.store.delete("courses/e.md");
    assert.deepEqual(counts(await t.pass()), { pushed: 1, pulled: 0, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.mem.store.get("courses/e.md")?.data.toString(), "two");

    // remote edit, local delete → the remote copy is pulled back
    t.write("courses/r.md", "one", 100);
    await t.pass();
    t.mem.store.set("courses/r.md", { data: Buffer.from("remote-two"), mtimeMs: Date.now() });
    fs.rmSync(path.join(t.data, "courses/r.md"));
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 1, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.read("courses/r.md"), "remote-two");
  } finally {
    t.cleanup();
  }
});

test("syncPass: deleting over 30% of the known files stops for confirmation, then applies it", async () => {
  const t = setup();
  try {
    for (const n of [1, 2, 3, 4]) t.write(`courses/a/${n}.md`, `f${n}`);
    await t.pass();
    fs.rmSync(path.join(t.data, "courses/a/1.md"));
    fs.rmSync(path.join(t.data, "courses/a/2.md"));
    const before = JSON.stringify(t.manifest());
    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    const held = await t.pass();
    assert.deepEqual(held.pendingDeletions, { count: 2, total: 4 });
    assert.equal(held.deleted, 0);
    assert.equal(JSON.stringify(t.manifest()), before);
    assert.equal(t.mem.store.size, 4);
    assert.deepEqual(t.mem.counters, { reads: 0, writes: 0, commits: 0 });

    const applied = await t.pass({ confirmDeletions: true });
    assert.equal(applied.pendingDeletions, undefined);
    assert.equal(applied.deleted, 2);
    assert.deepEqual([...t.mem.store.keys()].sort(), ["courses/a/3.md", "courses/a/4.md"]);
  } finally {
    t.cleanup();
  }
});

test("syncPass: RemoteMovedError once → the second attempt succeeds and reflects the moved remote", async () => {
  const t = setup();
  try {
    t.write("courses/n.md", "new");
    t.mem.hooks.movesLeft = 1;
    const r = await t.pass();
    assert.deepEqual(counts(r), { pushed: 1, pulled: 1, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.mem.store.get("courses/n.md")?.data.toString(), "new");
    assert.equal(t.read("curriculum.json"), "other pc");
  } finally {
    t.cleanup();
  }
});

test("syncPass: RemoteMovedError twice → throws, and the manifest is not modified", async () => {
  const t = setup();
  try {
    t.write("courses/n.md", "new");
    const before = JSON.stringify(t.manifest());
    t.mem.hooks.movesLeft = 2;
    await assert.rejects(t.pass(), /riprovo alla prossima/);
    assert.equal(JSON.stringify(t.manifest()), before);
  } finally {
    t.cleanup();
  }
});

test("syncPass: remote names outside the sync set are ignored", async () => {
  const t = setup({
    "courses/../../evil": { data: "evil", mtimeMs: Date.now() },
    "C:x": { data: "drive", mtimeMs: Date.now() },
    ".learner/secrets.json": { data: "{}", mtimeMs: Date.now() },
  });
  try {
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 0, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.mem.counters.reads, 0);
    assert.equal(fs.existsSync(path.join(t.tmp, "evil")), false);
  } finally {
    t.cleanup();
  }
});

test("inSyncSet accepts only the synced roots and rejects traversal and skipped names", () => {
  for (const ok of ["courses/a/x.md", "graph/nodes.jsonl", "decks/d.tsv", "curriculum.json"]) assert.equal(inSyncSet(ok), true, ok);
  for (const bad of [
    "", "courses/../../evil", "courses//x", "./courses/x", "C:x", "courses\\x", ".learner/settings.json",
    "curriculum.json/x", "curriculum.json.bak", "courses", "courses/a/node_modules/x.js", "graph/positions.json",
    "courses/a/x.md.ia-sync-tmp", "viz/x.html",
  ]) assert.equal(inSyncSet(bad), false, bad);
});

test("syncPass: N pushes + D removals make exactly one commit", async () => {
  const t = setup();
  try {
    for (let i = 0; i < 10; i++) t.write(`courses/a/f${i}.md`, `f${i}`);
    await t.pass();
    t.write("courses/b/n1.md", "1");
    t.write("courses/b/n2.md", "2");
    t.write("courses/b/n3.md", "3");
    fs.rmSync(path.join(t.data, "courses/a/f0.md"));
    fs.rmSync(path.join(t.data, "courses/a/f1.md"));
    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    assert.deepEqual(counts(await t.pass()), { pushed: 3, pulled: 0, deleted: 2, conflicts: 0, errors: [] });
    assert.equal(t.mem.counters.commits, 1);
  } finally {
    t.cleanup();
  }
});

test("syncPass: both changed, local newer → local pushed, the remote bytes kept as a .remoto copy", async () => {
  const t = setup();
  try {
    t.write("decks/d.tsv", "base", 1000);
    await t.pass();
    t.mem.store.set("decks/d.tsv", { data: Buffer.from("remote-edit-older"), mtimeMs: Date.now() - 500_000 });
    t.write("decks/d.tsv", "local-edit-newer", 0);
    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    assert.deepEqual(counts(await t.pass()), { pushed: 1, pulled: 0, deleted: 0, conflicts: 1, errors: [] });
    assert.equal(t.mem.store.get("decks/d.tsv")?.data.toString(), "local-edit-newer");
    assert.equal(t.read("decks/d.tsv"), "local-edit-newer");
    assert.equal(t.mem.counters.commits, 1);
    const kept = fs.readdirSync(path.join(t.conflictsDir, "decks"));
    assert.deepEqual(kept, [`d.tsv.remoto-${NOW}`]);
    assert.equal(fs.readFileSync(path.join(t.conflictsDir, "decks", kept[0]), "utf-8"), "remote-edit-older");

    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 0, deleted: 0, conflicts: 0, errors: [] });
    assert.deepEqual(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
  } finally {
    t.cleanup();
  }
});

test("syncPass: a RemoteMovedError retry keeps one conflict copy and counts the conflict once", async () => {
  const t = setup();
  try {
    t.write("decks/d.tsv", "base", 1000);
    await t.pass();
    t.mem.store.set("decks/d.tsv", { data: Buffer.from("remote-edit-older"), mtimeMs: Date.now() - 500_000 });
    t.write("decks/d.tsv", "local-edit-newer", 0);
    // A distinct clock per attempt: a duplicate copy would get its own file name, not overwrite the first.
    let tick = NOW;
    t.mem.hooks.movesLeft = 1;
    const r = await t.pass({ now: () => tick++ });
    assert.deepEqual(counts(r), { pushed: 1, pulled: 1, deleted: 0, conflicts: 1, errors: [] });
    assert.equal(t.mem.store.get("decks/d.tsv")?.data.toString(), "local-edit-newer");
    const kept = fs.readdirSync(path.join(t.conflictsDir, "decks"));
    assert.equal(kept.length, 1);
    assert.equal(fs.readFileSync(path.join(t.conflictsDir, "decks", kept[0]), "utf-8"), "remote-edit-older");
  } finally {
    t.cleanup();
  }
});

test("syncPass: both changed to the same bytes → no conflict, no transfer, and the next pass is a no-op", async () => {
  const t = setup();
  try {
    t.write("courses/s.md", "base", 1000);
    await t.pass();
    t.write("courses/s.md", "same-edit", 0);
    t.mem.store.set("courses/s.md", { data: Buffer.from("same-edit"), mtimeMs: Date.now() });
    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 0, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.mem.counters.writes, 0);
    assert.equal(t.mem.counters.commits, 0);
    assert.equal(fs.existsSync(t.conflictsDir), false);
    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 0, deleted: 0, conflicts: 0, errors: [] });
    assert.deepEqual(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
  } finally {
    t.cleanup();
  }
});

test("syncPass: remote deletions over 30% also stop for confirmation and keep the local files", async () => {
  const t = setup();
  try {
    for (const n of [1, 2, 3]) t.write(`courses/a/${n}.md`, `f${n}`);
    await t.pass();
    t.mem.store.delete("courses/a/1.md");
    t.mem.store.delete("courses/a/2.md");
    const held = await t.pass();
    assert.deepEqual(held.pendingDeletions, { count: 2, total: 3 });
    assert.equal(fs.existsSync(path.join(t.data, "courses/a/1.md")), true);
    assert.equal(fs.existsSync(t.trashDir), false);

    const applied = await t.pass({ confirmDeletions: true });
    assert.equal(applied.deleted, 2);
    assert.equal(fs.existsSync(path.join(t.data, "courses/a/1.md")), false);
    assert.equal(fs.readFileSync(path.join(t.trashDir, String(NOW), "courses/a/1.md"), "utf-8"), "f1");
    assert.equal(t.read("courses/a/3.md"), "f3");
  } finally {
    t.cleanup();
  }
});

test("syncPass: deletions at exactly 30% pass without confirmation", async () => {
  const t = setup();
  try {
    for (let i = 0; i < 10; i++) t.write(`courses/a/${i}.md`, `f${i}`);
    await t.pass();
    for (const i of [0, 1, 2]) fs.rmSync(path.join(t.data, `courses/a/${i}.md`));
    const r = await t.pass();
    assert.equal(r.pendingDeletions, undefined);
    assert.equal(r.deleted, 3);
  } finally {
    t.cleanup();
  }
});

test("syncPass: a file deleted on both sides only drops its manifest entry", async () => {
  const t = setup();
  try {
    t.write("courses/x.md", "x");
    for (let i = 0; i < 4; i++) t.write(`courses/k${i}.md`, "k");
    await t.pass();
    fs.rmSync(path.join(t.data, "courses/x.md"));
    t.mem.store.delete("courses/x.md");
    Object.assign(t.mem.counters, { reads: 0, writes: 0, commits: 0 });
    assert.deepEqual(counts(await t.pass()), { pushed: 0, pulled: 0, deleted: 0, conflicts: 0, errors: [] });
    assert.equal(t.manifest().files["courses/x.md"], undefined);
    assert.equal(t.mem.counters.commits, 0);
    assert.equal(fs.existsSync(t.trashDir), false);
  } finally {
    t.cleanup();
  }
});

test("syncPass: a pulled file keeps the remote mtime", async () => {
  const mtimeMs = 1_600_000_000_000;
  const t = setup({ "courses/old.md": { data: "old", mtimeMs } });
  try {
    await t.pass();
    assert.equal(Math.round(fs.statSync(path.join(t.data, "courses/old.md")).mtimeMs / 1000), mtimeMs / 1000);
  } finally {
    t.cleanup();
  }
});

test("readManifest: a manifest is only reused for the data folder it was written for", () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-manifest-"));
  try {
    const file = path.join(tmp, "m.json");
    const a = path.join(tmp, "a");
    const b = path.join(tmp, "b");
    writeManifest(file, { v: 2, key: KEY, dataRoot: a, files: { "courses/x.md": { l: { size: 1, mtimeMs: 1 }, r: { size: 1, mtimeMs: 1, token: "t" } } } });
    assert.equal(Object.keys(readManifest(file, KEY, a).files).length, 1);
    const other = readManifest(file, KEY, b);
    assert.deepEqual(other.files, {});
    assert.equal(other.dataRoot, path.resolve(b));
    assert.deepEqual(readManifest(file, "mem:other", a).files, {});
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("readManifest: a manifest without a data folder (written before the binding) is not reused", () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-manifest-"));
  try {
    const file = path.join(tmp, "m.json");
    const a = path.join(tmp, "a");
    fs.writeFileSync(file, JSON.stringify({ v: 2, key: KEY, files: { "courses/x.md": { l: { size: 1, mtimeMs: 1 }, r: { size: 1, mtimeMs: 1, token: "t" } } } }));
    const m = readManifest(file, KEY, a);
    assert.deepEqual(m.files, {});
    assert.equal(m.dataRoot, path.resolve(a));
    // a relative path and its absolute form are the same folder
    writeManifest(file, { ...m, files: { "courses/y.md": { l: { size: 1, mtimeMs: 1 }, r: { size: 1, mtimeMs: 1, token: "t" } } } });
    assert.equal(Object.keys(readManifest(file, KEY, path.relative(process.cwd(), a)).files).length, 1);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("syncPass: a RemoteMovedError retry keeps the pulled and trashed counts of the failed attempt", async () => {
  const t = setup();
  try {
    t.write("courses/gone/a.md", "bye");
    for (let i = 0; i < 4; i++) t.write(`courses/keep/k${i}.md`, "keep"); // 1 of 5 stays under the 30% guard
    await t.pass();
    // Attempt 1 pulls a new remote file and trashes a remotely deleted one, then the commit finds the ref moved.
    t.mem.store.delete("courses/gone/a.md");
    t.mem.store.set("courses/new.md", { data: Buffer.from("from pc2"), mtimeMs: Date.now() - 5_000 });
    t.write("courses/keep/push.md", "push me"); // forces a commit, so the move hits this pass
    const commit = t.mem.provider.commit.bind(t.mem.provider);
    let moved = true;
    t.mem.provider.commit = async (message) => {
      if (moved) {
        moved = false;
        throw new RemoteMovedError("ref moved");
      }
      return commit(message);
    };
    const r = await t.pass();
    assert.deepEqual(counts(r), { pushed: 1, pulled: 1, deleted: 1, conflicts: 0, errors: [] });
    assert.equal(t.read("courses/new.md"), "from pc2");
    assert.equal(fs.existsSync(path.join(t.data, "courses/gone/a.md")), false);
    assert.equal(t.mem.store.get("courses/keep/push.md")?.data.toString(), "push me");
  } finally {
    t.cleanup();
  }
});

test("syncPass: a RemoteMovedError retry counts a staged remote removal once and adds its own pulls to the carried ones", async () => {
  const t = setup();
  try {
    t.write("courses/drop/d.md", "drop me");
    for (let i = 0; i < 4; i++) t.write(`courses/keep/k${i}.md`, "keep"); // 1 of 5 stays under the 30% guard
    await t.pass();
    // Attempt 1: pulls one new remote file and stages the removal of a file deleted here. The commit then finds the
    // ref moved (the hook adds curriculum.json), so attempt 2 re-plans: the pulled file is a no-op, the removal is
    // staged again, and curriculum.json is a new pull.
    fs.rmSync(path.join(t.data, "courses/drop/d.md"));
    t.mem.store.set("courses/new.md", { data: Buffer.from("from pc2"), mtimeMs: Date.now() - 5_000 });
    t.mem.hooks.movesLeft = 1;
    const commitsBefore = t.mem.counters.commits;
    const r = await t.pass();
    assert.deepEqual(counts(r), { pushed: 0, pulled: 2, deleted: 1, conflicts: 0, errors: [] });
    assert.equal(t.mem.store.has("courses/drop/d.md"), false, "the retry's commit carries the removal");
    assert.equal(t.read("courses/new.md"), "from pc2");
    assert.equal(t.read("curriculum.json"), "other pc");
    assert.equal(t.mem.counters.commits, commitsBefore + 1, "one commit lands for the whole pass");
  } finally {
    t.cleanup();
  }
});

test("syncPass: a remote-wins conflict pulled by attempt 1 still counts as pulled after the retry", async () => {
  const t = setup();
  try {
    t.write("decks/d.tsv", "base", 1000);
    await t.pass();
    t.mem.store.set("decks/d.tsv", { data: Buffer.from("remote-edit-newer"), mtimeMs: Date.now() + 500_000 });
    t.write("decks/d.tsv", "local-edit-older", 0);
    t.write("decks/other.tsv", "push me", 0); // forces a commit, so the move hits this pass
    t.mem.hooks.movesLeft = 1;
    const r = await t.pass();
    // d.tsv (attempt 1) + curriculum.json (attempt 2); other.tsv is pushed once, by the commit that lands.
    assert.deepEqual(counts(r), { pushed: 1, pulled: 2, deleted: 0, conflicts: 1, errors: [] });
  } finally {
    t.cleanup();
  }
});

test("syncPass: a RemoteMovedError retry after a remote-wins conflict counts it once and keeps the local copy", async () => {
  const t = setup();
  try {
    t.write("decks/d.tsv", "base", 1000);
    await t.pass();
    t.mem.store.set("decks/d.tsv", { data: Buffer.from("remote-edit-newer"), mtimeMs: Date.now() + 500_000 });
    t.write("decks/d.tsv", "local-edit-older", 0);
    t.write("decks/other.tsv", "push me", 0); // forces a commit, so the move hits this pass
    t.mem.hooks.movesLeft = 1;
    const r = await t.pass();
    assert.equal(r.conflicts, 1);
    assert.deepEqual(r.errors, []);
    assert.equal(t.read("decks/d.tsv"), "remote-edit-newer");
    assert.equal(t.mem.store.get("decks/other.tsv")?.data.toString(), "push me");
    assert.equal(t.read("curriculum.json"), "other pc", "the retry picks up the other PC's push");
    const kept = fs.readdirSync(path.join(t.conflictsDir, "decks"));
    assert.ok(kept.some((f) => fs.readFileSync(path.join(t.conflictsDir, "decks", f), "utf-8") === "local-edit-older"));
  } finally {
    t.cleanup();
  }
});
