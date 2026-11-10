/**
 * homepage.ts — Status board over chapter -> lesson -> node, plus live Anki counts.
 *
 * Part of the Proxi teaching engine.
 * The homepage renders a join of three sources, and stores nothing of its own:
 *   - chapter.ts                     the plan, and its match onto taught lessons
 *   - courses/<c>/lessons/*.html     what has actually been taught (exists <=> taught)
 *   - AnkiConnect                    card study state, which lives in Anki and nowhere else
 *
 * A chapter is a course directory, and lesson numbering restarts inside each one, so a
 * lesson is identified by (course, slug) throughout.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { parseArgs } from "node:util";
import { APP_NAME } from "../app/shared/identity.mjs";
import { main } from "./cli.ts";
import { enterDataRoot } from "./data-root.ts";
import { escapeHtml, parseLessonDocument, type FullLessonDocument, type PedagogicalTier } from "./html-lesson.ts";
import { slugify } from "./slug.ts";
import { DECK_ROOT, courseDeckNameFor } from "./anki-sync.ts";
import { DEFAULT_ANKI_URL, invoke } from "./anki-client.ts";
import {
  formatCurriculum,
  matchCurriculum,
  planChapter,
  readCurriculum,
  type CurriculumMatch,
  type PlannedLesson
} from "./chapter.ts";

// ────────────────────────────────────────────────────────────────────────────
// Types & Data Contracts
// ────────────────────────────────────────────────────────────────────────────

/** A lesson that exists on disk, summarized from its data island. */
export interface LessonSummary {
  /** The course directory it lives in, i.e. its chapter's slug. */
  course: string;
  lessonNumber: number;
  slug: string;
  title: string;
  filename: string;
  nodeCount: number;
  /** One entry per captured node, in capture order. */
  tiers: PedagogicalTier[];
  /** True when `courses/<course>/reference/<slug>.html` exists: the sheet is what gets re-read. */
  hasReference?: boolean;
}

export interface AnkiStatus {
  due: number;
  newCards: number;
  learning: number;
  total: number;
  deckCount: number;
  generatedAt: string;
  /** Per-course counts, keyed by course slug. A course whose deck does not exist yet is all zeros. */
  byCourse?: Record<string, { due: number; learning: number; newCards: number }>;
}

export interface RenderHomepageOptions {
  match: CurriculumMatch;
  anki: AnkiStatus | null;
}

export interface GenerateHomepageOptions {
  baseDir?: string;
  coursesDir?: string;
  curriculumPath?: string;
  outFile?: string;
  noAnki?: boolean;
  ankiUrl?: string;
  timeoutMs?: number;
}

export interface GenerateHomepageResult {
  filePath: string;
  chapterCount: number;
  lessonCount: number;
  orphanCount: number;
  ankiOnline: boolean;
}

// ────────────────────────────────────────────────────────────────────────────
// What has been taught: the course directories
// ────────────────────────────────────────────────────────────────────────────

/** Path of a lesson relative to the repo root, which is where index.html sits. */
export function lessonHref(lesson: LessonSummary): string {
  return "courses/" + lesson.course + "/lessons/" + lesson.filename;
}

/** The cheat sheet compiled from the lesson — the artifact he actually re-reads. */
export function referenceHref(lesson: LessonSummary): string {
  return "courses/" + lesson.course + "/reference/" + lesson.slug + ".html";
}

/**
 * Walks every `courses/<course>/lessons/*.html` and summarizes the ones carrying a data
 * island. An HTML file without one is not a lesson artifact and is skipped, not guessed at.
 */
export function scanLessons(options: { baseDir?: string; coursesDir?: string } = {}): LessonSummary[] {
  return scanLessonDocs(options).map((l) => l.summary);
}

export function summarizeLesson(coursesDir: string, course: string, filename: string, doc: FullLessonDocument): LessonSummary {
  return {
    course,
    lessonNumber: doc.metadata.lessonNumber,
    slug: doc.metadata.slug,
    title: doc.metadata.title || doc.metadata.topic,
    filename,
    nodeCount: doc.nodes.length,
    tiers: doc.nodes.map((n) => n.tier),
    hasReference: fs.existsSync(path.join(coursesDir, course, "reference", doc.metadata.slug + ".html")),
  };
}

