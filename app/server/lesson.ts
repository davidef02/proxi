// The Lesson-artifact module: the one owner of "a lesson on disk" (courses/<course>/lessons/NNNN-<slug>.html,
// a JSON data island codec'd by extensions/html-lesson.ts). Everything the app server asks about a lesson
// goes through this interface: the path rule, reading (parsed once, with its closing record), status,
// plan writes, and the "courses changed" signal. Callers never touch the HTML or the path rule themselves.
import * as fs from "node:fs";
import * as path from "node:path";
import { scanLessonDocs, summarizeLesson, type LessonSummary } from "../../extensions/homepage.ts";
import { parseLessonDocument, parseReferenceDocument, updateLessonMetadata, buildSintesiDocument, type FullLessonDocument, type SintesiDocument } from "../../extensions/html-lesson.ts";
import { emitGlobal } from "./events.ts";
import { parseDagIds } from "./planstate.ts";
import { refreshBoard } from "./tools/board.ts";

// ───────────── path rule ─────────────

const LESSON_REL = /^courses\/[\w-]+\/lessons\/[\w.-]+\.html$/;

/** The repo-relative, forward-slash lesson path, or null when it is not `courses/<course>/lessons/<file>.html` (traversal included). */
export function lessonRel(rel: string): string | null {
  const r = rel.replace(/\\/g, "/");
  return LESSON_REL.test(r) && !r.includes("..") ? r : null;
}

/** Absolute path of a repo-relative lesson path; throws on a path that breaks the rule. */
export function lessonAbs(repoRoot: string, rel: string): string {
  const r = lessonRel(rel);
  if (!r) throw new Error(`Bad lesson path "${rel}": expected courses/<course>/lessons/<file>.html.`);
  return path.resolve(repoRoot, r);
}

/** A lesson or a reference sheet: `courses/<course>/(lessons|reference)/<file>.html`, what the in-app document reader may open. */
const DOC_REL = /^courses\/[\w-]+\/(?:lessons|reference)\/[\w.-]+\.html$/;
export function docRel(rel: string): string | null {
  const r = rel.replace(/\\/g, "/");
  return DOC_REL.test(r) && !r.includes("..") ? r : null;
}

/** Title and body markup of a lesson/reference file: scripts, styles and links stripped, inner HTML of <main> or <body> (the client sanitizes again). Null when missing. */
export function readDocBody(repoRoot: string, rel: string): { title?: string; html: string } | null {
  const r = docRel(rel);
  const abs = r && path.resolve(repoRoot, r);
  if (!abs || !fs.existsSync(abs) || !fs.statSync(abs).isFile()) return null;
  const src = fs.readFileSync(abs, "utf-8");
  const inner = (tag: string) => new RegExp(`<${tag}\\b[^>]*>([\\s\\S]*)</${tag}>`, "i").exec(src)?.[1];
  const html = (inner("main") ?? inner("body") ?? src).replace(/<(script|style)\b[\s\S]*?<\/\1\s*>/gi, "").replace(/<link\b[^>]*>/gi, "");
  return { title: /<title>([\s\S]*?)<\/title>/i.exec(src)?.[1].trim(), html };
}

const REF_REL = /^courses\/([\w-]+)\/reference\/([\w.-]+)\.html$/;

/**
 * Reads a structured SintesiDocument for a reference sheet:
 * 1. Checks if the reference HTML contains a `<script type="application/json" id="reference-data">` data island.
 * 2. If not, falls back to building it from the corresponding lesson artifact and learning record.
 */
export function readReferenceDocument(repoRoot: string, rel: string): SintesiDocument | null {
  const r = rel.replace(/\\/g, "/");
  const match = REF_REL.exec(r);
  if (!match || r.includes("..")) return null;
  const [, course, fileSlug] = match;
  const absRef = path.resolve(repoRoot, r);

  if (fs.existsSync(absRef) && fs.statSync(absRef).isFile()) {
    const src = fs.readFileSync(absRef, "utf-8");
    const doc = parseReferenceDocument(src);
    if (doc) return doc;
  }

  // Fallback: locate matching lesson in courses/<course>/lessons/
  const lessonsDir = path.join(repoRoot, "courses", course, "lessons");
  if (!fs.existsSync(lessonsDir)) return null;

  const lessonFiles = fs.readdirSync(lessonsDir).filter((f) => f.endsWith(".html"));
  const candidate = lessonFiles.find((f) => slugOfLesson(f) === fileSlug || f === `${fileSlug}.html`);
  if (!candidate) return null;

  const lessonAbsPath = path.join(lessonsDir, candidate);
  const rawHtml = fs.readFileSync(lessonAbsPath, "utf-8");
  const doc = parseLessonDocument(rawHtml);
  if (!doc) return null;

  return buildSintesiDocument(doc, {
    baseDir: repoRoot,
    lessonPath: lessonAbsPath,
  });
}

/** `courses/x/lessons/0003-the-slug.html` (or the bare file name) -> `the-slug`. */
export const slugOfLesson = (p: string) => path.basename(p).replace(/^\d+-/, "").replace(/\.html$/, "");

// ───────────── learning records: the one "closed" rule ─────────────

/** Last line of a record: `<!-- lesson: <slug> -->`. An empty slug marks a record of no lesson (plan/practice sessions). */
const MARKER = /<!--\s*lesson:\s*([^>]*?)\s*-->\s*$/;
export const lessonMarker = (slug: string) => (slug ? `<!-- lesson: ${slug} -->` : "<!-- lesson: -->");

