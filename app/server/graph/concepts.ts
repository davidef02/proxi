// Read-only concept catalog for the app tools: the graph's propositions joined with the course and
// birth lesson they came from. Built per tool call, so the system prompt never carries the list.
import { readSnapshot } from "./store.ts";
import { buildKnowledgeMap } from "../knowledge.ts";
import { readCurriculum } from "../../../extensions/chapter.ts";
import { STANDALONE_COURSE_SLUG, STANDALONE_COURSE_TITLE } from "../../../extensions/anki-sync.ts";
import { birthSessionOf, fuzzyScore } from "../../shared/graph/index.ts";
import type { GraphNode, GraphSnapshot } from "../../shared/graph/contracts.ts";
import type { MapNodeStatus } from "../../shared/protocol.ts";
import type { ToolOutcome } from "../types.ts";
import { EMBEDDING, type RankResult } from "./embeddings.ts";

export const SEARCH_LIMIT = 10;
export const BROWSE_LIMIT = 100;

export const SEMANTIC_UNAVAILABLE =
  "(Semantic search unavailable: the embedding model is not downloaded yet. Lexical results only.)";
/** A lexical match outranks a bare cosine: the model is good at meaning, not at exact names. */
export const LEXICAL_BONUS = 0.15;

export type Ranker = (query: string, nodes: GraphNode[]) => Promise<RankResult>;

export const STATUS_IT: Record<MapNodeStatus, string> = {
  solid: "solido",
  shaky: "fragile",
  taught: "insegnato",
  missed: "mancato",
};

export interface ConceptCatalog {
  snap: GraphSnapshot;
  /** Curriculum chapter slugs in plan order, standalone course excluded. */
  courseOrder: string[];
  /** slug -> title; the standalone course is always present. */
  courseTitles: Record<string, string>;
  /** "<course>/<lesson file stem>#<nodeId>" -> MapNode.key (same key rule as provenanceStatus). */
  mapKeys: Map<string, string>;
}

export async function loadCatalog(dataRoot: string): Promise<ConceptCatalog> {
  let curriculum: ReturnType<typeof readCurriculum> = { chapters: [] };
  try {
    curriculum = readCurriculum({ baseDir: dataRoot });
  } catch {
    /* malformed curriculum: course dirs still name themselves */
  }

  const mapKeys = new Map<string, string>();
  try {
    for (const n of (await buildKnowledgeMap(dataRoot)).nodes) {
      const stem = n.lessonPath.split(/[\\/]/).pop()!.replace(/\.html$/, "");
      mapKeys.set(`${n.course}/${stem}#${n.nodeId}`, n.key);
    }
  } catch {
    /* unreadable lessons: no MapNode keys to offer */
  }

  // not caught: an unreadable graph must surface as a tool error, not as "no concepts"
  const snap = await readSnapshot(dataRoot);

  const courseOrder = curriculum.chapters.map((c) => c.slug).filter((s) => s !== STANDALONE_COURSE_SLUG);
  const courseTitles: Record<string, string> = {};
  for (const c of curriculum.chapters) courseTitles[c.slug] = c.title;
  // the standalone course is always titled by the constant, whatever the plan says
  courseTitles[STANDALONE_COURSE_SLUG] = STANDALONE_COURSE_TITLE;

  return { snap, courseOrder, courseTitles, mapKeys };
}

// birthSessionOf is null when a node has no provenance: never split the raw result
export function birthOf(n: GraphNode): string {
  return birthSessionOf(n) ?? "";
}

export const courseOf = (n: GraphNode): string => birthOf(n).split("/")[0];

export const courseTitle = (cat: ConceptCatalog, slug: string): string =>
  slug === "" ? "(no lesson)" : cat.courseTitles[slug] ?? slug;

export const lessonLabel = (cat: ConceptCatalog, chatId: string): string =>
  chatId === "" ? "-" : cat.snap.sessions[chatId] ?? chatId;

