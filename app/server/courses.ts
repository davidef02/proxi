import * as fs from "node:fs";
import * as path from "node:path";
import { readCurriculum, matchCurriculum } from "../../extensions/chapter.ts";
import { STANDALONE_COURSE_SLUG } from "../../extensions/anki-sync.ts";
import { hasData } from "../../extensions/data-root.ts";
import type { LessonSummary } from "../../extensions/homepage.ts";
import { listLessons, type Lesson } from "./lesson.ts";
import type { CourseLesson, CourseOverview } from "../shared/protocol.ts";

export { STANDALONE_COURSE_SLUG };
/** MISSION.md written into courses/lezioni-singole/ when the container is created. */
export const STANDALONE_MISSION = [
  "# Lezioni singole",
  "",
  "## Why",
  "",
  "Studiare argomenti liberi e autocontenuti fuori dai corsi strutturati, mantenendo la stessa profondità dai primi principi.",
  "",
  "## Success looks like",
  "",
  "Ogni lezione singola installa un'intuizione solida e verificata sui concetti scelti dallo studente.",
  "",
  "## Constraints",
  "",
  "- Una lezione copre un solo argomento ben delimitato.",
  "",
  "## Out of scope",
  "",
  "- Percorsi multi-lezione: se il tema richiede un intero corso, proporre una sessione di pianificazione.",
  "",
].join("\n");

/**
 * Creates courses/lezioni-singole/ (lessons, reference, learning-records) + MISSION.md, create-only.
 * On read paths it stays off a data folder nobody has touched yet: creating courses/ there would flip
 * hasData() to true and hide the first-run sync.
 */
export function ensureStandaloneCourse(repoRoot: string, opts?: { force?: boolean }): void {
  const root = path.resolve(repoRoot);
  if (!opts?.force && !hasData(root) && !fs.existsSync(path.join(root, "courses"))) return;
  const dir = path.join(root, "courses", STANDALONE_COURSE_SLUG);
  for (const sub of ["lessons", "reference", "learning-records"]) fs.mkdirSync(path.join(dir, sub), { recursive: true });
  const mission = path.join(dir, "MISSION.md");
  if (!fs.existsSync(mission)) fs.writeFileSync(mission, STANDALONE_MISSION, { encoding: "utf-8" });
}

/** First paragraph under "## Why" in MISSION.md (else the first paragraph after the title), markdown stripped. */
export function missionDestination(repoRoot: string, slug: string): string | undefined {
  let md: string;
  try {
    md = fs.readFileSync(path.join(repoRoot, "courses", slug, "MISSION.md"), "utf-8");
  } catch {
    return undefined;
  }
  const body = md.split(/^## +Why\b.*$/m)[1] ?? md.replace(/^#.*$/m, "");
  const para = body.split(/\n\s*\n/).map((p) => p.trim()).find((p) => p && !p.startsWith("#"));
  if (!para) return undefined;
  const text = para.replace(/[*_`]/g, "").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1").replace(/\s+/g, " ");
  return text.length > 240 ? text.slice(0, 239).replace(/\s+\S*$/, "") + "…" : text;
}

export function toCourseLesson(root: string, lesson: Lesson): CourseLesson {
  const s = lesson.summary;
  const ref = `courses/${s.course}/reference/${s.slug}.html`;
  return {
    title: s.title,
    slug: s.slug,
    lessonPath: lesson.rel,
    lessonNumber: s.lessonNumber,
    nodeCount: s.nodeCount,
    ...(fs.existsSync(path.join(root, ref)) ? { referencePath: ref } : {}),
    inProgress: lesson.status === "inProgress",
  };
}

export async function listCourses(repoRoot: string): Promise<CourseOverview[]> {
  const root = path.resolve(repoRoot);
  ensureStandaloneCourse(root);
  const all = listLessons(root);
  const byKey = new Map(all.map((l) => [`${l.summary.course}/${l.summary.filename}`, l]));
  const match = matchCurriculum(readCurriculum({ baseDir: root }), all.map((l) => l.summary));

  const taughtInfo = (s: LessonSummary): CourseLesson =>
    toCourseLesson(root, byKey.get(`${s.course}/${s.filename}`)!);
  const flags = (slug: string) => ({
    hasExercises: fs.existsSync(path.join(root, "courses", slug, "exercises", "package.json")),
    hasMission: fs.existsSync(path.join(root, "courses", slug, "MISSION.md")),
    destination: missionDestination(root, slug),
  });

  const out: CourseOverview[] = match.chapters
    .filter((c) => c.slug !== STANDALONE_COURSE_SLUG)
    .map((c) => ({
      slug: c.slug,
      title: c.title,
      lessons: c.lessons.map((m) =>
        m.lesson
          ? { ...taughtInfo(m.lesson), title: m.planned.title, plannedNodes: m.planned.plannedNodes }
          : { title: m.planned.title, slug: m.planned.slug, plannedNodes: m.planned.plannedNodes },
      ),
      orphans: match.orphans.filter((o) => o.course === c.slug).map(taughtInfo),
      ...flags(c.slug),
    }));
  const known = new Set(out.map((c) => c.slug));
  for (const course of [...new Set(match.orphans.map((o) => o.course))]) {
    if (course === STANDALONE_COURSE_SLUG || known.has(course)) continue;
    out.push({
      slug: course,
      title: course,
      lessons: [],
      orphans: match.orphans.filter((o) => o.course === course).map(taughtInfo),
      ...flags(course),
    });
  }
  return out;
}

/** The lessons of courses/lezioni-singole/, in lesson order. Provisioning on read like listCourses. */
export async function listStandaloneLessons(repoRoot: string): Promise<CourseLesson[]> {
  const root = path.resolve(repoRoot);
  ensureStandaloneCourse(root);
  return listLessons(root)
    .filter((l) => l.summary.course === STANDALONE_COURSE_SLUG)
    .map((l) => toCourseLesson(root, l));
}
