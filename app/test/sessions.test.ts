import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import type { ChatItem, ServerEvent, SessionState } from "../shared/protocol.ts";

let mod: typeof import("../server/main.ts") | undefined;
try {
  mod = await import("../server/main.ts");
} catch {
  /* wave-1 modules not all present yet */
}
const skip = mod ? false : "waiting for wave-1 modules";
const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");

function tempRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-ses-"));
  fs.writeFileSync(path.join(root, "curriculum.json"), JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }] }));
  fs.mkdirSync(path.join(root, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(root, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(root, "courses", "demo", "NOTES.md"), "# Notes\n");
  return root;
}

async function waitFor<T>(fn: () => T | undefined | false, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

describe("sessions with fake-claude", { skip }, () => {
  let srv: Awaited<ReturnType<NonNullable<typeof mod>["createServer"]>>;
  let root: string;
  const post = (p: string, body: unknown = {}) =>
    fetch(srv.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const state = (id: string) => srv.manager.get(id) as SessionState;
  const texts = (id: string) => state(id).items.filter((i): i is Extract<ChatItem, { kind: "teacher" }> => i.kind === "teacher");
  const start = () => mod!.createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, claudeBin: process.execPath, claudeBinArgs: [FAKE], addDirs: [] });

  let id: string;
  const events: ServerEvent[] = [];

  before(async () => {
    root = tempRepo();
    srv = await start();
  });
  after(async () => {
    await srv.close();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it("creates a teach session: learner item, teacher deltas, idle", async () => {
    const res = await post("/api/sessions", { kind: "teach", course: "demo", model: "haiku", ankiOptIn: false, target: { mode: "planned", title: "Prima lezione" } });
    assert.equal(res.status, 200);
    id = (await res.json()).id;
    srv.manager.subscribe(id, (e) => events.push(e));
    await waitFor(() => state(id).status === "idle");
    assert.equal(events[0].type, "snapshot");
    assert.equal(state(id).title, "Prima lezione");
    assert.ok(state(id).items.some((i) => i.kind === "learner"));
    assert.ok(texts(id).some((t) => t.text.includes("docente") && !t.streaming));
    assert.ok(events.some((e) => e.type === "status" && e.status === "idle"));
    assert.equal(state(id).usage.turns, 1);
    assert.ok(fs.existsSync(path.join(root, ".learner", "sessions", id, "mcp.json")));
  });

  it("quiz: card pending, answer via API, result reaches the teacher", async () => {
    assert.equal((await post(`/api/sessions/${id}/message`, { text: "FAKE:quiz" })).status, 200);
    const quiz = await waitFor(() => state(id).items.find((i) => i.kind === "quiz") as Extract<ChatItem, { kind: "quiz" }> | undefined);
    await waitFor(() => state(id).status === "waiting" && state(id).pendingRequestId === quiz.requestId);
    assert.equal(quiz.result, undefined);
    const o = quiz.input.options[0];
    const r = await post(`/api/sessions/${id}/answer`, { requestId: quiz.requestId, kind: "quiz", answer: { selected: [o.value ?? o.label], dontKnow: false, confidence: 80 } });
    assert.equal(r.status, 200);
    await waitFor(() => state(id).status === "idle");
    assert.ok(quiz.result);
    assert.equal(state(id).pendingRequestId, null);
    assert.ok(texts(id).some((t) => /correct/.test(t.text)), "teacher text echoes the tool result");
  });

  it("409 while running, interrupt returns to idle", async () => {
    assert.equal((await post(`/api/sessions/${id}/message`, { text: "FAKE:slow" })).status, 200);
    const busy = await post(`/api/sessions/${id}/message`, { text: "ancora" });
    assert.equal(busy.status, 409);
    assert.ok((await busy.json()).error);
    assert.equal((await post(`/api/sessions/${id}/interrupt`)).status, 200);
    await waitFor(() => state(id).status === "idle");
    assert.ok(!state(id).items.some((i) => i.kind === "notice" && i.level === "error"), "interrupt is not an error notice");
  });

  it("DELETE stops a running session and removes it (needs the client header)", async () => {
    const id3 = (await (await post("/api/sessions", { kind: "practice", course: "demo", model: "haiku" })).json()).id;
    await waitFor(() => state(id3).status === "idle");
    assert.equal((await post(`/api/sessions/${id3}/message`, { text: "FAKE:slow" })).status, 200);
    assert.equal((await fetch(srv.url + `/api/sessions/${id3}`, { method: "DELETE" })).status, 403);
    const del = await fetch(srv.url + `/api/sessions/${id3}`, { method: "DELETE", headers: { "x-learner-client": "1" } });
    assert.equal(del.status, 200);
    assert.equal(srv.manager.get(id3), undefined);
    assert.ok(!fs.existsSync(path.join(root, ".learner", "sessions", id3)));
    assert.equal((await fetch(srv.url + `/api/sessions/${id3}`)).status, 404);
  });

  it("rejects an unplanned title (a missing opt-in is asked in the session: workflow.test.ts)", async () => {
    const a = await post("/api/sessions", { kind: "teach", course: "demo", model: "haiku", ankiOptIn: true, target: { mode: "planned", title: "Prima Lezione" } });
    assert.equal(a.status, 400);
  });

  it("handles turn failure with retry mechanism without duplicating messages in transcript (Issue #25)", async () => {
    // Send a message that fails on first attempt in fake-claude
    const learnerText = "FAKE:error_once spiegami il principio di induzione";
    assert.equal((await post(`/api/sessions/${id}/message`, { text: learnerText })).status, 200);

    await waitFor(() => state(id).status === "error");
    assert.equal(state(id).retryable, true);
    assert.ok(state(id).lastError);
    assert.ok(state(id).items.some((i) => i.kind === "notice" && i.level === "error"));

    const learnerItemsBefore = state(id).items.filter((i) => i.kind === "learner");
    const lastLearnerItem = learnerItemsBefore[learnerItemsBefore.length - 1];
    assert.equal(lastLearnerItem.text, learnerText);

    // Calling retry without client header fails with 403
    assert.equal((await fetch(srv.url + `/api/sessions/${id}/retry`, { method: "POST" })).status, 403);

    // Retrying re-submits the turn without adding a new learner bubble
    const retryRes = await post(`/api/sessions/${id}/retry`);
    assert.equal(retryRes.status, 200);

    await waitFor(() => state(id).status === "idle");
    assert.equal(state(id).retryable, false);
    assert.ok(!state(id).items.some((i) => i.kind === "notice" && i.level === "error"), "error notice is removed on retry");

    const learnerItemsAfter = state(id).items.filter((i) => i.kind === "learner");
    assert.equal(learnerItemsAfter.length, learnerItemsBefore.length, "learner bubble is NOT duplicated");

    // Calling retry when not failed returns 400
    const invalidRetry = await post(`/api/sessions/${id}/retry`);
    assert.equal(invalidRetry.status, 400);
    assert.match((await invalidRetry.json()).error, /Nessun turno fallito da riprovare/);
  });

  it("lists newest first, closes, reloads, resumes", async () => {
    const id2 = (await (await post("/api/sessions", { kind: "practice", course: "demo", model: "haiku" })).json()).id;
    await waitFor(() => state(id2).status === "idle");
    const list = (await (await fetch(srv.url + "/api/sessions")).json()) as { id: string }[];
    assert.deepEqual(list.map((s) => s.id).slice(0, 2), [id2, id]);
    assert.equal((await post(`/api/sessions/${id2}/close`)).status, 200);
    assert.equal(state(id2).status, "closed");
    assert.equal((await post(`/api/sessions/${id2}/message`, { text: "x" })).status, 409);
    await srv.close();
    srv = await start();
    assert.equal(state(id).status, "idle");
    assert.equal(state(id2).status, "closed");
    const before = state(id).usage.turns;
    assert.equal((await post(`/api/sessions/${id}/message`, { text: "ciao di nuovo" })).status, 200);
    await waitFor(() => state(id).status === "idle" && state(id).usage.turns > before);
  });

  it("starts a free teach session for a standalone lesson, provisioning courses/lezioni-singole/", async () => {
    fs.rmSync(path.join(root, "courses", "lezioni-singole"), { recursive: true, force: true });
    const res = await post("/api/sessions", { kind: "teach", course: "lezioni-singole", model: "haiku", ankiOptIn: false, target: { mode: "free" }, note: "Trasformata di Fourier" });
    assert.equal(res.status, 200);
    const sid = (await res.json()).id;
    await waitFor(() => state(sid).status === "idle");
    assert.equal(fs.existsSync(path.join(root, "courses", "lezioni-singole", "MISSION.md")), true);
    assert.equal(state(sid).config.course, "lezioni-singole");
    assert.equal(state(sid).config.target?.mode, "free");
    assert.equal(state(sid).title, "Lezione singola · Trasformata di Fourier");
    const opening = state(sid).items.find((i) => i.kind === "learner") as Extract<ChatItem, { kind: "learner" }>;
    assert.match(opening.text, /Trasformata di Fourier/);
    const courses = (await (await fetch(srv.url + "/api/courses")).json()) as { slug: string }[];
    assert.ok(!courses.some((c) => c.slug === "lezioni-singole"), "lezioni-singole stays out of GET /api/courses");
  });
});
