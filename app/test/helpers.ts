import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import type { AnswerBody, ChatItem, JobRecord, ExerciseInfo, PlanState, ProgressRecord, ServerEvent, SessionConfig } from "../shared/protocol.ts";
import type { EngineCallbacks, EngineEvent, EngineHandle, JobStarter, ToolContext } from "../server/types.ts";

export const FIXTURE_REPO = path.join(import.meta.dirname, "fixtures", "repo");

/** One process of the in-memory engine: what it was given and the callbacks a test drives. */
export interface MemProc {
  resume?: string;
  cb: EngineCallbacks;
  sent: string[];
  interrupts: number;
  stopped: boolean;
}

/** In-memory engine (a TurnHost.spawn): no process; failNext() makes the next spawn throw. */
export function memoryEngine() {
  const procs: MemProc[] = [];
  let fail: Error | undefined;
  return {
    procs,
    last: () => procs[procs.length - 1],
    failNext: (e: Error) => void (fail = e),
    async spawn(resume: string | undefined, cb: EngineCallbacks): Promise<EngineHandle> {
      if (fail) {
        const e = fail;
        fail = undefined;
        throw e;
      }
      const p: MemProc = { resume, cb, sent: [], interrupts: 0, stopped: false };
      procs.push(p);
      const pid = 1000 + procs.length;
      return { send: (t) => void p.sent.push(t), interrupt: () => void p.interrupts++, stop: async () => void (p.stopped = true), pid };
    },
  };
}

export const resultEvent = (extra: Partial<Extract<EngineEvent, { type: "result" }>> = {}): EngineEvent => ({
  type: "result", isError: false, subtype: "success", sessionId: "", inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheCreationTokens: 0, costUsd: 0, ...extra,
});

/** Copy of the fixture repo in a fresh tmp dir; returns its absolute path. */
export async function makeTempRepo(): Promise<string> {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "learner-repo-"));
  await fs.cp(FIXTURE_REPO, dir, { recursive: true });
  return dir;
}

/** In-memory JobStarter: nothing runs; records are live objects a test may settle (status/result) by hand. */
export function memoryJobs(sessionId = "test-session"): JobStarter & { started: { kind: string; input: unknown; job: JobRecord }[] } {
  const started: { kind: string; input: unknown; job: JobRecord }[] = [];
  return {
    started,
    start: (kind, input, opts) => {
      const t = new Date().toISOString();
      const job: JobRecord = { id: "job-" + (started.length + 1), kind, title: opts.title, sessionId: opts.sessionId ?? sessionId, model: "haiku", status: "running", activity: "", createdAt: t, updatedAt: t, costUsd: 0 };
      started.push({ kind, input, job });
      return job;
    },
    list: () => started.map((j) => j.job),
    get: (id) => started.find((j) => j.job.id === id)?.job,
    cancel: () => {},
    markDelivered: (id) => {
      const j = started.find((x) => x.job.id === id);
      if (j) j.job.delivered = true;
    },
  };
}

export interface FakeContext extends ToolContext {
  items: (Omit<ChatItem, "id" | "at"> & { id?: string })[];
  events: ServerEvent[];
  requests: any[];
  progress: Omit<ProgressRecord, "at" | "sessionId" | "course">[];
  /** Jobs started through ctx.jobs.start (nothing runs). */
  jobsStarted: { kind: string; input: unknown; job: JobRecord }[];
}

/**
 * Fake ToolContext. `answers` are returned in order by request(); an `Error` entry rejects
 * with it (use new Error("cancelled")). Progress is also appended to courses/<course>/progress.jsonl.
 */
export function fakeContext(repoRoot: string, config: SessionConfig, answers: (AnswerBody["answer"] | Error)[] = []): FakeContext {
  let lessonPath: string | undefined;
  let plan: PlanState | null = null;
  let exercises: ExerciseInfo[] = [];
  const queue = [...answers];
  const jobs = memoryJobs();
  const ctx: FakeContext = {
    repoRoot,
    dataRoot: repoRoot,
    sessionId: "test-session",
    config,
    items: [],
    events: [],
    requests: [],
    progress: [],
    getLessonPath: () => lessonPath,
    setLessonPath: (p) => void (lessonPath = p),
    getPlan: () => plan,
    setPlan: (p) => void (plan = p),
    jobs,
    jobsStarted: jobs.started,
    async request(item) {
      ctx.requests.push(item);
      const a = queue.shift();
      if (a === undefined) throw new Error("fakeContext: no answer queued");
      if (a instanceof Error) throw a;
      return a as never;
    },
    emit(item) {
      ctx.items.push(item);
      return item.id ?? `item-${ctx.items.length}`;
    },
    emitEvent: (e) => void ctx.events.push(e),
    async logProgress(rec) {
      ctx.progress.push(rec);
      const file = path.join(repoRoot, "courses", config.course, "progress.jsonl");
      const full: ProgressRecord = { ...rec, at: new Date().toISOString(), sessionId: "test-session", course: config.course };
      await fs.appendFile(file, JSON.stringify(full) + "\n");
    },
    setExercises: (l) => void (exercises = l),
    getExercises: () => exercises,
  };
  return ctx;
}
