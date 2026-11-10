// The course index: one line per course of what he already knows. It replaces the injected
// concept lists, so the prompt stays the same length however big the knowledge graph grows.
// Counts come from lesson nodes (bounded by the lessons on disk), never from graph propositions.
import { listLessons } from "./lesson.ts";
import { buildKnowledgeMap } from "./knowledge.ts";
import { readNodeLines } from "./graph/store.ts";
import { readCurriculum, matchCurriculum, type Curriculum } from "../../extensions/chapter.ts";
import { STANDALONE_COURSE_SLUG, STANDALONE_COURSE_TITLE } from "../../extensions/anki-sync.ts";
import type { MapNode, MapNodeStatus } from "../shared/protocol.ts";

export async function courseIndexBlock(dataRoot: string): Promise<string> {
  // Every read is guarded: a broken file must not stop a session from starting.
  let lessons: ReturnType<typeof listLessons> = [];
  try {
    lessons = listLessons(dataRoot);
  } catch {
    lessons = [];
  }

  let curriculum: Curriculum = { chapters: [] };
  try {
    curriculum = readCurriculum({ baseDir: dataRoot });
  } catch {
    /* malformed curriculum.json: no plan, the index still renders */
  }

  let nodes: MapNode[] = [];
  try {
    nodes = (await buildKnowledgeMap(dataRoot)).nodes;
  } catch {
    nodes = [];
  }

  let graphEmpty = true;
  try {
    graphEmpty = readNodeLines(dataRoot).length === 0;
  } catch {
    graphEmpty = true;
  }

  const match = matchCurriculum(curriculum, lessons.map((l) => l.summary));

  const counts = (slug: string) => {
    const out: Record<MapNodeStatus, number> = { solid: 0, shaky: 0, taught: 0, missed: 0 };
    for (const n of nodes) if (n.course === slug) out[n.status]++;
    return out;
  };

  const lastLesson = (slug: string) => {
    let last = "";
    for (const l of lessons) {
      if (l.summary.course !== slug) continue;
      const d = (l.doc.metadata.dateModified || l.doc.metadata.dateCreated || "").slice(0, 10);
      if (d > last) last = d;
    }
    return last || "-";
  };

  const row = (title: string, slug: string, lessonsText: string) => {
    const c = counts(slug);
    return (
      `- ${title} (courses/${slug}) | lessons ${lessonsText} | ` +
      `solido ${c.solid}, fragile ${c.shaky}, insegnato ${c.taught}, mancato ${c.missed} | ` +
      `last lesson ${lastLesson(slug)}`
    );
  };

  const rows: string[] = [];
  const planned = new Set(match.chapters.map((c) => c.slug));

  for (const c of match.chapters) {
    if (c.slug === STANDALONE_COURSE_SLUG) continue;
    const done = c.taught + match.orphans.filter((l) => l.course === c.slug).length;
    rows.push(row(c.title, c.slug, `${done}/${c.lessons.length}`));
  }

  const extraCourses = [...new Set(match.orphans.map((l) => l.course))]
    .filter((slug) => slug !== STANDALONE_COURSE_SLUG && !planned.has(slug))
    .sort();
  for (const slug of extraCourses) {
    const done = match.orphans.filter((l) => l.course === slug).length;
    rows.push(row(slug, slug, `${done} (no plan)`));
  }

  const standalone = lessons.filter((l) => l.summary.course === STANDALONE_COURSE_SLUG).length;
  rows.push(row(STANDALONE_COURSE_TITLE, STANDALONE_COURSE_SLUG, `${standalone} (no plan)`));

  return [
    "# Course index",
    "",
    "What he already knows, one line per course (concept counts by status). Look concepts up with `mcp__app__search_concepts`, `mcp__app__open_concept` and `mcp__app__browse_concepts`; their details are not in this prompt.",
    "",
    ...rows,
    ...(graphEmpty ? ["", "The knowledge graph is empty: the concept tools return nothing yet."] : []),
  ].join("\n");
}