/** scanLessons plus the parsed document of each artifact, so a caller that needs both parses once. */
export function scanLessonDocs(options: { baseDir?: string; coursesDir?: string } = {}): { summary: LessonSummary; doc: FullLessonDocument }[] {
  const coursesDir = options.coursesDir
    ? path.resolve(options.coursesDir)
    : path.join(path.resolve(options.baseDir || "."), "courses");
  if (!fs.existsSync(coursesDir)) return [];

  const summaries: { summary: LessonSummary; doc: FullLessonDocument }[] = [];
  for (const course of fs.readdirSync(coursesDir).sort()) {
    const lessonsDir = path.join(coursesDir, course, "lessons");
    if (!fs.existsSync(lessonsDir) || !fs.statSync(lessonsDir).isDirectory()) continue;

    for (const filename of fs.readdirSync(lessonsDir).sort()) {
      if (!filename.endsWith(".html")) continue;
      let doc;
      try {
        doc = parseLessonDocument(fs.readFileSync(path.join(lessonsDir, filename), "utf-8"));
      } catch (err: any) {
        // One unreadable artifact must not take the whole board down: name it and keep scanning.
        console.warn(`Ignorata ${course}/${filename}: ${err.message}`);
        continue;
      }
      if (!doc) continue; // not a lesson artifact (no data island)
      summaries.push({ doc, summary: summarizeLesson(coursesDir, course, filename, doc) });
    }
  }
  return summaries.sort(
    (a, b) => a.summary.course.localeCompare(b.summary.course) || a.summary.lessonNumber - b.summary.lessonNumber
  );
}

// ────────────────────────────────────────────────────────────────────────────
// Card study state: AnkiConnect
// ────────────────────────────────────────────────────────────────────────────

interface DeckStatsEntry {
  name: string;
  new_count: number;
  learn_count: number;
  review_count: number;
  total_in_deck: number;
}

/** Every course that should have a deck: the plan's chapters, then course dirs already on disk (orphans included). */
function courseSlugs(options: { baseDir?: string; coursesDir?: string; curriculumPath?: string }): string[] {
  const baseDir = path.resolve(options.baseDir || ".");
  let planned: string[] = [];
  try {
    planned = readCurriculum({ baseDir, curriculumPath: options.curriculumPath }).chapters.map((c) => c.slug);
  } catch {
    // A malformed plan is worth no Anki badges at all, but not a whole status board.
  }
  const onDisk = scanLessons({ baseDir, coursesDir: options.coursesDir }).map((l) => l.course);
  return [...new Set(planned.concat(onDisk))];
}

