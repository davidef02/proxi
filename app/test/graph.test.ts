import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { ensureLayout, graphDigest, mergeFindings, nextRevision, readPositions, readSnapshot, savePositions, validateFindings } from "../server/graph/store.ts";
import { incrementalLayout, simplifyChains, seededRandom, type GraphChain } from "../shared/graph/index.ts";

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "learner-graph-"));
const F = (props: [string, string][], chains: [string[], string][] = []) => ({
  propositions: props.map(([id, name]) => ({ id, name, statement: name + " vale." })),
  chains: chains.map(([premises, conclusion]) => ({ premises, conclusion })),
});

test("validateFindings reports every problem", () => {
  const errs = validateFindings(
    {
      propositions: [
        { id: "x", name: "", statement: "s", linkTo: "n_nope" },
        { id: "p1", name: "a", statement: "b" },
      ],
      chains: [
        { premises: [], conclusion: "p1" },
        { premises: ["p1", "p9"], conclusion: "p1" },
      ],
    },
    new Set(),
  );
  assert.ok(errs.length >= 5, errs.join("|"));
  assert.deepEqual(validateFindings(F([["p1", "a"], ["p2", "b"]], [[["p1"], "p2"]]), new Set()), []);
});

test("validateFindings: no new idea outside a chain; listed isolated ideas must be connected (directly or via linkTo)", () => {
  const lone = validateFindings(F([["p1", "a"], ["p2", "b"], ["p3", "c"]], [[["p1"], "p2"]]), new Set());
  assert.equal(lone.length, 1);
  assert.match(lone[0], /"p3" non è in nessuna catena/);
  const ids = new Set(["n_old", "n_gone"]);
  assert.match(validateFindings(F([["p1", "a"], ["p2", "b"]], [[["p1"], "p2"]]), ids, ["n_old"])[0], /"n_old" è ancora isolata/);
  assert.deepEqual(validateFindings(F([["p1", "a"]], [[["n_old"], "p1"]]), ids, ["n_old", "n_deleted"]), []);
  const viaLink = { propositions: [{ id: "p1", name: "a", statement: "a", linkTo: "n_old" }, { id: "p2", name: "b", statement: "b" }], chains: [{ premises: ["p1"], conclusion: "p2" }] };
  assert.deepEqual(validateFindings(viaLink, ids, ["n_old"]), []);
});

test("simplifyChains keeps a redundant chain when it is the only home of a premise", () => {
  const ch = (id: string, premises: string[], conclusion: string, at: string): GraphChain => ({ id, premises, conclusion, createdAt: at, provenance: [] });
  assert.deepEqual(simplifyChains([ch("a1", ["A"], "C", "1"), ch("a2", ["A", "B"], "C", "2")]).map((c) => c.id), ["a1", "a2"]);
  assert.deepEqual(simplifyChains([ch("a1", ["A"], "C", "1"), ch("a2", ["A", "B"], "C", "2"), ch("a3", ["B"], "D", "3")]).map((c) => c.id), ["a1", "a3"]);
});

test("merge creates, links, dedupes chains with provenance union, simplifies", async () => {
  const root = tmp();
  const r1 = await mergeFindings(root, "c/0001-a", F([["p1", "A"], ["p2", "B"], ["p3", "C"]], [[["p1"], "p3"], [["p1", "p2"], "p3"]]));
  assert.equal(r1.added, 3);
  assert.equal(r1.chains, 2);
  let snap = await readSnapshot(root);
  // [p1,p2]=>p3 is redundant given [p1]=>p3 (strict superset of premises), but it is B's only chain: kept (no isolated idea)
  assert.equal(snap.chains.length, 2);
  const a = snap.propositions.find((n) => n.name === "A")!;
  const c = snap.propositions.find((n) => n.name === "C")!;
  const r2 = await mergeFindings(root, "c/0002-b", {
    propositions: [
      { id: "p1", name: "A bis", statement: "A riformulato", linkTo: a.id, nodeId: "n7" },
      { id: "p2", name: "D", statement: "d" },
    ],
    chains: [
      { premises: [a.id], conclusion: c.id },
      { premises: ["p1"], conclusion: "p2" },
    ],
  });
  assert.deepEqual(r2, { added: 1, linked: 1, chains: 2 });
  snap = await readSnapshot(root);
  assert.equal(snap.propositions.length, 4);
  const linked = snap.propositions.find((n) => n.id === a.id)!;
  assert.deepEqual(linked.provenance.map((p) => p.chatId), ["c/0001-a", "c/0002-b"]);
  assert.ok(linked.aliases.includes("A riformulato"));
  assert.equal(linked.name, "A");
  const same = snap.chains.find((ch) => ch.conclusion === c.id)!;
  assert.equal(same.provenance.length, 2);
  assert.ok((await graphDigest(root)).includes(`${a.id} | A | `));
  assert.ok(Object.keys(snap.positions ?? {}).length >= 4);
});

