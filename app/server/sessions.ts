// SessionManager: sessions (create/load/persist), routing, EngineEvent -> ChatItems, pending learner requests.
// The turn lifecycle of each session lives in turns.ts; the engine behind it in engine.ts.
import { randomUUID } from "node:crypto";
import { AsyncLocalStorage } from "node:async_hooks";
import fs from "node:fs";
import path from "node:path";
import { z } from "zod";
import { readCurriculum } from "../../extensions/chapter.ts";
import { slugify } from "../../extensions/slug.ts";
import {
  ANKI_OPT_IN_QUESTION,
  MODELS,
  type AnswerBody,
  type AskAnswer,
  type AttachmentMeta,
  type ChatItem,
  type ExerciseInfo,
  type ExerciseRunResult,
  type InteractiveItem,
  type JobRecord,
  type ServerEvent,
  type SessionStatus,
  type SessionConfig,
  type SessionState,
  type SessionSummary,
  type Usage,
} from "../shared/protocol.ts";
import type { EngineCallbacks, EngineEvent, EngineHandle, JobStarter, Provider, ToolContext } from "./types.ts";
import { modelFor, effortFor, withEngineNote, spawnEngine, type EngineBins } from "./engine.ts";
import { isOwed, Turns, type LastTurnInput } from "./turns.ts";
import { emitGlobal } from "./events.ts";
import { readSettings } from "./settings.ts";
import { buildSystemPrompt, openingMessage } from "./context.ts";
import { appendProgress, readProgress } from "./knowledge.ts";
import { derivePlanState } from "./planstate.ts";
import { listExercises } from "./exercises.ts";
import { gradeQuiz } from "./tools/quiz.ts";
import { lessonRel, readLesson, slugOfLesson } from "./lesson.ts";
import { ensureStandaloneCourse, STANDALONE_COURSE_SLUG } from "./courses.ts";
import { importAnkiReviews } from "./anki-import.ts";

/** Request ids created inside one /mcp HTTP call (set by main.ts), so an abort cancels only those. */
export const callRequests = new AsyncLocalStorage<Set<string>>();

export class HttpError extends Error {
  constructor(public status: number, message: string, public code?: string) {
    super(message);
  }
}

const KEBAB = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

export const sessionConfigSchema = z.object({
  kind: z.enum(["teach", "plan", "practice"]),
  course: z.string().regex(KEBAB, "course must be a kebab-case directory name"),
  model: z.enum(MODELS as [string, ...string[]]).optional(),
  practicePropositions: z.array(z.string()).optional(),
  target: z
    .discriminatedUnion("mode", [
      z.object({ mode: z.literal("planned"), title: z.string().min(1) }),
      z.object({ mode: z.literal("existing"), lessonPath: z.string().min(1) }),
      z.object({ mode: z.literal("free") }),
    ])
    .optional(),
  ankiOptIn: z.boolean().optional(),
  chapterTitle: z.string().min(1).optional(),
  practiceNodes: z.array(z.string()).optional(),
  note: z.string().max(5000).optional(),
});

export interface ManagerOptions {
  repoRoot: string;
  /** Data folder (courses, graph, decks, curriculum.json). Defaults to repoRoot. */
  dataRoot?: string;
  /** Port the HTTP server listens on (mcp.json url). Set after listen when it was 0. */
  port: number;
  /** Secret the engine process presents on /mcp/*. */
  token: string;
  bins: EngineBins;
  addDirs?: string[];
  /** Background subagents (exposed to tools as ctx.jobs; the source of the results owed to each teacher). */
  jobs: JobStarter;
}

/** Interactive tools: their card is the representation, no chip. */
const NO_CHIP = new Set(["mcp__app__quiz", "mcp__app__ask", "mcp__app__propose_plan", "mcp__app__propose_curriculum"]);
const BUSY = new Set<SessionStatus>(["running", "waiting", "starting"]);

interface Pending {
  itemId: string;
  kind: AnswerBody["kind"];
  resolve(v: any): void;
  reject(e: Error): void;
}

