// The map extractor subagent + coverage ledger (graph/coverage.jsonl).
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";
import { z } from "zod";
import type { Hono } from "hono";
import { needsMapping, type GraphSource, type JobRecord } from "../../shared/protocol.ts";
import { emitGlobal, subscribeGlobal } from "../events.ts";
import { lessonRel, readLesson, readLessonHtml, type Lesson } from "../lesson.ts";
import type { JobManager } from "../jobs/index.ts";
import type { Findings, JobRole, JobStarter, JobTool, ToolOutcome } from "../types.ts";
import { isolatedBySource, nextRevision, readNodeLines } from "./store.ts";

/** The slice of graph/store.ts the extractor needs (injectable for tests; A4 owns the real module). */
export interface ExtractStore {
  graphDigest(repoRoot: string): Promise<string>;
  validateFindings(f: Findings, existingIds: Set<string>, mustConnect?: string[]): string[];
  mergeFindings(repoRoot: string, sourceId: string, f: Findings, afterWrite?: () => void): Promise<{ added: number; linked: number; chains: number }>;
}
const realStore = (): Promise<ExtractStore> => import("./store.ts");

const PROMPT = path.resolve(import.meta.dirname, "..", "..", "prompts", "subagents", "map.md");
const MAX_TEXT = 60_000;

export interface MapInput {
  /** GraphSource.id: `<course>/<lesson file stem>`. */
  source: string;
  /** Coverage fingerprint of the exact text the extractor was shown; filled when the job starts (firstMessage). */
  fp?: string;
  /** This lesson's propositions no chain uses, shown to the extractor at start; the submission must connect them. */
  isolated?: string[];
}