export async function fetchAnkiStatus(
  options: {
    ankiUrl?: string;
    timeoutMs?: number;
    baseDir?: string;
    coursesDir?: string;
    curriculumPath?: string;
  } = {}
): Promise<{ status?: AnkiStatus; error?: string }> {
  const url = options.ankiUrl || DEFAULT_ANKI_URL;
  const timeoutMs = options.timeoutMs ?? 2000;
  const baseDir = path.resolve(options.baseDir || ".");

  const names = await invoke<Record<string, number>>(url, "deckNamesAndIds", undefined, timeoutMs);
  if (names.error || !names.result || typeof names.result !== "object") {
    return { error: names.error || "AnkiConnect returned no deck list" };
  }
  const deckIds = names.result;

  // One batch for every course: the same getDeckStats call the total below needs, keyed by deck id.
  const slugs = courseSlugs(options);
  const byCourse: NonNullable<AnkiStatus["byCourse"]> = {};
  const fill = (stats: Record<string, DeckStatsEntry> | undefined) => {
    for (const slug of slugs) {
      const deckId = deckIds[courseDeckNameFor(baseDir, slug)];
      const entry = deckId === undefined ? undefined : stats?.[String(deckId)];
      byCourse[slug] = entry
        ? { due: entry.review_count || 0, learning: entry.learn_count || 0, newCards: entry.new_count || 0 }
        : { due: 0, learning: 0, newCards: 0 };
    }
    return byCourse;
  };

  const decks = Object.keys(deckIds).filter((n) => n === DECK_ROOT || n.startsWith(DECK_ROOT + "::"));
  if (decks.length === 0) {
    return {
      status: {
        due: 0,
        newCards: 0,
        learning: 0,
        total: 0,
        deckCount: 0,
        generatedAt: new Date().toISOString(),
        byCourse: fill(undefined)
      }
    };
  }

  const stats = await invoke<Record<string, DeckStatsEntry>>(url, "getDeckStats", { decks }, timeoutMs);
  if (stats.error || !stats.result) {
    return { error: stats.error || "AnkiConnect returned no deck stats" };
  }

  const entries = Object.values(stats.result);
  // Anki rolls subdecks (and its daily limits) into the parent's due/new/learn counts, so the root alone is what Anki
  // shows; summing it with its children double-counts. total_in_deck is per deck (no subdecks): that one is summed.
  // Matched by id: getDeckStats names are leaf names, so a "Proxi::X::Proxi" subdeck would also say "Proxi".
  const root = stats.result[String(deckIds[DECK_ROOT])];
  const counted = root ? [root] : entries;
  const status: AnkiStatus = {
    due: counted.reduce((s, d) => s + (d.review_count || 0), 0),
    newCards: counted.reduce((s, d) => s + (d.new_count || 0), 0),
    learning: counted.reduce((s, d) => s + (d.learn_count || 0), 0),
    total: entries.reduce((s, d) => s + (d.total_in_deck || 0), 0),
    deckCount: entries.length,
    generatedAt: new Date().toISOString(),
    byCourse: fill(stats.result)
  };
  return { status };
}

/** Due/new/learning of ONE deck (subdecks included, as Anki shows it). A missing deck is 0 due, not an error. */
export async function fetchDeckStatus(
  deck: string,
  options: { ankiUrl?: string; timeoutMs?: number } = {}
): Promise<{ status?: { due: number; newCards: number; learning: number; total: number; exists: boolean }; error?: string }> {
  const url = options.ankiUrl || DEFAULT_ANKI_URL;
  const timeoutMs = options.timeoutMs ?? 2000;

  const names = await invoke<Record<string, number>>(url, "deckNamesAndIds", undefined, timeoutMs);
  if (names.error || !names.result || typeof names.result !== "object") {
    return { error: names.error || "AnkiConnect returned no deck list" };
  }

  const deckId = names.result[deck];
  if (deckId === undefined) {
    return { status: { due: 0, newCards: 0, learning: 0, total: 0, exists: false } };
  }

  const stats = await invoke<Record<string, DeckStatsEntry>>(url, "getDeckStats", { decks: [deck] }, timeoutMs);
  if (stats.error || !stats.result) {
    return { error: stats.error || "AnkiConnect returned no deck stats" };
  }

  const entry = stats.result[String(deckId)] ?? Object.values(stats.result)[0];
  if (!entry) {
    return { status: { due: 0, newCards: 0, learning: 0, total: 0, exists: true } };
  }

  return {
    status: {
      due: entry.review_count || 0,
      newCards: entry.new_count || 0,
      learning: entry.learn_count || 0,
      total: entry.total_in_deck || 0,
      exists: true,
    },
  };
}

// ────────────────────────────────────────────────────────────────────────────
// Rendering: the join, as a standalone artifact
// ────────────────────────────────────────────────────────────────────────────

const TIER_CLASS: Record<PedagogicalTier, string> = {
  "unconditional-truth": "truth",
  axiom: "axiom",
  derivation: "deriv"
};

/** One dot per node: coloured for a captured node, hollow for a planned one. */
function renderDots(lesson: LessonSummary | undefined, plannedNodes?: number): string {
  const inner = lesson
    ? lesson.tiers.map((t) => '<i class="' + (TIER_CLASS[t] || "deriv") + '"></i>').join("")
    : '<i class="pending"></i>'.repeat(Math.max(0, plannedNodes || 0));
  if (!inner) return "";
  return '<span class="dots">' + inner + "</span>";
}

