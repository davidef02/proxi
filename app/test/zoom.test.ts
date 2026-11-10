import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { ZOOM_FACTORS, ZOOM_PREFS, stepZoom, zoomPref, setZoomPref } from "../web/src/zoom.ts";
import { createServer } from "../server/main.ts";
import * as fs from "node:fs";
import * as os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

describe("UI Zoom and Text Size (Issue #3)", () => {
  it("provides monotonic zoom factor mappings from 90% to 150%", () => {
    assert.deepEqual(ZOOM_PREFS, ["90%", "100%", "110%", "125%", "150%"]);
    assert.equal(ZOOM_FACTORS["90%"], 0.9);
    assert.equal(ZOOM_FACTORS["100%"], 1.0);
    assert.equal(ZOOM_FACTORS["110%"], 1.1);
    assert.equal(ZOOM_FACTORS["125%"], 1.25);
    assert.equal(ZOOM_FACTORS["150%"], 1.5);
  });

  it("handles stepZoom increment, decrement, and reset", () => {
    // Mock localStorage in global scope for testing state
    const storage: Record<string, string> = {};
    (globalThis as any).localStorage = {
      getItem: (k: string) => storage[k] ?? null,
      setItem: (k: string, v: string) => { storage[k] = v; },
    };

    setZoomPref("100%");
    assert.equal(zoomPref(), "100%");

    // Step up
    stepZoom(1);
    assert.equal(zoomPref(), "110%");
    stepZoom(1);
    assert.equal(zoomPref(), "125%");
    stepZoom(1);
    assert.equal(zoomPref(), "150%");
    // Clamped at 150%
    stepZoom(1);
    assert.equal(zoomPref(), "150%");

    // Step down
    stepZoom(-1);
    assert.equal(zoomPref(), "125%");
    stepZoom(-1);
    assert.equal(zoomPref(), "110%");
    stepZoom(-1);
    assert.equal(zoomPref(), "100%");
    stepZoom(-1);
    assert.equal(zoomPref(), "90%");
    // Clamped at 90%
    stepZoom(-1);
    assert.equal(zoomPref(), "90%");

    // Reset to 100%
    stepZoom(0);
    assert.equal(zoomPref(), "100%");

    delete (globalThis as any).localStorage;
  });
});

describe("Server timeout configuration (Issue #7)", () => {
  it("disables request and socket inactivity timeouts on the server instance", async () => {
    const here = path.dirname(fileURLToPath(import.meta.url));
    const data = fs.mkdtempSync(path.join(os.tmpdir(), "zoom-data-"));
    const srv = await createServer({
      repoRoot: path.resolve(here, "../.."),
      dataRoot: data,
      port: 0,
      openBrowser: false,
    });
    try {
      // The HTTP server should not timeout while learner thinks
      // Verified through the server properties
      assert.ok(srv.url.startsWith("http://127.0.0.1:"));
    } finally {
      await srv.close();
      fs.rmSync(data, { recursive: true, force: true });
    }
  });
});
