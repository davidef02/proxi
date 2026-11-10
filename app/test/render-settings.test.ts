import { test } from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_RENDER_SETTINGS,
  restoreRenderSettings,
  sanitizeRenderSettings,
  sparseRenderSettings,
} from "../shared/graph/render-settings.ts";

const D = DEFAULT_RENDER_SETTINGS;

test("the default render settings hide lesson labels", () => {
  assert.equal(D.clusterLabels, false);
});

test("sanitizing the default render settings returns them unchanged", () => {
  assert.deepEqual(sanitizeRenderSettings(D), D);
});

test("default settings are stored as an empty object", () => {
  assert.deepEqual(sparseRenderSettings(D), {});
  assert.equal(JSON.stringify(sparseRenderSettings(D)), "{}");
  assert.deepEqual(sparseRenderSettings(sanitizeRenderSettings({})), {});
});

test("changing one setting stores only that field", () => {
  assert.deepEqual(sparseRenderSettings({ ...D, nodeSize: 3 }), { nodeSize: 3 });
  assert.deepEqual(sparseRenderSettings({ ...D, regions: { ...D.regions, enabled: false } }), {
    regions: { enabled: false },
  });
  assert.deepEqual(sparseRenderSettings({ ...D, clusterLabels: true }), { clusterLabels: true });
});

test("a sparse blob restores to the settings it was saved from", () => {
  const x = {
    ...D,
    nodeSize: 3,
    labelZoom: 0.75,
    clusterLabels: true,
    regions: { ...D.regions, enabled: false },
  };
  assert.deepEqual(restoreRenderSettings(JSON.parse(JSON.stringify(sparseRenderSettings(x)))), x);
  assert.deepEqual(restoreRenderSettings({}), D);
});

test("a legacy full blob keeps its tuning but takes the defaults of the removed controls", () => {
  const legacy = {
    ...D,
    sizeUnits: "world" as const,
    nodeSize: 3,
    labelSize: 6,
    labelZoom: 0.75,
    clusterLabels: true,
    clusterLabelSize: 18,
    edgeWidth: 2,
    arrowLength: 4,
    regions: { ...D.regions, enabled: false, bitmap: 384, dilation: 40, blur: 60, edgeAlpha: 0.4 },
    physics: { ...D.physics, gravity: 0.2 },
  };
  const restored = restoreRenderSettings(legacy);
  assert.deepEqual(restored, {
    ...D,
    nodeSize: 3,
    labelSize: 6,
    labelZoom: 0.75,
    regions: { ...D.regions, enabled: false, bitmap: 384 },
    physics: { ...D.physics, gravity: 0.2 },
  });
  assert.deepEqual(sparseRenderSettings(restored), {
    nodeSize: 3,
    labelSize: 6,
    labelZoom: 0.75,
    regions: { enabled: false, bitmap: 384 },
    physics: { gravity: 0.2 },
  });
});

test("unreadable stored values restore the defaults", () => {
  for (const stored of [null, undefined, [], "x", 42]) {
    assert.deepEqual(restoreRenderSettings(stored), D);
  }
});

test("restoring and storing settings never mutate their input", () => {
  const legacy = {
    ...D,
    sizeUnits: "world" as const,
    clusterLabels: true,
    edgeWidth: 2,
    regions: { ...D.regions, dilation: 40 },
  };
  const before = JSON.parse(JSON.stringify(legacy));
  restoreRenderSettings(legacy);
  assert.deepEqual(legacy, before);
  const custom = { ...D, nodeSize: 3, regions: { ...D.regions, enabled: false } };
  const customBefore = JSON.parse(JSON.stringify(custom));
  sparseRenderSettings(custom);
  assert.deepEqual(custom, customBefore);
});

test("a legacy blob with an unreadable regions section restores the region defaults", () => {
  for (const regions of [null, "x", [], 42]) {
    const restored = restoreRenderSettings({ ...D, sizeUnits: "world", clusterLabels: true, nodeSize: 3, regions });
    assert.deepEqual(restored, { ...D, nodeSize: 3 });
  }
});

test("a pre-pivot legacy blob resets its sizes and takes the new lesson-label default", () => {
  const restored = restoreRenderSettings({ ...D, sizeUnits: "screen", nodeSize: 3, labelSize: 6, clusterLabels: true });
  assert.deepEqual(restored, D);
});

test("a stored off-ladder value snaps to the nearest ladder entry", () => {
  const restored = restoreRenderSettings({ nodeSize: 2.9, clusterLabels: "yes" });
  assert.equal(restored.nodeSize, 3);
  assert.equal(restored.clusterLabels, false);
  assert.deepEqual(sparseRenderSettings(restored), { nodeSize: 3 });
});

test("a single physics or region change stores only that sub-field", () => {
  assert.deepEqual(sparseRenderSettings({ ...D, physics: { ...D.physics, gravity: 0.2 } }), {
    physics: { gravity: 0.2 },
  });
  assert.deepEqual(sparseRenderSettings({ ...D, regions: { ...D.regions, bitmap: 384 } }), {
    regions: { bitmap: 384 },
  });
});
