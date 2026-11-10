// The per-session turn module, driven by an in-memory engine: no process, deterministic.
import assert from "node:assert/strict";
import { describe, it, mock } from "node:test";
import type { EngineEvent } from "../server/types.ts";
import type { SessionStatus } from "../shared/protocol.ts";
import { jobTurnText } from "../server/jobs/deliver.ts";
import { Turns, WATCHDOG_MS } from "../server/turns.ts";
import { memoryEngine, memoryJobs, resultEvent } from "./helpers.ts";

function harness(resume?: string) {
  const engine = memoryEngine();
  const jobs = memoryJobs("s");
  const h = { status: "idle" as SessionStatus, errors: [] as string[], notices: [] as string[], learner: [] as string[], late: [] as boolean[], pending: 0 };
  const turns = new Turns(
    {
      spawn: engine.spawn,
      event: (e, late) => void (e.type === "result" && h.late.push(late)),
      status: () => h.status,
      setStatus: (s, err) => {
        h.status = s;
        if (err) h.errors.push(err);
      },
      notice: (_l, t) => void h.notices.push(t),
      learner: (t) => void h.learner.push(t),
      pending: () => h.pending,
      cancelPending: () => void (h.pending = 0),
      jobs,
    },
    resume,
  );
  const emit = (e: EngineEvent) => engine.last().cb.onEvent(e);
  /** A settled research job owned by the session. */
  const doneJob = () => {
    const j = jobs.start("research", {}, { title: "Ricerca", sessionId: "s" });
    Object.assign(j, { status: "done", result: { kind: "research", brief: "B", sources: [] } });
    return j;
  };
  return { engine, jobs, h, turns, emit, doneJob };
}

