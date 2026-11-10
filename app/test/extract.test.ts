import { describe, it, after } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { Hono } from "hono";
import { anchorsByNode, appendCoverage, listSources, makeMapRole, mountExtract, readCoverage, readLessonText, startMapJob, withAnchorChains, type ExtractStore } from "../server/graph/extract.ts";
import { emitGlobal, subscribeGlobal } from "../server/events.ts";
import { handleMcp } from "../server/mcp.ts";
import { modelFor } from "../server/engine.ts";
import type { Findings, JobContext } from "../server/types.ts";
import type { GlobalEvent, JobResult } from "../shared/protocol.ts";
import { DEFAULT_SETTINGS } from "../shared/protocol.ts";

const dirs: string[] = [];
after(() => dirs.forEach((d) => fs.rmSync(d, { recursive: true, force: true })));

function island(nodes: unknown[], title = "Il ref è una scatola") {
  const doc = { metadata: { lessonNumber: 1, topic: "t", title, slug: "s", dateCreated: "", dateModified: "", version: "1" }, nodes };
  return `<html><head><title>x</title></head><body><script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script><p>chrome</p></body></html>`;
}
const NODE = {
  nodeId: "ref-scatola",
  title: "Il ref",
  tier: "axiom",
  content: "<p>Un <code>ref</code> &egrave; una scatola &amp; basta.</p>",
  quiz: { question: "Che cosa è un ref?", options: ["a", "una scatola"], correctIndex: 2, explanation: "e" },
};
const NODE_A = { ...NODE, anchors: [{ id: "n_aaa111", name: "Ref" }, { id: "n_gone", name: "Sparito" }] };

function repo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-extract-"));
  dirs.push(root);
  const l = path.join(root, "courses", "demo", "lessons");
  fs.mkdirSync(l, { recursive: true });
  fs.writeFileSync(path.join(l, "0001-il-ref.html"), island([NODE]));
  fs.writeFileSync(path.join(l, "0002-prosa.html"), "<html><head><title>Lezione in prosa</title><style>p{}</style></head><body><h1>Titolo</h1><p>Le props scendono.</p></body></html>");
  return root;
}

function fake(opts: { errors?: string[]; digest?: string } = {}) {
  const calls: { merged: Findings[]; existing: Set<string>[] } = { merged: [], existing: [] };
  const store: ExtractStore = {
    graphDigest: async () => opts.digest ?? "n_aaa111 | Ref | Un ref è una scatola\nn_bbb222 | Prop | Le props scendono",
    validateFindings: (_f, ids) => (calls.existing.push(ids), opts.errors ?? []),
    mergeFindings: async (_r, _s, f, afterWrite) => (
      calls.merged.push(f),
      afterWrite?.(),
      { added: f.propositions.filter((p) => !p.linkTo).length, linked: f.propositions.filter((p) => p.linkTo).length, chains: f.chains.length }
    ),
  };
  return { store, calls };
}
let jobSeq = 0;
function jobCtx(root: string) {
  const out: { finished?: JobResult } = {};
  const ctx: JobContext = { repoRoot: root, dataRoot: root, jobId: "j" + ++jobSeq, setActivity() {}, finish: (r) => void (out.finished = r) };
  return { ctx, out };
}

