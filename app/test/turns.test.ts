// The per-session turn module, driven by an in-memory engine. Retry, interrupt and attachments moved to crates/proxi/src/turns.rs (#20); what stays has no Rust home yet (jobs, agy/opencode action-denied).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { EngineEvent } from "../server/types.ts";
import type { SessionStatus } from "../shared/protocol.ts";
import { jobTurnText } from "../server/jobs/deliver.ts";
import { Turns } from "../server/turns.ts";
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
});
