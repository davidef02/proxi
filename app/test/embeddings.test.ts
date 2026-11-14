import { describe, test } from "node:test";
import assert from "node:assert/strict";
import { withLoadLock } from "../server/graph/embeddings.ts";

describe("embeddings", () => {
  test("withLoadLock serializes loads, and a rejection does not block the next one", async () => {
    const order: string[] = [];
    const first = withLoadLock(async () => {
      order.push("s1");
      await new Promise((r) => setTimeout(r, 20));
      order.push("e1");
    });
    const second = withLoadLock(async () => {
      order.push("s2");
      await new Promise((r) => setTimeout(r, 1));
      order.push("e2");
    });
    await Promise.all([first, second]);
    assert.deepEqual(order, ["s1", "e1", "s2", "e2"]);

    await assert.rejects(withLoadLock(async () => {
      throw new Error("boom");
    }));
    assert.equal(
      await withLoadLock(async () => "ok"),
      "ok",
    );
  });
});
