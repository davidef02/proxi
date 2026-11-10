import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { computeDockBudget, forwardDockWheel } from "../web/src/session/dock-budget.ts";

describe("computeDockBudget (Issue #26)", () => {
  it("allocates balanced height on standard desktop viewport (800px)", () => {
    const budget = computeDockBudget({
      columnHeight: 800,
      footerHeight: 48,
      naturalContentHeight: 350,
    });

    // Min transcript is max(140, 800 * 0.25 = 200) = 200px
    // Max dock space = 800 - 200 - 20 = 580px
    // Natural height (350) + footer (48) + border (2) = 400px < 580px
    assert.equal(budget.shellMaxHeight, 400);
    assert.equal(budget.scrollWholeContent, false);
    assert.ok(budget.transcriptSlice >= 200);
    assert.equal(budget.bodyMaxHeight, 580 - 50);
  });

  it("clamps dock height to protect transcript on constrained laptop viewport (550px)", () => {
    const budget = computeDockBudget({
      columnHeight: 550,
      footerHeight: 48,
      naturalContentHeight: 500, // natural question is taller than available dock space
    });

    // Min transcript is max(140, round(550 * 0.25 = 138)) = 140px
    // Max dock space = 550 - 140 - 20 = 390px
    assert.equal(budget.shellMaxHeight, 390);
    assert.equal(budget.bodyMaxHeight, 390 - 48 - 2);
    assert.equal(budget.transcriptSlice, 140);
    assert.equal(budget.scrollWholeContent, false);
  });

  it("manages short window (350px) preserving at least 140px for transcript", () => {
    const budget = computeDockBudget({
      columnHeight: 350,
      footerHeight: 50,
      naturalContentHeight: 300,
    });

    // Max dock space = 350 - 140 - 20 = 190px
    assert.equal(budget.shellMaxHeight, 190);
    assert.equal(budget.bodyMaxHeight, 190 - 50 - 2); // 138px
    assert.equal(budget.transcriptSlice, 140);
    assert.equal(budget.scrollWholeContent, false);
  });

  it("falls back to scrollWholeContent on extreme short window without negative values", () => {
    const budget = computeDockBudget({
      columnHeight: 200,
      footerHeight: 50,
      naturalContentHeight: 300,
    });

    // Max dock space = 200 - 140 - 20 = 40px
    // bodyMaxHeight = max(40 - 50 - 2, 0) = 0 <= bodyPadding (24) -> scrollWholeContent = true
    assert.equal(budget.scrollWholeContent, true);
    assert.equal(budget.shellMaxHeight, 40);
    assert.equal(budget.bodyMaxHeight, 0);
    assert.ok(budget.transcriptSlice >= 140);
  });
});

describe("forwardDockWheel (Issue #26)", () => {
  function makeMockElement(props: Partial<HTMLElement> & { overflowY?: string } = {}): HTMLElement {
    const el = {
      scrollTop: props.scrollTop ?? 0,
      scrollHeight: props.scrollHeight ?? 100,
      clientHeight: props.clientHeight ?? 100,
      parentElement: (props.parentElement as any) ?? null,
      style: {
        overflowY: props.overflowY ?? "",
      },
    };
    return el as unknown as HTMLElement;
  }

  function makeMockWheelEvent(deltaY: number, deltaMode = 0, target: HTMLElement): { event: WheelEvent; prevented: boolean } {
    let prevented = false;
    const event = {
      deltaY,
      deltaMode,
      target,
      preventDefault: () => {
        prevented = true;
      },
    } as unknown as WheelEvent;
    return { event, get prevented() { return prevented; } };
  }

  it("does nothing when deltaY is 0", () => {
    const transcript = makeMockElement({ scrollTop: 0 });
    const dockRoot = makeMockElement();
    const tracker = makeMockWheelEvent(0, 0, dockRoot);

    forwardDockWheel(tracker.event, dockRoot, transcript);
    assert.equal(tracker.prevented, false);
    assert.equal(transcript.scrollTop, 0);
  });

  it("does not forward when inner scrollable container can scroll down", () => {
    const dockRoot = makeMockElement();
    const inner = makeMockElement({
      scrollTop: 10,
      scrollHeight: 200,
      clientHeight: 100,
      overflowY: "auto",
      parentElement: dockRoot,
    });
    const transcript = makeMockElement({ scrollTop: 50 });
    const tracker = makeMockWheelEvent(20, 0, inner);

    forwardDockWheel(tracker.event, dockRoot, transcript);
    assert.equal(tracker.prevented, false);
    assert.equal(transcript.scrollTop, 50);
  });

  it("forwards scroll to transcript when inner container reaches bottom", () => {
    const dockRoot = makeMockElement();
    const inner = makeMockElement({
      scrollTop: 100, // at bottom (scrollHeight 200 - clientHeight 100 = 100)
      scrollHeight: 200,
      clientHeight: 100,
      overflowY: "auto",
      parentElement: dockRoot,
    });
    const transcript = makeMockElement({ scrollTop: 50 });
    const tracker = makeMockWheelEvent(25, 0, inner);

    forwardDockWheel(tracker.event, dockRoot, transcript);
    assert.equal(tracker.prevented, true);
    assert.equal(transcript.scrollTop, 75);
  });

  it("forwards scroll with line multiplier (deltaMode 1)", () => {
    const dockRoot = makeMockElement();
    const transcript = makeMockElement({ scrollTop: 100 });
    const tracker = makeMockWheelEvent(-2, 1, dockRoot);

    forwardDockWheel(tracker.event, dockRoot, transcript);
    assert.equal(tracker.prevented, true);
    assert.equal(transcript.scrollTop, 100 + -2 * 16); // 68
  });
});