export const statusOf = (n: GraphNode): string => STATUS_IT[n.status ?? "taught"];

export const conceptLine = (cat: ConceptCatalog, n: GraphNode): string =>
  "- " + [n.id, n.name, statusOf(n), courseTitle(cat, courseOf(n)), lessonLabel(cat, birthOf(n))].join(" | ");

/** Plan order first, other course dirs alphabetically, the standalone course last. */
export function compareCourses(cat: ConceptCatalog, a: string, b: string): number {
  const ia = cat.courseOrder.indexOf(a);
  const ib = cat.courseOrder.indexOf(b);
  if (ia >= 0 || ib >= 0) {
    if (ia < 0) return 1;
    if (ib < 0) return -1;
    return ia - ib;
  }
  if (a === STANDALONE_COURSE_SLUG || b === STANDALONE_COURSE_SLUG) {
    if (a === STANDALONE_COURSE_SLUG) return b === STANDALONE_COURSE_SLUG ? 0 : 1;
    return -1;
  }
  return a.localeCompare(b);
}

export async function searchConcepts(
  cat: ConceptCatalog,
  query: string,
  limit = SEARCH_LIMIT,
  ranker?: Ranker,
): Promise<ToolOutcome> {
  if (!query.trim()) return { text: "Empty query: pass words from the concept's name or statement.", isError: true };
  const cap = Math.max(1, Math.min(SEARCH_LIMIT, Math.floor(limit)));
  const scored: { n: GraphNode; score: number }[] = [];
  const r = ranker ? await ranker(query, cat.snap.propositions) : undefined;
  if (r?.ok) {
    const cosine = new Map(r.hits.map((h) => [h.id, h.cosine]));
    for (const n of cat.snap.propositions) {
      const c = cosine.get(n.id) ?? 0;
      const lexical = fuzzyScore(query, n.name, n.statement, ...n.aliases) !== null;
      if (c >= EMBEDDING.minScore || lexical) scored.push({ n, score: c + (lexical ? LEXICAL_BONUS : 0) });
    }
  } else {
    for (const n of cat.snap.propositions) {
      const score = fuzzyScore(query, n.name, n.statement, ...n.aliases);
      if (score !== null) scored.push({ n, score });
    }
  }
  // stable sort: equal scores keep ledger order
  scored.sort((a, b) => b.score - a.score);
  const hits = scored.slice(0, cap);
  const note = ranker && !r?.ok ? "\n" + SEMANTIC_UNAVAILABLE : "";
  if (hits.length === 0) return { text: `No concept matches "${query}".` + note };
  return {
    text:
      `${hits.length} concept(s), best first (id | name | status | course | lesson):\n` +
      hits.map((h) => conceptLine(cat, h.n)).join("\n") +
      note,
  };
}

function conceptRef(cat: ConceptCatalog, id: string): string {
  const p = cat.snap.propositions.find((n) => n.id === id);
  return p ? `${p.id} | ${p.name}` : id;
}