test("corrupt and unterminated ledger lines are skipped, appends stay on their own line", async () => {
  const root = tmp();
  await mergeFindings(root, "c/1", F([["p1", "A"]]));
  const f = path.join(root, "graph", "nodes.jsonl");
  fs.appendFileSync(f, '{not json\n{"id":3}\n{"id":"n_x","name":"cut');
  await mergeFindings(root, "c/2", F([["p1", "B"]]));
  const snap = await readSnapshot(root);
  assert.deepEqual(snap.propositions.map((n) => n.name).sort(), ["A", "B"]);
});

test("concurrent merges are serialized (no lost lines)", async () => {
  const root = tmp();
  await Promise.all([1, 2, 3, 4, 5].map((i) => mergeFindings(root, `c/${i}`, F([["p1", "N" + i]]))));
  assert.equal((await readSnapshot(root)).propositions.length, 5);
});

test("simplifyChains: ancestry cover", () => {
  const ch = (id: string, premises: string[], conclusion: string, createdAt: string): GraphChain => ({ id, premises, conclusion, createdAt, provenance: [] });
  const kept = simplifyChains([ch("a1", ["A", "B"], "X", "1"), ch("a2", ["X", "C"], "Y", "2"), ch("a3", ["A", "B", "C"], "Y", "3")]);
  assert.deepEqual(kept.map((c) => c.id), ["a1", "a2"]);
});

test("layout is deterministic for the same input", () => {
  const nodes = ["n_a", "n_b", "n_c", "n_d"].map((id) => ({ id }));
  const edges = [
    { from: "n_a", to: "n_b" },
    { from: "n_b", to: "n_c" },
    { from: "n_c", to: "n_d" },
  ];
  const a = incrementalLayout(nodes, edges, new Map());
  const b = incrementalLayout(nodes, edges, new Map());
  assert.deepEqual([...a], [...b]);
  assert.equal(seededRandom("x")(), seededRandom("x")());
  assert.notEqual(seededRandom("x")(), seededRandom("y")());
});

test("a layout failure does not fail the merge; ensureLayout rebuilds; positions ignore unknown ids", async () => {
  const root = tmp();
  fs.mkdirSync(path.join(root, "graph", "layout.json"), { recursive: true }); // writes to it throw
  const r = await mergeFindings(root, "c/1", F([["p1", "A"], ["p2", "B"]], [[["p1"], "p2"]]));
  assert.deepEqual([r.added, r.chains], [2, 1]);
  fs.rmSync(path.join(root, "graph", "layout.json"), { recursive: true });
  await ensureLayout(root);
  const snap = await readSnapshot(root);
  assert.ok(snap.nodes.every((n) => snap.positions?.[n.id]));
  await savePositions(root, { ghost: { x: 1, y: 2 }, [snap.propositions[0].id]: { x: 5, y: 6 } });
  const after = readPositions(root);
  assert.equal("ghost" in after, false);
  assert.deepEqual(after[snap.propositions[0].id], { x: 5, y: 6 });
});

test("a new lesson settles into its own island, not on top of the others", async () => {
  const root = tmp();
  const lesson = (k: string) => F([1, 2, 3, 4, 5, 6].map((i) => [`p${i}`, `${k}${i}`] as [string, string]), [[["p1"], "p2"], [["p2"], "p3"], [["p4"], "p5"], [["p5"], "p6"]]);
  for (const k of ["a", "b", "c"]) await mergeFindings(root, `x/000${k}`, lesson(k));
  const snap = await readSnapshot(root);
  const centroids = snap.clusters.map((c) => {
    const ps = c.members.map((m) => snap.positions![m]);
    return { x: ps.reduce((a, p) => a + p.x, 0) / ps.length, y: ps.reduce((a, p) => a + p.y, 0) / ps.length };
  });
  assert.equal(centroids.length, 3);
  for (let i = 0; i < 3; i++) for (let j = i + 1; j < 3; j++) assert.ok(Math.hypot(centroids[i].x - centroids[j].x, centroids[i].y - centroids[j].y) > 60);
});