describe("sources and coverage", () => {
  it("lists lessons, uncovered at first; reads node and prose lessons as text", () => {
    const root = repo();
    const s = listSources(root);
    assert.deepEqual(s.map((x) => [x.id, x.covered, x.stale]), [["demo/0001-il-ref", false, false], ["demo/0002-prosa", false, false]]);
    assert.equal(s[0].lessonTitle, "Il ref è una scatola");
    assert.equal(s[0].lessonPath, "courses/demo/lessons/0001-il-ref.html");
    assert.equal(s[1].lessonTitle, "Lezione in prosa");
    assert.equal(s[1].lessonNumber, 2);

    const t = readLessonText(root, "demo/0001-il-ref")!;
    assert.match(t.text, /Un ref è una scatola & basta\./);
    assert.match(t.text, /Risposta corretta: una scatola/);
    assert.doesNotMatch(t.text, /chrome/);
    assert.match(readLessonText(root, "demo/0002-prosa")!.text, /Le props scendono/);
    assert.equal(readLessonText(root, "../etc/passwd"), null);
  });

  it("coverage: corrupt lines skipped, stale after the lesson changes, last entry wins", async () => {
    const root = repo();
    const [submit] = makeMapRole(root, fake().store).tools({ source: "demo/0001-il-ref" });
    await submit.run({ propositions: [], chains: [] }, jobCtx(root).ctx);
    fs.appendFileSync(path.join(root, "graph", "coverage.jsonl"), "{not json\n");
    assert.equal(readCoverage(root).size, 1);
    let s = listSources(root);
    assert.deepEqual([s[0].covered, s[0].stale, s[1].covered], [true, false, false]);
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([{ ...NODE, content: "<p>cambiato</p>" }]));
    s = listSources(root);
    assert.deepEqual([s[0].covered, s[0].stale], [true, true]);
    await submit.run({ propositions: [], chains: [] }, jobCtx(root).ctx); // re-extraction: fingerprint refreshed
    assert.equal(listSources(root)[0].stale, false);
  });

  it("a covered lesson with an isolated idea is pending again, and its re-extraction is told to connect it", async () => {
    const root = repo();
    const { mergeFindings } = await import("../server/graph/store.ts");
    const src = "demo/0001-il-ref";
    await mergeFindings(root, src, { propositions: [{ id: "p1", name: "Sola", statement: "Sola." }, { id: "p2", name: "A", statement: "A." }, { id: "p3", name: "B", statement: "B." }], chains: [{ premises: ["p2"], conclusion: "p3" }] });
    appendCoverage(root, src, "fp");
    const s = listSources(root).find((x) => x.id === src)!;
    assert.equal(s.isolated, 1);
    const input: { source: string; isolated?: string[] } = { source: src };
    const msg = makeMapRole(root, fake().store).firstMessage(input);
    assert.match(msg, /isolate[\s\S]*\| Sola/);
    assert.equal(input.isolated?.length, 1);
  });
});

describe("map role", () => {
  it("is configured as the plan says and builds its prompts", async () => {
    const root = repo();
    const role = makeMapRole(root, fake().store);
    assert.deepEqual([role.kind, role.grant, role.maxTurns], ["map", [], 6]);
    assert.equal(modelFor({ ...DEFAULT_SETTINGS, mapModel: "haiku" }, "claude", "map"), "haiku");
    assert.equal(modelFor({ ...DEFAULT_SETTINGS, mapModel: "haiku" }, "agy", "map"), DEFAULT_SETTINGS.agyModels.map);
    const sys = await role.systemPrompt({ source: "demo/0001-il-ref" }, root);
    assert.match(sys, /proposizioni/);
    assert.match(sys, /n_aaa111 \| Ref \| Un ref/);
    const first = role.firstMessage({ source: "demo/0001-il-ref" });
    assert.match(first, /Il ref è una scatola/);
    assert.match(first, /Nodo ref-scatola/);
    assert.match(first, /Risposta corretta: una scatola/);
  });

  it("rejects invalid findings with isError, then merges, records coverage, emits graph and finishes", async () => {
    const root = repo();
    const f: Findings = {
      propositions: [{ id: "p1", name: "Ref", statement: "Un ref è una scatola.", linkTo: "n_aaa111" }, { id: "p2", name: "Nuovo", statement: "Altro." }],
      chains: [{ premises: ["p2", "n_bbb222"], conclusion: "p1" }],
    };
    const { ctx, out } = jobCtx(root);
    const events: GlobalEvent[] = [];
    const un = subscribeGlobal((e) => events.push(e));
    const [t1] = makeMapRole(root, fake({ errors: ["linkTo non esiste"] }).store).tools({ source: "demo/0001-il-ref" });
    const r1 = await t1.run(f, ctx);
    assert.equal(r1.isError, true);
    assert.match(r1.text, /linkTo non esiste/);
    assert.equal(out.finished, undefined);
    assert.equal(readCoverage(root).size, 0);

    const good = fake();
    const [t2] = makeMapRole(root, good.store).tools({ source: "demo/0001-il-ref" });
    const r2 = await t2.run(f, ctx);
    un();
    assert.equal(r2.isError, undefined);
    assert.deepEqual(out.finished, { kind: "map", source: "demo/0001-il-ref", added: 1, linked: 1, chains: 1 });
    assert.deepEqual([...good.calls.existing[0]].sort(), ["n_aaa111", "n_bbb222"]);
    assert.equal(readCoverage(root).get("demo/0001-il-ref")?.fp.length, 64);
    assert.ok(events.some((e) => e.type === "graph"));
  });

  it("serves submit_findings over MCP with the schema enforced", async () => {
    const root = repo();
    const tools = makeMapRole(root, fake().store).tools({ source: "demo/0001-il-ref" });
    const res: any = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "submit_findings", arguments: { propositions: "no" } } }, tools, jobCtx(root).ctx, "job");
    assert.equal(res.result.isError, true);
  });
});

