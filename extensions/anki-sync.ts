/**
 * anki-sync.ts — Spaced Repetition (Anki) Deck Exporter & AnkiConnect Sync Engine
 *
 * Part of the Proxi teaching engine.
 * Supports programmatic TS imports, CLI execution, and cross-agent interoperability.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { APP_NAME, DECK_ROOT as IDENTITY_DECK_ROOT } from "../app/shared/identity.mjs";
import { readCurriculum } from "./chapter.ts";
import { enterDataRoot } from "./data-root.ts";
import type {
  CognitiveNodePayload,
  FullLessonDocument,
  LessonMetadata,
  PedagogicalTier
} from "./html-lesson.ts";

// ────────────────────────────────────────────────────────────────────────────
// Types & Data Contracts
// ────────────────────────────────────────────────────────────────────────────

export type AnkiCardType = "basic" | "cloze";

export interface AnkiCardBase {
  id: string;
  nodeId: string;
  type: AnkiCardType;
  deck: string;
  tags: string[];
  sourceLesson?: string;
}

export interface BasicCard extends AnkiCardBase {
  type: "basic";
  front: string;
  back: string;
}

export interface ClozeCard extends AnkiCardBase {
  type: "cloze";
  text: string;
  extra?: string;
}

export type AnkiCard = BasicCard | ClozeCard;

// ── AnkiConnect JSON-RPC Protocol Types ──────────────────────────────────────

export interface AnkiConnectRequest<T = any> {
  action: string;
  version: number;
  params?: T;
  key?: string;
}

export interface AnkiConnectSuccessResponse<T = any> {
  result: T;
  error: null;
}

export interface AnkiConnectErrorResponse {
  result: null;
  error: string;
}

export type AnkiConnectResponse<T = any> =
  | AnkiConnectSuccessResponse<T>
  | AnkiConnectErrorResponse;

export interface AnkiConnectNoteFields {
  [fieldName: string]: string;
}

export interface AnkiConnectNote {
  deckName: string;
  modelName: "Basic" | "Cloze" | string;
  fields: AnkiConnectNoteFields;
  tags: string[];
  options?: {
    allowDuplicate?: boolean;
    duplicateScope?: "deck" | "collection" | "all";
    duplicateScopeOptions?: {
      deckName?: string;
      checkChildren?: boolean;
      checkAllModels?: boolean;
    };
  };
}

// ── Operation Options & Results ─────────────────────────────────────────────

export interface ExportDeckOptions {
  cards?: AnkiCard[];
  nodes?: CognitiveNodePayload[];
  lessonPath?: string;
  lessonDoc?: FullLessonDocument;
  topic?: string;
  slug?: string;
  deckName?: string;
  outDir?: string;
  outFile?: string;
  baseDir?: string;
}

export interface ExportResult {
  filePath: string;
  deckName: string;
  slug: string;
  cardCount: number;
  basicCount: number;
  clozeCount: number;
  tsvContent: string;
}

export interface SyncOptions {
  cards?: AnkiCard[];
  nodes?: CognitiveNodePayload[];
  lessonPath?: string;
  topic?: string;
  slug?: string;
  deckName?: string;
  ankiUrl?: string;
  timeoutMs?: number;
  createDeckIfMissing?: boolean;
  allowDuplicates?: boolean;
  baseDir?: string;
  decksDir?: string;
}

export interface SyncResult {
  synced: boolean;
  deckName: string;
  totalCards: number;
  addedCount: number;
  duplicateCount: number;
  failedCount: number;
  noteIds: (number | null)[];
  tsvExportPath?: string;
  warning?: string;
  error?: string;
}

export interface AnkiPingResult {
  online: boolean;
  version?: number;
  error?: string;
  responseTimeMs: number;
}

// ────────────────────────────────────────────────────────────────────────────
// Formatting & Sanitization Helpers
// ────────────────────────────────────────────────────────────────────────────

/**
 * Converts a human topic name into a safe, normalized kebab-case slug.
 */
