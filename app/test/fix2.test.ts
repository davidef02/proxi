import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { callRequests, SessionManager } from "../server/sessions.ts";
import { unsafeHtml } from "../server/tools/node.ts";
import type { Provider, SessionState } from "../shared/protocol.ts";
const BINS = { claude: { cmd: "claude" }, agy: { cmd: "agy" }, opencode: { cmd: "opencode" } };
import { memoryEngine, memoryJobs, resultEvent } from "./helpers.ts";

describe("unsafeHtml", () => {
  it("lets prose and code samples through", () => {
    for (const ok of [
      "<p>se x < y e one = 1</p>",
      "<p>javascript: è un URL</p>",
      "<code>javascript:alert(1)</code>",
      "<pre>&lt;a onclick=x&gt; <b onclick=1> <script></pre>".replace(" <script>", ""),
      "<pre><code>if (a <b && c) x.onload = 1</code></pre>",
    ]) assert.equal(unsafeHtml(ok), false, ok);
  });
  it("rejects active tags", () => {
    for (const bad of ['<a href="javascript:alert(1)">x</a>', "<img src=x onerror=alert(1)>", "<script>x</script>", "<iframe src=x>", '<img alt=">" onerror=x>'])
      assert.equal(unsafeHtml(bad), true, bad);
  });
});

function mgr(provider: Provider = "claude") {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-fix2-"));
  const engine = memoryEngine();
  const m = new SessionManager({ repoRoot: root, port: 1, token: "t", bins: BINS, jobs: memoryJobs("s1") }) as any;
  m.spawn = (_s: unknown, resume: string | undefined, cb: any) => engine.spawn(resume, cb); // no process
  const state: SessionState = {
    id: "s1", config: { kind: "practice", course: "demo", model: "haiku" } as any, title: "t", createdAt: "", updatedAt: "", status: "idle",
    usage: { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheCreationTokens: 0, turns: 0 },
    items: [], pendingRequestId: null, plan: null, exercises: [],
  };
  const s = m.makeSession(state, provider, "cid");
  m.sessions.set("s1", s);
  return { m, s, root, engine };
}

describe("session fixes", () => {
  it("an aborted /mcp call cancels only its own requests; streamed maps are cleared on result", async () => {
    const { m, s, engine } = mgr();
    await m.message("s1", "ciao");
    assert.equal(engine.last().resume, "cid", "the persisted token is resumed");
    const mine = new Set<string>();
    const a = callRequests.run(mine, () => s.ctx.request({ kind: "ask", question: "a", options: [] } as any));
    const b = s.ctx.request({ kind: "ask", question: "b", options: [] } as any);
    a.catch(() => {});
    assert.equal(mine.size, 1);
    m.cancelRequests("s1", mine);
    await assert.rejects(a, /cancelled/);
    assert.equal(s.pending.size, 1);
    let settled = false;
    b.then(() => (settled = true), () => (settled = true));
    await new Promise((r) => setImmediate(r));
    assert.equal(settled, false);

    s.streamed.set("m", ["x"]);
    s.finalised.set("m", 1);
    engine.last().cb.onEvent(resultEvent());
    assert.equal(s.streamed.size + s.finalised.size, 0);
  });

  it("a closing session never launches; the engine is pinned and persisted with the resume token", async () => {
    const { m, s, root, engine } = mgr("agy");
    m.flush(s);
    const saved = JSON.parse(fs.readFileSync(path.join(root, ".learner", "sessions", "s1", "state.json"), "utf-8"));
    assert.deepEqual([saved.provider, saved.resume], ["agy", "cid"]);
    await m.close("s1");
    await assert.rejects(s.turns.send("x", []), /closed/);
    assert.equal(engine.procs.length, 0);
    s.closing = true;
    await assert.rejects((SessionManager.prototype as any).spawn.call(m, s, undefined, {}), /closed/);
  });

  it("a session file from before pinning loads as claude, resuming claudeSessionId only if it ran", () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-fix2-"));
    const state = { id: "old", config: { kind: "practice", course: "demo" }, title: "t", createdAt: "", updatedAt: "", status: "idle", usage: {}, items: [], pendingRequestId: null, plan: null, exercises: [] };
    for (const [id, hasRun] of [["ran", true], ["never", false]] as const) {
      fs.mkdirSync(path.join(root, ".learner", "sessions", id), { recursive: true });
      fs.writeFileSync(path.join(root, ".learner", "sessions", id, "state.json"), JSON.stringify({ state: { ...state, id }, claudeSessionId: "uuid-" + id, hasRun }));
    }
    const m = new SessionManager({ repoRoot: root, port: 1, token: "t", bins: BINS, jobs: memoryJobs() }) as any;
    assert.deepEqual([m.sessions.get("ran").provider, m.sessions.get("ran").turns.resume], ["claude", "uuid-ran"]);
    assert.deepEqual([m.sessions.get("never").provider, m.sessions.get("never").turns.resume], ["claude", undefined]);
  });
});
