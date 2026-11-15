// Node-only: needs /answer, /interrupt and /mcp/{id}: moves to test:rust with #18/#20.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import type { ChatItem, SessionState } from "../shared/protocol.ts";
import { createServer, type RunningServer } from "../server/main.ts";
import { makeTempRepo } from "./helpers.ts";

const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-agy.mjs");
const FAKE_CLAUDE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs"); // a pinning bug must never reach the real claude
process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9";
const b64 = (o: unknown) => Buffer.from(JSON.stringify(o)).toString("base64");

describe("agy session control (fake-agy)", () => {
  let srv: RunningServer;
  let root: string;
  const api = (p: string, body?: unknown) =>
    fetch(srv.url + p, body === undefined ? undefined : { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const state = (id: string) => srv.manager.get(id) as SessionState;
  const texts = (id: string) => state(id).items.filter((i): i is Extract<ChatItem, { kind: "teacher" }> => i.kind === "teacher").map((t) => t.text);
  const waitFor = async <T,>(fn: () => T | undefined | false, ms = 15000): Promise<T> => {
    const end = Date.now() + ms;
    for (;;) {
      const v = fn();
      if (v) return v;
      if (Date.now() > end) throw new Error("waitFor timed out");
      await new Promise((r) => setTimeout(r, 25));
    }
  };
  const say = async (id: string, text: string) => assert.equal((await api(`/api/sessions/${id}/message`, { text })).status, 200);
  const open = async () => {
    const r = await api("/api/sessions", { kind: "teach", course: "demo", target: { mode: "planned", title: "Il primo passo" }, ankiOptIn: false });
    assert.equal(r.status, 200);
    const id = ((await r.json()) as { id: string }).id;
    await waitFor(() => state(id).status === "idle");
    return id;
  };

  before(async () => {
    root = await makeTempRepo();
    fs.mkdirSync(path.join(root, ".learner"), { recursive: true });
    fs.writeFileSync(path.join(root, ".learner", "settings.json"), JSON.stringify({ provider: "agy" }));
    srv = await createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, addDirs: [], agyBin: process.execPath, agyBinArgs: [FAKE], claudeBin: process.execPath, claudeBinArgs: [FAKE_CLAUDE] });
  });
  after(async () => {
    await srv.close();
    await new Promise((r) => setTimeout(r, 500));
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
  });

  it("an ask tool becomes a card whose /answer is shown to the engine", async () => {
    const id = await open();
    await say(id, "FAKE:tool:ask:" + b64({ question: "Pronto?" }));
    const ask = await waitFor(() => state(id).items.find((i) => i.kind === "ask" && !i.answer) as any);
    assert.equal((await api(`/api/sessions/${id}/answer`, { requestId: ask.requestId, kind: "ask", answer: { selected: [], other: "si" } })).status, 200);
    await waitFor(() => state(id).status === "idle" && texts(id).some((t) => t.includes("alwaysLoad=true")));
    assert.ok(texts(id).some((t) => t.includes('"other":"si"')));
  });

  it("FAKE:slow ends with an interrupt notice", async () => {
    const id = await open();
    await say(id, "FAKE:slow");
    await waitFor(() => state(id).status === "running");
    assert.equal((await api(`/api/sessions/${id}/interrupt`, {})).status, 200);
    await waitFor(() => state(id).status === "idle");
    assert.ok(state(id).items.some((i) => i.kind === "notice" && /interrotto/.test((i as any).text)));
  });
});