interface Session {
  state: SessionState;
  /** Engine pinned at creation: a resume token only means something to the engine that issued it. */
  provider: Provider;
  /** Title of a session whose opening turn was never sent (Anki question unanswered, start failed, app restarted). */
  pendingStart?: string;
  pending: Map<string, Pending>;
  subs: Set<(e: ServerEvent) => void>;
  closing: boolean;
  /** Deleted: flush() must not recreate its directory (a tool call may still finish after the kill). */
  deleted?: boolean;
  /** Teacher items by message id: stream order (for text-block ordinals) and count of blocks finalised. */
  streamed: Map<string, string[]>;
  finalised: Map<string, number>;
  saveTimer?: NodeJS.Timeout;
  ctx: ToolContext;
  turns: Turns;
}

const now = () => new Date().toISOString();
const zeroUsage = (): Usage => ({ inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheCreationTokens: 0, turns: 0 });
const isInteractive = (i: ChatItem): i is InteractiveItem => "requestId" in i;
const trunc = (s: string, n: number) => (s.length > n ? s.slice(0, n) + "…" : s);

export class SessionManager {
  private sessions = new Map<string, Session>();
  private sessionsTimer?: NodeJS.Timeout;

  constructor(public opts: ManagerOptions) {
    this.load();
  }

  private get dataRoot() {
    return this.opts.dataRoot ?? this.opts.repoRoot;
  }

  private get root() {
    return path.join(this.opts.repoRoot, ".learner", "sessions");
  }
  private dir(id: string) {
    return path.join(this.root, id);
  }

  // ───────────── persistence ─────────────

  private load() {
    if (!fs.existsSync(this.root)) return;
    for (const id of fs.readdirSync(this.root)) {
      try {
        const raw = JSON.parse(fs.readFileSync(path.join(this.dir(id), "state.json"), "utf-8"));
        const state: SessionState = raw.state;
        let canRetry = raw.canRetry ?? state.retryable ?? false;
        if (BUSY.has(state.status)) {
          if (raw.lastTurn) {
            state.status = "error";
            state.lastError = "Sessione interrotta prima del completamento del turno";
            canRetry = true;
          } else {
            state.status = "idle";
          }
        }
        state.pendingRequestId = null;
        for (const it of state.items) {
          if (it.kind === "job") {
            // The job manager is the source of truth; a card left "running" by a previous run is an interrupted job.
            const j = this.opts.jobs.get(it.job.id);
            if (j) it.job = j;
            else if (it.job.status === "running") it.job = { ...it.job, status: "error", error: "interrotto alla chiusura dell'app", activity: "" };
          }
          if (it.kind === "teacher") it.streaming = false;
          if (isInteractive(it) && !("result" in it && it.result) && !("answer" in it && it.answer) && !("decision" in it && it.decision)) it.cancelled = true;
        }
        // Files written before the engine was pinned / the token was adapter-owned: claude, claudeSessionId once it ran.
        const s = this.makeSession(state, raw.provider ?? "claude", raw.resume ?? (raw.hasRun ? raw.claudeSessionId : undefined), raw.lastTurn, canRetry);
        this.sessions.set(id, s);
        if (state.status !== "closed") {
          if (raw.pendingStart) {
            s.pendingStart = raw.pendingStart;
            if (state.config.kind === "teach" && typeof state.config.ankiOptIn !== "boolean") {
              // The card was cancelled by the reload: ask again instead of leaving a session that can never start well.
              state.items = state.items.filter((it) => !(it.kind === "ask" && it.input.question === ANKI_OPT_IN_QUESTION && it.cancelled));
              this.askAnkiOptIn(s, raw.pendingStart);
            }
          }
        }
      } catch {
        /* unreadable session dir: skip */
      }
    }
  }

  private scheduleSave(s: Session) {
    if (s.saveTimer) return;
    s.saveTimer = setTimeout(() => this.flush(s), 300);
    s.saveTimer.unref();
  }

  private flush(s: Session) {
    if (s.saveTimer) clearTimeout(s.saveTimer);
    s.saveTimer = undefined;
    if (s.deleted) return;
    try {
      fs.mkdirSync(this.dir(s.state.id), { recursive: true });
      fs.writeFileSync(
        path.join(this.dir(s.state.id), "state.json"),
        JSON.stringify({
          state: s.state,
          provider: s.provider,
          resume: s.turns.resume,
          pendingStart: s.pendingStart,
          lastTurn: s.turns.savedLastTurn,
          canRetry: s.turns.canRetry,
        })
      );
    } catch (e: any) {
      console.error("[sessions] save failed:", e.message);
    }
  }

