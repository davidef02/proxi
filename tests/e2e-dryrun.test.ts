import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import * as http from "node:http";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFileSync } from "node:child_process";
import {
  initLesson,
  appendNode,
  compileReference,
  type CognitiveNodePayload,
  type InitLessonOptions
} from "../extensions/html-lesson.ts";
import {
  extractCardsFromLesson,
  exportDeckTsv,
  syncToAnkiConnect,
  type AnkiCard,
  type SyncOptions
} from "../extensions/anki-sync.ts";

/**
 * Lightweight mock AnkiConnect HTTP server for integration and E2E testing.
 */
class MockAnkiConnectServer {
  server: http.Server | null = null;
  port: number = 0;
  requests: { action: string; version: number; params?: any }[] = [];
  createdDecks: string[] = [];
  addedNotes: any[] = [];

  async start(): Promise<string> {
    return new Promise((resolve, reject) => {
      this.server = http.createServer((req, res) => {
        let body = "";
        req.on("data", (chunk) => {
          body += chunk;
        });

        req.on("end", () => {
          try {
            const payload = JSON.parse(body || "{}");
            this.requests.push(payload);

            res.writeHead(200, { "Content-Type": "application/json; charset=utf-8" });

            if (payload.action === "version") {
              res.end(JSON.stringify({ result: 6, error: null }));
            } else if (payload.action === "createDeck") {
              const deckName = payload.params?.deck || "Default";
              this.createdDecks.push(deckName);
              res.end(JSON.stringify({ result: 1725128400000, error: null }));
            } else if (payload.action === "addNotes") {
              const notes = payload.params?.notes || [];
              this.addedNotes.push(...notes);
              const noteIds = notes.map((_: any, idx: number) => 2000 + idx);
              res.end(JSON.stringify({ result: noteIds, error: null }));
            } else {
              res.end(JSON.stringify({ result: null, error: `Unsupported action ${payload.action}` }));
            }
          } catch (err: any) {
            res.writeHead(400, { "Content-Type": "application/json" });
            res.end(JSON.stringify({ result: null, error: err.message }));
          }
        });
      });

      this.server.listen(0, "127.0.0.1", () => {
        const addr = this.server!.address();
        if (typeof addr === "object" && addr !== null) {
          this.port = addr.port;
          resolve(`http://127.0.0.1:${this.port}`);
        } else {
          reject(new Error("Could not retrieve mock server port"));
        }
      });
    });
  }

  async stop(): Promise<void> {
    if (this.server) {
      await new Promise<void>((resolve) => {
        this.server!.close(() => resolve());
      });
      this.server = null;
    }
  }

  reset(): void {
    this.requests = [];
    this.createdDecks = [];
    this.addedNotes = [];
  }
}

