/**
 * chapter.ts — The plan: curriculum.json, and its join onto what has been taught.
 *
 * Part of the Proxi teaching engine.
 * A chapter is a course: `courses/<slug>/`, holding its own lessons, reference sheets and
 * learning records. The chapter is the only place a lesson that does not yet exist can be
 * named, so this module owns curriculum.json end to end: writing it (/plan), reading it,
 * and matching its entries against the lessons actually on disk. The match lives here and
 * nowhere else — the homepage renders it, the CLI prints it, and both see the same orphans.
 *
 * Lesson numbering is per course (docker/0001 and app-cache/0001 both exist), so a lesson's
 * identity is the pair (course, slug), never the number.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { lessonSlug, slugify } from "./slug.ts";
import type { LessonSummary } from "./homepage.ts";

/** A lesson named by the plan. It has no file until it has been taught. */
export interface PlannedLesson {
  title: string;
  slug: string;
  plannedNodes?: number;
}

export interface Chapter {
  title: string;
  /** The course directory under courses/. Not always slugify(title): "App State" lives in app-cache. */
  slug: string;
  lessons: PlannedLesson[];
}

export interface Curriculum {
  chapters: Chapter[];
}

export interface CurriculumLocation {
  baseDir?: string;
  curriculumPath?: string;
}

export interface PlanChapterOptions extends CurriculumLocation {
  chapter: string;
  /** Course directory. Defaults to slugify(chapter); pass it when the directory differs. */
  slug?: string;
  /**
   * `slug` per lesson defaults to lessonSlug(title). Pass it only for a lesson whose file
   * already exists under a shorter name than its title (hand-authored lessons predate the
   * title-derived rule), otherwise the plan entry and the artifact never match.
   */
  lessons: Array<{ title: string; slug?: string; plannedNodes?: number }>;
}

export function curriculumFile(options: CurriculumLocation = {}): string {
  if (options.curriculumPath) return path.resolve(options.curriculumPath);
  return path.join(path.resolve(options.baseDir || "."), "curriculum.json");
}

export function readCurriculum(options: CurriculumLocation = {}): Curriculum {
  const filePath = curriculumFile(options);
  if (!fs.existsSync(filePath)) return { chapters: [] };
  const parsed = JSON.parse(fs.readFileSync(filePath, "utf-8"));
  // Refuse a wrong shape instead of degrading it to []: planChapter writes the result back, so
  // silently reading a malformed file as "no chapters" would erase every chapter in it.
  if (parsed?.chapters !== undefined && !Array.isArray(parsed.chapters)) {
    throw new Error('Malformed ' + filePath + ': "chapters" must be an array');
  }
  const chapters: Chapter[] = (parsed?.chapters || []).map((c: Chapter) => ({
    ...c,
    lessons: Array.isArray(c?.lessons) ? c.lessons : []
  }));
  return { chapters };
}

/**
 * Upserts one chapter's lesson list. Re-planning a chapter replaces its lessons
 * wholesale: the plan is mutable by design, and git keeps the history.
 */
export function planChapter(options: PlanChapterOptions): Curriculum {
  if (!options.chapter || !options.chapter.trim()) {
    throw new Error("A chapter title is required");
  }
  const curriculum = readCurriculum(options);
  const chapter: Chapter = {
    title: options.chapter.trim(),
    slug: options.slug ? slugify(options.slug) : slugify(options.chapter),
    lessons: (options.lessons || []).map((l) => {
      if (!l.title || !l.title.trim()) throw new Error("Every planned lesson needs a title");
      const planned: PlannedLesson = {
        title: l.title.trim(),
        slug: l.slug ? slugify(l.slug) : lessonSlug(l.title)
      };
      if (typeof l.plannedNodes === "number") planned.plannedNodes = l.plannedNodes;
      return planned;
    })
  };

  // Two entries with the same slug would both match the one artifact on disk, and the chapter
  // would read as complete with a lesson never taught. Titles can collide through slugify.
  const seen = new Map<string, string>();
  for (const planned of chapter.lessons) {
    const clash = seen.get(planned.slug);
    if (clash) {
      throw new Error(
        `Two planned lessons share the slug "${planned.slug}": "${clash}" and "${planned.title}". ` +
        "Give them distinct titles, or pass an explicit slug."
      );
    }
    seen.set(planned.slug, planned.title);
  }

  const existing = curriculum.chapters.findIndex((c) => c.slug === chapter.slug);
  if (existing >= 0) {
    curriculum.chapters[existing] = chapter;
  } else {
    curriculum.chapters.push(chapter);
  }

  const filePath = curriculumFile(options);
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify(curriculum, null, 2) + "\n", "utf-8");
  return curriculum;
}