function renderRow(planned: PlannedLesson | null, lesson: LessonSummary | undefined): string {
  const title = escapeHtml(lesson ? lesson.title : planned!.title);
  const cells: string[] = [];
  cells.push('<td class="mark">' + (lesson ? "&check;" : "&middot;") + "</td>");
  cells.push('<td class="num">' + (lesson ? String(lesson.lessonNumber).padStart(4, "0") : "&mdash;") + "</td>");
  cells.push(
    '<td class="title">' +
      (lesson ? '<a href="' + encodeURI(lessonHref(lesson)) + '">' + title + "</a>" : title) +
      (lesson?.hasReference
        ? ' <a class="ref" href="' + encodeURI(referenceHref(lesson)) + '">ref</a>'
        : "") +
      "</td>"
  );
  cells.push('<td class="r">' + renderDots(lesson, planned?.plannedNodes) + "</td>");
  cells.push(
    '<td class="r num">' +
      (lesson
        ? lesson.nodeCount === 0
          ? // a hand-authored lesson: taught, but never captured node by node
            "prosa"
          : lesson.nodeCount + (lesson.nodeCount === 1 ? " nodo" : " nodi")
        : planned?.plannedNodes
          ? planned.plannedNodes + " nodi previsti"
          : "da fare") +
      "</td>"
  );
  return '<tr class="' + (lesson ? "" : "is-todo") + '">' + cells.join("") + "</tr>";
}

/**
 * `course` links the chapter header to its directory, where MISSION.md and the rest live.
 * The directory, not MISSION.md itself: not every course has one, and a board full of dead
 * links is worse than one extra click.
 */
function renderSection(
  title: string,
  taught: number,
  total: number,
  rows: string,
  course?: string
): string {
  const pct = total > 0 ? (taught / total) * 100 : 0;
  const heading = course
    ? '<a href="courses/' + encodeURI(course) + '/">' + escapeHtml(title) + "</a>"
    : escapeHtml(title);
  return (
    '<div class="chapter-h">' +
    heading +
    '<span class="num">' +
    taught +
    "/" +
    total +
    "</span></div>" +
    '<div class="bar"><i style="width:' +
    pct.toFixed(1) +
    '%"></i></div>' +
    '<div class="card"><table class="reg">' +
    rows +
    "</table></div>"
  );
}

function renderAnkiStrip(anki: AnkiStatus): string {
  const stat = (value: number, label: string, hot = false) =>
    '<div class="stat' + (hot ? " hot" : "") + '"><b>' + value + "</b><span>" + label + "</span></div>";
  return (
    '<div class="card anki-strip">' +
    stat(anki.due, "da ripassare", true) +
    stat(anki.newCards, "nuove") +
    stat(anki.learning, "in apprendimento") +
    '<div class="spacer"></div>' +
    '<div class="stat" style="text-align:right"><b>' +
    anki.total +
    "</b><span>card in " +
    anki.deckCount +
    " deck</span></div></div>"
  );
}