describe("End-to-End Capture Integration & Multi-Agent Dry Run (Ticket #5)", () => {
  let sandboxDir: string;
  let mockServer: MockAnkiConnectServer;
  let mockServerUrl: string = "";

  beforeEach(async () => {
    sandboxDir = fs.mkdtempSync(path.join(os.tmpdir(), "learn-forme-e2e-test-"));
    mockServer = new MockAnkiConnectServer();
    mockServerUrl = await mockServer.start();
  });

  afterEach(async () => {
    await mockServer.stop();
    if (sandboxDir && fs.existsSync(sandboxDir)) {
      fs.rmSync(sandboxDir, { recursive: true, force: true });
    }
  });

  // ──────────────────────────────────────────────────────────────────────────
  // E2E DRY-RUN: Full Teaching Lifecycle (Probe -> Plan -> 5-Step Teach -> Synthesis)
  // ──────────────────────────────────────────────────────────────────────────
  it("executes a complete end-to-end 5-node session from Plan to Capture and validates all persistent artifacts", async () => {
    // ========================================================================
    // PHASE 2: PLAN (Initialize Lesson Artifact)
    // ========================================================================
    const topicName = "Calculus: Rates of Change & Derivatives";
    const lessonTitle = "Derivatives from First Principles";
    const dagMermaid = [
      "flowchart TD",
      "  N1[\"Average Rate of Change\"] --> N3[\"Secant-to-Tangent Limit\"]",
      "  N2[\"Formal Limit Invariant\"] --> N3",
      "  N3 --> N4[\"Derivative Function\"]",
      "  N4 --> N5[\"Power Rule Discovery\"]"
    ].join("\n");

    const initOptions: InitLessonOptions = {
      topic: topicName,
      title: lessonTitle,
      description: "A motivated discovery of derivatives, difference quotients, and the power rule.",
      dagMermaid,
      targetGoal: "Derive the power rule from first principles without memorizing formulas.",
      baseDir: sandboxDir
    };

    const initResult = await initLesson(initOptions);

    assert.strictEqual(initResult.lessonNumber, 1);
    assert.strictEqual(initResult.slug, "derivatives-from-first-principles");
    assert.ok(fs.existsSync(initResult.filePath), "Lesson file must be created on disk");

    const expectedLessonPath = path.join(sandboxDir, "lessons", "0001-derivatives-from-first-principles.html");
    assert.strictEqual(path.resolve(initResult.filePath), path.resolve(expectedLessonPath));

    // ========================================================================
    // PHASE 3: TEACH (5-Step Node Loop: Motivate -> Establish -> Connect -> Quiz -> Capture)
    // ========================================================================
    const nodes: CognitiveNodePayload[] = [
      // Node 1: Unconditional Truth (Root 1)
      {
        nodeId: "average-rate-axiom",
        title: "Average Rate of Change Invariant",
        tier: "unconditional-truth",
        motivation: "Why quantify change over discrete intervals before addressing instants.",
        content: "Across any finite interval, the average rate of change is strictly the ratio of the output change to the input change.",
        math: ["\\bar{v} = \\frac{\\Delta x}{\\Delta t}"],
        dependencies: [],
        quiz: {
          question: "What is the average rate of change over an interval $\\Delta t$?",
          options: [
            "1. $\\frac{\\Delta x}{\\Delta t}$",
            "2. $\\Delta x \\cdot \\Delta t$",
            "3. $\\frac{\\Delta t}{\\Delta x}$",
            "4. $(\\Delta x)^2$"
          ],
          correctIndex: 1,
          explanation: "By definition, average rate of change is output difference divided by input difference."
        },
        ankiParams: {
          targetFact: "The average rate of change over an interval is strictly \\(\\frac{\\Delta x}{\\Delta t}\\).",
          suggestedCloze: "Average rate of change over an interval is defined as {{c1:\\(\\frac{\\Delta x}{\\Delta t}\\) }}."
        }
      },
      // Node 2: Unconditional Truth (Root 2)
      {
        nodeId: "limit-concept-axiom",
        title: "The Limit Concept as Approaching Tendency",
        tier: "unconditional-truth",
        motivation: "Overcoming the impossibility of evaluating 0/0 directly.",
        content: "A limit describes the value that a function approaches as its input gets arbitrarily close to a point, without requiring evaluation at that exact point.",
        math: ["\\lim_{t \\to t_0} f(t) = L"],
        dependencies: [],
        quiz: {
          question: "What does the limit $\\lim_{t \\to t_0} f(t) = L$ assert?",
          options: [
            "1. $f(t)$ approaches $L$ as $t$ approaches $t_0$.",
            "2. $f(t_0)$ must evaluate to $L$ exactly.",
            "3. $f(t)$ is continuous everywhere.",
            "4. $t$ reaches $t_0$ in finite steps."
          ],
          correctIndex: 1,
          explanation: "Limits describe the approaching behavior, independent of whether $f(t_0)$ is defined."
        },
        ankiParams: {
          targetFact: "A limit captures the approaching tendency as \\(t \\to t_0\\), without requiring evaluation at \\(t_0\\)."
        }
      },
      // Node 3: Derivation (Intermediate)
      {
        nodeId: "secant-to-tangent-limit",
        title: "Secant Line Convergence to Instantaneous Tangent",
        tier: "derivation",
        motivation: "Transitioning from average velocity over a window to instantaneous velocity at an instant.",
        content: "As the observation interval shrinks toward zero, the secant line passing through $(t, x(t))$ and $(t+\\Delta t, x(t+\\Delta t))$ smoothly converges to the tangent line.",
        math: ["v(t) = \\lim_{\\Delta t \\to 0} \\frac{x(t + \\Delta t) - x(t)}{\\Delta t}"],
        visuals: [
          {
            type: "svg-inline",
            content: "<svg viewBox=\"0 0 200 100\"><line x1=\"10\" y1=\"90\" x2=\"190\" y2=\"10\" stroke=\"currentColor\" stroke-width=\"2\"/></svg>",
            caption: "Secant line rotating into the tangent line as \\Delta t \\to 0"
          }
        ],
        dependencies: ["average-rate-axiom", "limit-concept-axiom"],
        quiz: {
          question: "Geometrically, what happens to the secant line as $\\Delta t \\to 0$?",
          options: [
            "1. It converges to the tangent line at that point.",
            "2. It becomes perpendicular to the curve.",
            "3. It diverges to infinity.",
            "4. It collapses into a single coordinate point."
          ],
          correctIndex: 1,
          explanation: "The limiting position of a secant line as the two points merge is the tangent line."
        },
        ankiParams: {
          targetFact: "As \\(\\Delta t \\to 0\\), the secant line across the interval converges to the instantaneous tangent line.",
          suggestedCloze: "The geometric interpretation of the derivative is the {{c1:slope of the tangent line}}."
        }
      },
      // Node 4: Derivation (Generalization)
      {
        nodeId: "derivative-function-definition",
        title: "The Derivative as a Generating Function",
        tier: "derivation",
        motivation: "Generalizing the single-point tangent slope into an all-points mapping function.",
        content: "The derivative function $f'(x)$ maps each input $x$ to the instantaneous rate of change at that coordinate via the difference quotient limit.",
        math: ["f'(x) = \\lim_{h \\to 0} \\frac{f(x + h) - f(x)}{h}"],
        dependencies: ["secant-to-tangent-limit"],
        quiz: {
          question: "What does the function $f'(x)$ represent?",
          options: [
            "1. The instantaneous rate of change of $f$ at any coordinate $x$.",
            "2. The total accumulated area under $f(x)$.",
            "3. The average value of $f(x)$ over $[0, x]$.",
            "4. The maximum value attained by $f(x)$."
          ],
          correctIndex: 1,
          explanation: "The derivative function maps each coordinate $x$ to the instantaneous slope $f'(x)$."
        },
        ankiParams: {
          targetFact: "The derivative function \\(f'(x)\\) maps each \\(x\\) to the instantaneous rate of change \\(\\lim_{h \\to 0} \\frac{f(x+h) - f(x)}{h}\\).",
          suggestedCloze: "The formal definition of the derivative is {{c1:\\(f'(x) = \\lim_{h \\to 0} \\frac{f(x+h) - f(x)}{h}\\) }}."
        }
      },
      // Node 5: Derivation (Goal Sink)
      {
        nodeId: "power-rule-discovery",
        title: "Power Rule via Binomial Expansion",
        tier: "derivation",
        motivation: "Discovering a general shortcut to differentiate polynomials without evaluating limits manually each time.",
        content: "Expanding $(x+h)^n = x^n + n x^{n-1}h + \\mathcal{O}(h^2)$, subtracting $x^n$, dividing by $h$, and taking $h \\to 0$ isolates $n x^{n-1}$.",
        math: [
          "(x+h)^n - x^n = n x^{n-1}h + \\binom{n}{2} x^{n-2}h^2 + \\dots",
          "\\frac{d}{dx}[x^n] = n x^{n-1}"
        ],
        dependencies: ["derivative-function-definition"],
        quiz: {
          question: "What is the derivative of $x^n$ with respect to $x$?",
          options: [
            "1. $n x^{n-1}$",
            "2. $\\frac{x^{n+1}}{n+1}$",
            "3. $(n-1) x^n$",
            "4. $n^2 x^{n-1}$"
          ],
          correctIndex: 1,
          explanation: "By the power rule, \\frac{d}{dx}[x^n] = n x^{n-1}."
        },
        ankiParams: {
          targetFact: "The power rule states that the derivative of \\(x^n\\) is \\(n x^{n-1}\\).",
          suggestedCloze: "By the power rule, \\(\\frac{d}{dx}[x^n] =\\) {{c1:\\(n x^{n-1}\\) }}."
        }
      }
    ];

    // Append each node incrementally (Step 5: Capture)
    let currentNodeCount = 0;
    for (const node of nodes) {
      const appendResult = await appendNode({
        lessonPath: initResult.filePath,
        node
      });
      currentNodeCount++;
      assert.strictEqual(appendResult.nodeCount, currentNodeCount);
      assert.strictEqual(appendResult.nodeId, node.nodeId);
    }

    // Verify HTML Lesson content and data-island
    const finalHtml = fs.readFileSync(initResult.filePath, "utf-8");
    assert.ok(finalHtml.includes("Derivatives from First Principles"));
    assert.ok(finalHtml.includes("flowchart TD"));
    assert.ok(finalHtml.includes("Average Rate of Change Invariant"));
    assert.ok(finalHtml.includes("The Limit Concept as Approaching Tendency"));
    assert.ok(finalHtml.includes("Secant Line Convergence to Instantaneous Tangent"));
    assert.ok(finalHtml.includes("The Derivative as a Generating Function"));
    assert.ok(finalHtml.includes("Power Rule via Binomial Expansion"));
    assert.ok(finalHtml.includes("badge-axiom") || finalHtml.includes("badge-unconditional"));
    assert.ok(finalHtml.includes("badge-derivation"));
    assert.ok(finalHtml.includes("<svg viewBox=\"0 0 200 100\">"));
    assert.ok(finalHtml.includes("id=\"lesson-data\""));

    // Parse embedded data island to ensure JSON consistency
    const dataIslandMatch = finalHtml.match(/<script type=["']application\/json["'] id=["']lesson-data["']>([\s\S]*?)<\/script>/);
    assert.ok(dataIslandMatch, "HTML must contain #lesson-data JSON script tag");
    const parsedDataIsland = JSON.parse(dataIslandMatch[1]);
    assert.strictEqual(parsedDataIsland.nodes.length, 5);
    assert.strictEqual(parsedDataIsland.metadata.slug, "derivatives-from-first-principles");

    // ========================================================================
    // PHASE 4: SYNTHESIS (Reference Sheet & Anki Synchronization)
    // ========================================================================

    // 1. Compile Reference Cheat Sheet
    const refResult = await compileReference({
      lessonPath: initResult.filePath,
      baseDir: sandboxDir
    });

    assert.strictEqual(refResult.slug, "derivatives-from-first-principles");
    assert.strictEqual(refResult.unconditionalTruthCount, 2);
    assert.strictEqual(refResult.nodeCount, 5);
    assert.ok(refResult.formulaCount >= 5);
    assert.ok(fs.existsSync(refResult.filePath));

    const expectedRefPath = path.join(sandboxDir, "reference", "derivatives-from-first-principles.html");
    assert.strictEqual(path.resolve(refResult.filePath), path.resolve(expectedRefPath));

    const refHtml = fs.readFileSync(refResult.filePath, "utf-8");
    assert.ok(refHtml.includes("Reference Cheat Sheet") || refHtml.includes("Quick Reference") || refHtml.includes("Sintesi"));
    assert.ok(refHtml.includes("Average Rate of Change Invariant"));
    assert.ok(refHtml.includes("The Limit Concept as Approaching Tendency"));
    assert.ok(refHtml.includes("\\bar{v} = \\frac{\\Delta x}{\\Delta t}"));
    assert.ok(refHtml.includes("\\frac{d}{dx}[x^n] = n x^{n-1}"));
    assert.ok(refHtml.includes("Mathematical Inventory") || refHtml.includes("Inventario formule") || refHtml.includes("Formula"));

    // 2. Extract and Export Anki Cards
    const extraction = extractCardsFromLesson(initResult.filePath);
    assert.strictEqual(extraction.metadata.slug, "derivatives-from-first-principles");
    assert.strictEqual(extraction.cards.length, 4, "one card per node; node 2 has a fact but no question");

    const exportResult = await exportDeckTsv({
      cards: extraction.cards,
      topic: topicName,
      slug: extraction.metadata.slug,
      baseDir: sandboxDir
    });

    assert.ok(fs.existsSync(exportResult.filePath));
    const expectedTsvPath = path.join(sandboxDir, "decks", "derivatives-from-first-principles.tsv");
    assert.strictEqual(path.resolve(exportResult.filePath), path.resolve(expectedTsvPath));

    const tsvContent = fs.readFileSync(exportResult.filePath, "utf-8");
    assert.match(tsvContent, /^#separator:tab/m);
    assert.match(tsvContent, /^#html:true/m);
    assert.match(tsvContent, /^#tags column:3/m);
    assert.match(tsvContent, /^#deck:Proxi::Calculus: Rates of Change & Derivatives/m);
    assert.ok(tsvContent.includes("tier::unconditional-truth"));
    assert.ok(tsvContent.includes("tier::derivation"));
    assert.ok(tsvContent.includes("\\( \\frac{\\Delta x}{\\Delta t} \\)") || tsvContent.includes("\\(\\frac{\\Delta x}{\\Delta t}\\)"));

    // 3. Sync to AnkiConnect Server (Mock)
    const syncResult = await syncToAnkiConnect({
      cards: extraction.cards,
      topic: topicName,
      deckName: "Proxi::Calculus: Rates of Change & Derivatives",
      ankiUrl: mockServerUrl,
      baseDir: sandboxDir
    });

    assert.strictEqual(syncResult.synced, true);
    assert.strictEqual(syncResult.deckName, "Proxi::Calculus: Rates of Change & Derivatives");
    assert.strictEqual(syncResult.addedCount, 4);
    assert.strictEqual(syncResult.failedCount, 0);

    // Verify mock server state
    assert.ok(mockServer.createdDecks.includes("Proxi::Calculus: Rates of Change & Derivatives"));
    assert.strictEqual(mockServer.addedNotes.length, extraction.cards.length);
  });

  // ──────────────────────────────────────────────────────────────────────────
  // CLI WORKFLOW TEST: Multi-Agent CLI Invocations
  // ──────────────────────────────────────────────────────────────────────────
  it("executes full workflow via CLI subcommands as used by coding agents across platforms", () => {
    const htmlCli = path.resolve("extensions/html-lesson.ts");
    const ankiCli = path.resolve("extensions/anki-sync.ts");

    // 1. CLI Init
    const initOut = execFileSync(
      process.execPath,
      [
        "--import",
        "tsx",
        htmlCli,
        "init",
        "--topic",
        "Distributed Systems Consensus",
        "--title",
        "Paxos and Raft Consensus",
        "--dag",
        "flowchart TD\n A[\"Quorum Axiom\"] --> B[\"Leader Election\"]",
        "--base-dir",
        sandboxDir
      ],
      { encoding: "utf-8", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
    );

    const initJson = JSON.parse(initOut);
    assert.strictEqual(initJson.lessonNumber, 1);
    assert.strictEqual(initJson.slug, "paxos-and-raft-consensus");
    assert.ok(fs.existsSync(initJson.filePath));

    // 2. CLI Append Node
    const node: CognitiveNodePayload = {
      nodeId: "quorum-overlap-axiom",
      title: "Majority Quorum Overlap Invariant",
      tier: "unconditional-truth",
      motivation: "Why majorities prevent split-brain states",
      content: "In any cluster of size $N$, any two majorities $\\lceil (N+1)/2 \\rceil$ share at least one common member.",
      math: ["Q_1 \\cap Q_2 \\neq \\emptyset"],
      dependencies: [],
      quiz: {
        question: "Why is a majority quorum strictly greater than half?",
        options: [
          "1. To guarantee that any two quorums overlap by at least one node.",
          "2. To allow fast asynchronous consensus without network packets.",
          "3. To eliminate all network latency unconditionally."
        ],
        correctIndex: 1,
        explanation: "Pigeonhole principle ensures any two majorities of size > N/2 must overlap."
      },
      ankiParams: {
        targetQuestion: "Why can two majority quorums never disagree without noticing?",
        targetFact: "Any two majorities of size N share at least one overlapping member, preventing split-brain.",
        suggestedCloze: "Consensus requires majorities because any two majorities are guaranteed to {{c1:overlap by at least one node}}."
      }
    };

    const appendOut = execFileSync(
      process.execPath,
      [
        "--import",
        "tsx",
        htmlCli,
        "append",
        "--lesson",
        initJson.filePath,
        "--data",
        JSON.stringify(node)
      ],
      { encoding: "utf-8", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
    );

    const appendJson = JSON.parse(appendOut);
    assert.strictEqual(appendJson.nodeCount, 1);
    assert.strictEqual(appendJson.nodeId, "quorum-overlap-axiom");

    // 3. CLI Compile Reference Cheat Sheet
    const compileOut = execFileSync(
      process.execPath,
      [
        "--import",
        "tsx",
        htmlCli,
        "compile-ref",
        "--lesson",
        initJson.filePath,
        "--base-dir",
        sandboxDir
      ],
      { encoding: "utf-8", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
    );

    const compileJson = JSON.parse(compileOut);
    assert.strictEqual(compileJson.slug, "paxos-and-raft-consensus");
    assert.strictEqual(compileJson.unconditionalTruthCount, 1);
    assert.ok(fs.existsSync(compileJson.filePath));

    // 4. CLI Anki Sync from Lesson
    const ankiSyncOut = execFileSync(
      process.execPath,
      [
        "--import",
        "tsx",
        ankiCli,
        "sync",
        "--lesson",
        initJson.filePath,
        "--anki-url",
        mockServerUrl,
        "--base-dir",
        sandboxDir
      ],
      { encoding: "utf-8", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
    );

    assert.ok(
      ankiSyncOut.includes("Synced") ||
      ankiSyncOut.includes("TSV saved") ||
      ankiSyncOut.includes("TSV exported") ||
      ankiSyncOut.includes("Exported")
    );
    const expectedDeckTsv = path.join(sandboxDir, "decks", "paxos-and-raft-consensus.tsv");
    assert.ok(fs.existsSync(expectedDeckTsv));

    const tsvData = fs.readFileSync(expectedDeckTsv, "utf-8");
    assert.ok(tsvData.includes("Why can two majority quorums never disagree without noticing?"));
    assert.ok(tsvData.includes("tier::unconditional-truth"));
  });
});