export function slugifyTopic(topic: string): string {
  return topic
    .toLowerCase()
    // fold accents to ASCII first, so "Probabilità" slugs to "probabilita", not "probabilit"
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .replace(/[^\w\s-]/g, "")
    .trim()
    .replace(/[\s_-]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/**
 * The deck root every generated deck hangs under (one deck per course below it), from
 * app/shared/identity.mjs. Renaming it orphans every synced deck unless the cards are moved first
 * (changeDeck keeps their reviews).
 */
export const DECK_ROOT: string = IDENTITY_DECK_ROOT;

/**
 * Single source of truth for the free-topic lesson container: a real course directory
 * (`courses/lezioni-singole/`) with its own deck, but kept out of the structured course list.
 */
export const STANDALONE_COURSE_SLUG = "lezioni-singole";
export const STANDALONE_COURSE_TITLE = "Lezioni singole";

/**
 * Derives default Anki hierarchical deck name from topic.
 * E.g. "Calculus: Derivatives" -> "Proxi::CalculusDerivatives" or "Proxi::Calculus"
 */
export function formatDefaultDeckName(topic: string): string {
  const clean = topic.replace(/[\\/]/g, "::").trim();
  if (clean.startsWith(DECK_ROOT + "::")) {
    return clean;
  }
  return `${DECK_ROOT}::${clean}`;
}

/** "Proxi::<chapter title>" for course <course> under baseDir (directory name if the plan has no title). */
export function courseDeckNameFor(baseDir: string, course: string): string {
  let title: string | undefined;
  try {
    title = readCurriculum({ baseDir }).chapters.find((c) => c.slug === course)?.title;
  } catch {
    /* no readable plan: the directory name */
  }
  if (!title && course === STANDALONE_COURSE_SLUG) title = STANDALONE_COURSE_TITLE;
  return `${DECK_ROOT}::${(title || course).replace(/::|[\\/]/g, " ").trim()}`;
}

/**
 * One deck per course: a lesson at courses/<course>/lessons/<file>.html syncs into
 * "Proxi::<chapter title>" (the lesson stays findable through its topic tag).
 * Undefined for a lesson outside a course directory (the per-topic default applies).
 */
export function courseDeckName(lessonPath: string): string | undefined {
  const parts = path.resolve(lessonPath).split(path.sep);
  const i = parts.length - 2;
  if (i < 2 || parts[i] !== "lessons" || parts[i - 2] !== "courses") return undefined;
  return courseDeckNameFor(parts.slice(0, i - 2).join(path.sep), parts[i - 1]);
}

/**
 * Course and lesson tags for a lesson under courses/<course>/lessons/<file>.html.
 * Returns [] outside a course directory; drops lesson tag when lessonSlug is empty.
 */
export function courseLessonTags(lessonPath: string, lessonSlug: string | undefined): string[] {
  const parts = path.resolve(lessonPath).split(path.sep);
  const i = parts.length - 2;
  if (i < 2 || parts[i] !== "lessons" || parts[i - 2] !== "courses") return [];
  const course = parts[i - 1];
  const tags = [`course::${course}`];
  if (lessonSlug) tags.push(`lesson::${lessonSlug}`);
  return tags;
}

/**
 * Converts KaTeX math delimiters ($ / $$) to standard Anki MathJax (\( / \[\])
 * while preserving already formatted MathJax delimiters and applying brace workarounds.
 */
export function formatLatexForAnki(text: string): string {
  if (!text) return "";

  let result = text;

  // 1. Convert Display Math ($$...$$) to \[ ... \]
  result = result.replace(/\$\$([\s\S]*?)\$\$/g, (_match, math) => {
    return `\\[${math.trim()}\\]`;
  });

  // 2. Convert Inline Math ($...$) to \( ... \)
  // Match single dollar signs not preceded or followed by other dollars or backslashes
  result = result.replace(/(?<!\\|\$)\$(?!\$)(.+?)(?<!\\|\$)\$/g, (_match, math) => {
    return `\\(${math.trim()}\\)`;
  });

  // 3. Spacing workaround for nested Cloze deletions containing LaTeX closing braces:
  // e.g. {{c1:\frac{a}{b}}} -> {{c1:\frac{a}{b } }}
  result = result.replace(/\{\{c(\d+)::?([\s\S]*?)\}\}/g, (match, cNum, inner) => {
    // If the inner content ends with a brace like }\), insert a space before }}
    if (inner.endsWith("}") || inner.endsWith("}\\)")) {
      const spacedInner = inner.replace(/\}+$/, (m: string) => m.split("").join(" "));
      return `{{c${cNum}::${spacedInner} }}`;
    }
    return match;
  });

  return result;
}

/**
 * Escapes characters for clean TSV column formatting.
 * Replaces literal tabs with spaces and converts newlines into <br>.
 */
export function escapeTsvField(text: string): string {
  if (!text) return "";

  return text
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .replace(/\t/g, " ")
    .replace(/\n/g, "<br>")
    .trim();
}

// ────────────────────────────────────────────────────────────────────────────
// Flashcard Generation from Cognitive Nodes
// ────────────────────────────────────────────────────────────────────────────

/**
 * Derives atomic, high-retention AnkiCard items from a CognitiveNodePayload
 * adhering to the Minimum Information Principle (MIP).
 */
export function generateCardsFromNode(
  node: CognitiveNodePayload,
  topic: string,
  options: { defaultDeck?: string; slug?: string } = {}
): AnkiCard[] {
  const cards: AnkiCard[] = [];
  const topicSlug = options.slug || slugifyTopic(topic);
  const deck = options.defaultDeck || formatDefaultDeckName(topic);

  const baseTags = [
    "learn-forme",
    topicSlug,
    node.tier,
    `tier::${node.tier}`,
    `node::${node.nodeId}`
  ];

  if (node.ankiParams?.tags && Array.isArray(node.ankiParams.tags)) {
    for (const tag of node.ankiParams.tags) {
      const cleanTag = slugifyTopic(tag);
      if (cleanTag && !baseTags.includes(cleanTag)) {
        baseTags.push(cleanTag);
      }
    }
  }

  // At most ONE card per node. A cloze, a fact card and a quiz card per node restated the same
  // idea three times and buried the essentials under review load. The quiz stays in the lesson,
  // where it checks understanding once. A node authored with both fields keeps the question card.
  const params = node.ankiParams;
  if (params?.targetQuestion && params.targetFact) {
    // An authored question, never a wrapper around the title: the title states the answer.
    cards.push({
      id: `${node.nodeId}-fact`,
      nodeId: node.nodeId,
      type: "basic",
      deck,
      front: formatLatexForAnki(params.targetQuestion),
      back: formatLatexForAnki(params.targetFact),
      tags: [...baseTags]
    });
  } else if (params?.suggestedCloze) {
    // Only an authored one-liner goes on the back. The node prose used to be dumped here and
    // produced 4k-character cards nobody reads during review.
    cards.push({
      id: `${node.nodeId}-cloze`,
      nodeId: node.nodeId,
      type: "cloze",
      deck,
      text: formatLatexForAnki(params.suggestedCloze),
      extra: params.clozeExtra ? formatLatexForAnki(params.clozeExtra) : undefined,
      tags: [...baseTags]
    });
  }

  return cards;
}

/**
 * Extracts all cards from a FullLessonDocument object or on-disk HTML lesson file.
 */
export function extractCardsFromLesson(
  source: string | FullLessonDocument
): { metadata: LessonMetadata; cards: AnkiCard[]; deckName: string } {
  let doc: FullLessonDocument;

  if (typeof source === "string") {
    if (!fs.existsSync(source)) {
      throw new Error(`Lesson file not found at path: ${source}`);
    }
    const html = fs.readFileSync(source, "utf-8");
    const jsonMatch = html.match(/<script\s+type=["']application\/json["']\s+id=["']lesson-data["']>([\s\S]*?)<\/script>/i);

    if (!jsonMatch || !jsonMatch[1]) {
      throw new Error(`Could not find embedded #lesson-data JSON block in ${source}`);
    }

    try {
      doc = JSON.parse(jsonMatch[1].trim());
    } catch (err: any) {
      throw new Error(`Failed to parse lesson-data JSON in ${source}: ${err.message}`);
    }
  } else {
    doc = source;
  }

  const allCards: AnkiCard[] = [];
  const topic = doc.metadata.topic || doc.metadata.title || "Lesson";
  // Tags and deck group cards by SUBJECT, so both come from the topic. Deliberately not
  // metadata.slug: that is the artifact's identity and derives from the title, which would
  // silently re-tag every card whenever a lesson's title differs from its topic.
  const slug = slugifyTopic(topic);
  const defaultDeck = (typeof source === "string" && courseDeckName(source)) || formatDefaultDeckName(topic);

  for (const node of doc.nodes) {
    const nodeCards = generateCardsFromNode(node, topic, { defaultDeck, slug });
    allCards.push(...nodeCards);
  }

  if (typeof source === "string") {
    const extra = courseLessonTags(source, doc.metadata.slug);
    for (const card of allCards) {
      for (const tag of extra) {
        if (!card.tags.includes(tag)) {
          card.tags.push(tag);
        }
      }
    }
  }

  return {
    metadata: doc.metadata,
    deckName: defaultDeck,
    cards: allCards
  };
}

// ────────────────────────────────────────────────────────────────────────────
// TSV Serialization & Export Engine
// ────────────────────────────────────────────────────────────────────────────

/**
 * Serializes a list of AnkiCard objects into standard Anki TSV format with header directives.
 */
export function cardsToTsv(
  cards: AnkiCard[],
  options: { deckName?: string; topic?: string } = {}
): string {
  const deck = options.deckName || (cards.length > 0 ? cards[0].deck : `${DECK_ROOT}::General`);
  const hasCloze = cards.some((c) => c.type === "cloze");
  const hasBasic = cards.some((c) => c.type === "basic");
  const defaultNoteType = hasCloze && !hasBasic ? "Cloze" : "Basic";

  const headers = [
    "#separator:tab",
    "#html:true",
    "#tags column:3",
    `#deck:${deck}`,
    `#notetype:${defaultNoteType}`
  ];

  const rows: string[] = [...headers];

  for (const card of cards) {
    const tagsStr = card.tags.join(" ");

    if (card.type === "basic") {
      const front = escapeTsvField(card.front);
      const back = escapeTsvField(card.back);
      rows.push(`${front}\t${back}\t${tagsStr}`);
    } else if (card.type === "cloze") {
      const text = escapeTsvField(card.text);
      const extra = escapeTsvField(card.extra || "");
      rows.push(`${text}\t${extra}\t${tagsStr}`);
    }
  }

  return rows.join("\n") + "\n";
}

/**
 * Exports cards to `./decks/<slug>.tsv`, overwriting that file. With --lesson the slug is the
 * lesson's (one TSV per lesson); otherwise it is the topic's. Deliberately per-lesson: writing
 * every lesson of a topic to one file would have each export truncate the previous one.
 */
export async function exportDeckTsv(options: ExportDeckOptions): Promise<ExportResult> {
  const baseDir = options.baseDir || process.cwd();
  const decksDir = options.outDir || path.join(baseDir, "decks");

  let cards: AnkiCard[] = options.cards || [];
  let topic = options.topic || "Lesson";
  let slug = options.slug || "";
  let lessonDeck: string | undefined;

  if (options.lessonPath || options.lessonDoc) {
    const extracted = extractCardsFromLesson(options.lessonPath || options.lessonDoc!);
    cards = extracted.cards;
    topic = extracted.metadata.topic;
    slug = extracted.metadata.slug;
    lessonDeck = extracted.deckName;
  } else if (options.nodes) {
    topic = options.topic || "Lesson";
    slug = options.slug || slugifyTopic(topic);
    cards = [];
    for (const node of options.nodes) {
      cards.push(...generateCardsFromNode(node, topic, { slug }));
    }
  }

  if (!slug) {
    slug = slugifyTopic(topic);
  }

  const deckName = options.deckName || lessonDeck || formatDefaultDeckName(topic);
  const tsvContent = cardsToTsv(cards, { deckName, topic });

  if (!fs.existsSync(decksDir)) {
    fs.mkdirSync(decksDir, { recursive: true });
  }

  const targetFile = options.outFile || path.join(decksDir, `${slug}.tsv`);
  fs.writeFileSync(targetFile, tsvContent, "utf-8");

  const basicCount = cards.filter((c) => c.type === "basic").length;
  const clozeCount = cards.filter((c) => c.type === "cloze").length;

  return {
    filePath: targetFile,
    deckName,
    slug,
    cardCount: cards.length,
    basicCount,
    clozeCount,
    tsvContent
  };
}

// ────────────────────────────────────────────────────────────────────────────
// AnkiConnect API Client & Sync Engine
// ────────────────────────────────────────────────────────────────────────────

/**
 * Checks if local AnkiConnect server is running and responsive.
 */
export async function pingAnkiConnect(
  url = "http://127.0.0.1:8765",
  timeoutMs = 2000
): Promise<AnkiPingResult> {
  const startTime = Date.now();
  try {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);

    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json; charset=utf-8" },
      body: JSON.stringify({ action: "version", version: 6 }),
      signal: controller.signal
    });

    clearTimeout(timer);
    const duration = Date.now() - startTime;

    if (!res.ok) {
      return {
        online: false,
        error: `HTTP error status ${res.status}`,
        responseTimeMs: duration
      };
    }

    const data = (await res.json()) as AnkiConnectResponse<number>;
    if (data.error) {
      return {
        online: false,
        error: data.error,
        responseTimeMs: duration
      };
    }

    return {
      online: true,
      version: data.result ?? 6,
      responseTimeMs: duration
    };
  } catch (err: any) {
    return {
      online: false,
      error: err.name === "AbortError" ? "Connection timed out" : err.message || "Unreachable",
      responseTimeMs: Date.now() - startTime
    };
  }
}