export function renderHomepage(options: RenderHomepageOptions): string {
  const { match, anki } = options;
  const lessons = match.taught;

  const sections = match.chapters
    .map((chapter) =>
      renderSection(
        chapter.title,
        chapter.taught,
        chapter.lessons.length,
        chapter.lessons.map((m) => renderRow(m.planned, m.lesson)).join(""),
        chapter.slug
      )
    )
    .join("");

  // A taught lesson whose slug matches no plan entry still has to be reachable.
  const orphans = match.orphans;
  const orphanSection = orphans.length
    ? renderSection(
        "Fuori dal piano",
        orphans.length,
        orphans.length,
        orphans.map((l) => renderRow(null, l)).join("")
      )
    : "";

  const taughtTotal = lessons.length;
  const plannedTotal = match.chapters.reduce((s, c) => s + c.lessons.length, 0);
  const nodeTotal = lessons.reduce((s, l) => s + l.nodeCount, 0);
  const subtitle =
    match.chapters.length +
    (match.chapters.length === 1 ? " capitolo" : " capitoli") +
    " &middot; " +
    taughtTotal +
    "/" +
    Math.max(plannedTotal, taughtTotal) +
    " lezioni fatte &middot; " +
    nodeTotal +
    (nodeTotal === 1 ? " nodo catturato" : " nodi catturati");

  const stamp = anki
    ? "Dati Anki al " +
      escapeHtml(new Date(anki.generatedAt).toLocaleString("it-IT")) +
      " &mdash; rigenera con <code>npm run home</code>"
    : "Generata senza Anki (<code>--no-anki</code>) &mdash; rigenera con <code>npm run home</code>";

  return HOMEPAGE_SHELL.replace("<!-- BODY -->", () =>
    '<h1>' + APP_NAME + '</h1><div class="sub">' +
    subtitle +
    "</div>" +
    (anki ? renderAnkiStrip(anki) : "") +
    (sections || orphanSection
      ? sections + orphanSection
      : '<div class="card empty">Nessun capitolo pianificato. Comincia con <code>/plan &lt;argomento&gt;</code>.</div>') +
    LEGEND +
    '<div class="stamp">' +
    stamp +
    "</div>"
  );
}

const LEGEND =
  '<div class="legend">' +
  '<span><span class="dots"><i class="truth"></i></span> verità incondizionata</span>' +
  '<span><span class="dots"><i class="axiom"></i></span> assioma</span>' +
  '<span><span class="dots"><i class="deriv"></i></span> derivazione</span>' +
  '<span><span class="dots"><i class="pending"></i></span> nodo previsto</span></div>';

