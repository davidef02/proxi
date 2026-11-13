// HTTP server: API routes, SSE, /files, /mcp, SPA. `createServer` is the test seam; `main()` runs only when executed directly.
import { randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { execFileSync, spawn } from "node:child_process";
import type { AddressInfo } from "node:net";
import { fileURLToPath, pathToFileURL } from "node:url";
import { serve } from "@hono/node-server";
import { Hono } from "hono";
import { readLimits } from "./limits.ts";
import { streamSSE } from "hono/streaming";
import { z } from "zod";
import { fetchAnkiStatus, fetchDeckStatus } from "../../extensions/homepage.ts";
import { courseDeckNameFor } from "../../extensions/anki-sync.ts";
import { DEFAULT_ANKI_URL, invoke } from "../../extensions/anki-client.ts";
import { DEFAULT_PORT, type AnkiImportResult, type AnkiStatusResponse, type AnswerBody, type LearnerSettings, type SettingsResponse } from "../shared/protocol.ts";
import { APP_NAME } from "../shared/identity.mjs";
import { CODE_ROOT, resolveDataRoot } from "../../extensions/data-root.ts";
import { listCourses, listStandaloneLessons } from "./courses.ts";
import { docRel, lessonRel, readDocBody, readLesson, readLessonHtml, readReferenceDocument } from "./lesson.ts";
import { CONFLICT, fileVersion, getWorkspace, listExercises, openInEditor, revealHint, runExercise, saveExerciseFile } from "./exercises.ts";
import { buildKnowledgeMap, purgeProgress } from "./knowledge.ts";
import { importAnkiReviews } from "./anki-import.ts";
import { handleMcp } from "./mcp.ts";
import { emitGlobal, subscribeGlobal } from "./events.ts";
import { maskApiKey, readSettings, writeSettings } from "./settings.ts";
import { transcribeAudio } from "./stt.ts";
import { JobManager, mountJobs } from "./jobs/index.ts";
import { mountSync, SyncService } from "./sync/service.ts";
import type { SecretCipher } from "./sync/secrets.ts";
import { refreshBoard } from "./tools/board.ts";
import { mountGraph } from "./graph/routes.ts";
import { ensureModel, warmIndex } from "./graph/embeddings.ts";
import { readSnapshot } from "./graph/store.ts";
import { mountExtract } from "./graph/extract.ts";
import { callRequests, HttpError, SessionManager } from "./sessions.ts";
import { toolsFor } from "./tools/index.ts";
import { listAgyModels } from "./agy.ts";
import { listOpencodeModels } from "./opencode.ts";
import { locateClaudeBinary, probeClaudeCli } from "./claude.ts";
import type { EngineBins } from "./engine.ts";
import { applyUpdate, checkUpdate, countActiveWork, getStatus } from "./update.ts";

export interface ServerOptions {
  repoRoot: string;
  /** Data folder (courses, graph, decks, curriculum.json). Default: resolveDataRoot(repoRoot). */
  dataRoot?: string;
  /** Desktop: Electron safeStorage. Absent = plaintext secrets with a UI warning. */
  secretCipher?: SecretCipher;
  /** Tests: fake network for the sync providers. */
  fetchFn?: typeof fetch;
  /** 0 = pick a free port (tests). */
  port: number;
  claudeBin?: string;
  claudeBinArgs?: string[];
  agyBin?: string;
  agyBinArgs?: string[];
  opencodeBin?: string;
  opencodeBinArgs?: string[];
  addDirs?: string[];
  openBrowser?: boolean;
  relaunch?: () => Promise<void> | void;
  checkUpdateOnStart?: boolean;
  /** Download the embedding model in the background when missing. Default: only for the real code repo. */
  fetchModelOnStart?: boolean;
  fetchModel?: (repoRoot: string) => Promise<unknown>;
  gitBin?: string;
  npmBin?: string;
}

export interface RunningServer {
  url: string;
  port: number;
  token: string;
  manager: SessionManager;
  jobs: JobManager;
  close(): Promise<void>;
}

export { locateClaudeBinary };

const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8", ".css": "text/css; charset=utf-8", ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8", ".json": "application/json; charset=utf-8", ".svg": "image/svg+xml",
  ".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".gif": "image/gif", ".webp": "image/webp",
  ".ico": "image/x-icon", ".woff": "font/woff", ".woff2": "font/woff2", ".ttf": "font/ttf", ".md": "text/markdown; charset=utf-8",
  ".txt": "text/plain; charset=utf-8", ".map": "application/json",
};

