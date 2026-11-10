import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { derivePlanState, parseDagIds, validateDagLabels } from "../server/planstate.ts";
import type { ProgressRecord } from "../shared/protocol.ts";

const DAG = `graph TD
  N1["Nodo 1: la prima cosa è vera"] --> N2["Nodo 2: la seconda segue"]
  N2 --> N3["Nodo 3: il traguardo"]
  N1 --> N3`;

const rec = (r: Partial<ProgressRecord>): ProgressRecord => ({ at: "", sessionId: "s", course: "c", nodeRefs: [], kind: "check", ...r });

describe("parseDagIds", () => {
  it("returns ids in first-appearance order, deduplicated", () => {
    assert.deepEqual(parseDagIds(DAG), ["N1", "N2", "N3"]);
  });
  it("ignores ids never defined with a label and other prefixes", () => {
    assert.deepEqual(parseDagIds('graph TD\n L1["x"] --> N2["Nodo 2: y"]\n N9 --> N2'), ["N2"]);
  });
});

describe("validateDagLabels", () => {
  it("accepts a well-formed DAG", () => assert.deepEqual(validateDagLabels(DAG), []));
  it("accepts flowchart LR and leading blank lines", () => {
    assert.deepEqual(validateDagLabels('\n\nflowchart LR\nN1["Nodo 1: a b"]'), []);
  });
  it("rejects a missing header", () => {
    const e = validateDagLabels('N1["Nodo 1: a"]');
    assert.equal(e.length, 1);
    assert.match(e[0], /line 1/);
  });
  it("rejects unnumbered, unquoted and mis-numbered labels with line numbers", () => {
    const e = validateDagLabels('graph TD\nN1["Primo"] --> N2[Nodo 2: x]\nN3["Nodo 4: y"]');
    assert.equal(e.length, 3);
    assert.match(e[0], /line 2.*N1/);
    assert.match(e[1], /line 2.*N2/);
    assert.match(e[2], /line 3.*N3/);
  });
  it("requires at least one plan node", () => {
    assert.match(validateDagLabels("graph TD\nA --> B").join(), /no plan node/);
  });
});

describe("derivePlanState", () => {
  const base = { topic: "t", goal: "g", dag: DAG, approved: true };
  it("marks captured nodes done and the first todo current", () => {
    const s = derivePlanState(base, 1, []);
    assert.deepEqual(s.nodeStatus, { N1: "done", N2: "current", N3: "todo" });
    assert.equal(s.approved, true);
  });
  it("nothing captured: N1 current", () => {
    assert.deepEqual(derivePlanState(base, 0, []).nodeStatus, { N1: "current", N2: "todo", N3: "todo" });
  });
  it("a failed latest check overrides done with missed", () => {
    const s = derivePlanState(base, 2, [rec({ nodeRefs: ["N1"], correct: false })]);
    assert.deepEqual(s.nodeStatus, { N1: "missed", N2: "done", N3: "current" });
  });
  it("a later correct check clears the miss", () => {
    const s = derivePlanState(base, 1, [rec({ nodeRefs: ["N1"], correct: false }), rec({ nodeRefs: ["N1"], correct: true })]);
    assert.equal(s.nodeStatus.N1, "done");
  });
  it("current follows the latest predict/check when that node is todo", () => {
    const s = derivePlanState(base, 1, [rec({ kind: "predict", nodeRefs: ["N3"], correct: true })]);
    assert.deepEqual(s.nodeStatus, { N1: "done", N2: "todo", N3: "current" });
  });
  it("probe records and unknown refs do not move current", () => {
    const s = derivePlanState(base, 0, [rec({ kind: "probe", nodeRefs: ["N3"] }), rec({ nodeRefs: ["Z9"] })]);
    assert.equal(s.nodeStatus.N1, "current");
  });
});