  // ───────────── state helpers ─────────────

  private notify(s: Session, e: ServerEvent) {
    s.state.updatedAt = now();
    this.scheduleSave(s);
    for (const fn of [...s.subs]) {
      try {
        fn(e);
      } catch {
        s.subs.delete(fn);
      }
    }
  }

  private upsert(s: Session, item: ChatItem) {
    const i = s.state.items.findIndex((x) => x.id === item.id);
    if (i >= 0) s.state.items[i] = item;
    else s.state.items.push(item);
    this.notify(s, { type: "item", item });
  }

  /** Debounced (300 ms) app-wide hint: the sessions list moved. */
  private touchSessions() {
    if (this.sessionsTimer) return;
    this.sessionsTimer = setTimeout(() => {
      this.sessionsTimer = undefined;
      emitGlobal({ type: "sessions" });
    }, 300);
    this.sessionsTimer.unref();
  }

  private setStatus(s: Session, status: SessionState["status"], error?: string) {
    s.state.status = status;
    s.state.lastError = status === "error" ? error : undefined;
    s.state.retryable = s.turns?.canRetry ?? false;
    this.touchSessions();
    this.notify(s, { type: "status", status, error, retryable: s.state.retryable });
  }

  private setPending(s: Session) {
    const last = [...s.pending.keys()].pop() ?? null;
    s.state.pendingRequestId = last;
    this.notify(s, { type: "pending", requestId: last });
  }

  private cancelPending(s: Session, only?: Set<string>) {
    for (const [reqId, p] of [...s.pending]) {
      if (only && !only.has(reqId)) continue;
      const item = s.state.items.find((x) => x.id === p.itemId);
      if (item && isInteractive(item)) {
        item.cancelled = true;
        this.notify(s, { type: "item", item });
      }
      s.pending.delete(reqId);
      p.reject(new Error("cancelled"));
    }
    if (s.state.pendingRequestId) this.setPending(s);
    if (s.state.status === "waiting" && s.pending.size === 0) this.setStatus(s, "running");
  }

  /** The /mcp HTTP request was aborted: its pending card can never be answered. */
  cancelRequests(id: string, only?: Set<string>) {
    const s = this.sessions.get(id);
    if (s) this.cancelPending(s, only);
  }

  /** Pids of live engine processes (for the synchronous exit hook). */
  livePids(): number[] {
    return [...this.sessions.values()].flatMap((s) => (s.turns.pid !== undefined ? [s.turns.pid] : []));
  }

  private makeSession(
    state: SessionState,
    provider: Provider,
    resume: string | undefined,
    lastTurn?: LastTurnInput,
    canRetry = false,
  ): Session {
    const s: Session = {
      state, provider,
      pending: new Map(), subs: new Set(), closing: false, streamed: new Map(), finalised: new Map(),
      ctx: undefined as any, turns: undefined as any,
    };
    s.ctx = this.makeContext(s);
    s.turns = new Turns(
      {
        spawn: (r, cb) => this.spawn(s, r, cb),
        event: (e, late) => this.onEvent(s, e, late),
        status: () => s.state.status,
        setStatus: (st, error) => this.setStatus(s, st, error),
        notice: (level, text) => this.notice(s, level, text),
        learner: (text, attachments) => this.upsert(s, { id: "l:" + randomUUID(), at: now(), kind: "learner", text, ...(attachments?.length ? { attachments } : {}) }),
        pending: () => s.pending.size,
        cancelPending: () => this.cancelPending(s),
        jobs: { list: () => s.ctx.jobs.list(), markDelivered: (id) => this.opts.jobs.markDelivered(id) },
      },
      resume,
      lastTurn,
      canRetry,
    );
    state.retryable = s.turns.canRetry;
    return s;
  }