const HOMEPAGE_SHELL =
  "<!DOCTYPE html>\n" +
  '<html lang="it">\n' +
  "<head>\n" +
  '<meta charset="utf-8">\n' +
  '<meta name="viewport" content="width=device-width, initial-scale=1">\n' +
  "<title>" + APP_NAME + "</title>\n" +
  "<style>\n" +
  ":root{--font-body:Charter,\"Bitstream Charter\",\"Sitka Text\",Cambria,Georgia,serif;" +
  "--font-ui:-apple-system,BlinkMacSystemFont,\"Segoe UI\",Roboto,Inter,Helvetica,Arial,sans-serif;" +
  "--font-mono:ui-monospace,\"SF Mono\",Monaco,\"Cascadia Code\",Consolas,monospace;" +
  "--bg:#fcfbfa;--surface:#fff;--subtle:#f4f3ef;--fg:#1c1917;--fg2:#44403c;--muted:#78716c;" +
  "--border:#e7e5e0;--link:#0369a1;--ok:#0f766e;--due:#b91c1c;" +
  "--truth:#b45309;--axiom:#0f766e;--deriv:#4338ca}\n" +
  "@media(prefers-color-scheme:dark){:root{--bg:#0f172a;--surface:#1e293b;--subtle:#192434;" +
  "--fg:#f8fafc;--fg2:#cbd5e1;--muted:#94a3b8;--border:#334155;--link:#7dd3fc;--ok:#2dd4bf;--due:#fca5a5;" +
  "--truth:#fbbf24;--axiom:#2dd4bf;--deriv:#a5b4fc}}\n" +
  "*{box-sizing:border-box}\n" +
  "body{margin:0;background:var(--bg);color:var(--fg);font-family:var(--font-ui);font-size:15px;" +
  "line-height:1.5;padding:2rem 1.25rem 4rem}\n" +
  ".wrap{max-width:960px;margin:0 auto}\n" +
  "h1{font-family:var(--font-body);font-size:1.6rem;margin:0 0 .15rem}\n" +
  ".sub{color:var(--muted);font-size:.85rem;margin-bottom:1.75rem}\n" +
  "a{color:var(--link);text-decoration:none}a:hover{text-decoration:underline}\n" +
  ".num{font-family:var(--font-mono);font-size:.8rem;color:var(--muted)}\n" +
  ".card{background:var(--surface);border:1px solid var(--border);border-radius:8px}\n" +
  ".empty{padding:1.25rem;color:var(--muted);font-size:.9rem}\n" +
  "code{font-family:var(--font-mono);font-size:.85em;background:var(--subtle);padding:.1em .3em;border-radius:4px}\n" +
  ".bar{height:4px;background:var(--subtle);border-radius:2px;overflow:hidden;margin-bottom:.6rem}\n" +
  ".bar>i{display:block;height:100%;background:var(--ok)}\n" +
  ".dots{display:inline-flex;gap:2px;align-items:center;vertical-align:middle}\n" +
  ".dots i{width:6px;height:6px;border-radius:50%;display:block}\n" +
  ".dots i.truth{background:var(--truth)}.dots i.axiom{background:var(--axiom)}\n" +
  ".dots i.deriv{background:var(--deriv)}.dots i.pending{background:var(--border)}\n" +
  ".legend{font-size:.72rem;color:var(--muted);display:flex;gap:1rem;margin-top:2rem;" +
  "padding-top:.8rem;border-top:1px solid var(--border);flex-wrap:wrap}\n" +
  ".legend span{display:flex;gap:.3rem;align-items:center}\n" +
  ".stamp{font-size:.72rem;color:var(--muted);margin-top:.8rem}\n" +
  ".anki-strip{display:flex;align-items:center;gap:1.5rem;padding:.8rem 1rem;margin-bottom:1.75rem;flex-wrap:wrap}\n" +
  ".anki-strip .stat{display:flex;flex-direction:column}\n" +
  ".anki-strip .stat b{font-family:var(--font-mono);font-size:1.3rem;font-weight:600}\n" +
  ".anki-strip .stat.hot b{color:var(--due)}\n" +
  ".anki-strip .stat span{font-size:.7rem;text-transform:uppercase;letter-spacing:.04em;color:var(--muted)}\n" +
  ".anki-strip .spacer{flex:1}\n" +
  ".chapter-h{font-family:var(--font-body);font-size:1.05rem;margin:1.75rem 0 .4rem;" +
  "display:flex;align-items:baseline;gap:.6rem}\n" +
  ".chapter-h .num{margin-left:auto}\n" +
  "table.reg{width:100%;border-collapse:collapse}\n" +
  "table.reg td{padding:.45rem .5rem;border-bottom:1px solid var(--border);vertical-align:middle}\n" +
  "table.reg tr:last-child td{border-bottom:none}\n" +
  "table.reg td.mark{width:1.6rem;text-align:center;color:var(--ok)}\n" +
  "table.reg td.r{text-align:right;white-space:nowrap}\n" +
  "table.reg tr.is-todo td.title{color:var(--muted)}\n" +
  "a.ref{font-family:var(--font-mono);font-size:.7rem;text-transform:uppercase;letter-spacing:.05em;color:var(--muted);text-decoration:none;border:1px solid var(--muted);border-radius:.25rem;padding:0 .25rem;margin-left:.4rem;opacity:.7}\n" +
  "a.ref:hover{opacity:1}\n" +
  "</style>\n" +
  "</head>\n" +
  "<body>\n" +
  '<div class="wrap">\n' +
  "<!-- BODY -->\n" +
  "</div>\n" +
  "</body>\n" +
  "</html>\n";

// ────────────────────────────────────────────────────────────────────────────
// Generation
// ────────────────────────────────────────────────────────────────────────────

