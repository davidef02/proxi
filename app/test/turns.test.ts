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

  it("retry throws when not retryable", async () => {
    const { turns } = harness();
    await assert.rejects(turns.retry(), /Nessun turno fallito da riprovare/);
  });
});