const SAFE_NAME = /^[A-Za-z0-9._-]+$/;
const courseName = z.string().regex(/^[a-z0-9]+(?:-[a-z0-9]+)*$/);
const attachmentMetaSchema = z.object({
  id: z.string(),
  name: z.string(),
  path: z.string(),
  mimeType: z.string(),
  size: z.number(),
});
const textBody = z.object({
  text: z.string().min(1).max(20000),
  attachments: z.array(attachmentMetaSchema).optional(),
});
const quizAnswer = z.object({
  selected: z.array(z.string()), dontKnow: z.boolean(),
  confidence: z.number().min(0).max(100).optional(), note: z.string().max(5000).optional(),
});
const decision = z.object({ approved: z.boolean(), feedback: z.string().max(5000).optional() });
const answerBody = z.discriminatedUnion("kind", [
  z.object({ requestId: z.string(), kind: z.literal("quiz"), answer: quizAnswer }),
  z.object({ requestId: z.string(), kind: z.literal("ask"), answer: z.object({ selected: z.array(z.string()), other: z.string().max(5000).optional() }) }),
  z.object({ requestId: z.string(), kind: z.literal("plan"), answer: decision }),
  z.object({ requestId: z.string(), kind: z.literal("curriculum"), answer: decision }),
]);
const exerciseSave = z.object({ course: courseName, name: z.string().regex(SAFE_NAME), path: z.string().min(1).max(200), content: z.string().max(200_000), baseVersion: z.string().max(64).optional() });
const exerciseRef = z.object({ course: courseName, name: z.string().regex(SAFE_NAME), sessionId: z.string().optional() });

/** Resolves `rel` under one of `roots`; null when it escapes or is not a file. */
function resolveInside(roots: string[], rel: string): string | null {
  if (rel.includes("\0")) return null;
  for (const root of roots) {
    const abs = path.resolve(root, rel);
    if (abs.startsWith(root + path.sep) && fs.existsSync(abs) && fs.statSync(abs).isFile()) return abs;
  }
  return null;
}

function fileResponse(abs: string, hardened = false): Response {
  const type = MIME[path.extname(abs).toLowerCase()] ?? "application/octet-stream";
  const headers: Record<string, string> = { "content-type": type };
  if (hardened) {
    headers["x-content-type-options"] = "nosniff";
    // Lesson HTML is LLM-authored and may run script: sandbox it into an opaque origin so it cannot call /api (same-origin otherwise).
    if (type.startsWith("text/html")) headers["content-security-policy"] = "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox; frame-ancestors 'self'";
    // Fonts/modules requested from the opaque origin are cross-origin and need CORS (static read-only assets).
    if (/^(font\/|text\/javascript|text\/css)/.test(type)) headers["access-control-allow-origin"] = "*";
  }
  if (hardened && type.startsWith("text/html")) {
    // An opaque origin has no localStorage (it throws): lessons use it for the theme toggle, so give them an in-memory one.
    const html = fs.readFileSync(abs, "utf-8");
    return new Response(/<head[^>]*>/i.test(html) ? html.replace(/<head[^>]*>/i, (m) => m + STORAGE_SHIM) : html.replace(/^\s*(<!doctype[^>]*>)?/i, (m) => m + STORAGE_SHIM), { headers });
  }
  return new Response(fs.readFileSync(abs), { headers });
}