describe("anchors", () => {
  it("readLessonText shows the anchor names (names only, for the fingerprint)", () => {
    const root = repo();
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([NODE_A]));
    assert.match(readLessonText(root, "demo/0001-il-ref")!.text, /Si appoggia su: Ref; Sparito/);
  });

  it("firstMessage lists the live anchors with their ids, never a dangling one", () => {
    const root = repo();
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([NODE_A]));
    fs.mkdirSync(path.join(root, "graph"), { recursive: true });
    fs.writeFileSync(path.join(root, "graph", "nodes.jsonl"), JSON.stringify({
      id: "n_aaa111", name: "Ref", statement: "Un ref è una scatola.", aliases: [],
      createdAt: "2026-11-01T00:00:00.000Z", provenance: [{ chatId: "demo/0000-seed", messageId: "m1" }],
    }) + "\n");
    const msg = makeMapRole(root, fake().store).firstMessage({ source: "demo/0001-il-ref" });
    assert.match(msg, /nodo ref-scatola → n_aaa111 \| Ref/);
    assert.doesNotMatch(msg, /n_gone/);
  });

  it("firstMessage keeps the anchors block on the isolated-repair path", async () => {
    const root = repo();
    const src = "demo/0001-il-ref";
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([NODE_A]));
    fs.mkdirSync(path.join(root, "graph"), { recursive: true });
    fs.writeFileSync(path.join(root, "graph", "nodes.jsonl"), JSON.stringify({
      id: "n_aaa111", name: "Ref", statement: "Un ref è una scatola.", aliases: [],
      createdAt: "2026-11-01T00:00:00.000Z", provenance: [{ chatId: "demo/0000-seed", messageId: "m1" }],
    }) + "\n");
    const { mergeFindings } = await import("../server/graph/store.ts");
    await mergeFindings(root, src, {
      propositions: [{ id: "p1", name: "Sola", statement: "Sola." }, { id: "p2", name: "A", statement: "A." }, { id: "p3", name: "B", statement: "B." }],
      chains: [{ premises: ["p2"], conclusion: "p3" }],
    });
    const msg = makeMapRole(root, fake().store).firstMessage({ source: src });
    assert.match(msg, /Ancore dichiarate dal tutor/);
    assert.match(msg, /nodo ref-scatola → n_aaa111 \| Ref/);
    assert.doesNotMatch(msg, /n_gone/);
    assert.match(msg, /già nella mappa ma isolate[\s\S]*\| Sola/);
  });

  it("anchorsByNode is an empty map for a missing or unreadable lesson", () => {
    const root = repo();
    assert.equal(anchorsByNode(root, "demo/nope").size, 0);
    assert.equal(anchorsByNode(root, "../etc/passwd").size, 0);
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([NODE_A]));
    assert.deepEqual([...anchorsByNode(root, "demo/0001-il-ref")], [["ref-scatola", NODE_A.anchors]]);
  });

  it("withAnchorChains adds each live anchor chain once and leaves the input alone", () => {
    const f: Findings = { propositions: [{ id: "p1", name: "N", statement: "S", nodeId: "ref-scatola" }], chains: [{ premises: ["n_bbb222"], conclusion: "p1" }] };
    const anchors = new Map<string, { id: string; name: string }[]>([
      ["ref-scatola", [{ id: "n_aaa111", name: "Ref" }, { id: "n_gone", name: "Sparito" }, { id: "n_bbb222", name: "Prop" }]],
      ["nodo-mai-estratto", [{ id: "n_aaa111", name: "Ref" }]],
    ]);
    const g = withAnchorChains(f, anchors, new Set(["n_aaa111", "n_bbb222"]));
    assert.deepEqual(g.chains, [
      { premises: ["n_bbb222"], conclusion: "p1" }, // the chain was already there
      { premises: ["n_aaa111"], conclusion: "p1" }, // synthesized; n_gone is not live
    ]);
    assert.deepEqual(f.chains, [{ premises: ["n_bbb222"], conclusion: "p1" }]);
    assert.notEqual(g, f);
  });

  it("withAnchorChains does not repeat the linkTo the proposition already has", () => {
    const f: Findings = {
      propositions: [{ id: "p1", name: "N", statement: "S", nodeId: "ref-scatola", linkTo: "n_aaa111" }],
      chains: [{ premises: ["n_bbb222"], conclusion: "p1" }],
    };
    const g = withAnchorChains(f, new Map([["ref-scatola", [{ id: "n_aaa111", name: "Ref" }]]]), new Set(["n_aaa111"]));
    assert.deepEqual(g.chains, [{ premises: ["n_bbb222"], conclusion: "p1" }]);
  });

  it("withAnchorChains attaches to the node's first proposition, once", () => {
    const f: Findings = {
      propositions: [
        { id: "p1", name: "N", statement: "S", nodeId: "ref-scatola" },
        { id: "p2", name: "N di nuovo", statement: "S2", nodeId: "ref-scatola" },
      ],
      chains: [],
    };
    const g = withAnchorChains(f, new Map([["ref-scatola", [{ id: "n_aaa111", name: "Ref" }]]]), new Set(["n_aaa111"]));
    assert.deepEqual(g.chains, [{ premises: ["n_aaa111"], conclusion: "p1" }]);
  });

  it("submit_findings adds the anchor chain before merging", async () => {
    const root = repo();
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([NODE_A]));
    const { store, calls } = fake();
    const [submit] = makeMapRole(root, store).tools({ source: "demo/0001-il-ref" });
    const r = await submit.run({
      propositions: [{ id: "p1", name: "N", statement: "S", nodeId: "ref-scatola" }, { id: "p2", name: "M", statement: "M." }],
      chains: [{ premises: ["p2"], conclusion: "p1" }],
    }, jobCtx(root).ctx);
    assert.equal(r.isError, undefined);
    assert.ok(calls.merged[0].chains.some((c) => c.premises.includes("n_aaa111") && c.conclusion === "p1"));
    assert.ok(!calls.merged[0].chains.some((c) => c.premises.includes("n_gone")));
  });

  it("AC3: a declared anchor becomes a chain from the live concept to the new one", async () => {
    const root = repo();
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([NODE_A]));
    const { mergeFindings, readChainLines, readNodeLines, readSnapshot } = await import("../server/graph/store.ts");
    await mergeFindings(root, "demo/0000-seed", {
      propositions: [{ id: "p1", name: "Ref", statement: "Un ref è una scatola." }, { id: "p2", name: "Base", statement: "B." }],
      chains: [{ premises: ["p2"], conclusion: "p1" }],
    });
    const seedId = readNodeLines(root).find((n) => n.name === "Ref")!.id;
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([{ ...NODE_A, anchors: [{ id: seedId, name: "Ref" }] }]));

    // no anchor chain submitted: the merge has to synthesize it
    const [submit] = makeMapRole(root).tools({ source: "demo/0001-il-ref" });
    const r = await submit.run({
      propositions: [{ id: "p1", name: "Nuovo", statement: "Un nuovo nodo.", nodeId: "ref-scatola" }, { id: "p2", name: "M", statement: "M." }],
      chains: [{ premises: ["p2"], conclusion: "p1" }],
    }, jobCtx(root).ctx);
    assert.equal(r.isError, undefined);
    const newId = readNodeLines(root).find((n) => n.name === "Nuovo")!.id;
    assert.ok(readChainLines(root).some((c) => c.premises.includes(seedId) && c.conclusion === newId));
    const snap = await readSnapshot(root);
    assert.ok(snap.edges.some((e) => e.from === seedId && e.to === newId));
  });

  it("AC4: a deleted anchored concept degrades to text", async () => {
    const root = repo();
    const src = "demo/0001-il-ref";
    const { deleteNode, mergeFindings, readChainLines, readNodeLines, readSnapshot } = await import("../server/graph/store.ts");
    await mergeFindings(root, "demo/0000-seed", {
      propositions: [{ id: "p1", name: "Ref", statement: "Un ref è una scatola." }, { id: "p2", name: "Base", statement: "B." }],
      chains: [{ premises: ["p2"], conclusion: "p1" }],
    });
    const seedId = readNodeLines(root).find((n) => n.name === "Ref")!.id;
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([{ ...NODE_A, anchors: [{ id: seedId, name: "Ref" }] }]));

    // the first map pass really linked the node to the concept
    const [first] = makeMapRole(root).tools({ source: src });
    const r1 = await first.run({
      propositions: [{ id: "p1", name: "Nuovo", statement: "Un nuovo nodo.", nodeId: "ref-scatola" }, { id: "p2", name: "M", statement: "M." }],
      chains: [{ premises: ["p2"], conclusion: "p1" }],
    }, jobCtx(root).ctx);
    assert.equal(r1.isError, undefined);
    assert.ok(readChainLines(root).some((c) => c.premises.includes(seedId)));

    await deleteNode(root, seedId);

    const text = readLessonText(root, src);
    assert.ok(text);
    assert.match(text.text, /Si appoggia su: Ref/);
    const snap = await readSnapshot(root);
    assert.ok(!snap.edges.some((e) => e.from === seedId || e.to === seedId));
    assert.ok(!makeMapRole(root).firstMessage({ source: src }).includes(seedId));

    const [again] = makeMapRole(root).tools({ source: src });
    const r2 = await again.run({
      propositions: [{ id: "p1", name: "N2", statement: "S2", nodeId: "ref-scatola" }, { id: "p2", name: "M", statement: "M." }],
      chains: [{ premises: ["p2"], conclusion: "p1" }],
    }, jobCtx(root).ctx);
    assert.equal(r2.isError, undefined);
    assert.ok(!readChainLines(root).some((c) => c.premises.includes(seedId) || c.conclusion === seedId));
  });
});