export function openConcept(cat: ConceptCatalog, id: string): ToolOutcome {
  const node = cat.snap.propositions.find((n) => n.id === id);
  if (!node) {
    return { text: `Unknown concept id "${id}". Use search_concepts or browse_concepts to find ids.`, isError: true };
  }

  const lines = [`# ${node.name} (${node.id})`, `Statement: ${node.statement}`, `Status: ${statusOf(node)}`];
  if (node.aliases.length) lines.push(`Aliases: ${node.aliases.join(", ")}`);

  const courses: string[] = [];
  const lessons: string[] = [];
  const keys: string[] = [];
  for (const p of node.provenance) {
    const course = p.chatId.split("/")[0];
    if (!courses.includes(course)) courses.push(course);
    if (!lessons.includes(p.chatId)) lessons.push(p.chatId);
    if (p.messageId === "") continue;
    const key = cat.mapKeys.get(`${p.chatId}#${p.messageId}`);
    if (key !== undefined && !keys.includes(key)) keys.push(key);
  }
  lines.push(`Courses: ${courses.length ? courses.map((c) => `${courseTitle(cat, c)} (${c})`).join("; ") : "(none)"}`);
  lines.push(`Lessons: ${lessons.length ? lessons.map((l) => `${lessonLabel(cat, l)} (${l})`).join("; ") : "(none)"}`);
  lines.push(keys.length ? `MapNode keys (for problem.nodeKeys): ${keys.join(", ")}` : "MapNode keys: none");

  const premises: string[] = [];
  for (const c of cat.snap.chains) {
    if (c.conclusion !== node.id) continue;
    for (const p of c.premises) if (!premises.includes(p)) premises.push(p);
  }
  lines.push(premises.length ? "Premises:" : "Premises: (none)");
  for (const p of premises) lines.push(`- ${conceptRef(cat, p)}`);

  const consequences: string[] = [];
  for (const c of cat.snap.chains) {
    if (c.premises.includes(node.id) && !consequences.includes(c.conclusion)) consequences.push(c.conclusion);
  }
  lines.push(consequences.length ? "Consequences:" : "Consequences: (none)");
  for (const c of consequences) lines.push(`- ${conceptRef(cat, c)}`);

  return { text: lines.join("\n") };
}

export function browseConcepts(cat: ConceptCatalog, course?: string, lesson?: string): ToolOutcome {
  if (!course) {
    if (lesson) return { text: "Pass course with lesson.", isError: true };
    const counts = new Map<string, number>();
    for (const n of cat.snap.propositions) {
      const c = courseOf(n);
      counts.set(c, (counts.get(c) ?? 0) + 1);
    }
    if (counts.size === 0) return { text: "The knowledge graph is empty: no concept yet." };
    const rows = [...counts.entries()]
      .sort(([a], [b]) => compareCourses(cat, a, b))
      .map(([slug, k]) => `- ${slug} | ${courseTitle(cat, slug)} | ${k} concepts`);
    return { text: ["Courses (slug | title | concepts):", ...rows].join("\n") };
  }

  const members = cat.snap.propositions.filter((n) => courseOf(n) === course);
  if (!lesson) {
    if (members.length === 0) {
      const withConcepts = [...new Set(cat.snap.propositions.map((n) => courseOf(n)))].sort((a, b) =>
        compareCourses(cat, a, b),
      );
      return {
        text: `No concepts in course "${course}". Courses with concepts: ${withConcepts.length ? withConcepts.join(", ") : "(none)"}.`,
        isError: true,
      };
    }
    const counts = new Map<string, number>();
    for (const n of members) {
      const chatId = birthOf(n);
      counts.set(chatId, (counts.get(chatId) ?? 0) + 1);
    }
    const rows = [...counts.entries()]
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([chatId, k]) => `- ${chatId} | ${lessonLabel(cat, chatId)} | ${k} concepts`);
    return { text: [`Lessons of ${courseTitle(cat, course)} (lesson | label | concepts):`, ...rows].join("\n") };
  }

  // lesson may be the full chatId or just the stem
  const stem = lesson.startsWith(`${course}/`) ? lesson.slice(course.length + 1) : lesson;
  const chatId = `${course}/${stem}`;
  const born = members.filter((n) => birthOf(n) === chatId);
  if (born.length === 0) {
    return {
      text: `No concepts born in lesson "${chatId}". Call browse_concepts with course "${course}" to list its lessons.`,
      isError: true,
    };
  }
  const lines = [
    `Concepts of ${lessonLabel(cat, chatId)} (id | name | status | course | lesson):`,
    ...born.slice(0, BROWSE_LIMIT).map((n) => conceptLine(cat, n)),
  ];
  if (born.length > BROWSE_LIMIT) lines.push(`(+${born.length - BROWSE_LIMIT} more: use search_concepts)`);
  return { text: lines.join("\n") };
}