const STORAGE_SHIM =
  "<script>try{localStorage.getItem('_')}catch(e){var m={},s={getItem:function(k){return k in m?m[k]:null},setItem:function(k,v){m[k]=String(v)},removeItem:function(k){delete m[k]},clear:function(){m={}}};" +
  "['localStorage','sessionStorage'].forEach(function(n){try{Object.defineProperty(window,n,{value:s,configurable:true})}catch(e){}})}</script>";
/** Synchronous best-effort kill of claude process trees (exit hook: nothing async can run there). */
export function killTreesSync(pids: number[]) {
  for (const pid of pids) {
    try {
      if (process.platform === "win32") execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
      else process.kill(pid, "SIGTERM");
    } catch {
      /* already gone */
    }
  }
}

export async function createServer(opts: ServerOptions): Promise<RunningServer> {
  const repoRoot = path.resolve(opts.repoRoot);
  const resolved = opts.dataRoot ? { dataRoot: path.resolve(opts.dataRoot), source: "option" as const } : resolveDataRoot(repoRoot);
  const dataRoot = resolved.dataRoot;
  purgeProgress(dataRoot);
  const token = randomBytes(24).toString("hex");
  let port = opts.port;
  let manager!: SessionManager;
  const bins: EngineBins = {
    claude: { cmd: opts.claudeBin ?? locateClaudeBinary() ?? "claude", args: opts.claudeBinArgs },
    agy: { cmd: opts.agyBin ?? "agy", args: opts.agyBinArgs },
    opencode: { cmd: opts.opencodeBin ?? "opencode", args: opts.opencodeBinArgs },
  };
  const jobs = new JobManager({ repoRoot, dataRoot, getPort: () => port, token, bins, onSessionJob: (job) => manager.onJob(job) });
  manager = new SessionManager({ repoRoot, dataRoot, port: opts.port, token, bins, addDirs: opts.addDirs, jobs });
  const distDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../web/dist");
  const fileRoots = [path.join(dataRoot, "courses"), path.join(repoRoot, "assets")];

  const app = new Hono();

  app.onError((err, c) => {
    if (err instanceof HttpError) return c.json({ error: err.message, ...(err.code ? { code: err.code } : {}) }, err.status as any);
    if (err instanceof z.ZodError) return c.json({ error: err.issues.map((i) => i.path.join(".") + ": " + i.message).join("; ") }, 400);
    if (err instanceof SyntaxError) return c.json({ error: "Invalid JSON body" }, 400);
    console.error("[server]", err);
    return c.json({ error: err.message || "Internal error" }, 500);
  });
  app.notFound((c) => c.json({ error: "Not found" }, 404));

  // Guards. DNS rebinding: only our own Host. CSRF: a custom header a cross-site form cannot set.
  app.use("*", async (c, next) => {
    const host = c.req.header("host");
    if (host !== `127.0.0.1:${port}` && host !== `localhost:${port}`) return c.json({ error: "Forbidden host" }, 403);
    const p = c.req.path;
    if (c.req.method !== "GET" && c.req.method !== "HEAD" && p.startsWith("/api/") && c.req.header("x-learner-client") !== "1")
      return c.json({ error: "Missing x-learner-client header" }, 403);
    if (p.startsWith("/mcp/") && c.req.header("x-learner-token") !== token) return c.json({ error: "Unauthorized" }, 401);
    await next();
  });

  const json = async <T extends z.ZodType>(c: any, schema: T): Promise<z.infer<T>> => schema.parse(await c.req.json());

  // ───────────── read routes ─────────────
  app.get("/api/courses", async (c) => c.json(await listCourses(dataRoot)));
  app.get("/api/standalone-lessons", async (c) => c.json(await listStandaloneLessons(dataRoot)));
  app.get("/api/map", async (c) => c.json(await buildKnowledgeMap(dataRoot)));
  app.get("/api/anki", async (c) => {
    const course = c.req.query("course");
    if (course !== undefined) {
      if (!courseName.safeParse(course).success) {
        return c.json({ error: "Corso non valido" }, 400);
      }
      const ankiUrl = process.env.LEARNER_ANKI_URL || undefined;
      const deck = courseDeckNameFor(dataRoot, course);
      try {
        const r = await fetchDeckStatus(deck, { ankiUrl });
        if (r.error || !r.status) {
          return c.json<AnkiStatusResponse>({ online: false, error: r.error, deck });
        }
        return c.json<AnkiStatusResponse>({
          online: true,
          due: r.status.due,
          newCards: r.status.newCards,
          learning: r.status.learning,
          total: r.status.total,
          deck,
          deckExists: r.status.exists,
        });
      } catch (e: any) {
        return c.json<AnkiStatusResponse>({ online: false, error: e.message, deck });
      }
    }
    let out: AnkiStatusResponse;
    try {
      const r = await fetchAnkiStatus({ ankiUrl: process.env.LEARNER_ANKI_URL || undefined, baseDir: dataRoot });
      out = r.status
        ? { online: true, due: r.status.due, newCards: r.status.newCards, learning: r.status.learning, total: r.status.total, byCourse: r.status.byCourse }
        : { online: false, error: r.error };
    } catch (e: any) {
      out = { online: false, error: e.message };
    }
    return c.json(out);
  });
  app.post("/api/anki/review", async (c) => {
    const { course } = await json(c, z.object({ course: courseName }));
    const url = process.env.LEARNER_ANKI_URL || DEFAULT_ANKI_URL;
    const deck = courseDeckNameFor(dataRoot, course);
    const r = await fetchDeckStatus(deck, { ankiUrl: url });
    if (r.error) {
      return c.json({ error: r.error }, 502);
    }
    if (!r.status?.exists) {
      return c.json({ error: "Nessun mazzo Anki per questo corso" }, 404);
    }
    const inv = await invoke(url, "guiDeckReview", { name: deck });
    if (inv.error) {
      return c.json({ error: inv.error }, 502);
    }
    return c.json({ ok: true });
  });
  app.post("/api/anki/import", async (c) => {
    const { course } = await json(c, z.object({ course: courseName }));
    return c.json<AnkiImportResult>(await importAnkiReviews(dataRoot, course));
  });

  // ───────────── app-wide events + settings ─────────────
  app.get("/api/events", (c) =>
    streamSSE(c, async (stream) => {
      const unsub = subscribeGlobal((e) => void stream.write(`data: ${JSON.stringify(e)}\n\n`).catch(() => {}));
      const beat = setInterval(() => void stream.write(": keep-alive\n\n").catch(() => {}), 15_000);
      await new Promise<void>((resolve) => stream.onAbort(resolve));
      clearInterval(beat);
      unsub();
    }),
  );
  app.get("/api/limits", (c) => {
    const s = readSettings(repoRoot);
    if (s.provider === "agy" || s.provider === "opencode" || s.claudeAuth === "apiKey") return c.json(null);
    return c.json(readLimits(repoRoot));
  });
  app.get("/api/agy/models", async (c) => c.json(await listAgyModels(bins.agy)));
  app.get("/api/opencode/models", async (c) => c.json(await listOpencodeModels(bins.opencode)));
  app.get("/api/claude/status", async (c) => c.json(await probeClaudeCli({ binPath: bins.claude.cmd })));
  const formatSettings = (s: LearnerSettings): SettingsResponse => ({
    ...s,
    anthropicApiKey: maskApiKey(s.anthropicApiKey),
    repoRoot,
    dataRoot,
  });
  app.get("/api/settings", (c) => c.json(formatSettings(readSettings(repoRoot))));
  app.post("/api/settings", async (c) => {
    const s = writeSettings(repoRoot, await c.req.json());
    if (s.provider === "agy" || s.provider === "opencode" || s.claudeAuth === "apiKey") {
      emitGlobal({ type: "limits", limits: { at: new Date().toISOString() } });
    }
    return c.json(formatSettings(s));
  });
  app.post("/api/transcribe", async (c) => {
    try {
      const fd = await c.req.formData();
      const audio = fd.get("audio");
      if (!audio || !(audio instanceof Blob)) {
        return c.json({ error: "File audio mancante" }, 400);
      }
      const settings = readSettings(repoRoot);
      const res = await transcribeAudio(audio, settings, opts.fetchFn);
      return c.json(res);
    } catch (e: any) {
      return c.json({ error: e?.message || "Errore di trascrizione" }, 400);
    }
  });
  const sync = new SyncService({
    repoRoot,
    dataRoot,
    dataRootFromEnv: resolved.source === "env",
    getPort: () => port,
    cipher: opts.secretCipher,
    fetch: opts.fetchFn,
    relaunch: opts.relaunch,
    onStatus: (s) => emitGlobal({ type: "sync", status: s }),
    onPulled: () => {
      void refreshBoard(dataRoot);
      emitGlobal({ type: "courses" });
    },
  });
  mountSync(app, sync);
  const stopSync = sync.startAuto();

  // ───────────── update ─────────────
  app.get("/api/update", (c) => c.json({ ...getStatus(), activeSessions: countActiveWork(manager, jobs) }));
  app.post("/api/update/check", async (c) => {
    const status = await checkUpdate(repoRoot, { gitBin: opts.gitBin });
    return c.json({ ...status, activeSessions: countActiveWork(manager, jobs) });
  });
  app.post("/api/update/apply", async (c) => {
    const body = await c.req.json().catch(() => ({}));
    const force = typeof body === "object" && body !== null && Boolean((body as any).force);
    const result = await applyUpdate(repoRoot, {
      force,
      manager,
      jobs,
      relaunch: opts.relaunch,
      gitBin: opts.gitBin,
      npmBin: opts.npmBin,
    });
    return c.json(result);
  });

  // ───────────── background jobs + map ─────────────
  mountJobs(app, jobs);
  mountGraph(app, { repoRoot, dataRoot, jobs });
  mountExtract(app, { repoRoot, dataRoot, jobs });

  // ───────────── sessions ─────────────
  app.get("/api/sessions", (c) => c.json(manager.list()));
  app.post("/api/sessions", async (c) => c.json({ id: await manager.create(await c.req.json()) }));
  app.get("/api/sessions/:id", (c) => {
    const s = manager.get(c.req.param("id"));
    if (!s) throw new HttpError(404, "Session not found");
    return c.json(s);
  });
  app.get("/api/sessions/:id/events", (c) => {
    const id = c.req.param("id");
    if (!manager.get(id)) throw new HttpError(404, "Session not found");
    return streamSSE(c, async (stream) => {
      const send = (e: unknown) => void stream.write(`data: ${JSON.stringify(e)}\n\n`).catch(() => {});
      const unsub = manager.subscribe(id, send);
      const beat = setInterval(() => void stream.write(": keep-alive\n\n").catch(() => {}), 15_000);
      await new Promise<void>((resolve) => stream.onAbort(resolve));
      clearInterval(beat);
      unsub();
    });
  });
  app.post("/api/sessions/:id/attachments", async (c) => {
    const id = c.req.param("id");
    const fd = await c.req.formData();
    const rawFiles = [...fd.getAll("files"), ...fd.getAll("file")];
    const files = rawFiles.filter((f): f is File => typeof f === "object" && f !== null && "arrayBuffer" in f && "name" in f);
    if (files.length === 0) {
      return c.json({ error: "Nessun file caricato" }, 400);
    }
    const metas = await manager.saveAttachments(id, files);
    return c.json(metas);
  });
  app.post("/api/sessions/:id/message", async (c) => {
    const body = await json(c, textBody);
    await manager.message(c.req.param("id"), body.text, body.attachments);
    return c.json({ ok: true });
  });
  app.post("/api/sessions/:id/retry", async (c) => {
    await manager.retry(c.req.param("id"));
    return c.json({ ok: true });
  });
  app.post("/api/sessions/:id/answer", async (c) => {
    manager.answer(c.req.param("id"), (await json(c, answerBody)) as AnswerBody);
    return c.json({ ok: true });
  });
  app.post("/api/sessions/:id/interrupt", (c) => {
    manager.interrupt(c.req.param("id"));
    return c.json({ ok: true });
  });
  app.post("/api/sessions/:id/close", async (c) => {
    await manager.close(c.req.param("id"));
    return c.json({ ok: true });
  });
  app.delete("/api/sessions/:id", async (c) => {
    await manager.delete(c.req.param("id"));
    return c.json({ ok: true });
  });

  // ───────────── exercises ─────────────
  const exerciseDir = (course: string, name: string) => {
    const dir = path.join(dataRoot, "courses", course, "exercises", name);
    if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) throw new HttpError(404, "Esercizio non trovato");
    return dir;
  };
  // exercise edits must reach sync (it listens to "courses"), but autosave fires per keystroke: at most once per 10 s per exercise
  const lastTouch = new Map<string, number>();
  const touchCourses = (key: string) => {
    const now = Date.now();
    if (now - (lastTouch.get(key) ?? 0) < 10_000) return;
    lastTouch.set(key, now);
    emitGlobal({ type: "courses" });
  };
  app.post("/api/exercises/run", async (c) => {
    const { course, name, sessionId } = await json(c, exerciseRef);
    exerciseDir(course, name);
    const result = await runExercise(dataRoot, course, name);
    if (sessionId) await manager.reportExercise(sessionId, course, name, result);
    return c.json(result);
  });
  app.post("/api/exercises/open", async (c) => {
    const { course, name } = await json(c, exerciseRef);
    openInEditor(exerciseDir(course, name));
    return c.json({ ok: true });
  });
  app.get("/api/exercises/:course/:name", async (c) => {
    const ws = await getWorkspace(dataRoot, courseName.parse(c.req.param("course")), c.req.param("name"));
    if (!ws) throw new HttpError(404, "Esercizio non trovato");
    return c.json(ws);
  });
  app.post("/api/exercises/save", async (c) => {
    if (Number(c.req.header("content-length") ?? 0) > 600_000) throw new HttpError(413, "File troppo grande");
    const { course, name, path: rel, content, baseVersion } = await json(c, exerciseSave);
    const dir = exerciseDir(course, name);
    const err = saveExerciseFile(dataRoot, course, name, rel, content, baseVersion);
    if (err) throw err === CONFLICT ? new HttpError(409, err, "stale") : new HttpError(400, err);
    touchCourses(course + "/" + name);
    return c.json({ ok: true, version: fileVersion(path.join(dir, rel)) });
  });
  app.post("/api/exercises/hint", async (c) => {
    const { course, name, sessionId } = await json(c, exerciseRef);
    exerciseDir(course, name);
    const info = await revealHint(dataRoot, course, name);
    if (!info) throw new HttpError(404, "Nessun suggerimento per questo esercizio");
    touchCourses(course + "/" + name);
    if (sessionId) await manager.reportHint(sessionId, info);
    return c.json(info);
  });
  app.get("/api/exercises/:course", async (c) => c.json(await listExercises(dataRoot, courseName.parse(c.req.param("course")))));

  // ───────────── MCP (the claude process) ─────────────
  // /mcp/job/:id is mounted by mountJobs above (registered first, so it wins over /mcp/:id).
  app.post("/mcp/:id", async (c) => {
    const id = c.req.param("id");
    const ctx = manager.getContext(id);
    if (!ctx) throw new HttpError(404, "Session not found");
    // The claude side gave up (aborted/closed connection): its pending card can never be answered.
    const signal = c.req.raw.signal;
    const mine = new Set<string>(); // requests created during THIS http call
    const onAbort = () => manager.cancelRequests(id, mine);
    signal.addEventListener("abort", onAbort);
    let out;
    try {
      const body = await c.req.json();
      out = await callRequests.run(mine, () => handleMcp(body, toolsFor(ctx.config, dataRoot), ctx));
    } finally {
      signal.removeEventListener("abort", onAbort);
    }
    return out === null ? c.body(null, 202) : c.json(out);
  });

  // ───────────── lesson reader data ─────────────
  app.get("/api/lesson", (c) => {
    const rel = lessonRel(c.req.query("path") ?? "");
    if (!rel) throw new HttpError(400, "Percorso della lezione non valido");
    let lesson;
    try {
      lesson = readLesson(dataRoot, rel);
    } catch (e) {
      throw new HttpError(422, (e as Error).message);
    }
    if (!lesson) {
      if (readLessonHtml(dataRoot, rel) === null) throw new HttpError(404, "Lezione non trovata");
      throw new HttpError(422, "La lezione non ha dati strutturati");
    }
    return c.json(lesson.doc);
  });

  // A lesson or reference sheet as markup for the in-app reader (the client sanitizes it).
  app.get("/api/doc", (c) => {
    const rel = docRel(c.req.query("path") ?? "");
    if (!rel) throw new HttpError(400, "Percorso del documento non valido");
    const doc = readDocBody(dataRoot, rel);
    if (!doc) throw new HttpError(404, "Documento non trovato");
    return c.json(doc);
  });

  // A structured reference sheet (Sintesi) for the in-app reader
  app.get("/api/reference", (c) => {
    const rel = docRel(c.req.query("path") ?? "");
    if (!rel) throw new HttpError(400, "Percorso del documento non valido");
    const ref = readReferenceDocument(dataRoot, rel);
    if (!ref) throw new HttpError(404, "Sintesi non trovata");
    return c.json(ref);
  });

  // ───────────── files + SPA ─────────────
  app.get("/files/*", (c) => {
    let rel: string;
    try {
      rel = decodeURIComponent(c.req.path.slice("/files/".length));
    } catch {
      throw new HttpError(400, "Bad request");
    }
    // courses/ lives in the data folder, assets/ in the repo. Require the result to sit inside one of those roots.
    const target = rel.startsWith("courses/") ? path.resolve(dataRoot, rel) : path.resolve(repoRoot, rel);
    const ok = !rel.includes("\0") && fileRoots.some((r) => target.startsWith(r + path.sep)) && fs.existsSync(target) && fs.statSync(target).isFile();
    if (!ok) throw new HttpError(404, "Not found");
    // A symlink inside courses/ or assets/ must not lead outside them. The roots are resolved per request: courses/
    // may not exist yet when the server starts (a fresh PC), and a junction in dataRoot must still resolve.
    const real = fs.realpathSync(target);
    if (!fileRoots.some((r) => real.startsWith((fs.existsSync(r) ? fs.realpathSync(r) : r) + path.sep))) throw new HttpError(404, "Not found");
    return fileResponse(real, true);
  });
  app.get("*", (c) => {
    if (!fs.existsSync(path.join(distDir, "index.html")))
      return c.text(`${APP_NAME}: la UI non è stata compilata. Esegui \`npm run app:build\` e riapri questa pagina.`, 200);
    let rel: string;
    try {
      rel = decodeURIComponent(c.req.path).replace(/^\/+/, "");
    } catch {
      return c.json({ error: "Bad request" }, 400);
    }
    const abs = rel && resolveInside([distDir], rel);
    return fileResponse(abs || path.join(distDir, "index.html"));
  });

  // ───────────── listen ─────────────
  const server = serve({ fetch: app.fetch, port: opts.port, hostname: "127.0.0.1" });
  // Question cards and long-running MCP tool calls wait indefinitely for learner input:
  // disable Node socket/request timeouts so the connection never aborts on its own.
  const httpSrv = server as unknown as import("node:http").Server;
  httpSrv.timeout = 0;
  httpSrv.requestTimeout = 0;
  httpSrv.headersTimeout = 0;
  httpSrv.keepAliveTimeout = 120_000;
  await new Promise<void>((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  port = (server.address() as AddressInfo).port;
  manager.opts.port = port;
  const url = `http://127.0.0.1:${port}`;
  if (opts.openBrowser) openUrl(url);

  const updateCheck = opts.checkUpdateOnStart !== false ? checkUpdate(repoRoot, { gitBin: opts.gitBin }).catch(() => {}) : null;

  // Embedding model + index warming: only for the real repo, so the dozens of test servers never download.
  const fetchModelOnStart = opts.fetchModelOnStart ?? repoRoot === path.resolve(CODE_ROOT);
  if (fetchModelOnStart) void (opts.fetchModel ?? ensureModel)(repoRoot).catch(() => {});
  let warming = false;
  let warmAgain = false;
  const warm = () => {
    warming = true;
    void readSnapshot(dataRoot)
      .then((s) => warmIndex(repoRoot, s.propositions))
      .catch(() => {})
      .finally(() => {
        warming = false;
        if (warmAgain) {
          warmAgain = false;
          warm();
        }
      });
  };
  const stopWarm = fetchModelOnStart
    ? subscribeGlobal((e) => {
        if (e.type !== "graph") return;
        if (warming) warmAgain = true; // one warm at a time, at most one queued
        else warm();
      })
    : null;

  return {
    url, port, token, manager, jobs,
    async close() {
      stopWarm?.();
      if (updateCheck) await updateCheck;
      await Promise.all([manager.shutdown(), jobs.shutdown()]);
      await stopSync(); // last sync after the sessions wrote their final state
      await new Promise<void>((r) => {
        server.close(() => r());
        (server as any).closeAllConnections?.();
      });
    },
  };
}

function openUrl(url: string) {
  const [cmd, args, shell] =
    process.platform === "win32" ? ["cmd", ["/c", "start", '""', url], false]
    : process.platform === "darwin" ? ["open", [url], false]
    : ["xdg-open", [url], false];
  try {
    const p = spawn(cmd as string, args as string[], { stdio: "ignore", detached: true, windowsHide: true, shell: shell as boolean });
    p.on("error", () => {});
    p.unref();
  } catch {
    /* no browser: the URL is printed */
  }
}

async function main() {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const env = process.env;
  const srv = await createServer({
    repoRoot: env.LEARNER_REPO_ROOT || path.resolve(here, "../.."),
    port: Number(env.LEARNER_PORT) || DEFAULT_PORT,
    claudeBin: env.LEARNER_CLAUDE_BIN || undefined,
    claudeBinArgs: env.LEARNER_CLAUDE_BIN_ARGS ? JSON.parse(env.LEARNER_CLAUDE_BIN_ARGS) : undefined,
    agyBin: env.LEARNER_AGY_BIN || undefined,
    agyBinArgs: env.LEARNER_AGY_BIN_ARGS ? JSON.parse(env.LEARNER_AGY_BIN_ARGS) : undefined,
    addDirs: env.LEARNER_ADD_DIRS ? JSON.parse(env.LEARNER_ADD_DIRS) : undefined,
    openBrowser: env.LEARNER_NO_OPEN !== "1",
  });
  console.log(`${APP_NAME}: ${srv.url}`);
  let stopping = false;
  const stop = async (code = 0) => {
    if (stopping) return;
    stopping = true;
    await srv.close().catch(() => {});
    process.exit(code);
  };
  for (const sig of ["SIGINT", "SIGTERM", "SIGBREAK", "SIGHUP"] as const) process.on(sig, () => {
      if (stopping && (sig === "SIGINT" || sig === "SIGBREAK")) process.exit(130); // second Ctrl-C: do not wait
      void stop();
    });
  process.on("uncaughtException", (e) => {
    console.error("[server] uncaughtException", e);
    void stop(1);
  });
  // Last resort (also runs after process.exit): no claude process may outlive us.
  process.on("exit", () => killTreesSync([...srv.manager.livePids(), ...srv.jobs.livePids()]));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((e) => {
    console.error(e);
    process.exit(1);
  });
}
