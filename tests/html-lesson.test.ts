import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  type CognitiveNodePayload,
  type CognitiveStepPayload,
  parseMarkdownProse,
  renderNodeHtml,
  extractDistilledClaim
} from "../extensions/html-lesson.ts";

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
