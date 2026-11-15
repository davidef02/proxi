// Needs /answer and the MCP tools (#21): node mode until then, not in RUST_PORTED.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import type { ChatItem, SessionState } from "../shared/protocol.ts";
import { fakeClaude, startServer, type TestServer } from "./harness.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // closed port: never reach the real Anki

function tempRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-answer-"));
  fs.writeFileSync(path.join(root, "curriculum.json"), JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }] }));
  fs.mkdirSync(path.join(root, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(root, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(root, "courses", "demo", "NOTES.md"), "# Notes\n");
  return root;
}

async function waitFor<T>(fn: () => T | undefined | false | Promise<T | undefined | false>, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

describe("answering a quiz card", () => {
  let srv: TestServer;
  let root: string;
  const post = (p: string, body: unknown = {}) =>
    fetch(srv.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const state = async (id: string): Promise<SessionState> => {
    const r = await fetch(`${srv.url}/api/sessions/${id}`);
    assert.equal(r.status, 200);
    return (await r.json()) as SessionState;
  };
  const texts = async (id: string) =>
    (await state(id)).items.filter((i): i is Extract<ChatItem, { kind: "teacher" }> => i.kind === "teacher");

  before(async () => {
    root = tempRepo();
    srv = await startServer({ repoRoot: root, dataRoot: root, claude: fakeClaude() });
  });
  after(async () => {
    await srv.close();
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
  });

  it("quiz: card pending, answer via API, result reaches the teacher", async () => {
    const created = await post("/api/sessions", { kind: "teach", course: "demo", model: "haiku", ankiOptIn: false, target: { mode: "planned", title: "Prima lezione" } });
    assert.equal(created.status, 200);
    const id = ((await created.json()) as { id: string }).id;
    await waitFor(async () => (await state(id)).status === "idle");

    assert.equal((await post(`/api/sessions/${id}/message`, { text: "FAKE:quiz" })).status, 200);
    const quiz = await waitFor(async () => {
      const s = await state(id);
      return s.items.find((i): i is Extract<ChatItem, { kind: "quiz" }> => i.kind === "quiz" && !i.result);
    });
    await waitFor(async () => {
      const s = await state(id);
      return s.status === "waiting" && s.pendingRequestId === quiz.requestId;
    });
    assert.equal(quiz.result, undefined);

    const o = quiz.input.options[0];
    const r = await post(`/api/sessions/${id}/answer`, { requestId: quiz.requestId, kind: "quiz", answer: { selected: [o.value ?? o.label], dontKnow: false, confidence: 80 } });
    assert.equal(r.status, 200);
    await waitFor(async () => (await state(id)).status === "idle");

    const answered = await state(id);
    const card = answered.items.find((i): i is Extract<ChatItem, { kind: "quiz" }> => i.kind === "quiz");
    assert.ok(card?.result);
    assert.equal(answered.pendingRequestId, null);
    assert.ok((await texts(id)).some((t) => /correct/.test(t.text)), "teacher text echoes the tool result");
  });
});