  private makeContext(s: Session): ToolContext {
    const self = this;
    return {
      repoRoot: this.opts.repoRoot,
      dataRoot: this.dataRoot,
      sessionId: s.state.id,
      config: s.state.config,
      getLessonPath: () => s.state.lessonPath,
      setLessonPath(p) {
        s.state.lessonPath = p;
        self.notify(s, { type: "lesson", lessonPath: p });
      },
      getPlan: () => s.state.plan,
      setPlan(p) {
        s.state.plan = p;
        self.notify(s, { type: "plan", plan: p });
      },
      request(item: any) {
        if (s.closing || !s.turns.live) return Promise.reject(new Error("cancelled"));
        // At most one pending interactive request at a time: cancel/supersede older pending cards
        if (s.pending.size > 0) {
          self.cancelPending(s);
        }
        const requestId = randomUUID();
        callRequests.getStore()?.add(requestId);
        const full = { ...item, id: "req:" + requestId, at: now(), requestId } as InteractiveItem;
        return new Promise<any>((resolve, reject) => {
          s.pending.set(requestId, { itemId: full.id, kind: full.kind as AnswerBody["kind"], resolve, reject });
          self.upsert(s, full);
          self.setPending(s);
          self.setStatus(s, "waiting");
        });
      },
      emit(item) {
        const id = item.id ?? randomUUID();
        self.upsert(s, { ...item, id, at: now() } as ChatItem);
        return id;
      },
      emitEvent(e) {
        if (e.type === "lesson") s.state.lessonPath = e.lessonPath;
        else if (e.type === "plan") s.state.plan = e.plan;
        else if (e.type === "exercises") s.state.exercises = e.exercises;
        else if (e.type === "item") return void self.upsert(s, e.item);
        self.notify(s, e);
      },
      logProgress: (rec) =>
        appendProgress(self.dataRoot, { ...rec, at: now(), sessionId: s.state.id, course: s.state.config.course }),
      setExercises(list) {
        s.state.exercises = list;
        self.notify(s, { type: "exercises", exercises: list });
      },
      getExercises: () => s.state.exercises,
      jobs: {
        start: (kind, input, opts) => self.opts.jobs.start(kind, input, { ...opts, sessionId: s.state.id }),
        list: () => self.opts.jobs.list().filter((j) => j.sessionId === s.state.id),
        get: (id) => self.opts.jobs.get(id),
        cancel: (id) => self.opts.jobs.cancel(id),
        markDelivered: (id) => self.opts.jobs.markDelivered(id),
      },
    };
  }

  /** A job owned by a session changed: upsert its card; a result now owed goes to an idle teacher at once. */
  onJob(job: JobRecord): void {
    const s = job.sessionId ? this.sessions.get(job.sessionId) : undefined;
    if (!s || s.state.status === "closed") return;
    this.upsert(s, { id: "job:" + job.id, at: job.createdAt, kind: "job", job });
    if (isOwed(job)) s.turns.deliver();
  }

  // ───────────── public API ─────────────

  list(): SessionSummary[] {
    return [...this.sessions.values()]
      .map(({ state: { items, pendingRequestId, plan, exercises, lastError, ...summary } }) => summary)
      .sort((a, b) => b.createdAt.localeCompare(a.createdAt));
  }

  get(id: string): SessionState | undefined {
    return this.sessions.get(id)?.state;
  }

  getContext(id: string): ToolContext | undefined {
    return this.sessions.get(id)?.ctx;
  }

  subscribe(id: string, fn: (e: ServerEvent) => void): () => void {
    const s = this.need(id);
    fn({ type: "snapshot", state: s.state });
    s.subs.add(fn);
    return () => void s.subs.delete(fn);
  }

  private need(id: string): Session {
    const s = this.sessions.get(id);
    if (!s) throw new HttpError(404, "Session not found");
    return s;
  }

