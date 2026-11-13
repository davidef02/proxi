// GET /api/events over plain HTTP: the SSE stream and the limits event both servers emit.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import type { GlobalEvent } from "../shared/protocol.ts";
import { startServer, type TestServer } from "./harness.ts";

type LimitsEvent = Extract<GlobalEvent, { type: "limits" }>;

/** A fresh repo per test: settings written by one test must not reach another. */
async function withServer(fn: (server: TestServer, repo: string) => Promise<void>): Promise<void> {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "api-events-repo-"));
  let server: TestServer | undefined;
  try {
    server = await startServer({ repoRoot: repo });
    await fn(server, repo);
  } finally {
    await server?.close();
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
  }
}

/** Same poll as integration.test.ts. */
async function waitFor<T>(fn: () => T | undefined | false, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Minimal SSE client: `ready` resolves when the headers arrive, `close` aborts the stream. */
function sse(url: string) {
  const events: GlobalEvent[] = [];
  const ac = new AbortController();
  const ready = fetch(url, { signal: ac.signal });
  void ready
    .then(async (res) => {
      const dec = new TextDecoder();
      let buf = "";
      for await (const chunk of res.body as unknown as AsyncIterable<Uint8Array>) {
        buf += dec.decode(chunk, { stream: true });
        let i: number;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, i);
          buf = buf.slice(i + 2);
          const data = block.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join("");
          if (data) events.push(JSON.parse(data));
        }
      }
    })
    .catch(() => {});
  return { events, ready, close: () => ac.abort() };
}

function saveSettings(server: TestServer, patch: unknown): Promise<Response> {
  return fetch(`${server.url}/api/settings`, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: JSON.stringify(patch),
  });
}

async function limitsAfter(server: TestServer, patch: unknown): Promise<LimitsEvent> {
  const stream = sse(`${server.url}/api/events`);
  try {
    assert.equal((await stream.ready).status, 200);
    assert.equal((await saveSettings(server, patch)).status, 200);
    return await waitFor(() => stream.events.find((e): e is LimitsEvent => e.type === "limits"));
  } finally {
    stream.close();
  }
}

describe("GET /api/events", () => {
  it("streams text/event-stream", async () => {
    await withServer(async (server) => {
      const stream = sse(`${server.url}/api/events`);
      try {
        const res = await stream.ready;
        assert.equal(res.status, 200);
        assert.ok(res.headers.get("content-type")?.startsWith("text/event-stream"));
      } finally {
        stream.close();
      }
    });
  });

  it("emits a limits event when the provider switches to opencode", async () => {
    await withServer(async (server) => {
      const event = await limitsAfter(server, { provider: "opencode" });
      assert.equal(typeof event.limits.at, "string");
      assert.ok(!Number.isNaN(Date.parse(event.limits.at)));
      assert.equal(event.limits.fiveHour, undefined);
    });
  });

  it("emits a limits event when claudeAuth switches to apiKey", async () => {
    await withServer(async (server) => {
      const event = await limitsAfter(server, { claudeAuth: "apiKey" });
      assert.equal(typeof event.limits.at, "string");
      assert.ok(!Number.isNaN(Date.parse(event.limits.at)));
      assert.equal(event.limits.fiveHour, undefined);
    });
  });
});
