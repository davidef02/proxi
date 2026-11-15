// The session lifecycle over HTTP and SSE only (no server internals), so it runs against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import type { ChatItem, ServerEvent, SessionState } from "../shared/protocol.ts";
import { fakeClaude, startServer, type TestServer } from "./harness.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // closed port: never reach the real Anki

function tempRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-ses-"));
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

/** Minimal SSE client: parses `data:` lines from a fetch stream into ServerEvents. */
function sse(url: string) {
  const events: ServerEvent[] = [];
  const ac = new AbortController();
  void (async () => {
    const res = await fetch(url, { signal: ac.signal });
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
  })().catch(() => {});
  return { events, close: () => ac.abort() };
}

describe("sessions with fake-claude", () => {
  let srv: TestServer;
  let root: string;
  let inputFile: string;
  const post = (p: string, body: unknown = {}) =>
    fetch(srv.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const state = async (id: string): Promise<SessionState> => {
    const r = await fetch(`${srv.url}/api/sessions/${id}`);
    assert.equal(r.status, 200);
    return (await r.json()) as SessionState;
  };
  const idle = (id: string) => waitFor(async () => (await state(id)).status === "idle");
  const texts = async (id: string) =>
    (await state(id)).items.filter((i): i is Extract<ChatItem, { kind: "teacher" }> => i.kind === "teacher");
  /** Every raw stdin line the fake engine saw. */
  const rawInputs = (): string[] => {
    try {
      return fs.readFileSync(inputFile, "utf-8").split("\n").filter(Boolean);
    } catch {
      return [];
    }
  };
  const start = () => startServer({ repoRoot: root, dataRoot: root, claude: fakeClaude(["--input-out", inputFile]) });

  let id: string;

  before(async () => {
    root = tempRepo();
    inputFile = path.join(root, "input.jsonl");
    srv = await start();
  });
  after(async () => {
    await srv.close();
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
  });

  it("creates a teach session: learner item, teacher deltas, idle", async () => {
    const res = await post("/api/sessions", { kind: "teach", course: "demo", model: "haiku", ankiOptIn: false, target: { mode: "planned", title: "Prima lezione" } });
    assert.equal(res.status, 200);
    id = ((await res.json()) as { id: string }).id;
    const stream = sse(`${srv.url}/api/sessions/${id}/events`);
    try {
      await idle(id);
      const snapshot = await waitFor(() => stream.events[0]);
      assert.equal(snapshot.type, "snapshot");
      const s = await state(id);
      assert.equal(s.title, "Prima lezione");
      assert.ok(s.items.some((i) => i.kind === "learner"));
      assert.ok((await texts(id)).some((t) => t.text.includes("docente") && !t.streaming));
      await waitFor(() => stream.events.some((e) => e.type === "status" && (e as { status: string }).status === "idle"));
      assert.equal(s.usage.turns, 1);
      assert.ok(fs.existsSync(path.join(root, ".learner", "sessions", id, "mcp.json")));
    } finally {
      stream.close();
    }
  });

  it("409 while running, interrupt returns to idle", async () => {
    const stream = sse(`${srv.url}/api/sessions/${id}/events`);
    try {
      assert.equal((await post(`/api/sessions/${id}/message`, { text: "FAKE:slow" })).status, 200);
      const busy = await post(`/api/sessions/${id}/message`, { text: "ancora" });
      assert.equal(busy.status, 409);
      assert.ok(((await busy.json()) as { error: string }).error);
      // The fake drops an interrupt that lands in the same stdin read as the user line: wait for its
      // first streamed delta, so the request finds the slow script already running.
      await waitFor(() => stream.events.some((e) => e.type === "delta"));
      assert.equal((await post(`/api/sessions/${id}/interrupt`)).status, 200);
      await idle(id);

      const notices = (await state(id)).items.filter((i): i is Extract<ChatItem, { kind: "notice" }> => i.kind === "notice");
      const infos = notices.filter((n) => n.level === "info");
      assert.equal(infos.length, 1, "one info notice for the interrupted turn");
      assert.equal(infos[0].text, "Turno interrotto.");
      assert.ok(!notices.some((n) => n.level === "error"), "interrupt is not an error notice");
      assert.ok(rawInputs().includes('{"type":"control_request","request_id":"int-1","request":{"subtype":"interrupt"}}'));
    } finally {
      stream.close();
    }
  });

  it("DELETE stops a running session and removes it (needs the client header)", async () => {
    const id3 = ((await (await post("/api/sessions", { kind: "practice", course: "demo", model: "haiku" })).json()) as { id: string }).id;
    await idle(id3);
    assert.equal((await post(`/api/sessions/${id3}/message`, { text: "FAKE:slow" })).status, 200);
    assert.equal((await fetch(srv.url + `/api/sessions/${id3}`, { method: "DELETE" })).status, 403);
    const del = await fetch(srv.url + `/api/sessions/${id3}`, { method: "DELETE", headers: { "x-learner-client": "1" } });
    assert.equal(del.status, 200);
    assert.deepEqual(await del.json(), { ok: true });
    assert.ok(!fs.existsSync(path.join(root, ".learner", "sessions", id3)));
    assert.equal((await fetch(srv.url + `/api/sessions/${id3}`)).status, 404);
    const list = (await (await fetch(srv.url + "/api/sessions")).json()) as { id: string }[];
    assert.ok(!list.some((s) => s.id === id3), "the deleted session is gone from the list");
  });

  it("rejects an unplanned title (a missing opt-in is asked in the session: workflow.test.ts)", async () => {
    const a = await post("/api/sessions", { kind: "teach", course: "demo", model: "haiku", ankiOptIn: true, target: { mode: "planned", title: "Prima Lezione" } });
    assert.equal(a.status, 400);
  });

  it("handles turn failure with retry mechanism without duplicating messages in transcript (Issue #25)", async () => {
    // Send a message that fails on first attempt in fake-claude
    const learnerText = "FAKE:error_once spiegami il principio di induzione";
    assert.equal((await post(`/api/sessions/${id}/message`, { text: learnerText })).status, 200);

    const failed = await waitFor(async () => {
      const s = await state(id);
      return s.status === "error" ? s : undefined;
    });
    assert.equal(failed.retryable, true);
    assert.ok(failed.lastError);
    assert.ok(failed.items.some((i) => i.kind === "notice" && i.level === "error"));

    const learnerItemsBefore = failed.items.filter((i): i is Extract<ChatItem, { kind: "learner" }> => i.kind === "learner");
    const lastLearnerItem = learnerItemsBefore[learnerItemsBefore.length - 1];
    assert.equal(lastLearnerItem.text, learnerText);

    // Calling retry without client header fails with 403
    assert.equal((await fetch(srv.url + `/api/sessions/${id}/retry`, { method: "POST" })).status, 403);

    // Retrying re-submits the turn without adding a new learner bubble: snapshot first, then the rerun's statuses.
    const stream = sse(`${srv.url}/api/sessions/${id}/events`);
    try {
      await waitFor(() => stream.events[0]?.type === "snapshot");
      const before = stream.events.length;
      assert.equal((await post(`/api/sessions/${id}/retry`)).status, 200);
      await waitFor(() => stream.events.slice(before).some((e) => e.type === "status" && (e as { status: string }).status === "idle"));

      const live = stream.events.slice(before) as { type: string; status?: string; item?: { kind: string } }[];
      assert.equal(live[0].type, "snapshot");
      const runningAt = live.findIndex((e) => e.type === "status" && e.status === "running");
      const idleAt = live.findIndex((e) => e.type === "status" && e.status === "idle");
      assert.ok(runningAt >= 0 && idleAt > runningAt, "running comes before idle");
      assert.ok(!live.some((e) => e.type === "item" && e.item?.kind === "learner"), "retry adds no learner bubble");
    } finally {
      stream.close();
    }

    const retried = await state(id);
    assert.equal(retried.retryable, false);
    assert.ok(!retried.items.some((i) => i.kind === "notice" && i.level === "error"), "error notice is removed on retry");
    const learnerItemsAfter = retried.items.filter((i) => i.kind === "learner");
    assert.equal(learnerItemsAfter.length, learnerItemsBefore.length, "learner bubble is NOT duplicated");

    // Calling retry when not failed returns 400
    const invalidRetry = await post(`/api/sessions/${id}/retry`);
    assert.equal(invalidRetry.status, 400);
    assert.match(((await invalidRetry.json()) as { error: string }).error, /Nessun turno fallito da riprovare/);
  });

  it("lists newest first, closes, reloads, resumes", async () => {
    const id2 = ((await (await post("/api/sessions", { kind: "practice", course: "demo", model: "haiku" })).json()) as { id: string }).id;
    await idle(id2);
    const list = (await (await fetch(srv.url + "/api/sessions")).json()) as { id: string }[];
    assert.deepEqual(list.map((s) => s.id).slice(0, 2), [id2, id]);
    assert.equal((await post(`/api/sessions/${id2}/close`)).status, 200);
    assert.equal((await state(id2)).status, "closed");
    assert.equal((await post(`/api/sessions/${id2}/message`, { text: "x" })).status, 409);
    // state.json is written 300 ms after the last event: closing the binary before that leaves the
    // last saved turn as `running` with a lastTurn, which the restart would rightly call a cut turn.
    await waitFor(() => {
      try {
        const saved = JSON.parse(fs.readFileSync(path.join(root, ".learner", "sessions", id, "state.json"), "utf-8"));
        return saved.state?.status === "idle" && !saved.lastTurn ? saved : undefined;
      } catch {
        return undefined; // not written yet
      }
    });
    await srv.close();
    srv = await start();
    assert.equal((await state(id)).status, "idle");
    assert.equal((await state(id2)).status, "closed");
    const before = (await state(id)).usage.turns;
    assert.equal((await post(`/api/sessions/${id}/message`, { text: "ciao di nuovo" })).status, 200);
    await waitFor(async () => {
      const s = await state(id);
      return s.status === "idle" && s.usage.turns > before;
    });
  });

  it("starts a free teach session for a standalone lesson, provisioning courses/lezioni-singole/", async () => {
    fs.rmSync(path.join(root, "courses", "lezioni-singole"), { recursive: true, force: true });
    const res = await post("/api/sessions", { kind: "teach", course: "lezioni-singole", model: "haiku", ankiOptIn: false, target: { mode: "free" }, note: "Trasformata di Fourier" });
    assert.equal(res.status, 200);
    const sid = ((await res.json()) as { id: string }).id;
    await idle(sid);
    assert.equal(fs.existsSync(path.join(root, "courses", "lezioni-singole", "MISSION.md")), true);
    const s = await state(sid);
    assert.equal(s.config.course, "lezioni-singole");
    assert.equal(s.config.target?.mode, "free");
    assert.equal(s.title, "Lezione singola · Trasformata di Fourier");
    const opening = s.items.find((i): i is Extract<ChatItem, { kind: "learner" }> => i.kind === "learner");
    assert.ok(opening);
    assert.match(opening.text, /Trasformata di Fourier/);
    const courses = (await (await fetch(srv.url + "/api/courses")).json()) as { slug: string }[];
    assert.ok(!courses.some((c) => c.slug === "lezioni-singole"), "lezioni-singole stays out of GET /api/courses");
  });

  it("an interrupted turn that never reports a result is given up by the watchdog", async () => {
    const created = await post("/api/sessions", { kind: "practice", course: "demo", model: "haiku" });
    assert.equal(created.status, 200);
    const wid = ((await created.json()) as { id: string }).id;
    await idle(wid);
    // The engine ignores the interrupt and never sends a result: only the 10 s watchdog can end the turn.
    assert.equal((await post(`/api/sessions/${wid}/message`, { text: "FAKE:stubborn" })).status, 200);
    await waitFor(async () => (await state(wid)).items.some((i) => i.kind === "teacher"));
    const started = Date.now();
    assert.equal((await post(`/api/sessions/${wid}/interrupt`)).status, 200);
    await waitFor(async () => (await state(wid)).status === "idle", 20000);
    assert.ok(Date.now() - started >= 9000, "the idle came from the watchdog, not the engine");
    const notices = (await state(wid)).items.filter((i): i is Extract<ChatItem, { kind: "notice" }> => i.kind === "notice");
    assert.equal(notices.filter((n) => n.level === "info" && n.text === "Turno interrotto.").length, 1);
    assert.ok(!notices.some((n) => n.level === "error"), "the watchdog is not an error notice");
  });

  it("a turn cut by a restart comes back as a retryable error", async () => {
    const created = await post("/api/sessions", { kind: "practice", course: "demo", model: "haiku" });
    assert.equal(created.status, 200);
    const rid = ((await created.json()) as { id: string }).id;
    await idle(rid);

    assert.equal((await post(`/api/sessions/${rid}/message`, { text: "FAKE:slow:15" })).status, 200);
    await waitFor(async () => (await state(rid)).status === "running");

    // The cut needs the turn on disk before the process dies: status running and the last turn saved.
    const file = path.join(root, ".learner", "sessions", rid, "state.json");
    await waitFor(() => {
      try {
        const saved = JSON.parse(fs.readFileSync(file, "utf-8"));
        return saved.state?.status === "running" && saved.lastTurn ? saved : undefined;
      } catch {
        return undefined; // not written yet
      }
    });
    await srv.close();
    srv = await start();

    const cut = await state(rid);
    assert.equal(cut.status, "error");
    assert.equal(cut.retryable, true);
    assert.equal(cut.lastError, "Sessione interrotta prima del completamento del turno");
    const list = (await (await fetch(srv.url + "/api/sessions")).json()) as { id: string }[];
    assert.ok(list.some((s) => s.id === rid), "the restarted server lists the saved session");

    const learners = cut.items.filter((i) => i.kind === "learner").length;
    assert.equal((await post(`/api/sessions/${rid}/retry`)).status, 200);
    await waitFor(async () => (await state(rid)).status === "idle", 20000);
    assert.equal((await state(rid)).items.filter((i) => i.kind === "learner").length, learners, "retry adds no learner bubble");
  });
});