/**
 * Anki localizes the built-in note types: an Italian profile has "Basilare" and "Testo da
 * completare" instead of "Basic" and "Cloze", and their field names differ too. Resolve both
 * against the running Anki, falling back to the English defaults when the query fails.
 */
interface NoteModel { name: string; fields: [string, string] }
interface NoteModels { basic: NoteModel; cloze: NoteModel }

const DEFAULT_MODELS: NoteModels = {
  basic: { name: "Basic", fields: ["Front", "Back"] },
  cloze: { name: "Cloze", fields: ["Text", "Extra"] }
};

async function resolveNoteModels(ankiUrl: string, timeoutMs: number): Promise<NoteModels> {
  const names = await callAnkiConnect<string[]>(ankiUrl, "modelNames", {}, timeoutMs);
  if (names.error || !Array.isArray(names.result)) return DEFAULT_MODELS;
  const all = names.result;

  const resolve = async (fallback: NoteModel, exact: string, loose: RegExp): Promise<NoteModel> => {
    const name = all.find((n) => n === exact) ?? all.find((n) => loose.test(n));
    if (!name) return fallback;
    const fields = await callAnkiConnect<string[]>(ankiUrl, "modelFieldNames", { modelName: name }, timeoutMs);
    if (fields.error || !fields.result || fields.result.length < 2) return { ...fallback, name };
    return { name, fields: [fields.result[0], fields.result[1]] };
  };

  return {
    basic: await resolve(DEFAULT_MODELS.basic, "Basic", /^basi(c|lare)$/i),
    cloze: await resolve(DEFAULT_MODELS.cloze, "Cloze", /cloze|completare/i)
  };
}