// ───────────── lesson text (the extractor input; its hash is the coverage fingerprint) ─────────────

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ",
  agrave: "à", egrave: "è", igrave: "ì", ograve: "ò", ugrave: "ù", eacute: "é", Agrave: "À", Egrave: "È", Eacute: "É", hellip: "…", mdash: "—", ndash: "–",
};
export function htmlToText(html: string): string {
  return html
    .replace(/<(script|style|svg|head)[\s\S]*?<\/\1>/gi, " ")
    .replace(/<br\s*\/?>|<\/(p|div|li|h[1-6]|tr|pre|blockquote|section|article)>/gi, "\n")
    .replace(/<[^>]+>/g, "")
    .replace(/&(#x?[0-9a-f]+|\w+);/gi, (m, e: string) => {
      if (e[0] === "#") {
        const cp = e[1].toLowerCase() === "x" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
        return cp > 0 && cp <= 0x10ffff ? String.fromCodePoint(cp) : m;
      }
      return ENTITIES[e] ?? ENTITIES[e.toLowerCase()] ?? m;
    })
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/** `<course>/<stem>` -> repo-relative lesson path; null when the id is not a plausible lesson id. */
export function sourceRel(source: string): string | null {
  const [course, stem, ...rest] = source.split("/");
  return !course || !stem || rest.length ? null : lessonRel(`courses/${course}/lessons/${stem}.html`);
}

export interface LessonText {
  title: string;
  course: string;
  lessonNumber: number;
  lessonPath: string; // repo-relative
  text: string;
}

// correctIndex is 1-based (see QuizCheckpointData in extensions/html-lesson.ts).
const correctAnswer = (q: { options?: string[]; correctAnswer?: string; correctIndex?: number }) =>
  htmlToText(q.correctAnswer ?? (q.options && q.correctIndex !== undefined ? (q.options[q.correctIndex - 1] ?? "") : ""));

export function readLessonText(repoRoot: string, source: string): LessonText | null {
  const rel = sourceRel(source);
  if (!rel) return null;
  const raw = readLessonHtml(repoRoot, rel);
  if (raw === null) return null;
  let doc: Lesson["doc"] | undefined;
  try {
    doc = readLesson(repoRoot, rel)?.doc;
  } catch {
    /* unreadable island: fall back to the visible text */
  }
  const [course, stem] = source.split("/");
  const title = doc?.metadata?.title || htmlToText(/<title>([\s\S]*?)<\/title>/i.exec(raw)?.[1] ?? "") || stem;
  const lessonNumber = doc?.metadata?.lessonNumber || Number(/^(\d+)-/.exec(stem)?.[1]) || 0;
  let text: string;
  if (doc?.nodes?.length) {
    text = doc.nodes
      .map((n) => {
        const parts = [`## Nodo ${n.nodeId}: ${n.title} (${n.tier})`, htmlToText(n.content ?? "")];
        // names, not ids: the fingerprint must stay put when a concept is deleted
        if (n.anchors?.length) parts.push(`Si appoggia su: ${n.anchors.map((a) => a.name).join("; ")}`);
        if (n.connection) parts.push(htmlToText(n.connection));
        if (n.quiz) parts.push(`Domanda di verifica: ${htmlToText(n.quiz.question)}`, `Risposta corretta: ${correctAnswer(n.quiz)}`);
        return parts.join("\n\n");
      })
      .join("\n\n");
  } else {
    text = htmlToText(/<body[^>]*>([\s\S]*)<\/body>/i.exec(raw)?.[1] ?? raw);
  }
  return { title, course, lessonNumber, lessonPath: rel, text: text.slice(0, MAX_TEXT) };
}

const fingerprint = (l: LessonText) => createHash("sha256").update(l.title + "\n" + l.text).digest("hex");

/** Lesson node id -> the anchors write_node stored on it. Missing/unreadable lesson: empty map. */
export function anchorsByNode(repoRoot: string, source: string): Map<string, { id: string; name: string }[]> {
  const out = new Map<string, { id: string; name: string }[]>();
  const rel = sourceRel(source);
  if (!rel) return out;
  let doc: Lesson["doc"] | undefined;
  try {
    doc = readLesson(repoRoot, rel)?.doc;
  } catch {
    return out;
  }
  for (const n of doc?.nodes ?? []) if (n.anchors?.length) out.set(n.nodeId, n.anchors);
  return out;
}

/** Turn each live declared anchor into a chain `anchor → the node's proposition`, so the arrows show the cross-lesson
 *  link even when the model forgot it. Dangling anchors (concept deleted) are skipped. Pure: `f` is not mutated. */
export function withAnchorChains(f: Findings, anchors: Map<string, { id: string; name: string }[]>, live: Set<string>): Findings {
  const chains = [...f.chains];
  for (const [nodeId, list] of anchors) {
    const p = f.propositions.find((x) => x.nodeId === nodeId);
    if (!p) continue; // the node was not extracted (or the model linked it elsewhere): nothing to attach to
    for (const a of list) {
      if (!live.has(a.id) || a.id === p.linkTo) continue;
      if (chains.some((c) => c.conclusion === p.id && c.premises.includes(a.id))) continue;
      chains.push({ premises: [a.id], conclusion: p.id });
    }
  }
  return { ...f, chains };
}

// ───────────── coverage ledger ─────────────

interface Coverage {
  source: string;
  fp: string;
  at: string;
}
const coverageFile = (repoRoot: string) => path.join(repoRoot, "graph", "coverage.jsonl");

export function readCoverage(repoRoot: string): Map<string, Coverage> {
  const m = new Map<string, Coverage>();
  let raw = "";
  try {
    raw = fs.readFileSync(coverageFile(repoRoot), "utf-8");
  } catch {
    return m;
  }
  for (const line of raw.split("\n")) {
    try {
      const c = JSON.parse(line) as Coverage;
      if (typeof c?.source === "string" && typeof c.fp === "string") m.set(c.source, c); // the last entry wins
    } catch {
      /* corrupt or blank line: skip */
    }
  }
  return m;
}

export function appendCoverage(repoRoot: string, source: string, fp: string): void {
  fs.mkdirSync(path.join(repoRoot, "graph"), { recursive: true });
  fs.appendFileSync(coverageFile(repoRoot), JSON.stringify({ source, fp, at: new Date().toISOString() } satisfies Coverage) + "\n");
}

/** Every lesson under courses/<course>/lessons/*.html with covered/stale. */
export function listSources(repoRoot: string): GraphSource[] {
  const cov = readCoverage(repoRoot);
  const isolated = isolatedBySource(repoRoot); // the ledger readers tolerate a missing graph/
  const out: GraphSource[] = [];
  const coursesDir = path.join(repoRoot, "courses");
  let courses: string[] = [];
  try {
    courses = fs.readdirSync(coursesDir).sort();
  } catch {
    return out;
  }
  for (const course of courses) {
    let files: string[] = [];
    try {
      files = fs.readdirSync(path.join(coursesDir, course, "lessons")).filter((f) => f.endsWith(".html")).sort();
    } catch {
      continue;
    }
    for (const f of files) {
      const id = `${course}/${f.replace(/\.html$/, "")}`;
      const lesson = readLessonText(repoRoot, id);
      if (!lesson) continue;
      const c = cov.get(id);
      out.push({
        id, course, lessonTitle: lesson.title, lessonNumber: lesson.lessonNumber, lessonPath: lesson.lessonPath,
        covered: !!c, stale: !!c && c.fp !== fingerprint(lesson), isolated: isolated.get(id)?.length ?? 0,
      });
    }
  }
  return out;
}

export const mapJobTitle = (lessonTitle: string) => `Mappa: ${lessonTitle}`;

/** jobId -> source of the map jobs still running, across every session. Cleared by the job's final event. */
const inflight = new Map<string, string>();
subscribeGlobal((e) => {
  if (e.type === "job" && e.job.kind === "map" && e.job.status !== "running") inflight.delete(e.job.id);
});

type MapStarter = { start(kind: "map", input: MapInput, opts: { title: string; sessionId?: string }): JobRecord };

/** Starts a map job for one lesson unless it is covered and unchanged, or already being mapped (by any job). Null = skipped. */
export function startMapJob(jobs: MapStarter, sources: GraphSource[], source: string, force = false): JobRecord | null {
  const s = sources.find((x) => x.id === source);
  if (!s || (!force && !needsMapping(s))) return null;
  if ([...inflight.values()].includes(source)) return null;
  const rec = jobs.start("map", { source }, { title: mapJobTitle(s.lessonTitle) });
  if (rec.status === "running") inflight.set(rec.id, source);
  return rec;
}

// ───────────── the role ─────────────

const findingsSchema = z.object({
  propositions: z
    .array(
      z.object({
        id: z.string().describe('Local id "p1", "p2", … unique in this submission.'),
        name: z.string().describe("Short handle (2-6 words)."),
        statement: z.string().describe("The full true statement, self-contained, one idea."),
        aliases: z.array(z.string()).optional().describe("Other ways of saying it."),
        nodeId: z.string().optional().describe("Lesson node id the idea comes from."),
        linkTo: z.string().optional().describe('Existing "n_…" id when this is the same idea: link instead of creating.'),
      })
    )
    .describe("New ideas, plus existing ones being linked via linkTo."),
  chains: z
    .array(z.object({ premises: z.array(z.string()).min(1), conclusion: z.string() }))
    .describe('Premises that jointly entail the conclusion. Ids: local "pN" or existing "n_…". [] when none.'),
  remove: z
    .array(z.string())
    .optional()
    .describe('Only for the isolated "n_…" ideas listed in the message: the ones that are not load-bearing leave the map.'),
});

const existingIds = (digest: string) => new Set([...digest.matchAll(/^\s*(n_[^\s|]+)\s*\|/gm)].map((m) => m[1]));

/** One accepted submission per job: a repeated submit_findings (model retry after a lost reply) must not merge twice. */
const submitted = new Map<string, Promise<ToolOutcome>>();

export function makeMapRole(repoRoot: string, store?: ExtractStore): JobRole<MapInput> {
  const st = async () => store ?? (await realStore());
  return {
    kind: "map",
    grant: [],
    maxTurns: 6,
    async systemPrompt(_input, root) {
      const digest = (await (await st()).graphDigest(root)).trim();
      return fs.readFileSync(PROMPT, "utf-8").trim() + "\n\n---\n\n# Il grafo finora\n\n" + (digest || "(vuoto: nessuna proposizione ancora)") + "\n";
    },
    firstMessage(input) {
      const l = readLessonText(repoRoot, input.source);
      if (!l) return `Lezione non trovata: ${input.source}. Consegna liste vuote.`;
      input.fp = fingerprint(l); // what he is shown is what coverage records: a later edit shows as stale
      const isolatedHere = isolatedBySource(repoRoot).get(input.source) ?? [];
      input.isolated = isolatedHere.map((n) => n.id);
      const live = new Set(readNodeLines(repoRoot).map((n) => n.id));
      const anchored = [...anchorsByNode(repoRoot, input.source)].flatMap(([nid, list]) =>
        list.filter((a) => live.has(a.id)).map((a) => `- nodo ${nid} → ${a.id} | ${a.name}`),
      );
      const anchorsBlock = anchored.length
        ? `\n\nAncore dichiarate dal tutor (concetti già nella mappa su cui il nodo costruisce):\n${anchored.join("\n")}`
        : "";
      const repair = isolatedHere.length
        ? `\n\nIdee di questa lezione già nella mappa ma isolate (nessuna catena le usa). Collegale ora, ciascuna in almeno una catena, citandole col loro id; se una non è davvero portante, mettila in remove:\n${isolatedHere.map((n) => `- ${n.id} | ${n.name}`).join("\n")}`
        : "";
      return `Lezione: ${l.title}\nCorso: ${l.course}\nSorgente: ${input.source}\n\n${l.text}${anchorsBlock}${repair}\n\nEstrai le proposizioni e le catene, poi chiama submit_findings.`;
    },
    cleanup(jobId) {
      submitted.delete(jobId);
      inflight.delete(jobId);
    },
    tools(input): JobTool<any>[] {
      const submit: JobTool<Findings> = {
        name: "submit_findings",
        description: "Deliver the extraction for this lesson (call once). Returns validation errors to fix and resubmit, or confirms the merge.",
        schema: findingsSchema as unknown as z.ZodType<Findings>,
        async run(f, ctx): Promise<ToolOutcome> {
          const s = await st();
          const live = existingIds(await s.graphDigest(ctx.dataRoot));
          const errors = s.validateFindings(f, live, input.isolated);
          if (errors.length) return { text: "Findings rejected, fix these and call submit_findings again:\n- " + errors.join("\n- "), isError: true };
          const prior = submitted.get(ctx.jobId);
          if (prior) return prior;
          const fp = input.fp ?? (() => { const l = readLessonText(ctx.dataRoot, input.source); return l ? fingerprint(l) : undefined; })();
          // synthesized chains use only live ids and submitted pN, so they stay valid without re-validating
          const g = withAnchorChains(f, anchorsByNode(ctx.dataRoot, input.source), live);
          const merge = (async (): Promise<ToolOutcome> => {
            const r = await s.mergeFindings(ctx.dataRoot, input.source, g, () => {
              try {
                if (fp) appendCoverage(ctx.dataRoot, input.source, fp);
              } catch {
                /* coverage is retried by the next extraction; the merge must not be reported as failed */
              }
            });
            emitGlobal({ type: "graph", revision: nextRevision() });
            ctx.finish({ kind: "map", source: input.source, ...r });
            return { text: `Merged: ${r.added} new, ${r.linked} linked, ${r.chains} chains. Done.` };
          })();
          submitted.set(ctx.jobId, merge);
          merge.catch(() => submitted.delete(ctx.jobId));
          return merge;
        },
      };
      return [submit];
    },
  };
}

/** /api/graph/sources, /api/graph/extract. Also registers the "map" role on `jobs`. */
export function mountExtract(app: Hono, deps: { repoRoot: string; dataRoot?: string; jobs: JobManager; store?: ExtractStore }): void {
  const root = deps.dataRoot ?? deps.repoRoot;
  const jobs: JobStarter = deps.jobs;
  deps.jobs.register(makeMapRole(root, deps.store));

  app.get("/api/graph/sources", (c) => c.json(listSources(root)));

  app.post("/api/graph/extract", async (c) => {
    const body = (await c.req.json().catch(() => ({}))) as { sources?: unknown };
    const all = listSources(root);
    let picked: GraphSource[];
    if (Array.isArray(body.sources)) {
      const ids = body.sources.filter((x): x is string => typeof x === "string");
      const bad = ids.filter((id) => !all.some((s) => s.id === id));
      if (bad.length) return c.json({ error: `unknown source: ${bad.join(", ")}` }, 400);
      picked = all.filter((s) => ids.includes(s.id));
    } else {
      picked = all.filter(needsMapping);
    }
    const started: JobRecord[] = [];
    for (const s of picked) {
      const rec = startMapJob(jobs, all, s.id, Array.isArray(body.sources));
      if (rec) started.push(rec);
    }
    return c.json({ jobs: started });
  });
}