describe("routes", () => {
  it("GET sources; POST extract starts one job per uncovered/stale lesson (skips running ones, 400 on unknown)", async () => {
    const root = repo();
    const started: { kind: string; input: any; title: string }[] = [];
    const registered: string[] = [];
    const jobs: any = {
      register: (r: any) => registered.push(r.kind),
      start: (kind: string, input: any, o: any) => (started.push({ kind, input, title: o.title }), { id: "j" + started.length, kind, title: o.title, status: "running" }),
      list: () => [],
    };
    const app = new Hono();
    mountExtract(app, { repoRoot: root, jobs, store: fake().store });
    assert.deepEqual(registered, ["map"]);
    assert.equal((await (await app.request("/api/graph/sources")).json()).length, 2);
    appendCoverage(root, "demo/0002-prosa", "stale-fp");
    const post = (body?: unknown) => app.request("/api/graph/extract", { method: "POST", body: JSON.stringify(body ?? {}), headers: { "content-type": "application/json" } });
    const r = await (await post()).json();
    assert.deepEqual(started.map((s) => s.input.source), ["demo/0001-il-ref", "demo/0002-prosa"]);
    assert.equal(r.jobs.length, 2);
    assert.equal(started[0].title, "Mappa: Il ref è una scatola");
    for (const id of ["j1", "j2"]) emitGlobal({ type: "job", job: { id, kind: "map", status: "done" } as any });
    started.length = 0;
    assert.equal((await post({ sources: ["demo/0001-il-ref"] })).status, 200);
    assert.equal(started.length, 1);
    assert.equal((await post({ sources: ["demo/nope"] })).status, 400);
    // still running (any session, any title): not started twice
    started.length = 0;
    await post({ sources: ["demo/0001-il-ref"] });
    assert.equal(started.length, 0);
    // the job's final event frees the source
    emitGlobal({ type: "job", job: { id: "j1", kind: "map", status: "done" } as any });
    await post({ sources: ["demo/0001-il-ref"] });
    assert.equal(started.length, 1);
    emitGlobal({ type: "job", job: { id: "j1", kind: "map", status: "done" } as any });
  });
});