/**
 * Sends a single JSON-RPC action to AnkiConnect.
 */
async function callAnkiConnect<TResult = any>(
  url: string,
  action: string,
  params?: any,
  timeoutMs = 2000
): Promise<AnkiConnectResponse<TResult>> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json; charset=utf-8" },
      body: JSON.stringify({ action, version: 6, params }),
      signal: controller.signal
    });

    clearTimeout(timer);

    if (!res.ok) {
      return { result: null, error: `HTTP ${res.status} ${res.statusText}` };
    }

    const data = (await res.json()) as AnkiConnectResponse<TResult>;
    return data;
  } catch (err: any) {
    clearTimeout(timer);
    return {
      result: null,
      error: err.name === "AbortError" ? "AnkiConnect request timed out" : err.message || "Network error"
    };
  }
}

/**
 * Synchronizes cards to AnkiConnect with guaranteed non-blocking fallback to TSV.
 */
export async function syncToAnkiConnect(options: SyncOptions): Promise<SyncResult> {
  const ankiUrl = options.ankiUrl || "http://127.0.0.1:8765";
  const timeoutMs = options.timeoutMs ?? 2000;

  // 1. Always export TSV as permanent local record
  let exportRes: ExportResult | undefined;
  try {
    exportRes = await exportDeckTsv({
      cards: options.cards,
      nodes: options.nodes,
      lessonPath: options.lessonPath,
      topic: options.topic,
      slug: options.slug,
      deckName: options.deckName,
      baseDir: options.baseDir,
      outDir: options.decksDir
    });
  } catch (err: any) {
    // Non-fatal: a cards-only sync has nothing to export. Say so anyway — this also catches a
    // genuine write failure (permissions, bad --out), which used to vanish while the run
    // reported success with an undefined TSV path.
    console.warn(`[anki-sync] TSV export skipped: ${err.message}`);
  }

  let cards: AnkiCard[] = options.cards || [];
  let topic = options.topic || (exportRes ? exportRes.slug : "Lesson");
  let deckName = options.deckName || (exportRes ? exportRes.deckName : formatDefaultDeckName(topic));

  if (cards.length === 0 && options.lessonPath) {
    const extracted = extractCardsFromLesson(options.lessonPath);
    cards = extracted.cards;
    topic = extracted.metadata.topic;
    deckName = options.deckName || extracted.deckName;
  }

  // 2. Probe AnkiConnect reachability
  const ping = await pingAnkiConnect(ankiUrl, timeoutMs);
  if (!ping.online) {
    const warningMsg = `[anki-sync] Note: AnkiConnect offline (${ankiUrl} - ${ping.error}). Saved ${cards.length} cards to ${exportRes?.filePath || "./decks"}.`;
    return {
      synced: false,
      deckName,
      totalCards: cards.length,
      addedCount: 0,
      duplicateCount: 0,
      failedCount: cards.length,
      noteIds: [],
      tsvExportPath: exportRes?.filePath,
      warning: warningMsg,
      error: ping.error
    };
  }

  // 3. Create target deck if needed (idempotent)
  if (options.createDeckIfMissing !== false) {
    const deckRes = await callAnkiConnect(ankiUrl, "createDeck", { deck: deckName }, timeoutMs);
    if (deckRes.error) {
      return {
        synced: false,
        deckName,
        totalCards: cards.length,
        addedCount: 0,
        duplicateCount: 0,
        failedCount: cards.length,
        noteIds: [],
        tsvExportPath: exportRes?.filePath,
        error: deckRes.error,
        warning: `[anki-sync] Failed to create deck '${deckName}': ${deckRes.error}`
      };
    }
  }

  // 4. Batch build notes payload
  const models = await resolveNoteModels(ankiUrl, timeoutMs);
  const notesPayload: AnkiConnectNote[] = cards.map((card): AnkiConnectNote => {
    if (card.type === "basic") {
      const fields: AnkiConnectNoteFields = {
        [models.basic.fields[0]]: card.front,
        [models.basic.fields[1]]: card.back
      };
      return {
        deckName,
        modelName: models.basic.name,
        fields,
        tags: card.tags,
        options: {
          allowDuplicate: options.allowDuplicates ?? false,
          duplicateScope: "deck"
        }
      };
    } else {
      const fields: AnkiConnectNoteFields = {
        [models.cloze.fields[0]]: card.text,
        [models.cloze.fields[1]]: card.extra || ""
      };
      return {
        deckName,
        modelName: models.cloze.name,
        fields,
        tags: card.tags,
        options: {
          allowDuplicate: options.allowDuplicates ?? false,
          duplicateScope: "deck"
        }
      };
    }
  });

  // 5. Send batch addNotes request
  const notesRes = await callAnkiConnect<(number | null)[]>(
    ankiUrl,
    "addNotes",
    { notes: notesPayload },
    timeoutMs * 2
  );

  if (notesRes.error) {
    return {
      synced: false,
      deckName,
      totalCards: cards.length,
      addedCount: 0,
      duplicateCount: 0,
      failedCount: cards.length,
      noteIds: [],
      tsvExportPath: exportRes?.filePath,
      error: notesRes.error,
      warning: `[anki-sync] addNotes failed: ${notesRes.error}`
    };
  }

  const noteIds = notesRes.result || [];
  let addedCount = 0;
  let duplicateCount = 0;

  for (const id of noteIds) {
    if (typeof id === "number") {
      addedCount++;
    } else {
      duplicateCount++;
    }
  }

  return {
    synced: true,
    deckName,
    totalCards: cards.length,
    addedCount,
    duplicateCount,
    failedCount: 0,
    noteIds,
    tsvExportPath: exportRes?.filePath
  };
}