export async function generateHomepage(
  options: GenerateHomepageOptions = {}
): Promise<GenerateHomepageResult> {
  const baseDir = path.resolve(options.baseDir || ".");
  const outFile = options.outFile ? path.resolve(options.outFile) : path.join(baseDir, "index.html");

  let anki: AnkiStatus | null = null;
  if (!options.noAnki) {
    // byCourse needs the same baseDir the plan and the course scan below use, not the cwd's.
    const res = await fetchAnkiStatus({ ...options, baseDir });
    if (!res.status) {
      throw new Error(
        "AnkiConnect non raggiungibile (" +
          (res.error || "unknown") +
          "). Apri Anki, oppure genera senza i dati delle card con --no-anki."
      );
    }
    anki = res.status;
  }

  const match = matchCurriculum(
    readCurriculum({ baseDir, curriculumPath: options.curriculumPath }),
    scanLessons({ baseDir, coursesDir: options.coursesDir })
  );

  fs.writeFileSync(outFile, renderHomepage({ match, anki }), "utf-8");

  return {
    filePath: outFile,
    chapterCount: match.chapters.length,
    lessonCount: match.taught.length,
    orphanCount: match.orphans.length,
    ankiOnline: anki !== null
  };
}

// ────────────────────────────────────────────────────────────────────────────
// CLI
// ────────────────────────────────────────────────────────────────────────────

export async function runCli(argv: string[]): Promise<number> {
  try {
    const { values, positionals } = parseArgs({
      args: argv,
      allowPositionals: true,
      options: {
        chapter: { type: "string" },
        slug: { type: "string" },
        "lessons-json": { type: "string" },
        curriculum: { type: "string" },
        dir: { type: "string" },
        out: { type: "string" },
        "anki-url": { type: "string" },
        "no-anki": { type: "boolean" },
        help: { type: "boolean", short: "h" }
      }
    });
    const command = positionals[0] || "generate";

    if (command === "help" || values.help) {
      console.log(
        [
          `${APP_NAME} homepage`,
          "",
          "  npm run home                      generate index.html (requires AnkiConnect)",
          "  npm run home -- --no-anki         generate without card counts",
          "  npm run home -- plan --chapter <title> [--slug <course-dir>] --lessons-json <json>",
          "  npm run home -- chapters          print the plan as text (writes nothing)",
          "",
          "  --lessons-json  [{\"title\":\"...\",\"plannedNodes\":4}, ...]",
          "  --slug          course directory, when it differs from the chapter title",
          "  --dir           courses directory (default ./courses)",
          "  --anki-url      AnkiConnect endpoint (default " + DEFAULT_ANKI_URL + ")"
        ].join("\n")
      );
      return 0;
    }

    if (command === "plan") {
      const chapter = values.chapter;
      if (!chapter) throw new Error("plan requires --chapter <title>");
      const raw = values["lessons-json"];
      if (!raw) throw new Error("plan requires --lessons-json '[{\"title\":\"...\"}]'");
      const lessons = JSON.parse(raw);
      if (!Array.isArray(lessons)) throw new Error("--lessons-json must be a JSON array");
      const curriculum = planChapter({
        chapter,
        slug: values.slug,
        lessons,
        curriculumPath: values.curriculum
      });
      const wanted = slugify(values.slug || chapter);
      const saved = curriculum.chapters.find((c) => c.slug === wanted)!;
      console.log(
        "Planned " + saved.title + " (courses/" + saved.slug + "): " + saved.lessons.length +
          " lessons (" + saved.lessons.map((l) => l.slug).join(", ") + ")"
      );
      return 0;
    }

    if (command === "chapters") {
      console.log(
        formatCurriculum(
          matchCurriculum(
            readCurriculum({ curriculumPath: values.curriculum }),
            scanLessons({ coursesDir: values.dir })
          )
        )
      );
      return 0;
    }

    if (command === "generate") {
      const res = await generateHomepage({
        noAnki: values["no-anki"] === true,
        coursesDir: values.dir,
        curriculumPath: values.curriculum,
        outFile: values.out,
        ankiUrl: values["anki-url"]
      });
      console.log(
        res.filePath + " — " + res.chapterCount + " chapters, " + res.lessonCount + " lessons taught" +
          (res.orphanCount ? ", " + res.orphanCount + " outside the plan" : "") +
          (res.ankiOnline ? "" : ", without Anki")
      );
      return 0;
    }

    throw new Error("Unknown command: " + command);
  } catch (err: any) {
    console.error("Error: " + err.message);
    return 1;
  }
}

main(import.meta.url, (argv) => {
  enterDataRoot();
  return runCli(argv);
});
