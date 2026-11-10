import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import { createServer, type RunningServer } from "../server/main.ts";
import { handleMcp } from "../server/mcp.ts";
import { toolsFor } from "../server/tools/index.ts";
import { fakeContext } from "./helpers.ts";
import type { KnowledgeMap } from "../shared/protocol.ts";

const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");

function tempRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-purge-"));
  fs.writeFileSync(
    path.join(root, "curriculum.json"),
    JSON.stringify({
      chapters: [
        {
          title: "Demo",
          slug: "demo",
          lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }],
        },
      ],
    }),
  );
  fs.mkdirSync(path.join(root, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(root, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(root, "courses", "demo", "NOTES.md"), "# Notes\n");
  const doc = {
    metadata: { lessonNumber: 1, topic: "Demo", title: "Prima lezione", slug: "t", dateCreated: "", dateModified: "", version: "1" },
    nodes: [{ nodeId: "alpha", title: "A", tier: "axiom", content: "x" }],
  };
  fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-t.html"), `<script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script>`);
  return root;
}

describe("progress-purge at server start", () => {
  let srv: RunningServer;
  let root: string;
  let file: string;

  const P = JSON.stringify({ kind: "practice", course: "demo", nodeRefs: ["demo/t#beta"], outcome: "solved" });
  const C = JSON.stringify({ kind: "check", course: "demo", nodeRefs: ["demo/t#alpha"], correct: true });
  const E = JSON.stringify({ kind: "exercise", course: "demo", nodeRefs: ["demo/t#gamma"], correct: true });

  const start = () =>
    createServer({
      repoRoot: root,
      dataRoot: root,
      port: 0,
      openBrowser: false,
      claudeBin: process.execPath,
      claudeBinArgs: [FAKE],
      addDirs: [],
    });

  before(async () => {
    root = tempRepo();
    file = path.join(root, "courses", "demo", "progress.jsonl");
    fs.writeFileSync(file, P + "\n" + C + "\r\n" + E + "\n" + "not json\n", "utf-8");
    srv = await start();
  });

  after(async () => {
    if (srv) await srv.close();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it("server start keeps only lesson checks and malformed lines (AC2)", async () => {
    assert.equal(fs.readFileSync(file, "utf-8"), C + "\r\n" + "not json\n");
    const res = await fetch(srv.url + "/api/map");
    assert.equal(res.status, 200);
  });

  it("second start is byte-identical (AC3)", async () => {
    const bytesBefore = fs.readFileSync(file);
    await srv.close();
    srv = await start();
    const bytesAfter = fs.readFileSync(file);
    assert.deepEqual(bytesAfter, bytesBefore);
    assert.equal(fs.readFileSync(file, "utf-8"), C + "\r\n" + "not json\n");
    const demoFiles = fs.readdirSync(path.join(root, "courses", "demo"));
    assert.ok(!demoFiles.some((f) => f.endsWith(".tmp")));
  });

  it("start without progress.jsonl is fine", async () => {
    fs.mkdirSync(path.join(root, "courses", "other"), { recursive: true });
    await srv.close();
    srv = await start();
    assert.ok(srv.url);
    const res = await fetch(srv.url + "/api/map");
    assert.equal(res.status, 200);
  });

  it("concluding a practice problem and running an exercise in a session leave progress.jsonl unchanged and GET /api/map reports the same node status (AC1)", async () => {
    const bytesBefore = fs.readFileSync(file);
    const mapResBefore = await fetch(srv.url + "/api/map");
    const mapBefore = (await mapResBefore.json()) as KnowledgeMap;
    assert.equal(mapBefore.nodes.length, 1);
    assert.equal(mapBefore.nodes[0].key, "demo/t#alpha");
    assert.equal(mapBefore.nodes[0].status, "solid");
    assert.equal(mapBefore.nodes[0].attempts, 1);

    const ctx = fakeContext(root, { kind: "practice", course: "demo", model: "haiku" });
    const tools = toolsFor({ kind: "practice", course: "demo", model: "haiku" }, root);
    const call = async (name: string, args: unknown) => {
      const res: any = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } }, tools, ctx);
      return { isError: res.result.isError as boolean, text: res.result.content[0].text as string };
    };

    const prob = { statement: "S", mode: "recall", nodeKeys: ["demo/t#alpha"], modelSolution: "M" };
    const concl = { outcome: "solvedWithHints", verdict: "ok", rung: "R2", missClass: null, retest: false };
    assert.equal((await call("problem", prob)).isError, false);
    assert.equal((await call("conclude", concl)).isError, false);
    assert.deepEqual(ctx.progress, []);

    await call("run_exercise", { name: "01-missing" });
    assert.deepEqual(ctx.progress, []);

    const runResult = {
      ok: false,
      passed: 0,
      failed: 1,
      total: 1,
      failures: [{ name: "t1", message: "err" }],
      durationMs: 10,
      at: new Date().toISOString(),
    };
    await srv.manager.reportExercise("dummy-session", "demo", "01-missing", runResult);
    await srv.manager.reportHint("dummy-session", {
      course: "demo",
      name: "01-missing",
      dir: "courses/demo/exercises/01-missing",
      files: [],
      hintsRevealed: 1,
      hints: ["h1"],
    });

    const bytesAfter = fs.readFileSync(file);
    assert.deepEqual(bytesAfter, bytesBefore);

    const mapResAfter = await fetch(srv.url + "/api/map");
    const mapAfter = (await mapResAfter.json()) as KnowledgeMap;
    assert.deepEqual(mapAfter, mapBefore);
    assert.equal(mapAfter.nodes[0].status, "solid");
    assert.equal(mapAfter.nodes[0].attempts, 1);
  });
});