  async create(raw: unknown): Promise<string> {
    const parsed = sessionConfigSchema.safeParse(raw);
    if (!parsed.success) throw new HttpError(400, parsed.error.issues.map((i) => i.path.join(".") + ": " + i.message).join("; "));
    const config = parsed.data as SessionConfig;
    config.model ??= readSettings(this.opts.repoRoot).teacherModel;
    const root = this.dataRoot;
    const courseDir = path.join(root, "courses", config.course);
    let title: string;
    let lessonPath: string | undefined;
    let restored: { topic: string; goal: string; dag: string; nodeCount: number } | undefined;

    if (config.course === STANDALONE_COURSE_SLUG) ensureStandaloneCourse(root, { force: true });
    if (config.kind !== "plan" && !fs.existsSync(courseDir)) throw new HttpError(400, `Course "${config.course}" does not exist`);
    if (config.kind === "teach") void importAnkiReviews(root, config.course).catch(() => {});

    if (config.kind === "plan") {
      if (!config.chapterTitle) throw new HttpError(400, "chapterTitle is required for plan sessions");
      title = `Piano · ${config.chapterTitle}`;
    } else if (config.kind === "practice") {
      title = `Esercizi · ${config.course}`;
    } else {
      const t = config.target;
      if (!t) throw new HttpError(400, "target is required for teach sessions");
      if (t.mode === "planned") {
        const chapter = readCurriculum({ baseDir: root }).chapters.find((c) => c.slug === config.course);
        if (!chapter?.lessons.some((l) => l.title === t.title)) throw new HttpError(400, `"${t.title}" is not a planned lesson of ${config.course}`);
        const slug = slugify(t.title);
        const lessonsDir = path.join(courseDir, "lessons");
        if (fs.existsSync(lessonsDir) && fs.readdirSync(lessonsDir).some((f) => f.endsWith(".html") && slugOfLesson(f) === slug))
          throw new HttpError(400, `"${t.title}" è già stata svolta: usa "Continua" sulla lezione esistente.`);
        title = t.title;
      } else if (t.mode === "existing") {
        const rel = lessonRel(t.lessonPath);
        const prefix = `courses/${config.course}/lessons/`;
        if (!rel || !rel.startsWith(prefix) || !fs.existsSync(path.resolve(root, rel))) throw new HttpError(400, `lessonPath must be an existing file under ${prefix}`);
        let lesson;
        try {
          lesson = readLesson(root, rel);
        } catch (e: any) {
          throw new HttpError(400, e.message);
        }
        if (!lesson) throw new HttpError(400, "That lesson has no data island and cannot be continued");
        const doc = lesson.doc;
        if (doc.nodes.length === 0 && !doc.metadata.initialDag)
          throw new HttpError(400, "Questa lezione è in prosa (scritta prima del motore, senza nodi né piano): non si può continuare. Scegli una lezione del piano o una lezione libera.");
        title = doc.metadata.title;
        lessonPath = rel;
        // The plan is already approved in the artifact: restore it so the Piano tab and quiz nodeId checks work.
        if (doc.metadata.initialDag) restored = { topic: doc.metadata.topic, goal: doc.metadata.targetGoal ?? "", dag: doc.metadata.initialDag, nodeCount: doc.nodes.length };
      } else {
        title = config.course === STANDALONE_COURSE_SLUG
          ? (config.note?.trim() ? `Lezione singola · ${trunc(config.note.trim(), 60)}` : "Lezione singola")
          : `Lezione libera · ${config.course}`;
      }
    }

    const id = randomUUID();
    const t = now();
    const state: SessionState = {
      id, config, title, createdAt: t, updatedAt: t, status: "starting", lessonPath,
      usage: zeroUsage(), items: [], pendingRequestId: null, plan: null, exercises: [],
    };
    if (restored) {
      const slug = slugOfLesson(lessonPath!);
      const progress = (await readProgress(root, config.course)).filter((r) => r.lessonSlug === slug);
      state.plan = derivePlanState({ topic: restored.topic, goal: restored.goal, dag: restored.dag, approved: true }, restored.nodeCount, progress);
    }
    const s = this.makeSession(state, readSettings(this.opts.repoRoot).provider, undefined);
    this.sessions.set(id, s);
    this.touchSessions();
    s.pendingStart = title;
    this.flush(s);
    if (config.kind === "teach" && typeof config.ankiOptIn !== "boolean") {
      this.askAnkiOptIn(s, title);
      return id;
    }
    await this.start(s, title);
    return id;
  }

  /** The opening turn (launches the engine). Failure: status "error", pendingStart kept, throws. */
  private async start(s: Session, title: string, learnerText?: string, attachments?: AttachmentMeta[]): Promise<void> {
    const opening = openingMessage(s.state.config, title);
    // One turn: the opening, then what the learner typed (the process has to see both, in this order).
    const text = learnerText ? [opening, "Messaggio dello studente:", learnerText].join("\n\n") : opening;
    await s.turns.send(text, learnerText ? [opening, learnerText] : [opening], attachments);
    s.pendingStart = undefined;
    this.flush(s);
  }