describe("Turns (in-memory engine)", () => {
  it("first send launches fresh, pushes the bubbles, runs; init issues the resume token; result idles", async () => {
    const { engine, h, turns, emit } = harness();
    await turns.send("ciao", ["ciao"]);
    assert.equal(engine.procs.length, 1);
    assert.equal(engine.last().resume, undefined);
    assert.deepEqual([h.status, h.learner, engine.last().sent], ["running", ["ciao"], ["ciao"]]);
    emit({ type: "init", sessionId: "tok-1", tools: [], mcpServers: [] });
    emit(resultEvent({ sessionId: "tok-1" }));
    assert.equal(h.status, "idle");
    assert.equal(turns.resume, "tok-1");
  });

  it("interrupt, then the late result of the given-up turn does not idle the newer turn", async () => {
    mock.timers.enable({ apis: ["setTimeout"] });
    try {
      const { engine, h, turns, emit } = harness();
      await turns.send("uno", ["uno"]);
      turns.interrupt();
      assert.equal(engine.last().interrupts, 1);
      mock.timers.tick(WATCHDOG_MS);
      assert.equal(h.status, "idle");
      assert.deepEqual(h.notices, ["Turno interrotto."]);
      await turns.send("due", ["due"]);
      emit(resultEvent()); // late result of "uno"
      assert.equal(h.status, "running");
      emit(resultEvent());
      assert.equal(h.status, "idle");
      assert.deepEqual(h.late, [true, false]);
    } finally {
      mock.timers.reset();
    }
  });

  it("interrupt with a prompt result: one 'Turno interrotto.' notice, no error notice, watchdog disarmed", async () => {
    mock.timers.enable({ apis: ["setTimeout"] });
    try {
      const { h, turns, emit } = harness();
      await turns.send("uno", ["uno"]);
      turns.interrupt();
      emit(resultEvent({ isError: true, subtype: "error_during_execution" }));
      mock.timers.tick(WATCHDOG_MS);
      assert.equal(h.status, "idle");
      assert.deepEqual(h.notices, ["Turno interrotto."]);
    } finally {
      mock.timers.reset();
    }
  });

  it("a failed relaunch leaves the owed results owed (status error); they ride along with the next learner message", async () => {
    const { engine, jobs, h, turns, emit, doneJob } = harness();
    const j = doneJob();
    engine.failNext(new Error("no binary"));
    turns.deliver();
    await new Promise((r) => setImmediate(r));
    assert.equal(h.status, "error");
    assert.deepEqual(h.errors, ["no binary"]);
    assert.equal(jobs.get(j.id)!.delivered, undefined);
    turns.deliver(); // not idle: nothing happens
    assert.equal(engine.procs.length, 0);

    await turns.send("eccomi", ["eccomi"]);
    assert.deepEqual(engine.last().sent, [jobTurnText(j) + "\n\n---\n\neccomi"]);
    assert.deepEqual(h.learner, ["eccomi"], "the job result is not a learner bubble");
    assert.equal(jobs.get(j.id)!.delivered, true);
    emit(resultEvent());
    assert.equal(engine.last().sent.length, 1, "nothing left to deliver");
  });

  it("a job settling mid-turn waits; the turn's end hands it over as one turn; map jobs are never owed", async () => {
    const { engine, jobs, h, turns, emit, doneJob } = harness();
    await turns.send("a", ["a"]);
    const j = doneJob();
    const map = jobs.start("map", {}, { title: "Mappa", sessionId: "s" });
    map.status = "done";
    turns.deliver();
    assert.equal(engine.last().sent.length, 1, "busy: not delivered");
    emit(resultEvent());
    assert.deepEqual(engine.last().sent[1], jobTurnText(j));
    assert.equal(h.status, "running");
    assert.equal(map.delivered, undefined);
  });

  it("resume refused: fresh relaunch (no token) re-sends the same turn once, without a second bubble", async () => {
    const { engine, h, turns, emit } = harness("old-token");
    await turns.send("dopo il crash", ["dopo il crash"]);
    assert.equal(engine.last().resume, "old-token");
    emit({ type: "resume-failed" });
    engine.last().cb.onExit(1, null);
    await new Promise((r) => setImmediate(r));
    assert.equal(engine.procs.length, 2);
    assert.equal(engine.last().resume, undefined);
    assert.deepEqual(engine.last().sent, ["dopo il crash"]);
    assert.deepEqual(h.learner, ["dopo il crash"]);
    assert.equal(h.status, "running");
    assert.match(h.notices[0], /Ripresa della conversazione non riuscita/);
    emit({ type: "init", sessionId: "new-token", tools: [], mcpServers: [] });
    assert.equal(turns.resume, "new-token");
  });

  it("an exit without a refused resume is an error (cards cancelled), and the next send relaunches with the token", async () => {
    const { engine, h, turns, emit } = harness();
    await turns.send("x", ["x"]);
    emit({ type: "init", sessionId: "tok", tools: [], mcpServers: [] });
    h.pending = 2;
    engine.last().cb.onStderr("boom");
    engine.last().cb.onExit(1, null);
    assert.equal(h.pending, 0);
    assert.equal(h.status, "error");
    assert.match(h.errors[0], /exited \(1\): boom/);
    assert.equal(turns.live, false);
    await turns.send("y", ["y"]);
    assert.equal(engine.last().resume, "tok");
  });

  it("action-denied becomes an info notice; stop() kills, ignores the dead handle's exit and refuses to launch again", async () => {
    const { engine, h, turns, emit } = harness();
    await turns.send("x", ["x"]);
    emit({ type: "action-denied", action: "ReadUrlContent" });
    assert.match(h.notices[0], /bloccato.*ReadUrlContent/);
    const p = engine.last();
    await turns.stop();
    assert.equal(p.stopped, true);
    p.cb.onExit(null, null);
    assert.equal(h.status, "running", "an exit after stop() is not an error");
    await assert.rejects(turns.send("z", ["z"]), /closed/);
    assert.equal(engine.procs.length, 1);
  });

  it("formats attachments into the turn prompt and passes them to learner bubbles", async () => {
    const { engine, turns } = harness();
    const atts = [
      { id: "1", name: "diagram.png", path: ".learner/sessions/s/attachments/diagram.png", mimeType: "image/png", size: 100 },
      { id: "2", name: "book.pdf", path: ".learner/sessions/s/attachments/book.pdf", mimeType: "application/pdf", size: 500 },
    ];
    await turns.send("Spiegami questo diagramma", ["Spiegami questo diagramma"], atts);
    const sent = engine.last().sent[0];
    assert.match(sent, /\[Allegati forniti dallo studente:/);
    assert.match(sent, /- diagram\.png \(percorso: \.learner\/sessions\/s\/attachments\/diagram\.png\)/);
    assert.match(sent, /- book\.pdf \(percorso: \.learner\/sessions\/s\/attachments\/book\.pdf\)/);
    assert.match(sent, /Spiegami questo diagramma/);
  });

  it("model error (result isError: true) enables retry; retry re-submits without duplicating learner bubbles", async () => {
    const { engine, h, turns, emit } = harness();
    await turns.send("Qual è la derivata di x^2?", ["Qual è la derivata di x^2?"]);
    assert.equal(turns.canRetry, false);
    assert.deepEqual(h.learner, ["Qual è la derivata di x^2?"]);

    emit({ type: "init", sessionId: "tok-err", tools: [], mcpServers: [] });
    emit(resultEvent({ isError: true, subtype: "error_during_execution", text: "Rate limit reached" }));

    assert.equal(h.status, "error");
    assert.equal(turns.canRetry, true);
    assert.match(h.notices[0], /Errore del docente.*Rate limit reached/);

    // Retrying re-submits the exact input prompt without duplicating learner bubbles
    await turns.retry();
    assert.equal(h.status, "running");
    assert.equal(turns.canRetry, false);
    assert.deepEqual(h.learner, ["Qual è la derivata di x^2?"]); // still 1 bubble!
    assert.deepEqual(engine.last().sent, ["Qual è la derivata di x^2?", "Qual è la derivata di x^2?"]);

    // When the retry finishes successfully, it idles and resets canRetry
    emit(resultEvent({ isError: false }));
    assert.equal(h.status, "idle");
    assert.equal(turns.canRetry, false);
    assert.equal(turns.savedLastTurn, undefined);
  });

  it("process crash mid-turn enables retry; retry re-launches with resume token and re-sends turn", async () => {
    const { engine, h, turns, emit } = harness();
    await turns.send("Spiegazione", ["Spiegazione"]);
    emit({ type: "init", sessionId: "tok-crash", tools: [], mcpServers: [] });

    // Engine crashes while waiting for result
    engine.last().cb.onStderr("SIGSEGV");
    engine.last().cb.onExit(139, null);

    assert.equal(h.status, "error");
    assert.equal(turns.canRetry, true);
    assert.equal(turns.live, false);

    // Retrying re-spawns engine with resume token and re-sends
    await turns.retry();
    assert.equal(turns.live, true);
    assert.equal(engine.last().resume, "tok-crash");
    assert.deepEqual(engine.last().sent, ["Spiegazione"]);
    assert.deepEqual(h.learner, ["Spiegazione"]); // no duplicate bubble
    assert.equal(h.status, "running");
  });

  it("retry throws when not retryable", async () => {
    const { turns } = harness();
    await assert.rejects(turns.retry(), /Nessun turno fallito da riprovare/);
  });
});