/**
 * The learning record that closes the lesson, repo-relative. A record closes a lesson by carrying its marker;
 * only a record with no marker at all (written before markers existed) closes it by number.
 */
function findRecord(records: { file: string; marker?: string }[], course: string, slug: string, number: number): string | undefined {
  const no = String(number).padStart(4, "0") + "-";
  const hit = records.find((r) => r.marker === slug) ?? records.find((r) => r.marker === undefined && r.file.startsWith(no));
  return hit && `courses/${course}/learning-records/${hit.file}`;
}

function readRecords(root: string, course: string): { file: string; marker?: string }[] {
  const dir = path.join(root, "courses", course, "learning-records");
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir)
    .filter((f) => /^\d{4}-.*\.md$/.test(f))
    .sort()
    .map((file) => ({ file, marker: MARKER.exec(fs.readFileSync(path.join(dir, file), "utf-8"))?.[1] }));
}

export const closingRecord = (repoRoot: string, course: string, slug: string, number: number) =>
  findRecord(readRecords(path.resolve(repoRoot), course), course, slug, number);

// ───────────── reading + status ─────────────

/** taught: nodes captured, plan complete (or none); inProgress: planned nodes left and no record; closed: its record exists. */
export type LessonStatus = "taught" | "inProgress" | "closed";

export interface Lesson {
  /** Repo-relative path. */
  rel: string;
  abs: string;
  summary: LessonSummary;
  doc: FullLessonDocument;
  /** Nodes in the plan DAG (0 without a plan). */
  dagNodes: number;
  /** Plan nodes not captured yet, closed or not. */
  pendingNodes: number;
  /** Repo-relative learning record that closes it. */
  record?: string;
  status: LessonStatus;
}

function build(root: string, rel: string, summary: LessonSummary, doc: FullLessonDocument, records: { file: string; marker?: string }[]): Lesson {
  const dagNodes = parseDagIds(doc.metadata.initialDag ?? "").length;
  const record = findRecord(records, summary.course, summary.slug, summary.lessonNumber);
  const pendingNodes = Math.max(0, dagNodes - doc.nodes.length);
  const status: LessonStatus = record ? "closed" : pendingNodes > 0 ? "inProgress" : "taught";
  return { rel, abs: path.resolve(root, rel), summary, doc, dagNodes, pendingNodes, record, status };
}

/** Every readable lesson artifact, each parsed once. Unreadable ones are skipped (the scan warns). */
export function listLessons(repoRoot: string): Lesson[] {
  const root = path.resolve(repoRoot);
  const records = new Map<string, ReturnType<typeof readRecords>>();
  return scanLessonDocs({ baseDir: root }).map(({ summary: s, doc }) => {
    if (!records.has(s.course)) records.set(s.course, readRecords(root, s.course));
    return build(root, `courses/${s.course}/lessons/${s.filename}`, s, doc, records.get(s.course)!);
  });
}

/** One lesson by repo-relative path. Null when the file is missing or has no data island; throws on a bad path or a malformed island. */
export function readLesson(repoRoot: string, rel: string): Lesson | null {
  const root = path.resolve(repoRoot);
  const abs = lessonAbs(root, rel);
  if (!fs.existsSync(abs) || !fs.statSync(abs).isFile()) return null;
  const doc = parseLessonDocument(fs.readFileSync(abs, "utf-8"));
  if (!doc) return null;
  const r = rel.replace(/\\/g, "/");
  const [, course] = r.split("/");
  const summary = summarizeLesson(path.join(root, "courses"), course, path.basename(r), doc);
  return build(root, r, summary, doc, readRecords(root, course));
}

/** Raw HTML of a lesson file, for artifacts without a readable island (prose lessons). Null when missing. */
export function readLessonHtml(repoRoot: string, rel: string): string | null {
  const abs = lessonAbs(repoRoot, rel);
  return fs.existsSync(abs) ? fs.readFileSync(abs, "utf-8") : null;
}

// ───────────── writes ─────────────

/** Approved revision of a lesson's plan: the artifact's DAG and goal follow it. Returns whether anything changed. */
export async function revisePlan(repoRoot: string, rel: string, plan: { dag: string; goal: string }): Promise<boolean> {
  const lesson = readLesson(repoRoot, rel);
  if (!lesson || (lesson.doc.metadata.initialDag === plan.dag && lesson.doc.metadata.targetGoal === plan.goal)) return false;
  await updateLessonMetadata({ lessonPath: lesson.abs, baseDir: path.resolve(repoRoot), patch: { initialDag: plan.dag, targetGoal: plan.goal } });
  return true;
}

// ───────────── the write seam ─────────────

const pending = new Map<string, NodeJS.Timeout>();

/**
 * A lesson, reference sheet, plan, exercise or record changed on disk: regenerate the board and tell the app
 * to refetch /api/courses. Raised once per successful write tool by toolsFor; debounced (300 ms) per repo.
 */
export function coursesChanged(repoRoot: string): void {
  if (pending.has(repoRoot)) return;
  const t = setTimeout(() => {
    pending.delete(repoRoot);
    void refreshBoard(repoRoot);
    emitGlobal({ type: "courses" });
  }, 300);
  t.unref();
  pending.set(repoRoot, t);
}