// ────────────────────────────────────────────────────────────────────────────
// CLI Subcommand Handler
// ────────────────────────────────────────────────────────────────────────────

export async function runCli(argv: string[]): Promise<void> {
  const command = argv[0] || "--help";

  function getArg(flag: string): string | undefined {
    const idx = argv.indexOf(flag);
    if (idx !== -1 && idx + 1 < argv.length) {
      return argv[idx + 1];
    }
    return undefined;
  }

  function hasFlag(flag: string): boolean {
    return argv.includes(flag);
  }

  if (command === "--help" || command === "-h" || command === "help") {
    console.log(`
${APP_NAME} Anki Sync & Deck Exporter

Usage:
  npx tsx extensions/anki-sync.ts <command> [options]

Commands:
  export     Export cards to ./decks/<slug>.tsv (lesson slug with --lesson, else topic slug)
  sync       Synchronize cards to local AnkiConnect (http://127.0.0.1:8765)
  extract    Extract cards from an HTML lesson file as JSON
  ping       Test connection to local AnkiConnect server

Options:
  --lesson <path>       Path to standalone HTML lesson file
  --topic <name>        Topic or subject title
  --slug <slug>         Topic slug
  --deck <name>         Target Anki deck name (default: ${DECK_ROOT}::<Topic>)
  --data <json>         Direct JSON payload containing cards or lessonDoc
  --stdin               Read JSON payload from stdin
  --out <file>          Custom output TSV file path
  --base-dir <dir>      Workspace root directory
  --anki-url <url>      AnkiConnect URL (default: http://127.0.0.1:8765)
  --timeout <ms>        Network timeout in milliseconds (default: 2000)
  --allow-duplicates    Allow duplicate cards in AnkiConnect
`);
    return;
  }

  if (command === "ping") {
    const ankiUrl = getArg("--anki-url") || "http://127.0.0.1:8765";
    const timeout = parseInt(getArg("--timeout") || "2000", 10);
    const result = await pingAnkiConnect(ankiUrl, timeout);

    if (result.online) {
      console.log(`✅ AnkiConnect is ONLINE (${ankiUrl}) — Version ${result.version} (${result.responseTimeMs}ms)`);
    } else {
      console.log(`⚠️ AnkiConnect is OFFLINE (${ankiUrl}): ${result.error} (${result.responseTimeMs}ms)`);
    }
    return;
  }

  // Helper to read payload
  async function readPayload(): Promise<any> {
    const dataArg = getArg("--data");
    if (dataArg) {
      return JSON.parse(dataArg);
    }
    if (hasFlag("--stdin")) {
      const chunks: Buffer[] = [];
      for await (const chunk of process.stdin) {
        chunks.push(Buffer.from(chunk));
      }
      const raw = Buffer.concat(chunks).toString("utf-8");
      return JSON.parse(raw);
    }
    return null;
  }

  if (command === "extract") {
    const lessonPath = getArg("--lesson");
    if (!lessonPath) {
      console.error("Error: --lesson <path> is required for extract command");
      process.exit(1);
    }
    const extracted = extractCardsFromLesson(lessonPath);
    console.log(JSON.stringify(extracted, null, 2));
    return;
  }

  if (command === "export") {
    const payload = await readPayload();
    const lessonPath = getArg("--lesson");
    const topic = getArg("--topic") || payload?.topic;
    const slug = getArg("--slug") || payload?.slug;
    const out = getArg("--out");
    const baseDir = getArg("--base-dir");

    const cards = payload?.cards;
    const nodes = payload?.nodes;

    const result = await exportDeckTsv({
      cards,
      nodes,
      lessonPath,
      topic,
      slug,
      outFile: out,
      baseDir
    });

    console.log(`✅ Exported ${result.cardCount} cards to ${result.filePath}`);
    return;
  }

  if (command === "sync") {
    const payload = await readPayload();
    const lessonPath = getArg("--lesson");
    const topic = getArg("--topic") || payload?.topic;
    const slug = getArg("--slug") || payload?.slug;
    const deck = getArg("--deck") || payload?.deck;
    const ankiUrl = getArg("--anki-url");
    const timeout = getArg("--timeout") ? parseInt(getArg("--timeout")!, 10) : undefined;
    const baseDir = getArg("--base-dir");
    const allowDuplicates = hasFlag("--allow-duplicates");

    const cards = payload?.cards;
    const nodes = payload?.nodes;

    const result = await syncToAnkiConnect({
      cards,
      nodes,
      lessonPath,
      topic,
      slug,
      deckName: deck,
      ankiUrl,
      timeoutMs: timeout,
      baseDir,
      allowDuplicates
    });

    if (result.synced) {
      console.log(`✅ Synced ${result.addedCount} cards to deck '${result.deckName}' (${result.duplicateCount} duplicates skipped)`);
      if (result.tsvExportPath) {
        console.log(`📁 Backup TSV saved to ${result.tsvExportPath}`);
      }
    } else {
      console.log(`⚠️ Sync incomplete: ${result.warning || result.error}`);
      if (result.tsvExportPath) {
        console.log(`📁 Deck TSV exported to ${result.tsvExportPath}`);
      }
    }
    return;
  }

  console.error(`Unknown command: ${command}`);
  process.exit(1);
}

// ────────────────────────────────────────────────────────────────────────────
// Entry Point
// ────────────────────────────────────────────────────────────────────────────

const isMain = process.argv[1] && (
  process.argv[1] === fileURLToPath(import.meta.url) ||
  process.argv[1].endsWith("anki-sync.ts") ||
  process.argv[1].endsWith("anki-sync.js")
);

if (isMain) {
  enterDataRoot();
  runCli(process.argv.slice(2)).catch((err) => {
    console.error("Fatal error:", err);
    process.exit(1);
  });
}
