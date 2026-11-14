import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  pingAnkiConnect,
  syncToAnkiConnect,
  type AnkiCard,
  type SyncOptions
} from "../extensions/anki-sync.ts";
import {
  initLesson,
  appendNode
} from "../extensions/html-lesson.ts";
import { MockAnki } from "./mock-anki.ts";

describe("Atomic Flashcard Maker & Anki Sync Extension (Ticket #4)", () => {
  let sandboxDir: string;
  let mockServer: MockAnki;
  let mockServerUrl: string = "";

  beforeEach(async () => {
    sandboxDir = fs.mkdtempSync(path.join(os.tmpdir(), "learn-forme-anki-test-"));
    mockServer = new MockAnki();
    mockServerUrl = await mockServer.start();
  });

  afterEach(async () => {
    await mockServer.stop();
    if (sandboxDir && fs.existsSync(sandboxDir)) {
      fs.rmSync(sandboxDir, { recursive: true, force: true });
    }
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 5: AnkiConnect Synchronization & Network Error Fallback
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 5: AnkiConnect Sync & Offline Fallback", () => {
    it("successfully pings online AnkiConnect server", async () => {
      const ping = await pingAnkiConnect(mockServerUrl, 1000);
      assert.strictEqual(ping.online, true);
      assert.strictEqual(ping.version, 6);
    });

    it("successfully creates deck and synchronizes notes via AnkiConnect", async () => {
      const cards: AnkiCard[] = [
        {
          id: "card-1",
          nodeId: "node-1",
          type: "basic",
          deck: "Proxi::Calculus",
          front: "What is derivative of \\(x^2\\)?",
          back: "\\(2x\\)",
          tags: ["learn-forme", "calculus", "tier::derivation"]
        },
        {
          id: "card-2",
          nodeId: "node-2",
          type: "cloze",
          deck: "Proxi::Calculus",
          text: "The derivative of \\(e^x\\) is {{c1:\\(e^x\\) }}.",
          extra: "Rate of change equals function value.",
          tags: ["learn-forme", "calculus", "tier::unconditional-truth"]
        }
      ];

      const syncOptions: SyncOptions = {
        cards,
        topic: "Calculus",
        deckName: "Proxi::Calculus",
        ankiUrl: mockServerUrl,
        baseDir: sandboxDir
      };

      const result = await syncToAnkiConnect(syncOptions);

      assert.strictEqual(result.synced, true);
      assert.strictEqual(result.addedCount, 2);
      assert.strictEqual(result.failedCount, 0);
      assert.strictEqual(result.deckName, "Proxi::Calculus");

      // Verify mock server received createDeck and addNotes requests
      assert.ok(mockServer.createdDecks.includes("Proxi::Calculus"));
      assert.strictEqual(mockServer.addedNotes.length, 2);

      const addedBasic = mockServer.addedNotes.find((n) => n.modelName === "Basic");
      assert.ok(addedBasic);
      assert.strictEqual(addedBasic.fields.Front, "What is derivative of \\(x^2\\)?");
      assert.strictEqual(addedBasic.fields.Back, "\\(2x\\)");

      const addedCloze = mockServer.addedNotes.find((n) => n.modelName === "Cloze");
      assert.ok(addedCloze);
      assert.ok(addedCloze.fields.Text.includes("{{c1:"));

      // Verify TSV was also written as permanent portable backup
      assert.ok(result.tsvExportPath && fs.existsSync(result.tsvExportPath));
    });

    it("uses the profile's localized note types and field names instead of hard-coded Basic/Cloze", async () => {
      // An Italian Anki profile: no "Basic", no "Cloze". This is what made the first real sync
      // fail with "model was not found: Cloze" while still creating an empty deck.
      mockServer.models = {
        "Basilare": ["Fronte", "Retro"],
        "Testo da completare": ["Testo", "Back Extra"]
      };
      const cards: AnkiCard[] = [
        { id: "b", nodeId: "n", type: "basic", deck: "Proxi::Vue", front: "F", back: "B", tags: [] },
        { id: "c", nodeId: "n", type: "cloze", deck: "Proxi::Vue", text: "{{c1::x}}", extra: "E", tags: [] }
      ];

      const result = await syncToAnkiConnect({
        cards,
        topic: "Vue",
        deckName: "Proxi::Vue",
        ankiUrl: mockServerUrl,
        baseDir: sandboxDir
      });

      assert.strictEqual(result.synced, true);
      const basic = mockServer.addedNotes.find((n) => n.modelName === "Basilare");
      assert.deepStrictEqual(basic?.fields, { Fronte: "F", Retro: "B" });
      const cloze = mockServer.addedNotes.find((n) => n.modelName === "Testo da completare");
      assert.deepStrictEqual(cloze?.fields, { Testo: "{{c1::x}}", "Back Extra": "E" });
    });

    it("falls back gracefully without throwing when Anki is closed or unreachable", async () => {
      const cards: AnkiCard[] = [
        {
          id: "card-offline",
          nodeId: "node-offline",
          type: "basic",
          deck: "Proxi::Physics",
          front: "What is Newton's Second Law?",
          back: "\\(F = ma\\)",
          tags: ["learn-forme", "physics"]
        }
      ];

      // Point to an unused/closed port
      const offlineUrl = "http://127.0.0.1:59999";

      const syncOptions: SyncOptions = {
        cards,
        topic: "Physics",
        ankiUrl: offlineUrl,
        timeoutMs: 300,
        baseDir: sandboxDir
      };

      // Must NOT throw
      const result = await syncToAnkiConnect(syncOptions);

      assert.strictEqual(result.synced, false);
      assert.ok(result.warning && result.warning.includes("offline"));
      assert.ok(result.tsvExportPath, "Must export TSV even when AnkiConnect is offline");
      assert.ok(fs.existsSync(result.tsvExportPath));

      const tsvContent = fs.readFileSync(result.tsvExportPath, "utf-8");
      assert.ok(tsvContent.includes("What is Newton's Second Law?"));
    });

    it("falls back gracefully on network timeout without blocking", async () => {
      mockServer.shouldTimeout = true;

      const cards: AnkiCard[] = [
        {
          id: "card-timeout",
          nodeId: "node-timeout",
          type: "basic",
          deck: "Proxi::Geometry",
          front: "Sum of angles in a triangle?",
          back: "\\(180^\\circ\\)",
          tags: ["learn-forme", "geometry"]
        }
      ];

      const syncOptions: SyncOptions = {
        cards,
        topic: "Geometry",
        ankiUrl: mockServerUrl,
        timeoutMs: 300,
        baseDir: sandboxDir
      };

      const result = await syncToAnkiConnect(syncOptions);

      assert.strictEqual(result.synced, false);
      assert.ok(result.warning && (result.warning.includes("timeout") || result.warning.includes("offline")));
      assert.ok(result.tsvExportPath && fs.existsSync(result.tsvExportPath));
    });

    it("handles AnkiConnect API-level error responses gracefully", async () => {
      mockServer.responseError = "model 'CustomCloze' not found";

      const cards: AnkiCard[] = [
        {
          id: "card-err",
          nodeId: "node-err",
          type: "basic",
          deck: "Proxi::Test",
          front: "Q",
          back: "A",
          tags: ["test"]
        }
      ];

      const result = await syncToAnkiConnect({
        cards,
        topic: "Test",
        ankiUrl: mockServerUrl,
        baseDir: sandboxDir
      });

      assert.strictEqual(result.synced, false);
      assert.strictEqual(result.error, "model 'CustomCloze' not found");
      assert.ok(result.tsvExportPath && fs.existsSync(result.tsvExportPath));
    });

    it("AC7: sends notes with course, lesson and node tags when syncing a course lesson", async () => {
      const lessonsDir = path.join(sandboxDir, "courses", "demo", "lessons");
      const lessonResult = await initLesson({
        topic: "Demo",
        title: "Demo",
        slug: "x",
        baseDir: sandboxDir,
        lessonsDir
      });

      await appendNode({
        lessonPath: lessonResult.filePath,
        node: {
          nodeId: "alpha",
          title: "Alpha",
          tier: "unconditional-truth",
          content: "Alpha node content",
          ankiParams: {
            suggestedCloze: "Alpha cloze {{c1::test}}."
          }
        }
      });

      const result = await syncToAnkiConnect({
        lessonPath: lessonResult.filePath,
        ankiUrl: mockServerUrl,
        decksDir: path.join(sandboxDir, "decks")
      });

      assert.strictEqual(result.synced, true);
      assert.ok(mockServer.addedNotes.length > 0);
      for (const note of mockServer.addedNotes) {
        assert.ok(note.tags.includes("course::demo"));
        assert.ok(note.tags.includes("lesson::x"));
        assert.ok(note.tags.includes("node::alpha"));
      }
    });
  });

});
