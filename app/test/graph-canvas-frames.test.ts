import { test } from "node:test";
import assert from "node:assert/strict";
import {
  COURSE_TITLE_PX,
  courseFrameExtraBottom,
  courseTitleFontSize,
  drawsLessonLabels,
} from "../web/src/map/graph-canvas.ts";
import { DEFAULT_RENDER_SETTINGS } from "../shared/graph/render-settings.ts";

test("course frames reserve no room for lesson names when lesson labels are off", () => {
  const settings = { clusterLabels: false, clusterLabelSize: 14 };
  for (const k of [0.25, 1, 3]) {
    assert.equal(courseFrameExtraBottom(settings, k), 0);
  }
});

test("turning lesson labels on restores the room under course frames", () => {
  const settings = { clusterLabels: true, clusterLabelSize: 14 };
  const at1 = courseFrameExtraBottom(settings, 1);
  const at2 = courseFrameExtraBottom(settings, 2);
  assert.ok(Math.abs(at1 - 64.6) < 1e-9, `k=1 gave ${at1}`);
  assert.ok(Math.abs(at2 - 32.3) < 1e-9, `k=2 gave ${at2}`);
});

test("course titles keep the same screen size at any zoom", () => {
  for (const k of [0.25, 1, 3]) {
    assert.ok(Math.abs(courseTitleFontSize(k) * k - COURSE_TITLE_PX) < 1e-9);
  }
  assert.equal(COURSE_TITLE_PX, 15);
});

test("the lesson label pass runs only when lesson labels are on", () => {
  assert.equal(drawsLessonLabels({ clusterLabels: false }, 0.9), false);
  assert.equal(drawsLessonLabels({ clusterLabels: true }, 0.9), true);
  assert.equal(drawsLessonLabels({ clusterLabels: true }, 0.55), true);
  assert.equal(drawsLessonLabels({ clusterLabels: true }, 0), false);
});

test("with default settings the map skips lesson labels and their frame padding", () => {
  for (const alpha of [0.55, 0.9]) {
    assert.equal(drawsLessonLabels(DEFAULT_RENDER_SETTINGS, alpha), false);
  }
  assert.equal(courseFrameExtraBottom(DEFAULT_RENDER_SETTINGS, 1), 0);
});
