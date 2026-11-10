import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it } from "node:test";
import { createServer, type RunningServer } from "../server/main.ts";
import { MockAnki } from "../../tests/mock-anki.ts";
import type { AnkiImportResult, AnkiStatusResponse, KnowledgeMap } from "../shared/protocol.ts";

const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");

function tempRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-anki-import-"));
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
  fs.writeFileSync(
    path.join(root, "courses", "demo", "lessons", "0001-t.html"),
    `<script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script>`,
  );
  return root;
}

describe("POST /api/anki/import route", () => {
  let srv: RunningServer;
  const api = (p: string, body?: unknown) =>
    fetch(srv.url + p, body === undefined ? undefined : { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });

  it("AC1: imports reviews and derives shaky status on node demo/t#alpha", async () => {
    const root = tempRepo();
    const mock = new MockAnki();
    mock.cards = [
      {
        deckName: "Proxi::Demo",
        tags: ["node::alpha"],
        interval: 1,
        lapses: 0,
        reviews: [
          { id: 1700000000000, ease: 3 },
          { id: 1700000100000, ease: 1 },
        ],
      },
    ];
    const url = await mock.start();
    const prevUrl = process.env.LEARNER_ANKI_URL;
    process.env.LEARNER_ANKI_URL = url;
    try {
      srv = await createServer({
        repoRoot: root,
        dataRoot: root,
        port: 0,
        openBrowser: false,
        claudeBin: process.execPath,
        claudeBinArgs: [FAKE],
        addDirs: [],
      });

      const res = await api("/api/anki/import", { course: "demo" });
      assert.equal(res.status, 200);
      const data = (await res.json()) as AnkiImportResult;
      assert.equal(data.online, true);
      assert.equal(data.imported, 2);

      const mapRes = await api("/api/map");
      assert.equal(mapRes.status, 200);
      const map = (await mapRes.json()) as KnowledgeMap;
      const node = map.nodes.find((n) => n.key === "demo/t#alpha");
      assert.ok(node);
      assert.equal(node.status, "shaky");
    } finally {
      if (srv) await srv.close();
      await mock.stop();
      if (prevUrl === undefined) delete process.env.LEARNER_ANKI_URL;
      else process.env.LEARNER_ANKI_URL = prevUrl;
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it("AC6: answers online:false when Anki is down without modifying progress or map", async () => {
    const root = tempRepo();
    const prevUrl = process.env.LEARNER_ANKI_URL;
    process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9";
    try {
      srv = await createServer({
        repoRoot: root,
        dataRoot: root,
        port: 0,
        openBrowser: false,
        claudeBin: process.execPath,
        claudeBinArgs: [FAKE],
        addDirs: [],
      });

      const res = await api("/api/anki/import", { course: "demo" });
      assert.equal(res.status, 200);
      const data = (await res.json()) as AnkiImportResult;
      assert.equal(data.online, false);
      assert.equal(data.imported, 0);
      assert.equal(data.skipped, 0);
      assert.ok(data.error);

      const mapRes = await api("/api/map");
      assert.equal(mapRes.status, 200);
      const map = (await mapRes.json()) as KnowledgeMap;
      const node = map.nodes.find((n) => n.key === "demo/t#alpha");
      assert.ok(node);
      assert.equal(node.status, "taught");

      const ankiRes = await api("/api/anki?course=demo");
      assert.equal(ankiRes.status, 200);
      const ankiData = (await ankiRes.json()) as AnkiStatusResponse;
      assert.equal(ankiData.online, false);

      const progressFile = path.join(root, "courses", "demo", "progress.jsonl");
      assert.equal(fs.existsSync(progressFile), false);
    } finally {
      if (srv) await srv.close();
      if (prevUrl === undefined) delete process.env.LEARNER_ANKI_URL;
      else process.env.LEARNER_ANKI_URL = prevUrl;
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it("triggers import on teach session creation, not on plan session", async () => {
    const root = tempRepo();
    const mock = new MockAnki();
    mock.cards = [
      {
        deckName: "Proxi::Demo",
        tags: ["node::alpha"],
        interval: 10,
        lapses: 0,
        reviews: [{ id: 1700000009999, ease: 3 }],
      },
    ];
    const url = await mock.start();
    const prevUrl = process.env.LEARNER_ANKI_URL;
    process.env.LEARNER_ANKI_URL = url;
    try {
      srv = await createServer({
        repoRoot: root,
        dataRoot: root,
        port: 0,
        openBrowser: false,
        claudeBin: process.execPath,
        claudeBinArgs: [FAKE],
        addDirs: [],
      });

      await srv.manager.create({ kind: "plan", course: "demo", chapterTitle: "Demo", model: "haiku" });
      const progressPath = path.join(root, "courses", "demo", "progress.jsonl");
      assert.equal(fs.existsSync(progressPath), false);

      await srv.manager.create({ kind: "teach", course: "demo", model: "haiku", target: { mode: "planned", title: "Prima lezione" } });
      for (let i = 0; i < 30; i++) {
        if (fs.existsSync(progressPath)) break;
        await new Promise((r) => setTimeout(r, 50));
      }
      assert.ok(fs.existsSync(progressPath));
      const content = fs.readFileSync(progressPath, "utf-8");
      assert.ok(content.includes("1700000009999"));
    } finally {
      if (srv) await srv.close();
      await mock.stop();
      if (prevUrl === undefined) delete process.env.LEARNER_ANKI_URL;
      else process.env.LEARNER_ANKI_URL = prevUrl;
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
