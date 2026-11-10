// Background subagents: one headless engine process per job, with its own system prompt, a restricted tool set and a
// per-job MCP endpoint (/mcp/job/:id). Records live in .learner/jobs/<id>.json (+ <id>/ for prompt and mcp config).
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import type { Hono } from "hono";
import type { JobKind, JobRecord, JobResult, Provider } from "../../shared/protocol.ts";
import { modelFor, effortFor, withEngineNote, spawnEngine, type EngineBins } from "../engine.ts";
import { emitGlobal } from "../events.ts";
import { handleMcp } from "../mcp.ts";
import { readSettings } from "../settings.ts";
import type { EngineEvent, EngineHandle, JobContext, JobRole, JobStarter } from "../types.ts";
import { closeBrowser } from "./render.ts";
import { demoRole } from "./roles/demo.ts";
import { researchRole } from "./roles/research.ts";
import { visualRole } from "./roles/visual.ts";
import { CLAUDE_AUTH_ERROR_MESSAGE, CLAUDE_AUTH_ERROR_RE } from "../claude.ts";

export interface JobManagerOptions {
  repoRoot: string;
  /** Data folder (courses, graph, decks, curriculum.json). Defaults to repoRoot. */
  dataRoot?: string;
  /** Read at spawn time: set after listen when it was 0. */
  getPort(): number;
  token: string;
  bins: EngineBins;
  /** Called when a job owned by a session changes (sessions.ts upserts the job card and delivers the result). */
  onSessionJob(job: JobRecord): void;
  /** At most this many jobs run at once; the rest wait with activity "in coda". Default 2. */
  maxConcurrent?: number;
  /** Wall-clock limit per job, from the moment it leaves the queue. Default 10 minutes. */
  jobTimeoutMs?: number;
}

const RETENTION_MS = 30 * 24 * 3600 * 1000;
/** The global feed and the list endpoint carry no result (a demo can be 500 KB); GET /api/jobs/:id has it. */
const slim = (rec: JobRecord): JobRecord => ({ ...rec, result: undefined });

interface Entry {
  rec: JobRecord;
  input: unknown;
  /** Engine at creation: the model shown on the card belongs to it. */
  provider: Provider;
  handle?: EngineHandle;
  /** Set by ctx.finish(); the job is "done" iff this exists when the process ends. */
  result?: JobResult;
  settled: boolean;
  stderr: string[];
  deadline?: NodeJS.Timeout;
}

const now = () => new Date().toISOString();
const trunc = (s: string, n: number) => (s.length > n ? s.slice(0, n - 1) + "…" : s);

/** "mcp__job__render_svg" -> "render_svg"; built-ins get a readable line. */
function activityOf(name: string, input: any): string {
  if (name === "WebSearch") return trunc(`cerca: ${input?.query ?? ""}`, 120);
  if (name === "WebFetch") return trunc(`legge: ${input?.url ?? ""}`, 120);
  return name.replace(/^mcp__job__/, "");
}

export class JobManager implements JobStarter {
  readonly roles = new Map<string, JobRole<any>>();
  private entries = new Map<string, Entry>();
  private queue: string[] = [];
  private closed = false;
  /** Spawned handles whose processes have not all exited yet (settling does not drop them: the exit hook must still kill them). */
  private live = new Set<EngineHandle>();

  constructor(public opts: JobManagerOptions) {
    for (const r of [researchRole, visualRole, demoRole]) this.register(r);
    this.load();
  }

  private get dir() {
    return path.join(this.opts.repoRoot, ".learner", "jobs");
  }

  register(role: JobRole<any>): void {
    this.roles.set(role.kind, role);
  }

  // ───────────── persistence ─────────────

  private load() {
    if (!fs.existsSync(this.dir)) return;
    for (const f of fs.readdirSync(this.dir)) {
      if (!f.endsWith(".json")) continue;
      try {
        const rec: JobRecord = JSON.parse(fs.readFileSync(path.join(this.dir, f), "utf-8"));
        if (rec.status !== "running" && Date.now() - Date.parse(rec.updatedAt) > RETENTION_MS) {
          fs.rmSync(path.join(this.dir, f), { force: true });
          fs.rmSync(path.join(this.dir, rec.id), { recursive: true, force: true });
          continue;
        }
        if (rec.status === "running") {
          rec.status = "error";
          rec.error = "interrotto alla chiusura dell'app";
          rec.activity = "";
          this.persist(rec);
        }
        this.entries.set(rec.id, { rec, input: undefined, provider: "claude", settled: true, stderr: [] });
      } catch {
        /* unreadable record: skip */
      }
    }
  }

  private persist(rec: JobRecord) {
    try {
      fs.mkdirSync(this.dir, { recursive: true });
      fs.writeFileSync(path.join(this.dir, `${rec.id}.json`), JSON.stringify(rec));
    } catch (e: any) {
      console.error("[jobs] save failed:", e.message);
    }
  }

