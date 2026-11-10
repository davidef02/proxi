import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFileSync } from "node:child_process";
import {
  initLesson,
  appendNode,
  rebuildLesson,
  compileReference,
  type CognitiveNodePayload,
  type CognitiveStepPayload,
  type InitLessonOptions,
  parseLessonDocument,
  findLessonBySlug,
  parseMarkdownProse,
  renderNodeHtml,
  extractDistilledClaim
} from "../extensions/html-lesson.ts";

describe("Standalone HTML Lesson Engine & Responsive Template (Ticket #3)", () => {
  let sandboxDir: string;

  beforeEach(() => {
    sandboxDir = fs.mkdtempSync(path.join(os.tmpdir(), "learn-forme-lesson-test-"));
  });

  afterEach(() => {
    if (sandboxDir && fs.existsSync(sandboxDir)) {
      fs.rmSync(sandboxDir, { recursive: true, force: true });
    }
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 1: Lesson Initialization & Sequential Numbering
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 1: Lesson Initialization", () => {
    it("initializes 0001-<slug>.html with full responsive template and metadata", async () => {
      const options: InitLessonOptions = {
        topic: "Calculus: Derivatives & Rates of Change",
        title: "Understanding Derivatives from First Principles",
        description: "Motivated discovery of rates of change, tangents, and the power rule.",
        dagMermaid: "flowchart TD\n  A[\"Instantaneous Rate\"] --> B[\"Difference Quotient\"]\n  B --> C[\"Power Rule\"]",
        baseDir: sandboxDir,
      };

      const result = await initLesson(options);

      // Verify returned metadata
      assert.strictEqual(result.lessonNumber, 1);
      assert.strictEqual(result.slug, "understanding-derivatives-from-first-principles");
      assert.ok(fs.existsSync(result.filePath), "Lesson HTML file must be created on disk");

      const expectedPath = path.join(sandboxDir, "lessons", "0001-understanding-derivatives-from-first-principles.html");
      assert.strictEqual(path.resolve(result.filePath), path.resolve(expectedPath));

      // Read HTML and verify structural integrity
      const html = fs.readFileSync(result.filePath, "utf-8");

      assert.ok(html.includes("<title>Understanding Derivatives from First Principles | Proxi</title>"));
      assert.ok(html.includes('content="Personalized standalone lesson synthesized by Proxi"'));
      assert.ok(!html.includes("APP_NAME_SLOT"));

      // 1. HTML5 document & Viewport
      assert.match(html, /<!DOCTYPE html>/i);
      assert.match(html, /<meta\s+name=["']viewport["']\s+content=["'][^"']*width=device-width[^"']*["']/i);
      assert.ok(html.includes("Understanding Derivatives from First Principles") || html.includes("Calculus: Derivatives & Rates of Change"));

      // 2. KaTeX Integration
      assert.match(html, /https:\/\/cdn\.jsdelivr\.net\/npm\/katex/);
      assert.match(html, /renderMathInElement/);

      // 3. Mermaid Integration & DAG
      assert.match(html, /https:\/\/cdn\.jsdelivr\.net\/npm\/mermaid/);
      assert.match(html, /<pre class=["']mermaid["']>/);
      assert.ok(html.includes("flowchart TD"), "HTML must contain the Mermaid DAG");
      assert.ok(html.includes("Instantaneous Rate"), "HTML must include node labels from DAG");

      // 4. Dark/Light Theme Support
      assert.match(html, /data-theme/);
      assert.match(html, /--bg/);
      assert.match(html, /--fg/);

      // 5. Container for Cognitive Nodes
      assert.match(html, /id=["']lesson-nodes["']/);
    });

    it("rebuildLesson keeps the Proxi title and fills the app-name slot", async () => {
      const init = await initLesson({ topic: "Reti", title: "Il modello OSI", baseDir: sandboxDir });
      const rebuilt = await rebuildLesson({ lessonPath: init.filePath, baseDir: sandboxDir });
      const html = fs.readFileSync(rebuilt.filePath, "utf-8");
      assert.ok(html.includes("<title>Il modello OSI | Proxi</title>"));
      assert.ok(!html.includes("APP_NAME_SLOT"));
    });

    it("assigns sequential numbers properly (0001 -> 0002 -> 0003)", async () => {
      const lessonsDir = path.join(sandboxDir, "lessons");
      fs.mkdirSync(lessonsDir, { recursive: true });

      // Pre-seed an existing lesson
      fs.writeFileSync(path.join(lessonsDir, "0001-linear-algebra.html"), "<html></html>", "utf-8");

      const result1 = await initLesson({
        topic: "Calculus",
        title: "Calculus Part 1",
        dagMermaid: "flowchart TD\n  A --> B",
        baseDir: sandboxDir,
      });
      assert.strictEqual(result1.lessonNumber, 2);
      assert.strictEqual(path.basename(result1.filePath), "0002-calculus-part-1.html");

      const result2 = await initLesson({
        topic: "Information Theory",
        title: "Entropy & Compression",
        dagMermaid: "flowchart TD\n  A --> B",
        baseDir: sandboxDir,
      });
      assert.strictEqual(result2.lessonNumber, 3);
      assert.strictEqual(path.basename(result2.filePath), "0003-entropy-and-compression.html");
    });
  });

  describe("Test 1a: Slug identity inside a course", () => {
    it("refuses a second lesson with the same slug", async () => {
      await initLesson({ topic: "Docker", title: "Compose e consegna", baseDir: sandboxDir });
      await assert.rejects(
        () => initLesson({ topic: "Docker", title: "Compose e consegna", baseDir: sandboxDir }),
        /already exists in this course/
      );
    });

    it("resolves --slug on the whole filename, not a substring of it", async () => {
      const long = await initLesson({
        topic: "Docker",
        title: "Come nginx trova il backend",
        baseDir: sandboxDir,
      });
      const lessonsDir = path.dirname(long.filePath);

      // "come-nginx" is a prefix of the lesson above: matching loosely rewrote that file.
      assert.equal(findLessonBySlug(lessonsDir, "come-nginx"), undefined);
      assert.equal(
        findLessonBySlug(lessonsDir, "come-nginx-trova-il-backend"),
        long.filePath
      );
    });
  });

  describe("Test 1b: Data island robustness", () => {
    it("survives a node whose content closes the script tag", async () => {
      const initRes = await initLesson({
        topic: "Script Tags",
        title: "Script Tags",
        baseDir: sandboxDir,
      });

      await appendNode({
        lessonPath: initRes.filePath,
        node: {
          nodeId: "closing-tag",
          title: "A node with </script> in its title",
          tier: "derivation",
          content: "Inline JS ends at </script>, which would terminate the island unescaped.",
        },
      });

      // The island must still be readable: the board, the append path and the Anki sync all
      // parse it, and an early </script> silently truncated the JSON.
      const doc = parseLessonDocument(fs.readFileSync(initRes.filePath, "utf-8"));
      assert.ok(doc);
      assert.strictEqual(doc.nodes.length, 1);
      assert.ok(doc.nodes[0].content.includes("</script>"));
      assert.ok(doc.nodes[0].title.includes("</script>"));
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 2: Incremental Node Updates & Node Structure
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 2: Incremental Node Updates", () => {
    it("appends multiple cognitive nodes incrementally while preserving existing content", async () => {
      const initRes = await initLesson({
        topic: "Derivatives",
        title: "Calculus Derivatives",
        dagMermaid: "flowchart TD\n  A --> B",
        baseDir: sandboxDir,
      });

      // 1. Append Node 1: Unconditional Truth
      const node1: CognitiveNodePayload = {
        nodeId: "rate-of-change-definition",
        title: "Average Rate of Change",
        tier: "unconditional-truth",
        motivation: "Understanding baseline rates",
        content: "Average speed over any interval is strictly total distance divided by elapsed time.",
        math: ["\\bar{v} = \\frac{\\Delta x}{\\Delta t}"],
        quiz: {
          question: "What is the average velocity over interval $\\Delta t$?",
          options: ["1. $\\frac{\\Delta x}{\\Delta t}$", "2. $\\Delta x \\cdot \\Delta t$", "3. $\\Delta t$"],
          correctIndex: 1,
          explanation: "By definition, average rate of change is the ratio of output delta to input delta.",
        },
      };

      const appendRes1 = await appendNode({ lessonPath: initRes.filePath, node: node1 });
      assert.strictEqual(appendRes1.nodeCount, 1);

      let html = fs.readFileSync(initRes.filePath, "utf-8");
      assert.ok(html.includes("Average Rate of Change"));
      assert.ok(html.includes("unconditional-truth") || html.includes("Unconditional Truth"));
      assert.ok(html.includes("\\frac{\\Delta x}{\\Delta t}"));
      assert.ok(html.includes("Average speed over any interval"));

      // 2. Append Node 2: Derivation with SVG and Code Block
      const node2: CognitiveNodePayload = {
        nodeId: "instantaneous-velocity-limit",
        title: "Instantaneous Velocity via Limit",
        tier: "derivation",
        motivation: "Going from macroscopic to infinitesimal",
        content: "As the time window shrinks toward zero, the secant line approaches the tangent line.",
        math: ["v(t) = \\lim_{\\Delta t \\to 0} \\frac{x(t + \\Delta t) - x(t)}{\\Delta t}"],
        visuals: [{ type: "svg-inline", content: "<svg viewBox=\"0 0 100 100\"><circle cx=\"50\" cy=\"50\" r=\"40\" fill=\"none\" stroke=\"currentColor\"/></svg>" }],
        quiz: {
          question: "What happens as $\\Delta t \\to 0$?",
          options: [
            "1. The secant slope converges to the instantaneous tangent slope.",
            "2. The slope becomes infinite unconditionally.",
            "3. The secant line becomes undefined."
          ],
          correctIndex: 1,
          explanation: "The difference quotient limit represents the derivative at that exact point.",
        },
      };

      const appendRes2 = await appendNode({ lessonPath: initRes.filePath, node: node2 });
      assert.strictEqual(appendRes2.nodeCount, 2);

      html = fs.readFileSync(initRes.filePath, "utf-8");

      // Verify Node 1 is STILL intact
      assert.ok(html.includes("rate-of-change-definition"));
      assert.ok(html.includes("Average Rate of Change"));

      // Verify Node 2 is present
      assert.ok(html.includes("instantaneous-velocity-limit"));
      assert.ok(html.includes("Instantaneous Velocity via Limit"));
      assert.ok(html.includes("derivation") || html.includes("Derivation"));
      assert.ok(html.includes("<svg viewBox=\"0 0 100 100\">"));
      assert.ok(html.includes("<details"));
      assert.ok(html.includes("The secant slope converges"));
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 3: Math & Formatting Syntax Integrity
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 3: Math & Formatting Syntax Integrity", () => {
    it("preserves complex LaTeX backslashes, KaTeX delimiters, and Mermaid syntax without corruption", async () => {
      const initRes = await initLesson({
        topic: "Advanced Calculus",
        title: "Integration by Parts & Multivariable Limits",
        dagMermaid: "flowchart TD\n  A[\"$$\\int u dv$$\"] --> B[\"$$\\iint_R f(x,y) dA$$\"]",
        baseDir: sandboxDir,
      });

      const complexNode: CognitiveNodePayload = {
        nodeId: "complex-math-node",
        title: "Integral Definition & Multi-line Proof",
        tier: "derivation",
        motivation: "Formalizing multi-dimensional calculus",
        content: "Consider the multi-line transformation:\n\n$$\\begin{aligned} \\int u \\, dv &= u v - \\int v \\, du \\\\ \\sum_{k=1}^n k^2 &= \\frac{n(n+1)(2n+1)}{6} \\end{aligned}$$\n\nNotice how $\\lim_{x \\to 0} \\frac{\\sin x}{x} = 1$.",
        math: [
          "\\int_{-\\infty}^{\\infty} e^{-x^2} dx = \\sqrt{\\pi}",
          "\\frac{\\partial^2 f}{\\partial x^2} + \\frac{\\partial^2 f}{\\partial y^2} = 0",
        ],
      };

      await appendNode({ lessonPath: initRes.filePath, node: complexNode });
      const html = fs.readFileSync(initRes.filePath, "utf-8");

      // Backslash checks
      assert.ok(html.includes("\\begin{aligned}"), "LaTeX environment \\begin{aligned} must be intact");
      assert.ok(html.includes("\\int u"), "Integral notation must be intact");
      assert.ok(html.includes("\\frac{n(n+1)(2n+1)}{6}"), "Fractions must preserve backslashes");
      assert.ok(html.includes("\\lim_{x"), "Limits must preserve backslashes");
      assert.ok(html.includes("\\sqrt{\\pi}"), "Square root and Greek letters must preserve backslashes");
      assert.ok(html.includes("\\partial^2 f"), "Partial derivatives must preserve backslashes");

      // KaTeX delimiter checks
      assert.ok(html.includes("$$\\begin{aligned}"), "Display math $$ delimiters must be preserved");
      assert.ok(html.includes("$\\lim_{x"), "Inline math $ delimiters must be preserved");

      // Mermaid DAG in HTML must remain intact
      assert.ok(html.includes("flowchart TD"));
      assert.ok(html.includes("$$\\int u dv$$"));
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 4: Reference Cheat Sheet Compilation
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 4: Reference Cheat Sheet Compilation", () => {
    it("extracts unconditional truths, formulas, and DAG into reference/<slug>.html", async () => {
      const initRes = await initLesson({
        topic: "Packet Transmission",
        title: "Networking Fundamentals",
        dagMermaid: "flowchart TD\n  A[\"Packet Transmission\"] --> B[\"TCP Flow Control\"]",
        baseDir: sandboxDir,
      });

      // 1. Add an Unconditional Truth
      await appendNode({
        lessonPath: initRes.filePath,
        node: {
          nodeId: "packet-axiom",
          title: "Packet Transmission Axiom",
          tier: "unconditional-truth",
          motivation: "Foundational law of computer networks",
          content: "ALL communication between computers across networks is performed via packet transmission.",
          math: ["\\text{Throughput} = \\frac{\\text{Window Size}}{\\text{RTT}}"],
        },
      });

      // 2. Add a Derived Concept
      await appendNode({
        lessonPath: initRes.filePath,
        node: {
          nodeId: "tcp-handshake",
          title: "3-Way Handshake",
          tier: "derivation",
          motivation: "Connection establishment in hostile transport layers",
          content: "SYN, SYN-ACK, ACK sequence synchronizes sequence numbers between endpoints.",
        },
      });

      // 3. Add a Node with explicit summary distillation
      await appendNode({
        lessonPath: initRes.filePath,
        node: {
          nodeId: "tcp-teardown",
          title: "Connection Teardown",
          tier: "derivation",
          summary: "FIN e ACK chiudono ordinatamente la connessione.",
          content: "Spiegazione estesa con molti dettagli tecnici secondari...",
        },
      });

      // Compile Reference Sheet
      const refRes = await compileReference({
        lessonPath: initRes.filePath,
        baseDir: sandboxDir,
      });

      assert.strictEqual(refRes.slug, "networking-fundamentals");
      assert.strictEqual(refRes.unconditionalTruthCount, 1);
      assert.ok(fs.existsSync(refRes.filePath), "Reference file must exist on disk");

      const expectedPath = path.join(sandboxDir, "reference", "networking-fundamentals.html");
      assert.strictEqual(path.resolve(refRes.filePath), path.resolve(expectedPath));

      const refHtml = fs.readFileSync(refRes.filePath, "utf-8");

      assert.match(refHtml, /<title>Sintesi: [^<]* \| Proxi<\/title>/);

      // Verifications for Reference Sheet
      assert.match(refHtml, /Sintesi|Reference Cheat Sheet|Quick Reference/i);
      assert.ok(refHtml.includes("Packet Transmission Axiom"));
      assert.ok(refHtml.includes("ALL communication between computers"));
      assert.ok(refHtml.includes("\\text{Throughput}"));
      assert.ok(refHtml.includes("flowchart TD"));
      assert.match(refHtml, /@media\s+print/);
      // Issue #6 verifications: derivations are included, Italian badges, and data island
      assert.ok(refHtml.includes("3-Way Handshake"), "Derivations must be included in reference synthesis");
      assert.ok(refHtml.includes("Derivazione"), "Must display Italian Derivazione badge");
      assert.ok(refHtml.includes("Verità incondizionata"), "Must display Italian Verità badge");
      assert.ok(refHtml.includes('id="reference-data"'), "Must embed #reference-data JSON script");
      assert.ok(refHtml.includes("FIN e ACK chiudono ordinatamente"), "Uses explicit summary when provided");
      assert.strictEqual(refRes.nodeCount, 3);
      assert.strictEqual(refRes.derivationCount, 2);
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // TEST 5: CLI Execution & Error Handling
  // ──────────────────────────────────────────────────────────────────────────
  describe("Test 5: CLI Execution & Error Handling", () => {
    const cliScript = path.resolve("extensions/html-lesson.ts");

    it("executes init command via CLI and creates lesson file", () => {
      const output = execFileSync(
        process.execPath,
        [
          "--import",
          "tsx",
          cliScript,
          "init",
          "--topic",
          "Linear Algebra",
          "--title",
          "Matrix Transformations",
          "--dag",
          "flowchart TD\n A-->B",
          "--base-dir",
          sandboxDir,
        ],
        { encoding: "utf-8", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
      );

      const parsed = JSON.parse(output);
      assert.strictEqual(parsed.lessonNumber, 1);
      assert.strictEqual(parsed.slug, "matrix-transformations");
      assert.ok(fs.existsSync(parsed.filePath));
    });

    it("executes append command via CLI with JSON payload", async () => {
      const initRes = await initLesson({
        topic: "Graph Theory",
        title: "Eulerian Paths",
        dagMermaid: "flowchart TD\n A-->B",
        baseDir: sandboxDir,
      });

      const nodePayload: CognitiveNodePayload = {
        nodeId: "euler-degree-condition",
        title: "Degree Parity Theorem",
        tier: "unconditional-truth",
        motivation: "Conditions for Eulerian paths",
        content: "A connected graph has an Eulerian path if and only if 0 or 2 vertices have odd degree.",
      };

      const output = execFileSync(
        process.execPath,
        [
          "--import",
          "tsx",
          cliScript,
          "append",
          "--lesson",
          initRes.filePath,
          "--data",
          JSON.stringify(nodePayload),
        ],
        { encoding: "utf-8", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
      );

      const parsed = JSON.parse(output);
      assert.strictEqual(parsed.nodeId, "euler-degree-condition");
      assert.strictEqual(parsed.nodeCount, 1);

      const html = fs.readFileSync(initRes.filePath, "utf-8");
      assert.ok(html.includes("Degree Parity Theorem"));
    });

    it("gracefully handles missing parameters or invalid JSON with non-zero exit code", () => {
      assert.throws(
        () => {
          execFileSync(
            process.execPath,
            ["--import", "tsx", cliScript, "init", "--base-dir", sandboxDir],
            { stdio: "pipe", env: { ...process.env, LEARNER_DATA_ROOT: sandboxDir } }
          );
        },
        (error: any) => {
          assert.strictEqual(error.status, 1);
          const stderr = error.stderr.toString();
          assert.match(stderr, /Missing required/i);
          return true;
        }
      );
    });
  });
});

describe("parseMarkdownProse keeps authored HTML intact", () => {
  it("does not wrap the lines of a multi-line <pre> block in <p>, even when a line closes one block and opens the next", () => {
    const html = "<p>a</p><pre><code>x = 1;\ny = 2;</code></pre><p>b</p><pre><code>z = 3;\n  w = 4;\n</code></pre><p>c</p>";
    const out = parseMarkdownProse(html);
    assert.ok(!out.includes("<p>y = 2"), "second line of first block was wrapped");
    assert.ok(out.includes("\n  w = 4;\n"), "indentation inside the second block was lost");
    assert.equal((out.match(/<p>/g) || []).length, 3);
  });

  it("still wraps plain prose lines in <p>", () => {
    assert.equal(parseMarkdownProse("solo `testo`"), "<p>solo <code>testo</code></p>");
  });
});

describe("CognitiveStepPayload and Multi-Step Node Rendering (Issue #13)", () => {
  it("renders steps inside <div class=\"node-steps\"> with stepId, title, content, math, codeBlocks, visuals", () => {
    const step1: CognitiveStepPayload = {
      stepId: "step-1-observe",
      title: "Osservazione del pattern",
      content: "Notiamo che il segnale **cresce esponenzialmente**.",
      math: ["f(x) = 2^x"],
      codeBlocks: [{ language: "ts", code: "const y = Math.pow(2, x);", caption: "Calcolo" }],
      visuals: [{ type: "svg-inline", content: "<svg viewBox=\"0 0 10 10\"><circle cx=\"5\" cy=\"5\" r=\"5\"/></svg>" }],
    };
    const step2: CognitiveStepPayload = {
      stepId: "step-2-derive",
      title: "Derivazione della proprietà",
      content: "La derivata preserva la forma esponenziale.",
    };

    const node: CognitiveNodePayload = {
      nodeId: "exponential-growth",
      title: "Crescita Esponenziale",
      tier: "derivation",
      content: "",
      steps: [step1, step2],
    };

    const html = renderNodeHtml(node, 0);

    assert.ok(html.includes("class=\"node-steps\""), "Must contain node-steps container");
    assert.ok(html.includes("id=\"step-step-1-observe\""), "Must contain step 1 id");
    assert.ok(html.includes("Osservazione del pattern"), "Must contain step 1 title");
    assert.ok(html.includes("<strong>cresce esponenzialmente</strong>"), "Must parse inline markdown in step 1");
    assert.ok(html.includes("f(x) = 2^x"), "Must contain step 1 math");
    assert.ok(html.includes("const y = Math.pow(2, x);"), "Must contain step 1 code");
    assert.ok(html.includes("Calcolo"), "Must contain code caption");
    assert.ok(html.includes("<svg viewBox=\"0 0 10 10\">"), "Must contain step 1 visual");
    assert.ok(html.includes("id=\"step-step-2-derive\""), "Must contain step 2 id");
    assert.ok(html.includes("Derivazione della proprietà"), "Must contain step 2 title");
    assert.ok(!html.includes("step-establish"), "Must not render step-establish when steps are present");
  });

  it("maintains backwards compatibility when steps is absent or empty", () => {
    const legacyNode: CognitiveNodePayload = {
      nodeId: "legacy-node",
      title: "Nodo Classico",
      tier: "unconditional-truth",
      motivation: "Perché serve questo concetto",
      content: "Contenuto stabilito direttamente nel nodo.",
    };

    const html = renderNodeHtml(legacyNode, 0);
    assert.ok(html.includes("step-motivate"), "Must render step-motivate for legacy node");
    assert.ok(html.includes("Perché serve questo concetto"), "Must render motivation");
    assert.ok(html.includes("step-establish prose"), "Must render step-establish for legacy node");
    assert.ok(html.includes("Contenuto stabilito direttamente nel nodo."), "Must render node content");
    assert.ok(!html.includes("node-steps"), "Must not render node-steps for legacy node");
  });

  it("extractDistilledClaim extracts from first step when summary/targetFact/caveat are absent", () => {
    const nodeWithSteps: CognitiveNodePayload = {
      nodeId: "multi-step",
      title: "Titolo del Nodo",
      tier: "derivation",
      content: "",
      steps: [
        {
          stepId: "step-1",
          content: "La prima affermazione fondamentale del nodo. Questa è la seconda frase esplicativa. E una terza frase non inclusa.",
        },
        {
          stepId: "step-2",
          content: "Ulteriori dettagli che non dovrebbero essere estratti come claim principale.",
        },
      ],
    };

    const claim = extractDistilledClaim(nodeWithSteps);
    assert.equal(claim, "La prima affermazione fondamentale del nodo. Questa è la seconda frase esplicativa.");
  });

  it("extractDistilledClaim prioritizes summary, targetFact, and caveat over first step", () => {
    const nodeWithSummary: CognitiveNodePayload = {
      nodeId: "step-summary",
      title: "Titolo",
      tier: "derivation",
      content: "",
      summary: "Sintesi esplicita dell'insegnante.",
      steps: [{ stepId: "s1", content: "Contenuto del passo 1." }],
    };
    assert.equal(extractDistilledClaim(nodeWithSummary), "Sintesi esplicita dell'insegnante.");

    const nodeWithFact: CognitiveNodePayload = {
      nodeId: "step-fact",
      title: "Titolo",
      tier: "derivation",
      content: "",
      ankiParams: { targetFact: "Fatto chiave memorizzabile." },
      steps: [{ stepId: "s1", content: "Contenuto del passo 1." }],
    };
    assert.equal(extractDistilledClaim(nodeWithFact), "Fatto chiave memorizzabile.");

    const nodeWithCaveat: CognitiveNodePayload = {
      nodeId: "step-caveat",
      title: "Titolo",
      tier: "derivation",
      content: "",
      steps: [{ stepId: "s1", content: "<strong>La verità da tenere senza caveat.</strong> Verità estratta dal caveat." }],
    };
    assert.equal(extractDistilledClaim(nodeWithCaveat), "Verità estratta dal caveat.");
  });
});
