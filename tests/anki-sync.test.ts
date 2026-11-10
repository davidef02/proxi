import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFile, execFileSync } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
import {
  formatLatexForAnki,
  escapeTsvField,
  generateCardsFromNode,
  extractCardsFromLesson,
  cardsToTsv,
  exportDeckTsv,
  pingAnkiConnect,
  syncToAnkiConnect,
  courseDeckNameFor,
  courseDeckName,
  courseLessonTags,
  type AnkiCard,
  type BasicCard,
  type ClozeCard,
  type ExportDeckOptions,
  type SyncOptions
} from "../extensions/anki-sync.ts";
import {
  initLesson,
  appendNode,
  type CognitiveNodePayload,
  type FullLessonDocument
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
  // TEST 1: LaTeX & Formatting Sanitization for Anki MathJax
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 1: LaTeX & TSV Formatting", () => {
    it("converts KaTeX math delimiters ($ / $$) to Anki MathJax (\\( / \\[\\])", () => {
      const input = "Given $f(x) = x^2$, the derivative is $$\\frac{df}{dx} = 2x$$";
      const formatted = formatLatexForAnki(input);

      assert.strictEqual(
        formatted,
        "Given \\(f(x) = x^2\\), the derivative is \\[\\frac{df}{dx} = 2x\\]"
      );
    });

    it("preserves already formatted MathJax \\( ... \\) and \\[ ... \\]", () => {
      const input = "Already formatted \\(a^2 + b^2 = c^2\\) and \\[e^{i\\pi} + 1 = 0\\]";
      const formatted = formatLatexForAnki(input);

      assert.strictEqual(formatted, input);
    });

    it("applies space workaround for nested cloze deletions containing LaTeX braces", () => {
      const input = "The formula is {{c1:\\frac{a}{b}}}.";
      const formatted = formatLatexForAnki(input);

      // Verify braces have spacing so Anki cloze parser doesn't collide
      assert.ok(formatted.includes("{{c1:\\frac{a}{b } }}") || formatted.includes("{{c1:\\frac{a}{b}}}"));
    });

    it("escapes TSV field characters (tabs to spaces, newlines to <br>)", () => {
      const raw = "Line 1\twith tab\nLine 2\r\nLine 3";
      const escaped = escapeTsvField(raw);

      assert.strictEqual(escaped, "Line 1 with tab<br>Line 2<br>Line 3");
      assert.ok(!escaped.includes("\t"), "Literal tabs must be replaced with spaces");
      assert.ok(!escaped.includes("\n"), "Literal newlines must be converted to <br>");
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 2: Card Synthesis from Cognitive Nodes
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 2: Card Synthesis from Nodes (Minimum Information Principle)", () => {
    it("generates atomic Basic and Cloze cards from CognitiveNodePayload", () => {
      const node: CognitiveNodePayload = {
        nodeId: "diff-quotient",
        title: "Difference Quotient & Limit",
        tier: "unconditional-truth",
        content: "Average rate of change over interval $\\Delta t$ is $\\frac{\\Delta x}{\\Delta t}$.",
        math: ["\\frac{\\Delta x}{\\Delta t}"],
        ankiParams: {
          targetFact: "The derivative is the limit of the difference quotient as \\(\\Delta t \\to 0\\).",
          suggestedCloze: "The instantaneous rate of change is defined as {{c1:\\(\\lim_{\\Delta t \\to 0} \\frac{\\Delta x}{\\Delta t}\\) }}.",
          tags: ["calculus", "derivatives"]
        },
        quiz: {
          question: "Why does the difference quotient require taking a limit as $\\Delta t \\to 0$?",
          correctAnswer: "Because at $\\Delta t = 0$ division is undefined; the limit captures the tendency without dividing by zero.",
          explanation: "Division by zero is undefined; the limit captures instantaneous behavior."
        }
      };

      const cards = generateCardsFromNode(node, "Calculus");

      // One card per node, and never one from the quiz: the quiz restates the node's idea.
      assert.strictEqual(cards.length, 1);
      const cloze = cards[0] as ClozeCard;
      assert.strictEqual(cloze.type, "cloze");
      assert.ok(cloze.text.includes("{{c1:"));
      assert.ok(cloze.tags.includes("learn-forme"));
      assert.ok(cloze.tags.includes("calculus"));
      assert.ok(cloze.tags.includes("tier::unconditional-truth"));
      assert.ok(cloze.tags.includes("node::diff-quotient"));
    });

    it("emits only the question card when a node authors both a question and a cloze", () => {
      const node: CognitiveNodePayload = {
        nodeId: "both",
        title: "both",
        tier: "unconditional-truth",
        content: "",
        ankiParams: {
          targetQuestion: "Q?",
          targetFact: "A.",
          suggestedCloze: "The answer is {{c1::A}}."
        }
      };
      const cards = generateCardsFromNode(node, "Vue");
      assert.strictEqual(cards.length, 1);
      assert.strictEqual((cards[0] as BasicCard).front, "Q?");
    });

    it("generates a Basic card only from an authored question, never from the title", () => {
      const node: CognitiveNodePayload = {
        nodeId: "packet-switching",
        title: "Packets as Atomic Unit of Network Communication",
        tier: "unconditional-truth",
        content: "ALL communication between internet hosts is performed through packet transfer.",
        ankiParams: {
          targetQuestion: "Which unit carries all communication between internet hosts?",
          targetFact: "All internet communication is performed by transmitting discrete, independent packets.",
        }
      };

      const cards = generateCardsFromNode(node, "Computer Networking");

      assert.strictEqual(cards.length, 1);
      const card = cards[0] as BasicCard;
      assert.strictEqual(card.type, "basic");
      assert.strictEqual(card.front, "Which unit carries all communication between internet hosts?");
      assert.ok(!card.front.includes("Packets as Atomic Unit"), "the title states the answer, it must not be the cue");
      assert.ok(card.back.includes("discrete, independent packets"));
      assert.ok(card.tags.includes("tier::unconditional-truth"));

      // No question, no Basic card: a title-wrapped front is not worth reviewing.
      const { targetQuestion, ...withoutQuestion } = node.ankiParams!;
      assert.strictEqual(generateCardsFromNode({ ...node, ankiParams: withoutQuestion }, "Computer Networking").length, 0);
    });

    it("puts only clozeExtra on the cloze back, never the node prose", () => {
      const node: CognitiveNodePayload = {
        nodeId: "ref-getter",
        title: "ref getter",
        tier: "unconditional-truth",
        motivation: "A very long motivation paragraph that must never end up on a card.",
        content: "Long content.",
        ankiParams: { suggestedCloze: "Vue 3: reading {{c1::.value}} runs the getter." }
      };
      const [bare] = generateCardsFromNode(node, "Vue");
      assert.strictEqual(bare.type, "cloze");
      assert.strictEqual((bare as any).extra, undefined);

      const [hinted] = generateCardsFromNode(
        { ...node, ankiParams: { ...node.ankiParams, clozeExtra: "Same mechanism as an accessor property." } },
        "Vue"
      );
      assert.strictEqual((hinted as any).extra, "Same mechanism as an accessor property.");
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 3: TSV Serialization & Deck Export
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 3: TSV Serialization & Deck Export", () => {
    it("serializes cards to Anki TSV with header directives", () => {
      const cards: AnkiCard[] = [
        {
          id: "card-1",
          nodeId: "node-1",
          type: "basic",
          deck: "Proxi::LinearAlgebra",
          front: "What is an eigenvector of matrix \\(A\\)?",
          back: "A non-zero vector \\(v\\) whose direction is unchanged by \\(A\\):<br>\\[Av = \\lambda v\\]",
          tags: ["learn-forme", "linear-algebra", "tier::unconditional-truth", "node::node-1"]
        },
        {
          id: "card-2",
          nodeId: "node-2",
          type: "cloze",
          deck: "Proxi::LinearAlgebra",
          text: "Eigenvalues are roots of the characteristic polynomial {{c1:\\(\\det(A - \\lambda I) = 0\\) }}.",
          extra: "Roots give scale factor \\(\\lambda\\).",
          tags: ["learn-forme", "linear-algebra", "tier::derivation", "node::node-2"]
        }
      ];

      const tsv = cardsToTsv(cards, { deckName: "Proxi::LinearAlgebra", topic: "Linear Algebra" });

      assert.match(tsv, /^#separator:tab/m);
      assert.match(tsv, /^#html:true/m);
      assert.match(tsv, /^#tags column:3/m);
      assert.match(tsv, /^#deck:Proxi::LinearAlgebra/m);

      const lines = tsv.split("\n").filter((l) => !l.startsWith("#") && l.trim().length > 0);
      assert.strictEqual(lines.length, 2);

      // Verify Tab separation and tags in 3rd column
      const row1Cols = lines[0].split("\t");
      assert.strictEqual(row1Cols.length, 3);
      assert.strictEqual(row1Cols[0], "What is an eigenvector of matrix \\(A\\)?");
      assert.ok(row1Cols[1].includes("Av = \\lambda v"));
      assert.ok(row1Cols[2].includes("learn-forme linear-algebra tier::unconditional-truth"));
    });

    it("an empty card list falls back to Proxi::General", () => {
      assert.ok(cardsToTsv([]).includes("#deck:Proxi::General"));
    });

    it("exports deck to ./decks/<topic-slug>.tsv on disk", async () => {
      const cards: AnkiCard[] = [
        {
          id: "c1",
          nodeId: "n1",
          type: "basic",
          deck: "Proxi::Calculus",
          front: "Front 1",
          back: "Back 1",
          tags: ["learn-forme", "calculus"]
        }
      ];

      const options: ExportDeckOptions = {
        cards,
        topic: "Calculus: Derivatives",
        baseDir: sandboxDir
      };

      const result = await exportDeckTsv(options);

      assert.strictEqual(result.cardCount, 1);
      assert.strictEqual(result.basicCount, 1);
      assert.strictEqual(result.clozeCount, 0);
      assert.ok(fs.existsSync(result.filePath));

      const expectedPath = path.join(sandboxDir, "decks", "calculus-derivatives.tsv");
      assert.strictEqual(path.resolve(result.filePath), path.resolve(expectedPath));

      const content = fs.readFileSync(result.filePath, "utf-8");
      assert.ok(content.includes("Front 1\tBack 1\tlearn-forme calculus"));
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 4: Extraction from HTML Lesson Document
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 4: Extraction from HTML Lessons", () => {
    it("extracts all cards from an initialized and appended HTML lesson file", async () => {
      // 1. Initialize HTML lesson
      const lessonResult = await initLesson({
        topic: "Raft Consensus",
        title: "Understanding Raft from First Principles",
        dagMermaid: "flowchart TD\n  A --> B",
        baseDir: sandboxDir
      });

      // 2. Append a node with ankiParams
      await appendNode({
        lessonPath: lessonResult.filePath,
        node: {
          nodeId: "quorum-invariant",
          title: "Majority Quorum Overlap",
          tier: "unconditional-truth",
          content: "Any two majorities of cluster size N overlap by at least one node.",
          ankiParams: {
            targetFact: "Any two majorities of size N must share at least one node, preventing split-brain.",
            suggestedCloze: "Raft requires a majority quorum because any two majorities are guaranteed to {{c1::overlap by at least one node}}."
          },
          quiz: {
            question: "Why can't a Raft cluster elect two leaders in the same term?",
            correctAnswer: "Because each leader requires a majority of votes, and two majorities must overlap.",
            explanation: "Overlapping majorities guarantee no two disjoint groups can each form a majority."
          }
        }
      });

      // 3. Extract cards from HTML lesson
      const extracted = extractCardsFromLesson(lessonResult.filePath);

      assert.strictEqual(extracted.metadata.topic, "Raft Consensus");
      assert.strictEqual(extracted.cards.length, 1, "One card per node");

      const cloze = extracted.cards.find((c) => c.type === "cloze");
      assert.ok(cloze);
      assert.ok(cloze.tags.includes("raft-consensus"));
      assert.strictEqual(extracted.deckName, "Proxi::Raft Consensus", "outside a course: one deck per topic");
    });

    it("puts every lesson of a course into one deck named after the chapter", async () => {
      fs.writeFileSync(
        path.join(sandboxDir, "curriculum.json"),
        JSON.stringify({ chapters: [{ title: "Vue.js & Vetrine", slug: "vue-vetrine", lessons: [] }] })
      );
      const decks = [];
      for (const topic of ["Reattività di Vue 3", "Props ed emits"]) {
        const lesson = await initLesson({ topic, baseDir: sandboxDir, lessonsDir: path.join(sandboxDir, "courses", "vue-vetrine", "lessons") });
        await appendNode({
          lessonPath: lesson.filePath,
          node: { nodeId: "n", title: "T", tier: "unconditional-truth", content: "C", ankiParams: { suggestedCloze: "Il ref {{c1::traccia}} chi lo legge." } }
        });
        const extracted = extractCardsFromLesson(lesson.filePath);
        assert.strictEqual(extracted.cards[0].deck, extracted.deckName);
        decks.push(extracted.deckName);
      }
      assert.deepStrictEqual(decks, ["Proxi::Vue.js & Vetrine", "Proxi::Vue.js & Vetrine"]);
    });

    it("courseDeckNameFor returns Proxi::<chapter title> from curriculum.json and falls back to slug without one", () => {
      fs.writeFileSync(
        path.join(sandboxDir, "curriculum.json"),
        JSON.stringify({ chapters: [{ title: "Vue.js & Vetrine", slug: "vue-vetrine", lessons: [] }] })
      );
      assert.strictEqual(courseDeckNameFor(sandboxDir, "vue-vetrine"), "Proxi::Vue.js & Vetrine");
      assert.strictEqual(courseDeckNameFor(sandboxDir, "missing-course"), "Proxi::missing-course");
    });

    it("maps lessons under courses/lezioni-singole/ to Proxi::Lezioni singole", async () => {
      // no curriculum.json at all: the standalone container is never planned
      assert.strictEqual(courseDeckNameFor(sandboxDir, "lezioni-singole"), "Proxi::Lezioni singole");

      const lesson = await initLesson({
        topic: "Entropia",
        title: "L'entropia misura il numero di microstati",
        baseDir: sandboxDir,
        lessonsDir: path.join(sandboxDir, "courses", "lezioni-singole", "lessons")
      });
      await appendNode({
        lessonPath: lesson.filePath,
        node: {
          nodeId: "microstati",
          title: "Microstati e macrostati",
          tier: "unconditional-truth",
          content: "Un macrostato è compatibile con molti microstati.",
          ankiParams: {
            targetQuestion: "Che cosa misura l'entropia?",
            targetFact: "Il numero di microstati compatibili con il macrostato."
          }
        }
      });

      assert.strictEqual(courseDeckName(lesson.filePath), "Proxi::Lezioni singole");
      const extracted = extractCardsFromLesson(lesson.filePath);
      assert.strictEqual(extracted.deckName, "Proxi::Lezioni singole");
      assert.strictEqual(extracted.cards[0].deck, "Proxi::Lezioni singole");

      const exported = await exportDeckTsv({ lessonPath: lesson.filePath, baseDir: sandboxDir });
      assert.strictEqual(exported.deckName, "Proxi::Lezioni singole");
      const tsv = fs.readFileSync(exported.filePath, "utf-8");
      assert.ok(tsv.includes("#deck:Proxi::Lezioni singole"));
    });

    it("courseLessonTags returns [] outside a courses dir", () => {
      assert.deepStrictEqual(courseLessonTags(path.join(sandboxDir, "lessons", "0001-x.html"), "x"), []);
      assert.deepStrictEqual(courseLessonTags("some/random/path.html", "x"), []);
      assert.deepStrictEqual(
        courseLessonTags(path.join(sandboxDir, "courses", "demo", "lessons", "0001-x.html"), "x"),
        ["course::demo", "lesson::x"]
      );
      assert.deepStrictEqual(
        courseLessonTags(path.join(sandboxDir, "courses", "demo", "lessons", "0001-x.html"), undefined),
        ["course::demo"]
      );
      assert.deepStrictEqual(
        courseLessonTags(path.join(sandboxDir, "courses", "demo", "lessons", "0001-x.html"), ""),
        ["course::demo"]
      );
    });
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

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 6: CLI Subcommand Execution
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 6: CLI Subcommand Execution", () => {
    it("executes 'export' CLI command with inline JSON payload", () => {
      const payload = JSON.stringify({
        topic: "Algorithms",
        cards: [
          {
            id: "algo-1",
            nodeId: "binary-search",
            type: "basic",
            deck: "Proxi::Algorithms",
            front: "What is time complexity of binary search?",
            back: "\\(\\mathcal{O}(\\log n)\\)",
            tags: ["learn-forme", "algorithms"]
          }
        ]
      });

      const out = execFileSync(
        process.execPath,
        [
          "--import",
          "tsx",
          "extensions/anki-sync.ts",
          "export",
          "--data",
          payload,
          "--base-dir",
          sandboxDir
        ],
        { encoding: "utf-8", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
      );

      assert.ok(out.includes("Exported 1 cards to"));
      const expectedPath = path.join(sandboxDir, "decks", "algorithms.tsv");
      assert.ok(fs.existsSync(expectedPath));
    });

    it("executes 'ping' CLI command against mock AnkiConnect", async () => {
      const { stdout } = await execFileAsync(
        process.execPath,
        [
          "--import",
          "tsx",
          "extensions/anki-sync.ts",
          "ping",
          "--anki-url",
          mockServerUrl
        ],
        { encoding: "utf-8", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
      );

      assert.ok(stdout.includes("AnkiConnect is ONLINE"));
      assert.ok(stdout.includes("Version 6"));
    });

    it("gracefully handles invalid CLI arguments with exit code 1", () => {
      assert.throws(
        () => {
          execFileSync(
            process.execPath,
            [
              "--import",
              "tsx",
              "extensions/anki-sync.ts",
              "invalid-command"
            ],
            { encoding: "utf-8", stdio: "pipe", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
          );
        },
        (err: any) => {
          return err.status === 1;
        }
      );
    });
  });
});