  /** Every change: persist, tell the app (SSE), tell the owning session. */
  private changed(e: Entry) {
    e.rec.updatedAt = now();
    this.persist(e.rec);
    emitGlobal({ type: "job", job: slim(e.rec) });
    if (e.rec.sessionId && !this.closed) {
      try {
        this.opts.onSessionJob({ ...e.rec }); // the session card renders the full result
      } catch (err: any) {
        console.error("[jobs] onSessionJob failed:", err?.message ?? err);
      }
    }
  }

  // ───────────── JobStarter ─────────────

  list(): JobRecord[] {
    return [...this.entries.values()].map((e) => ({ ...e.rec })).sort((a, b) => b.createdAt.localeCompare(a.createdAt));
  }

  get(id: string): JobRecord | undefined {
    const e = this.entries.get(id);
    return e && { ...e.rec };
  }

  /** @internal used by the MCP route. */
  entry(id: string): Entry | undefined {
    return this.entries.get(id);
  }

  start<I>(kind: JobKind, input: I, opts: { title: string; sessionId?: string }): JobRecord {
    const role = this.roles.get(kind);
    if (!role) throw new Error(`Subagente sconosciuto: ${kind}`);
    if (this.closed) throw new Error("L'app si sta chiudendo");
    const t = now();
    const settings = readSettings(this.opts.repoRoot);
    const rec: JobRecord = {
      id: randomUUID(), kind, title: opts.title, sessionId: opts.sessionId,
      model: modelFor(settings, settings.provider, kind === "map" ? "map" : "subagent"), status: "running", activity: "in coda",
      createdAt: t, updatedAt: t, costUsd: 0,
    };
    const e: Entry = { rec, input, provider: settings.provider, settled: false, stderr: [] };
    this.entries.set(rec.id, e);
    this.queue.push(rec.id);
    this.changed(e);
    this.pump();
    return { ...e.rec };
  }

  cancel(id: string): void {
    const e = this.entries.get(id);
    if (!e || e.settled) return;
    this.queue = this.queue.filter((x) => x !== id);
    this.settle(e, "cancelled");
  }

  markDelivered(id: string): void {
    const e = this.entries.get(id);
    if (!e || e.rec.delivered) return;
    e.rec.delivered = true;
    e.rec.updatedAt = now();
    this.persist(e.rec);
    emitGlobal({ type: "job", job: slim(e.rec) });
  }

  livePids(): number[] {
    return [...this.live].flatMap((h) => (h.pid !== undefined ? [h.pid] : []));
  }

  async shutdown(): Promise<void> {
    this.closed = true;
    this.queue = [];
    const live = [...this.entries.values()].filter((e) => !e.settled);
    const handles = live.map((e) => e.handle);
    for (const e of live) {
      e.rec.error = "interrotto alla chiusura dell'app";
      this.settle(e, "error");
    }
    await Promise.all(handles.map((h) => h?.stop().catch(() => {})));
    await closeBrowser();
  }

  // ───────────── running ─────────────

  private pump() {
    const max = this.opts.maxConcurrent ?? 2;
    const running = () => [...this.entries.values()].filter((x) => !x.settled && x.handle);
    // Map jobs run one at a time, outside the pool: each extractor must see the previous lesson's propositions in its digest.
    const mapRunning = () => running().filter((x) => x.rec.kind === "map").length;
    for (;;) {
      if (this.closed) return;
      const pool = running().filter((x) => x.rec.kind !== "map").length;
      const i = this.queue.findIndex((id) => {
        const q = this.entries.get(id);
        if (!q || q.settled) return true; // stale: dropped below
        return q.rec.kind === "map" ? mapRunning() < 1 : pool < max;
      });
      if (i < 0) return;
      const e = this.entries.get(this.queue.splice(i, 1)[0]);
      if (!e || e.settled) continue;
      // Claim the slot synchronously (placeholder handle) so the next loop turn counts it.
      e.handle = { send() {}, interrupt() {}, async stop() {}, pid: undefined };
      e.deadline = setTimeout(() => {
        e.rec.error = "tempo scaduto";
        this.settle(e, "error");
      }, this.opts.jobTimeoutMs ?? 10 * 60_000);
      e.deadline.unref();
      void this.launch(e).catch((err) => {
        e.rec.error = `avvio non riuscito: ${err?.message ?? err}`;
        this.settle(e, "error");
      });
    }
  }

