// The one primary action of a course page. Pure: no I/O, easy to test (app/test/next-action.test.ts).
import type { CourseLesson, CourseOverview, SessionConfig, SessionSummary } from "../../shared/protocol.ts";

export interface NextAction {
  kind: "continue" | "start" | "plan";
  /** Short action name: "Continua" | "Inizia la lezione N" | "Pianifica le prossime lezioni". */
  heading: string;
  /** Full lesson title (never truncated), absent for "plan". */
  lessonTitle?: string;
  /** Session to create when the learner clicks (model is left to the server: settings.teacherModel). */
  config: SessionConfig;
}

export function nextAction(course: CourseOverview): NextAction {
  const progress = (l: CourseLesson) => !!l.lessonPath && !!l.inProgress;
  const going = course.lessons.find(progress) ?? course.orphans.find(progress);
  if (going)
    return {
      kind: "continue",
      heading: "Continua",
      lessonTitle: going.title,
      config: { kind: "teach", course: course.slug, target: { mode: "existing", lessonPath: going.lessonPath! } },
    };
  const i = course.lessons.findIndex((l) => !l.lessonPath);
  if (i >= 0)
    return {
      kind: "start",
      heading: `Inizia la lezione ${i + 1}`,
      lessonTitle: course.lessons[i].title,
      config: { kind: "teach", course: course.slug, target: { mode: "planned", title: course.lessons[i].title } },
    };
  return {
    kind: "plan",
    heading: "Pianifica le prossime lezioni",
    config: { kind: "plan", course: course.slug, chapterTitle: course.title },
  };
}

/** Backlog at or below which an in-progress lesson still comes first. */
export const SMALL_BACKLOG = 5;

export type Recommended = "lesson" | "review";

/** Which of the two primary actions to mark "Consigliato ora". anki undefined = still loading. */
export function recommend(next: NextAction, anki?: { online: boolean; due?: number }): Recommended {
  const due = anki?.online ? anki.due ?? 0 : 0;
  if (due === 0) return "lesson";
  if (next.kind === "continue" && due <= SMALL_BACKLOG) return "lesson";
  return "review";
}

/** A live (not closed) teach session already working on the same course + target, so the primary action resumes it
 *  instead of spawning a second claude process on the same lesson. Only teach sessions are matched. */
export function findOpenSession(sessions: SessionSummary[], config: SessionConfig): SessionSummary | undefined {
  const t = config.target;
  if (config.kind !== "teach" || !t || t.mode === "free") return undefined;
  return sessions.find((s) => {
    if (s.status === "closed" || s.config.kind !== "teach" || s.config.course !== config.course) return false;
    const st = s.config.target;
    if (t.mode === "existing") return s.lessonPath === t.lessonPath || (st?.mode === "existing" && st.lessonPath === t.lessonPath);
    return st?.mode === "planned" && st.title === t.title;
  });
}