// ────────────────────────────────────────────────────────────────────────────
// The join: plan entries x taught lessons
// ────────────────────────────────────────────────────────────────────────────

/** A plan entry, with the taught lesson claiming it if there is one. */
export interface MatchedLesson {
  planned: PlannedLesson;
  lesson?: LessonSummary;
}

export interface MatchedChapter {
  title: string;
  slug: string;
  lessons: MatchedLesson[];
  taught: number;
}

export interface CurriculumMatch {
  chapters: MatchedChapter[];
  /** Every taught lesson, matched or not. */
  taught: LessonSummary[];
  /** Taught lessons no plan entry claims. Computed once, here. */
  orphans: LessonSummary[];
}

/**
 * Joins the plan onto the lessons on disk. A plan entry is claimed only by a lesson in
 * that chapter's own course directory, so two courses may hold same-named lessons without
 * stealing each other's entries.
 */
export function matchCurriculum(curriculum: Curriculum, lessons: LessonSummary[]): CurriculumMatch {
  const key = (l: LessonSummary) => l.course + "/" + l.slug;
  const byCourseAndSlug = new Map<string, LessonSummary>();
  for (const lesson of lessons) {
    // First wins, and the shadowed file stays in `orphans` below: two artifacts with one slug is a
    // broken state, and hiding one of them from the board entirely is how it stays unnoticed.
    if (!byCourseAndSlug.has(key(lesson))) byCourseAndSlug.set(key(lesson), lesson);
  }
  const claimed = new Set<LessonSummary>();

  const chapters = curriculum.chapters.map((chapter) => {
    const matched = chapter.lessons.map((planned) => {
      const lesson = byCourseAndSlug.get(chapter.slug + "/" + planned.slug);
      if (lesson) claimed.add(lesson);
      return { planned, lesson };
    });
    return {
      title: chapter.title,
      slug: chapter.slug,
      lessons: matched,
      taught: matched.filter((m) => m.lesson).length
    };
  });

  return {
    chapters,
    taught: lessons,
    orphans: lessons.filter((l) => !claimed.has(l))
  };
}

/** The plan as plain text, for an agent that needs to read it without parsing JSON. */
export function formatCurriculum(match: CurriculumMatch): string {
  const line = (m: MatchedLesson) =>
    "  " +
    (m.lesson ? "[x] " + String(m.lesson.lessonNumber).padStart(4, "0") : "[ ] ----") +
    "  " +
    (m.lesson ? m.lesson.title : m.planned.title) +
    (m.lesson
      ? m.lesson.nodeCount === 0
        ? " (prosa)"
        : " (" + m.lesson.nodeCount + (m.lesson.nodeCount === 1 ? " nodo)" : " nodi)")
      : m.planned.plannedNodes
        ? " (" + m.planned.plannedNodes + " nodi previsti)"
        : "");

  const blocks = match.chapters.map(
    (c) =>
      c.title + " (courses/" + c.slug + ") — " + c.taught + "/" + c.lessons.length + "\n" +
      c.lessons.map(line).join("\n")
  );
  if (match.orphans.length) {
    blocks.push(
      "Fuori dal piano\n" +
        match.orphans
          .map((l) =>
            line({ planned: { title: l.title, slug: l.slug }, lesson: l }) + "  [courses/" + l.course + "]"
          )
          .join("\n")
    );
  }
  return blocks.length ? blocks.join("\n\n") : "Nessun capitolo pianificato.";
}