describe("review fixes", () => {
  it("submit_findings is idempotent per job (a retry does not merge twice)", async () => {
    const root = repo();
    const { store, calls } = fake();
    const [submit] = makeMapRole(root, store).tools({ source: "demo/0001-il-ref" });
    const { ctx } = jobCtx(root);
    const f: Findings = { propositions: [{ id: "p1", name: "A", statement: "a." }], chains: [] };
    const a = await submit.run(f, ctx);
    const b = await submit.run(f, ctx);
    assert.equal(calls.merged.length, 1);
    assert.deepEqual(a, b);
  });

  it("coverage records the fingerprint of the text the extractor saw, so an edit mid-job reads stale", async () => {
    const root = repo();
    const role = makeMapRole(root, fake().store);
    const input: { source: string; fp?: string } = { source: "demo/0001-il-ref" };
    role.firstMessage(input);
    assert.equal(input.fp?.length, 64);
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-il-ref.html"), island([{ ...NODE, content: "<p>cambiato durante il job</p>" }]));
    await role.tools(input)[0].run({ propositions: [], chains: [] }, jobCtx(root).ctx);
    assert.equal(listSources(root)[0].stale, true);
  });

  it("startMapJob dedupes by source across every job until its final event", () => {
    const root = repo();
    const started: string[] = [];
    const jobs = { start: (_k: "map", input: { source: string }) => (started.push(input.source), { id: "dd" + started.length, kind: "map", status: "running" } as any) };
    const sources = listSources(root);
    assert.ok(startMapJob(jobs, sources, "demo/0001-il-ref"));
    assert.equal(startMapJob(jobs, sources, "demo/0001-il-ref"), null); // e.g. another session's autoMap
    assert.ok(startMapJob(jobs, sources, "demo/0002-prosa"));
    emitGlobal({ type: "job", job: { id: "dd1", kind: "map", status: "error" } as any });
    assert.ok(startMapJob(jobs, sources, "demo/0001-il-ref"));
    assert.equal(started.length, 3);
  });
});
