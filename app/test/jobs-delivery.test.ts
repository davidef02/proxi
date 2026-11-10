// Job results reach the owning session: card upsert, delivery to an idle teacher, deferral while busy/waiting, relaunch.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import { z } from "zod";
import type { ChatItem, SessionState } from "../shared/protocol.ts";
import { createServer, type RunningServer } from "../server/main.ts";
import { jobTurnText } from "../server/jobs/deliver.ts";
import { makeTempRepo } from "./helpers.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9";
const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");
const script = (steps: unknown[]) => "FAKE:job:" + Buffer.from(JSON.stringify(steps)).toString("base64");
const BRIEF = "Vue 3.5 rende reattivo un ref leggendo .value dentro un effect: la dipendenza viene registrata alla lettura.";
const SUBMIT = { tool: "submit_research", input: { brief: BRIEF, sources: [{ title: "Vue docs", url: "https://vuejs.org/" }] } };

async function waitFor<T>(fn: () => T | undefined | false, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

describe("job delivery", () => {
  let srv: RunningServer;
  let root: string;
  const opts = () => ({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, claudeBin: process.execPath, claudeBinArgs: [FAKE], addDirs: [] });
  const api = (p: string, body: unknown = {}) =>
    fetch(srv.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const state = (id: string) => srv.manager.get(id) as SessionState;
  const cards = (id: string) => state(id).items.filter((i): i is Extract<ChatItem, { kind: "job" }> => i.kind === "job");
  const create = async () => {
    const r = await api("/api/sessions", { kind: "teach", course: "demo", model: "haiku", ankiOptIn: false, target: { mode: "free" } });
    assert.equal(r.status, 200);
    const id = ((await r.json()) as { id: string }).id;
    await waitFor(() => state(id).status === "idle");
    return id;
  };
  const jobOf = (id: string, steps: unknown[], kind: "research" | "visual" = "research") =>
    srv.manager.getContext(id)!.jobs.start(kind, kind === "research" ? { question: "q " + script(steps) } : { brief: "b " + script(steps) }, { title: "Ricerca: q" });

  before(async () => {
    root = await makeTempRepo();
    srv = await createServer(opts());
  });
  after(async () => {
    await srv.close();
    await new Promise((r) => setTimeout(r, 500));
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  });

  it("jobTurnText formats research / visual / demo / error", () => {
    const base = { id: "j1", title: "t", model: "haiku", activity: "", createdAt: "", updatedAt: "", costUsd: 0 } as const;
    const research = jobTurnText({ ...base, kind: "research", status: "done", result: { kind: "research", brief: "## Sintesi\nx", sources: [{ title: "A", url: "https://a" }] } });
    assert.match(research, /^\[Risultato del subagente research · job j1\]\n\n## Sintesi/);
    assert.match(research, /- A: https:\/\/a/);
    assert.match(jobTurnText({ ...base, kind: "visual", status: "done", result: { kind: "visual", svg: "<svg/>", caption: "Cap" } }), /già visibile allo studente nella chat\.\nDidascalia: Cap/);
    assert.match(jobTurnText({ ...base, kind: "demo", status: "done", result: { kind: "demo", title: "T", html: "x", notes: "N" } }), /La demo "T" è già visibile.*\nNote: N/);
    assert.match(jobTurnText({ ...base, kind: "research", status: "error", error: "boom" }), /non ha consegnato: boom/);
  });

  it("idle session: card upserted, result delivered as a turn, markDelivered", async () => {
    const id = await create();
    const turns = state(id).usage.turns;
    const j = jobOf(id, [SUBMIT]);
    assert.equal(cards(id).find((c) => c.id === "job:" + j.id)?.job.status, "running");
    await waitFor(() => srv.jobs.get(j.id)!.delivered);
    assert.equal(cards(id).filter((c) => c.id === "job:" + j.id).length, 1, "one card, upserted");
    assert.equal(cards(id)[0].job.status, "done");
    await waitFor(() => state(id).usage.turns === turns + 1 && state(id).status === "idle");
  });

  it("busy session: delivery waits for the turn to end", async () => {
    const id = await create();
    assert.equal((await api(`/api/sessions/${id}/message`, { text: "FAKE:slow" })).status, 200);
    await waitFor(() => state(id).status === "running");
    const j = jobOf(id, [SUBMIT]);
    await waitFor(() => srv.jobs.get(j.id)!.status === "done");
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(srv.jobs.get(j.id)!.delivered, undefined, "not delivered mid-turn");
    assert.equal(cards(id)[0].job.status, "done", "the card is still updated");
    await api(`/api/sessions/${id}/interrupt`);
    await waitFor(() => srv.jobs.get(j.id)!.delivered);
    await waitFor(() => state(id).status === "idle");
  });

  it("pending card: delivery waits until the learner answers and the turn ends", async () => {
    const id = await create();
    assert.equal((await api(`/api/sessions/${id}/message`, { text: "FAKE:quiz" })).status, 200);
    await waitFor(() => state(id).status === "waiting");
    const j = jobOf(id, [SUBMIT]);
    await waitFor(() => srv.jobs.get(j.id)!.status === "done");
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(srv.jobs.get(j.id)!.delivered, undefined, "never while a card is pending");
    assert.equal(state(id).status, "waiting");
    const quiz = state(id).items.find((i) => i.kind === "quiz") as any;
    await api(`/api/sessions/${id}/answer`, { requestId: quiz.requestId, kind: "quiz", answer: { selected: ["4"], dontKnow: false } });
    await waitFor(() => srv.jobs.get(j.id)!.delivered);
  });

  it("failed job is delivered too; map jobs get a card but are never delivered", async () => {
    const id = await create();
    const bad = jobOf(id, []); // no submit: error
    await waitFor(() => srv.jobs.get(bad.id)!.delivered);
    assert.equal(srv.jobs.get(bad.id)!.status, "error");
    srv.jobs.register({
      kind: "map", grant: [], maxTurns: 3, systemPrompt: async () => "x",
      firstMessage: (i: any) => i.hint,
      tools: () => [{ name: "done", description: "d", schema: z.object({}), run: async (_i: unknown, ctx: any) => (ctx.finish({ kind: "map", source: "demo/x", added: 1, linked: 0, chains: 0 }), { text: "ok" }) }],
    });
    await waitFor(() => state(id).status === "idle"); // the failed job's turn has ended
    const turns = state(id).usage.turns;
    const m = srv.manager.getContext(id)!.jobs.start("map", { hint: script([{ tool: "done" }]) }, { title: "Mappa: x" });
    await waitFor(() => srv.jobs.get(m.id)!.status === "done");
    await waitFor(() => cards(id).find((c) => c.id === "job:" + m.id)?.job.status === "done");
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(srv.jobs.get(m.id)!.delivered, undefined);
    assert.equal(state(id).usage.turns, turns, "no turn was sent for the map job");
  });

  it("session without a claude process (after a restart): the process is relaunched to deliver", async () => {
    const id = await create();
    await srv.close();
    srv = await createServer(opts());
    assert.equal(state(id).status, "idle");
    assert.deepEqual(srv.manager.livePids(), []);
    const j = jobOf(id, [SUBMIT]);
    await waitFor(() => srv.jobs.get(j.id)!.delivered);
    assert.ok(srv.manager.livePids().length === 1, "claude relaunched");
    await waitFor(() => state(id).status === "idle");
  });

  it("session open cards left 'running' by a previous run show the real (errored) job after a restart", async () => {
    const id = await create();
    const j = jobOf(id, [{ sleep: 5000 }, SUBMIT]);
    await waitFor(() => cards(id).length > 0);
    await srv.close();
    srv = await createServer(opts());
    const card = cards(id).find((c) => c.id === "job:" + j.id)!;
    assert.equal(card.job.status, "error");
  });
});