test("graphDigest groups by lesson; revisions only grow", async () => {
  const root = tmp();
  await mergeFindings(root, "c/0001-a", F([["p1", "Uno"]]));
  await mergeFindings(root, "c/0002-b", F([["p1", "Due"]]));
  const d = await graphDigest(root);
  assert.match(d, /^## c\/0001-a\nn_\w+ \| Uno \| Uno vale\.\n\n## c\/0002-b\nn_\w+ \| Due/);
  const a = nextRevision();
  assert.ok(nextRevision() > a);
});

test("layered layout: premises below conclusions, one column block per lesson, courses side by side", async () => {
  const root = tmp();
  await mergeFindings(root, "a/0001-x", F([["p1", "A"], ["p2", "B"], ["p3", "C"]], [[["p1", "p2"], "p3"]]));
  const s1 = await readSnapshot(root);
  const id = (name: string) => s1.propositions.find((n) => n.name === name)!.id;
  await mergeFindings(root, "a/0002-y", { propositions: [{ id: "p1", name: "D", statement: "D." }], chains: [{ premises: [id("C")], conclusion: "p1" }] });
  await mergeFindings(root, "b/0001-z", F([["p1", "E"], ["p2", "G"]], [[["p1"], "p2"]]));
  const snap = await readSnapshot(root);
  const at = (name: string) => snap.positions![snap.propositions.find((n) => n.name === name)!.id];
  assert.ok(at("C").y < at("A").y && at("A").y === at("B").y, "a conclusion sits a row above its premises (y grows down)");
  assert.ok(at("D").y < at("C").y, "a later lesson building on C rises above it");
  assert.ok(at("D").x > Math.max(at("A").x, at("B").x, at("C").x), "lesson 2 has its own column block to the right");
  const xs = (course: string) => snap.courses.find((c) => c.id === course)!.members.map((m) => snap.positions![m].x);
  assert.ok(Math.min(...xs("b")) > Math.max(...xs("a")) + 100, "course frames never overlap");
  assert.deepEqual(snap.courses.map((c) => c.id), ["a", "b"]);
});

test("repair: an isolated idea can be removed (only a listed one), and the merge drops it", async () => {
  const root = tmp();
  await mergeFindings(root, "a/0001-x", F([["p1", "Sola"], ["p2", "A"], ["p3", "B"]], [[["p2"], "p3"]]));
  const lone = (await readSnapshot(root)).propositions.find((n) => n.name === "Sola")!.id;
  const ids = new Set([lone, "n_other"]);
  assert.deepEqual(validateFindings({ propositions: [], chains: [], remove: [lone] }, ids, [lone]), []);
  assert.match(validateFindings({ propositions: [], chains: [], remove: ["n_other"] }, ids, [lone]).join("|"), /non è tra le idee isolate/);
  await mergeFindings(root, "a/0001-x", { propositions: [], chains: [], remove: [lone] });
  assert.ok(!(await readSnapshot(root)).propositions.some((n) => n.id === lone));
});

test("keyConcepts: tops of each course's own chains, a later course reusing them does not demote them; at least three", async () => {
  const { keyConcepts } = await import("../shared/graph/index.ts");
  const root = tmp();
  await mergeFindings(root, "a/0001-x", F([["p1", "A1"], ["p2", "A2"], ["p3", "A3"], ["p4", "A4"]], [[["p1"], "p2"], [["p2"], "p3"], [["p3"], "p4"]]));
  const s1 = await readSnapshot(root);
  const a4 = s1.propositions.find((n) => n.name === "A4")!.id;
  await mergeFindings(root, "b/0001-y", { propositions: [{ id: "p1", name: "B1", statement: "B1." }], chains: [{ premises: [a4], conclusion: "p1" }] });
  const snap = await readSnapshot(root);
  const names = (ids: Set<string>) => snap.propositions.filter((n) => ids.has(n.id)).map((n) => n.name).sort();
  const keys = keyConcepts(snap);
  assert.ok(keys.has(a4), "A4 stays course a's top even though course b builds on it");
  assert.equal(names(keys).filter((n) => n.startsWith("A")).length, 3);
  assert.deepEqual(names(keys).filter((n) => n.startsWith("B")), ["B1"]);
  const at = (name: string) => snap.positions![snap.propositions.find((n) => n.name === name)!.id];
  assert.ok(at("B1").y < at("A4").y, "a conclusion in course b sits above its course-a premise (global bands)");
});

test("keyConcepts: selects landmarks per lesson so shallow-chain lessons are not starved by deep ones", async () => {
  const { keyConcepts } = await import("../shared/graph/index.ts");
  const root = tmp();
  // Lesson 1: deep chain (p1 -> p2 -> p3 -> p4 -> p5)
  await mergeFindings(
    root,
    "course1/0001-deep",
    F(
      [["p1", "D1"], ["p2", "D2"], ["p3", "D3"], ["p4", "D4"], ["p5", "D5"]],
      [[["p1"], "p2"], [["p2"], "p3"], [["p3"], "p4"], [["p4"], "p5"]]
    )
  );
  // Lesson 2: shallow chain (max depth 1, e.g. s1 -> s2, s3 -> s4)
  await mergeFindings(
    root,
    "course1/0002-shallow",
    F(
      [["p1", "S1"], ["p2", "S2"], ["p3", "S3"], ["p4", "S4"]],
      [[["p1"], "p2"], [["p3"], "p4"]]
    )
  );
  const snap = await readSnapshot(root);
  const keys = keyConcepts(snap);
  const names = (ids: Set<string>) => snap.propositions.filter((n) => ids.has(n.id)).map((n) => n.name);
  const keyNames = names(keys);
  assert.ok(keyNames.some((n) => n.startsWith("S")), "shallow-chain lesson has at least one landmark");
  assert.ok(keyNames.some((n) => n.startsWith("D")), "deep-chain lesson has landmarks");
});
