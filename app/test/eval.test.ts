// Unit tests for the eval scenario loader and the anchor checker (no model, no server).
import { describe, test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import type { CognitiveNodePayload } from "../../extensions/html-lesson.ts";
import { checkAnchors, loadScenario } from "../eval/check.ts";

const RUN = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "eval", "run.ts");
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

const node = (over: Partial<CognitiveNodePayload> = {}): CognitiveNodePayload => ({
  nodeId: "n1",
  title: "Un nodo",
  tier: "unconditional-truth",
  content: "Contenuto.",
  ...over,
});

const VUE = new Map([["n_vue_ref", "Ref di Vue"]]);

describe("loadScenario", () => {
  test("seeds exactly the known ids the scenario declares", () => {
    const { scenario, dir } = loadScenario("lezione-singola-nuxt");
    assert.equal(scenario.knownIds.length, 5);
    const ids = fs
      .readFileSync(path.join(dir, "graph", "nodes.jsonl"), "utf-8")
      .split(/\r?\n/)
      .filter((l) => l.trim())
      .map((l) => JSON.parse(l).id as string);
    for (const id of scenario.knownIds) assert.ok(ids.includes(id), `${id} missing from the scenario graph`);
  });

  test("an unknown scenario throws, naming it", () => {
    assert.throws(() => loadScenario("nope"), /unknown eval scenario/);
  });
});

describe("checkAnchors", () => {
  test("passes when the prose names the anchored concept", () => {
    const r = checkAnchors(
      [node({ content: "Una scatola reattiva, come la ref di Vue.", anchors: [{ id: "n_vue_ref", name: "Ref di Vue" }] })],
      VUE,
    );
    assert.equal(r.ok, true);
    assert.deepEqual(r.failures, []);
  });

  test("a node without anchors and one anchored outside the known map fail as no anchor", () => {
    const r = checkAnchors(
      [
        node({ nodeId: "a" }),
        node({ nodeId: "b", anchors: [{ id: "n_other", name: "Altro" }] }),
      ],
      VUE,
    );
    assert.equal(r.ok, false);
    assert.ok(r.failures.includes("a: no anchor"));
    assert.ok(r.failures.includes("b: no anchor"));
  });

  test("fails when the prose omits the anchored name", () => {
    const r = checkAnchors([node({ nodeId: "c", content: "Niente di utile.", anchors: [{ id: "n_vue_ref", name: "Ref di Vue" }] })], VUE);
    assert.deepEqual(r.failures, ["c: prose does not name Ref di Vue"]);
  });

  test("markup and inline-code formatting around the name still count", () => {
    const r = checkAnchors(
      [
        node({
          content: "<p>Una scatola reattiva, come la <strong><code>ref</code></strong> di Vue.</p>",
          anchors: [{ id: "n_vue_ref", name: "Ref di Vue" }],
        }),
      ],
      VUE,
    );
    assert.deepEqual(r.failures, []);
  });

  test("markdown emphasis, case and the stored name all still name the concept", () => {
    const r = checkAnchors(
      [
        node({
          nodeId: "md",
          content: "<p>come **`Ref`** di Vue</p>",
          anchors: [{ id: "n_vue_ref", name: "Nome che il tutor ha scelto" }],
        }),
        node({
          nodeId: "stored",
          content: "<p>Qui conta il nome salvato: Proprietà calcolate.</p>",
          anchors: [{ id: "n_vue_ref", name: "Proprietà calcolate" }],
        }),
      ],
      VUE,
    );
    assert.deepEqual(r.failures, []);
  });

  test("fails on an empty node list", () => {
    const r = checkAnchors([], VUE);
    assert.equal(r.ok, false);
    assert.equal(r.failures.length, 1);
  });
});
