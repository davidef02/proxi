import { test } from "node:test";
import assert from "node:assert/strict";
import {
  RING_SCALE,
  ringColor,
  statusRings,
  type CanvasNode,
} from "../web/src/map/graph-canvas.ts";

test("statusRings returns rings for visible shaky and missed concepts, skipping invisible, imp, and other statuses", () => {
  const nodes: CanvasNode[] = [
    { id: "n_shaky", x: 10, y: 20, label: "Shaky", status: "shaky" },
    { id: "n_missed", x: 30, y: 40, label: "Missed", status: "missed" },
    { id: "n_solid", x: 50, y: 60, label: "Solid", status: "solid" },
    { id: "n_taught", x: 70, y: 80, label: "Taught", status: "taught" },
    { id: "n_none", x: 90, y: 100, label: "None" },
    { id: "arg_x", x: 110, y: 120, label: "Arg", status: "shaky" },
    { id: "n_invisible", x: 130, y: 140, label: "Invisible", status: "shaky" },
  ];

  const keyNodes = new Set<string>();
  const isVisible = (id: string) => id !== "n_invisible";
  const nodeR = 2;
  const keyR = 3.4;

  const rings = statusRings(nodes, keyNodes, isVisible, nodeR, keyR);

  assert.equal(rings.length, 2);
  assert.deepEqual(
    rings.map((r) => ({ id: r.id, status: r.status })),
    [
      { id: "n_shaky", status: "shaky" },
      { id: "n_missed", status: "missed" },
    ],
  );
});

test("statusRings computes r = keyR * RING_SCALE for key nodes and nodeR * RING_SCALE for plain nodes", () => {
  const nodes: CanvasNode[] = [
    { id: "n_plain", x: 0, y: 0, label: "Plain", status: "shaky" },
    { id: "n_key", x: 10, y: 10, label: "Key", status: "missed" },
  ];

  const keyNodes = new Set<string>(["n_key"]);
  const isVisible = () => true;
  const nodeR = 2;
  const keyR = 4;

  const rings = statusRings(nodes, keyNodes, isVisible, nodeR, keyR);

  assert.equal(rings.length, 2);
  const plainRing = rings.find((r) => r.id === "n_plain");
  const keyRing = rings.find((r) => r.id === "n_key");

  assert.ok(plainRing);
  assert.ok(keyRing);
  assert.equal(plainRing.r, nodeR * RING_SCALE);
  assert.equal(keyRing.r, keyR * RING_SCALE);
  // The ring is centred on the disc it belongs to, not on the origin.
  assert.equal(plainRing.x, 0);
  assert.equal(plainRing.y, 0);
  assert.equal(keyRing.x, 10);
  assert.equal(keyRing.y, 10);
});

test("ringColor maps shaky -> warn, missed -> bad, and solid/taught/undefined -> null", () => {
  const colors = { warn: "#e0b765", bad: "#e5806f" };

  assert.equal(ringColor("shaky", colors), colors.warn);
  assert.equal(ringColor("missed", colors), colors.bad);
  assert.equal(ringColor("solid", colors), null);
  assert.equal(ringColor("taught", colors), null);
  assert.equal(ringColor(undefined, colors), null);
});