  /** Teach session without an explicit opt-in: ask first, with no claude process until the answer arrives. */
  private askAnkiOptIn(s: Session, title: string) {
    const requestId = randomUUID();
    const item: InteractiveItem = {
      id: "req:" + requestId, at: now(), kind: "ask", requestId,
      input: { question: ANKI_OPT_IN_QUESTION, options: [{ label: "Sì", value: "si" }, { label: "No", value: "no" }] },
    };
    new Promise<AskAnswer>((resolve, reject) => s.pending.set(requestId, { itemId: item.id, kind: "ask", resolve, reject }))
      .then(async (a) => {
        s.state.config.ankiOptIn = a.selected[0] === "si";
        this.flush(s);
        await this.start(s, title).catch(() => {}); // status "error" already set
      })
      .catch(() => {
        // Interrupted or closed before the answer: nothing ran; pendingStart stays, so the next message() runs the start path.
        if (!s.closing) this.setStatus(s, "idle");
      });
    this.upsert(s, item);
    this.setPending(s);
    this.setStatus(s, "waiting");
  }

  async saveAttachments(id: string, files: Array<{ name: string; type?: string; size?: number; arrayBuffer(): Promise<ArrayBuffer> }>): Promise<AttachmentMeta[]> {
    const s = this.need(id);
    if (s.state.status === "closed") throw new HttpError(409, "Session is closed");
    const attachDir = path.join(this.dir(id), "attachments");
    fs.mkdirSync(attachDir, { recursive: true });

    const allowedExts = new Set([".png", ".jpg", ".jpeg", ".webp", ".gif", ".svg", ".pdf", ".txt", ".md"]);
    const metas: AttachmentMeta[] = [];
    for (const file of files) {
      if (!file || !file.name) continue;
      const rawName = path.basename(file.name);
      const safeBase = rawName.replace(/[^A-Za-z0-9._-]/g, "_") || "attachment";
      const ext = path.extname(safeBase).toLowerCase();
      if (!allowedExts.has(ext)) {
        throw new HttpError(400, `Tipo di file non supportato: ${ext || "sconosciuto"}`);
      }
      const fileId = randomUUID();
      const storedName = `${fileId.slice(0, 8)}_${safeBase}`;
      const destPath = path.join(attachDir, storedName);
      const buf = Buffer.from(await file.arrayBuffer());
      fs.writeFileSync(destPath, buf);

      const repoRel = path.posix.join(".learner", "sessions", id, "attachments", storedName);
      metas.push({
        id: fileId,
        name: rawName,
        path: repoRel,
        mimeType: file.type || "application/octet-stream",
        size: buf.length,
      });
    }
    return metas;
  }

  async message(id: string, text: string, attachments?: AttachmentMeta[]): Promise<void> {
    const s = this.need(id);
    if (s.state.status === "closed") throw new HttpError(409, "Session is closed");
    if (BUSY.has(s.state.status)) throw new HttpError(409, "The teacher is busy: wait for the current turn to finish");
    if (s.pendingStart) {
      // Never ran (Anki card dismissed, start failed, app restarted): open the lesson, with his message in the same turn.
      // No answer to the Anki question means no opt-in: cards are never created by default.
      if (typeof s.state.config.ankiOptIn !== "boolean" && s.state.config.kind === "teach") s.state.config.ankiOptIn = false;
      return this.start(s, s.pendingStart, text, attachments);
    }
    await s.turns.send(text, [text], attachments); // results still owed to the teacher travel with this turn
  }

  answer(id: string, body: AnswerBody): void {
    const s = this.need(id);
    let p = s.pending.get(body.requestId);
    if (!p) {
      // If the specific requestId is no longer pending (e.g. superseded by a retry),
      // forward to the active pending request of the same kind so the learner's answer is not lost.
      const active = [...s.pending.entries()][0];
      if (active && active[1].kind === body.kind) {
        body.requestId = active[0];
        p = active[1];
      } else {
        throw new HttpError(409, "No pending request with that id");
      }
    }
    if (p.kind !== body.kind) throw new HttpError(400, `Request is a ${p.kind}, not a ${body.kind}`);
    const item = s.state.items.find((x) => x.id === p.itemId) as any;
    if (body.kind === "quiz") item.result = gradeQuiz(item.input, body.answer);
    else if (body.kind === "ask") item.answer = body.answer;
    else item.decision = body.answer;
    s.pending.delete(body.requestId);
    this.notify(s, { type: "item", item });
    this.setPending(s);
    if (s.pending.size === 0) this.setStatus(s, "running");
    p.resolve(body.answer);
  }

  interrupt(id: string): void {
    this.need(id).turns.interrupt();
  }

