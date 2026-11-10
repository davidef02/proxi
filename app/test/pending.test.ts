import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { ChatItem, InteractiveItem } from "../shared/protocol.ts";
import { openProblem, pendingItem } from "../web/src/session/pending.ts";

describe("pendingItem authority & alignment (Issue #27)", () => {
  const quiz1: InteractiveItem = {
    kind: "quiz",
    id: "req:req-1",
    at: "2026-10-30T10:00:00.000Z",
    requestId: "req-1",
    input: {
      kind: "check",
      question: "Prima domanda?",
      options: [{ label: "A" }, { label: "B" }],
      correctAnswer: "B",
      explanation: "Perché sì",
    },
    displayOrder: [0, 1],
  };

  const quiz2: InteractiveItem = {
    kind: "quiz",
    id: "req:req-2",
    at: "2026-10-30T10:01:00.000Z",
    requestId: "req-2",
    input: {
      kind: "check",
      question: "Seconda domanda?",
      options: [{ label: "X" }, { label: "Y" }],
      correctAnswer: "Y",
      explanation: "Perché no",
    },
    displayOrder: [0, 1],
  };

  const ask1: InteractiveItem = {
    kind: "ask",
    id: "req:req-ask-1",
    at: "2026-10-30T10:00:30.000Z",
    requestId: "req-ask-1",
    input: {
      question: "Come vuoi procedere?",
      options: [{ label: "Avanti" }, { label: "Approfondisci" }],
    },
  };

  it("returns null when pendingRequestId is null, even with open interactive items in history", () => {
    const items: ChatItem[] = [quiz1, ask1, quiz2];
    assert.equal(pendingItem(items, null), null);
  });

  it("returns null when pendingRequestId is undefined", () => {
    const items: ChatItem[] = [quiz1];
    assert.equal(pendingItem(items, undefined), null);
  });

  it("returns null when pendingRequestId is not found in items (never falls back to last open item)", () => {
    const items: ChatItem[] = [quiz1, quiz2];
    assert.equal(pendingItem(items, "req-non-existent"), null);
  });

  it("returns the exact item when pendingRequestId matches an open interactive item", () => {
    const items: ChatItem[] = [quiz1, quiz2];
    const match = pendingItem(items, "req-2");
    assert.equal(match?.requestId, "req-2");
    assert.equal(match?.id, "req:req-2");
  });

  it("returns null if the matching item is cancelled", () => {
    const cancelledQuiz: InteractiveItem = { ...quiz1, cancelled: true };
    const items: ChatItem[] = [cancelledQuiz];
    assert.equal(pendingItem(items, "req-1"), null);
  });

  it("returns null if the matching item already has a result (quiz answered)", () => {
    const answeredQuiz: InteractiveItem = {
      ...quiz1,
      result: { selected: ["B"], dontKnow: false, correct: true },
    };
    const items: ChatItem[] = [answeredQuiz];
    assert.equal(pendingItem(items, "req-1"), null);
  });

  it("returns null if the matching item already has an answer (ask answered)", () => {
    const answeredAsk: InteractiveItem = {
      ...ask1,
      answer: { selected: ["Avanti"] },
    };
    const items: ChatItem[] = [answeredAsk];
    assert.equal(pendingItem(items, "req-ask-1"), null);
  });

  it("returns null if the matching item already has a decision (plan decided)", () => {
    const planItem: InteractiveItem = {
      kind: "plan",
      id: "req:req-plan",
      at: "2026-10-30T10:00:00.000Z",
      requestId: "req-plan",
      input: { topic: "T", approach: "prosa", goal: "G", dag: "" },
      decision: { approved: true },
    };
    const items: ChatItem[] = [planItem];
    assert.equal(pendingItem(items, "req-plan"), null);
  });

  it("supersedes older unanswered questions and stays aligned with server authority", () => {
    // 1. First question arrives: pending is req-1
    let items: ChatItem[] = [quiz1];
    assert.equal(pendingItem(items, "req-1")?.requestId, "req-1");

    // 2. Second question arrives: pending shifts to req-2
    items = [quiz1, quiz2];
    assert.equal(pendingItem(items, "req-2")?.requestId, "req-2");

    // 3. Turn completes or aborts: server pendingRequestId becomes null
    // Stale questions must NEVER be resurrected
    assert.equal(pendingItem(items, null), null);
  });
});

describe("openProblem", () => {
  it("returns null when no problem items exist", () => {
    assert.equal(openProblem([]), null);
  });

  it("returns open problem when conclusion is missing", () => {
    const problem: ChatItem = {
      kind: "problem",
      id: "prob-1",
      at: "2026-10-30T10:00:00.000Z",
      input: {
        statement: "Do this",
        mode: "recall",
        nodeKeys: ["k1"],
        modelSolution: "Sol",
      },
    };
    assert.equal(openProblem([problem])?.id, "prob-1");
  });

  it("returns null when problem already has a conclusion", () => {
    const problem: ChatItem = {
      kind: "problem",
      id: "prob-1",
      at: "2026-10-30T10:00:00.000Z",
      input: {
        statement: "Do this",
        mode: "recall",
        nodeKeys: ["k1"],
        modelSolution: "Sol",
      },
      conclusion: {
        outcome: "solved",
        verdict: "ottimo",
        rung: "R3",
        missClass: null,
        retest: false,
      },
    };
    assert.equal(openProblem([problem]), null);
  });
});
