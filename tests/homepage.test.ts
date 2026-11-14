// The homepage CLI and its library are tested in Rust (crates/proxi); fetchDeckStatus stays here until the server's /api/anki is ported (#8).

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { fetchDeckStatus } from "../extensions/homepage.ts";

describe("fetchDeckStatus", () => {
  it("returns counts and exists true for an existing deck", async () => {
    const http = await import("node:http");
    let getDeckStatsCalls = 0;
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { "Proxi::Vue": 42 }, error: null }));
        } else if (action === "getDeckStats") {
          getDeckStatsCalls++;
          res.end(JSON.stringify({
            result: {
              "42": { deck_id: 42, name: "Vue", new_count: 5, learn_count: 2, review_count: 8, total_in_deck: 20 },
            },
            error: null,
          }));
        } else {
          res.end(JSON.stringify({ result: null, error: "unknown action" }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status, error } = await fetchDeckStatus("Proxi::Vue", { ankiUrl: `http://127.0.0.1:${port}` });
      assert.equal(error, undefined);
      assert.deepEqual(status, { due: 8, newCards: 5, learning: 2, total: 20, exists: true });
      assert.equal(getDeckStatsCalls, 1);
    } finally {
      server.close();
    }
  });

  it("returns zeros and exists false when deck is missing, and getDeckStats is never called", async () => {
    const http = await import("node:http");
    let getDeckStatsCalls = 0;
    const server = http.createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const { action } = JSON.parse(body);
        if (action === "deckNamesAndIds") {
          res.end(JSON.stringify({ result: { "Proxi::Other": 1 }, error: null }));
        } else if (action === "getDeckStats") {
          getDeckStatsCalls++;
          res.end(JSON.stringify({ result: {}, error: null }));
        } else {
          res.end(JSON.stringify({ result: null, error: "unknown action" }));
        }
      });
    });
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const { port } = server.address() as { port: number };
    try {
      const { status, error } = await fetchDeckStatus("Proxi::Vue", { ankiUrl: `http://127.0.0.1:${port}` });
      assert.equal(error, undefined);
      assert.deepEqual(status, { due: 0, newCards: 0, learning: 0, total: 0, exists: false });
      assert.equal(getDeckStatsCalls, 0);
    } finally {
      server.close();
    }
  });

  it("returns error when port is closed", async () => {
    const { status, error } = await fetchDeckStatus("Proxi::Vue", { ankiUrl: "http://127.0.0.1:1", timeoutMs: 200 });
    assert.equal(status, undefined);
    assert.ok(typeof error === "string" && error.length > 0);
  });
});
