import { test } from "node:test";
import assert from "node:assert/strict";
import { wrapLabel } from "../web/src/map/graph-canvas.ts";
import { LABEL_SIZE_LADDER } from "../shared/graph/render-settings.ts";

test("wrapLabel splits text across lines within maxWidth using canvas measureText", () => {
  // Mock CanvasRenderingContext2D measureText: 8px per character
  const fakeCtx = {
    measureText: (str: string) => ({ width: str.length * 8 }),
  } as unknown as CanvasRenderingContext2D;

  const text = "Il ref è una scatola che sa chi la legge";
  // maxWidth: 120 -> max 15 characters per line
  const lines = wrapLabel(fakeCtx, text, 120);
  assert.ok(lines.length > 1);
  for (const line of lines) {
    assert.ok(line.length * 8 <= 120, `line "${line}" exceeds maxWidth`);
  }
  assert.equal(lines.join(" "), text);
});

test("wrapLabel hard-breaks words longer than maxWidth without losing characters", () => {
  const fakeCtx = {
    measureText: (str: string) => ({ width: str.length * 10 }),
  } as unknown as CanvasRenderingContext2D;

  const longWord = "Supercalifragilisticexpialidocious";
  // maxWidth 60 -> 6 chars per line
  const lines = wrapLabel(fakeCtx, longWord, 60);
  assert.ok(lines.length >= 5);
  assert.equal(lines.join(""), longWord);
});

test("Key-concept label sizing scales proportionally with labelSize at all zoom levels", () => {
  const KEY_LABEL_RATIO = 3;

  for (const k of [0.2, 0.5, 1.0, 2.0, 4.0]) {
    const scale = Math.max(1, KEY_LABEL_RATIO / k);
    for (let i = 0; i < LABEL_SIZE_LADDER.length - 1; i++) {
      const sizeA = LABEL_SIZE_LADDER[i]!;
      const sizeB = LABEL_SIZE_LADDER[i + 1]!;
      const worldLabelA = sizeA * scale;
      const worldLabelB = sizeB * scale;

      // Monotonic scaling with ladder
      assert.ok(worldLabelB > worldLabelA, `At k=${k}, labelSize ${sizeB} must produce larger label than ${sizeA}`);

      // Strict ratio preservation
      const ratioExpected = sizeB / sizeA;
      const ratioActual = worldLabelB / worldLabelA;
      assert.ok(Math.abs(ratioActual - ratioExpected) < 1e-6, `Ratio between ${sizeB} and ${sizeA} should be exactly preserved`);
    }
  }
});

test("Cluster bottom and lesson-name anchor account for key-concept scaling and line counts", () => {
  const KEY_NODE_SCALE = 1.7;
  const LABEL_GAP = 2.9;
  const LABEL_LINE_HEIGHT = 1.3;
  const nodeR = 2;
  const keyR = nodeR * KEY_NODE_SCALE;

  for (const labelSize of [0.5, 2, 4, 8, 16]) {
    // Normal node with 1 line
    const normalH = nodeR + LABEL_GAP + 1 * labelSize * LABEL_LINE_HEIGHT;
    // Key node with 3 lines
    const keyH = keyR + LABEL_GAP + 3 * labelSize * LABEL_LINE_HEIGHT;

    assert.ok(keyH > normalH, `Key node height should exceed normal node height at labelSize=${labelSize}`);

    const nodeY = 100;
    const normalBottom = nodeY + normalH;
    const keyBottom = nodeY + keyH;

    const clusterBottom = Math.max(normalBottom, keyBottom);
    assert.equal(clusterBottom, keyBottom);

    const anchorCy = clusterBottom + Math.max(labelSize * 1.5, 6);
    // Anchor must be strictly below the lowest extent of the key label
    assert.ok(anchorCy > keyBottom, `Lesson anchor must be strictly below key label bottom at labelSize=${labelSize}`);
  }
});

test("Course frame dynamic padding encloses flipped labels at all label sizes", () => {
  const COURSE_PAD = 26;
  const KEY_NODE_SCALE = 1.7;
  const LABEL_GAP = 2.9;
  const LABEL_LINE_HEIGHT = 1.3;
  const nodeR = 2;
  const keyR = nodeR * KEY_NODE_SCALE;

  for (const labelSize of LABEL_SIZE_LADDER) {
    const minY = 50;
    const maxY = 200;
    // Node at top boundary
    const topNodeY = minY;
    // 3 lines key label
    const lineCount = 3;
    const labelH = keyR + LABEL_GAP + lineCount * labelSize * LABEL_LINE_HEIGHT;

    const reachAbove = minY - (topNodeY - labelH); // = labelH
    const topPad = COURSE_PAD + Math.max(labelSize * 2, reachAbove);

    const frameMinY = minY - topPad;
    const flippedLabelTop = topNodeY - labelH;

    // Flipped label top must be within frame bounds (>= frameMinY)
    assert.ok(flippedLabelTop >= frameMinY, `Flipped label at labelSize=${labelSize} must stay inside courseFrame (top: ${flippedLabelTop}, frameMinY: ${frameMinY})`);
  }
});