  async retry(id: string): Promise<void> {
    const s = this.need(id);
    if (s.state.status === "closed") throw new HttpError(409, "Session is closed");
    if (BUSY.has(s.state.status)) throw new HttpError(409, "The teacher is busy: wait for the current turn to finish");
    if (!s.turns.canRetry) throw new HttpError(400, "Nessun turno fallito da riprovare");

    const items = s.state.items;
    if (s.turns.savedLastTurn?.pushedLearner) {
      const lastLearnerIndex = items.findLastIndex((it) => it.kind === "learner");
      if (lastLearnerIndex >= 0) {
        s.state.items = items.slice(0, lastLearnerIndex + 1);
      }
    } else {
      s.state.items = items.filter((it) => !(it.kind === "notice" && it.level === "error") && !(it.kind === "teacher" && it.streaming));
    }
    s.streamed.clear();
    s.finalised.clear();
    this.notify(s, { type: "snapshot", state: s.state });

    await s.turns.retry();
    this.flush(s);
  }

  private notice(s: Session, level: "info" | "error", text: string) {
    this.upsert(s, { id: "n:" + randomUUID(), at: now(), kind: "notice", level, text });
  }

  /** No more turns: pending cards cancelled, engine process stopped. */
  private async halt(s: Session) {
    s.closing = true;
    this.cancelPending(s);
    await s.turns.stop();
  }

  async close(id: string): Promise<void> {
    const s = this.need(id);
    await this.halt(s);
    this.setStatus(s, "closed");
    this.flush(s);
  }

  /** Stops the session and forgets it (its .learner/sessions/<id> dir). Lessons, exercises and the map are untouched. */
  async delete(id: string): Promise<void> {
    const s = this.need(id);
    await this.halt(s);
    s.deleted = true;
    this.flush(s); // clears a pending save timer, writes nothing
    this.sessions.delete(id);
    fs.rmSync(this.dir(id), { recursive: true, force: true, maxRetries: 3 });
    this.touchSessions();
  }

  /** Server shutdown: stop every process, keep statuses (reload turns busy ones into idle). */
  async shutdown(): Promise<void> {
    await Promise.all(
      [...this.sessions.values()].map(async (s) => {
        await this.halt(s);
        this.flush(s);
      })
    );
  }

  /** Exercise run reported from the UI: remember lastRun, tell an idle teacher. */
  async reportExercise(id: string, course: string, name: string, result: ExerciseRunResult): Promise<void> {
    const s = this.sessions.get(id);
    if (!s) return;
    let list = s.state.exercises;
    let ex: ExerciseInfo | undefined = list.find((e) => e.name === name);
    if (!ex) {
      ex = (await listExercises(this.dataRoot, course)).find((e) => e.name === name);
      if (ex) list = [...list, ex];
    }
    if (ex) s.ctx.setExercises(list.map((e) => (e.name === name ? { ...e, lastRun: result } : e)));
    if (s.state.status === "idle" && s.turns.live) {
      const failed = result.failures.map((f) => f.name).join(", ");
      const text = result.error
        ? `Esito test ${name}: impossibile eseguire (${result.error})`
        : `Esito test ${name}: ${result.passed}/${result.total} verdi` + (result.failed ? `, falliti: ${failed}` : "");
      await this.message(id, text).catch(() => {});
    }
  }

  /** Hint revealed from the UI: refresh the session's exercise, tell an idle teacher. */
  async reportHint(id: string, info: ExerciseInfo): Promise<void> {
    const s = this.sessions.get(id);
    if (!s) return;
    const list = s.state.exercises;
    s.ctx.setExercises(list.some((e) => e.name === info.name) ? list.map((e) => (e.name === info.name ? { ...e, ...info, lastRun: e.lastRun } : e)) : [...list, info]);
    if (s.state.status === "idle" && s.turns.live) {
      await this.message(id, `Suggerimento ${info.hintsRevealed}/${info.hints?.length} aperto per ${info.name}`).catch(() => {});
    }
  }

  // ───────────── engine plumbing ─────────────