  private async launch(e: Entry) {
    const { rec } = e;
    const role = this.roles.get(rec.kind)!;
    const dir = path.join(this.dir, rec.id);
    fs.mkdirSync(dir, { recursive: true });
    const dataRoot = this.opts.dataRoot ?? this.opts.repoRoot;
    const systemPrompt = await role.systemPrompt(e.input, dataRoot);
    if (e.settled) return; // cancelled while preparing
    rec.activity = "avvio";
    this.changed(e);
    const h: EngineHandle = spawnEngine(
      {
        provider: e.provider,
        bin: this.opts.bins[e.provider],
        repoRoot: this.opts.repoRoot,
        dataRoot,
        scratchDir: dir,
        model: rec.model,
        effort: effortFor(readSettings(this.opts.repoRoot), e.provider, rec.kind === "map" ? "map" : "subagent"),
        systemPrompt: withEngineNote(systemPrompt, e.provider, role.grant),
        grant: role.grant,
        mcp: { name: "job", url: `http://127.0.0.1:${this.opts.getPort()}/mcp/job/${rec.id}`, token: this.opts.token },
        addDirs: [],
        maxTurns: role.maxTurns,
      },
      {
        onEvent: (ev) => this.onClaude(e, ev),
        onStderr: (line) => {
          e.stderr.push(line);
          if (e.stderr.length > 20) e.stderr.shift();
        },
        onExit: (code, signal) => {
          this.live.delete(h);
          if (e.settled) return;
          if (e.result) return this.settle(e, "done");
          if (CLAUDE_AUTH_ERROR_RE.test(e.stderr.join(" "))) {
            e.rec.error = CLAUDE_AUTH_ERROR_MESSAGE;
            return this.settle(e, "error");
          }
          const tail = e.stderr.slice(-3).join(" ");
          e.rec.error = `il subagente è terminato inaspettatamente (${signal ?? code})` + (tail ? `: ${tail}` : "");
          this.settle(e, "error");
        },
      },
    );
    e.handle = h;
    this.live.add(h);
    if (e.settled) return void h.stop(); // cancelled during spawn
    h.send(role.firstMessage(e.input));
  }

  private onClaude(e: Entry, ev: EngineEvent) {
    if (e.settled) return;
    if (ev.type === "auth-failed") {
      e.rec.error = ev.message || CLAUDE_AUTH_ERROR_MESSAGE;
      return this.settle(e, "error");
    }
    if (ev.type === "assistant") {
      const use = ev.toolUses.at(-1);
      if (use) {
        e.rec.activity = activityOf(use.name, use.input);
        this.changed(e);
      }
    } else if (ev.type === "result") {
      e.rec.costUsd += ev.costUsd;
      if (e.result) return this.settle(e, "done");
      e.rec.error = ev.isError
        ? `il subagente si è fermato (${ev.subtype})${ev.text ? ": " + trunc(ev.text, 300) : ""}`
        : "il subagente ha terminato senza consegnare";
      this.settle(e, "error");
    }
  }

  /** Idempotent terminal transition; stops the process. */
  private settle(e: Entry, status: "done" | "error" | "cancelled") {
    if (e.settled) return;
    e.settled = true;
    e.rec.status = status;
    e.rec.activity = "";
    clearTimeout(e.deadline);
    this.roles.get(e.rec.kind)?.cleanup?.(e.rec.id);
    if (status === "done") e.rec.result = e.result;
    if (status === "cancelled") e.rec.error = undefined;
    this.changed(e);
    const h = e.handle;
    e.handle = undefined;
    void h?.stop().catch(() => {});
    this.pump();
  }

  /** The tools' view of a job. */
  context(id: string): JobContext | undefined {
    const e = this.entries.get(id);
    if (!e) return undefined;
    return {
      repoRoot: this.opts.repoRoot,
      dataRoot: this.opts.dataRoot ?? this.opts.repoRoot,
      jobId: id,
      setActivity: (text) => {
        if (e.settled) return;
        e.rec.activity = trunc(text, 120);
        this.changed(e);
      },
      finish: (result) => {
        if (!e.settled) e.result = result;
      },
    };
  }
}

/** Routes: /api/jobs, /api/jobs/:id/cancel, /mcp/job/:id. */
export function mountJobs(app: Hono, jobs: JobManager): void {
  app.get("/api/jobs", (c) => c.json(jobs.list().map(slim)));
  app.get("/api/jobs/:id", (c) => {
    const j = jobs.get(c.req.param("id"));
    return j ? c.json(j) : c.json({ error: "Job not found" }, 404);
  });
  app.post("/api/jobs/:id/cancel", (c) => {
    const id = c.req.param("id");
    if (!jobs.get(id)) return c.json({ error: "Job not found" }, 404);
    jobs.cancel(id);
    return c.json({ ok: true });
  });
  // Registered here (not in main.ts) so it wins over /mcp/:id; the global middleware already checks x-learner-token.
  app.post("/mcp/job/:id", async (c) => {
    const id = c.req.param("id");
    const entry = jobs.entry(id);
    const role = entry && jobs.roles.get(entry.rec.kind);
    const ctx = jobs.context(id);
    if (!entry || !role || !ctx) return c.json({ error: "Job not found" }, 404);
    if (entry.rec.status !== "running") return c.json({ error: "Job is not running" }, 409);
    const out = await handleMcp(await c.req.json(), role.tools(entry.input), ctx, "job");
    return out === null ? c.body(null, 202) : c.json(out);
  });
}