  /** Starts this session's pinned engine (TurnHost.spawn). */
  private async spawn(s: Session, resume: string | undefined, cb: EngineCallbacks): Promise<EngineHandle> {
    const id = s.state.id;
    const systemPrompt = await buildSystemPrompt({
      config: s.state.config,
      repoRoot: this.opts.repoRoot,
      dataRoot: this.dataRoot,
      lessonPath: s.state.lessonPath,
      sessionId: id,
      today: now().slice(0, 10),
    });
    if (s.closing) throw new Error("Session is closed");
    return spawnEngine(
      {
        provider: s.provider,
        bin: this.opts.bins[s.provider],
        repoRoot: this.opts.repoRoot,
        dataRoot: this.dataRoot,
        scratchDir: this.dir(id),
        model: modelFor(readSettings(this.opts.repoRoot), s.provider, "teacher", s.state.config.model),
        effort: effortFor(readSettings(this.opts.repoRoot), s.provider, "teacher"),
        systemPrompt: withEngineNote(systemPrompt, s.provider, ["read", "web"]),
        grant: ["read", "web"],
        mcp: { name: "app", url: `http://127.0.0.1:${this.opts.port}/mcp/${id}`, token: this.opts.token },
        addDirs: this.opts.addDirs ?? [path.dirname(this.opts.repoRoot)],
        resume,
      },
      cb,
    );
  }

  /** Engine event -> chat items and usage (TurnHost.event). */
  private onEvent(s: Session, e: EngineEvent, late: boolean) {
    const items = s.state.items;
    // Streamed text blocks are keyed by the stream index; the CLI's `assistant` events carry one block each with
    // index 0, so finalisation pairs the k-th streamed text block of a message with its k-th finalised one.
    const teacher = (mid: string, idx: number): Extract<ChatItem, { kind: "teacher" }> => {
      const id = `t:${mid}:${idx}`;
      let it = items.find((x) => x.id === id) as any;
      if (!it) {
        it = { id, at: now(), kind: "teacher", text: "", streaming: true };
        s.streamed.set(mid, [...(s.streamed.get(mid) ?? []), id]);
        this.upsert(s, it);
      }
      return it;
    };
    const finalise = (mid: string, text: string) => {
      const k = s.finalised.get(mid) ?? 0;
      s.finalised.set(mid, k + 1);
      const id = s.streamed.get(mid)?.[k] ?? `t:${mid}:f${k}`;
      let it = items.find((x) => x.id === id) as any;
      if (!it) it = { id, at: now(), kind: "teacher", text };
      it.text = text;
      it.streaming = false;
      this.upsert(s, it);
    };
    switch (e.type) {
      case "init": {
        this.scheduleSave(s); // a new resume token
        const app = e.mcpServers.find((m) => m.name === "app");
        if (app && app.status !== "connected")
          this.upsert(s, { id: "n:" + randomUUID(), at: now(), kind: "notice", level: "error", text: `Il server degli strumenti non è connesso (stato: ${app.status}).` });
        break;
      }
      case "text-start":
        teacher(e.messageId, e.index);
        break;
      case "text-delta":
        teacher(e.messageId, e.index).text += e.text;
        this.notify(s, { type: "delta", itemId: `t:${e.messageId}:${e.index}`, text: e.text });
        break;
      case "assistant":
        for (const t of e.texts) finalise(e.messageId, t.text);
        for (const u of e.toolUses) {
          if (NO_CHIP.has(u.name)) continue;
          this.upsert(s, { id: "tool:" + u.id, at: now(), kind: "tool", tool: u.name, input: u.input, status: "running" });
        }
        break;
      case "tool-result": {
        const it = items.find((x) => x.id === "tool:" + e.toolUseId);
        if (it?.kind === "tool") {
          it.status = e.isError ? "error" : "done";
          it.resultText = trunc(e.text, 2000);
          this.upsert(s, it);
        }
        break;
      }
      case "result": {
        const u = s.state.usage;
        u.inputTokens += e.inputTokens;
        u.outputTokens += e.outputTokens;
        u.cacheReadTokens += e.cacheReadTokens;
        u.cacheCreationTokens += e.cacheCreationTokens;
        u.turns += 1;
        this.notify(s, { type: "usage", usage: u });
        // Late result of a watchdog-aborted turn while a newer turn runs: usage counted, state untouched.
        if (late) break;
        s.streamed.clear();
        s.finalised.clear();
        for (const it of items) if (it.kind === "teacher" && it.streaming) { it.streaming = false; this.upsert(s, it); }
        break;
      }
      default:
        break;
    }
  }
}
