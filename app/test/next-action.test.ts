import { test } from "node:test";
import assert from "node:assert/strict";
import { findOpenSession, nextAction, recommend, type NextAction } from "../web/src/next-action.ts";
import type { CourseLesson, CourseOverview, SessionConfig, SessionSummary } from "../shared/protocol.ts";

const taught = (title: string, inProgress = false): CourseLesson => ({ title, slug: title, lessonPath: `courses/c/lessons/${title}.html`, inProgress });
const todo = (title: string): CourseLesson => ({ title, slug: title });
const course = (lessons: CourseLesson[], orphans: CourseLesson[] = []): CourseOverview => ({
  slug: "c", title: "Corso", lessons, orphans, hasExercises: false, hasMission: true,
});

test("lesson in progress wins over the next planned one", () => {
  const a = nextAction(course([taught("A"), taught("B", true), todo("C")]));
  assert.equal(a.kind, "continue");
  assert.equal(a.heading, "Continua");
  assert.equal(a.lessonTitle, "B");
  assert.deepEqual(a.config.target, { mode: "existing", lessonPath: "courses/c/lessons/B.html" });
});

test("an in-progress orphan is continued too", () => {
  const a = nextAction(course([todo("A")], [taught("X", true)]));
  assert.equal(a.kind, "continue");
  assert.equal(a.heading, "Continua");
  assert.equal(a.lessonTitle, "X");
});

test("first planned lesson not taught, numbered by plan position", () => {
  const a = nextAction(course([taught("A"), todo("B"), todo("C")]));
  assert.equal(a.kind, "start");
  assert.equal(a.heading, "Inizia la lezione 2");
  assert.equal(a.lessonTitle, "B");
  assert.deepEqual(a.config, { kind: "teach", course: "c", target: { mode: "planned", title: "B" } });
});

test("everything taught (or nothing planned): plan the next lessons", () => {
  for (const c of [course([taught("A")]), course([])]) {
    const a = nextAction(c);
    assert.equal(a.kind, "plan");
    assert.equal(a.heading, "Pianifica le prossime lezioni");
    assert.equal(a.lessonTitle, undefined);
    assert.deepEqual(a.config, { kind: "plan", course: "c", chapterTitle: "Corso" });
  }
});

test("recommend: anki loading, offline or 0 due -> lesson", () => {
  const start: NextAction = { kind: "start", heading: "Inizia", lessonTitle: "B", config: planned };
  assert.equal(recommend(start, undefined), "lesson");
  assert.equal(recommend(start, { online: false, due: 9 }), "lesson");
  assert.equal(recommend(start, { online: true, due: 0 }), "lesson");
});

test("recommend: due 1 with start or plan -> review", () => {
  const start: NextAction = { kind: "start", heading: "Inizia", lessonTitle: "B", config: planned };
  const plan: NextAction = { kind: "plan", heading: "Pianifica", config: { kind: "plan", course: "c" } };
  assert.equal(recommend(start, { online: true, due: 1 }), "review");
  assert.equal(recommend(plan, { online: true, due: 1 }), "review");
});

test("recommend: continue + due <= 5 -> lesson; continue + due 6 -> review", () => {
  const cont: NextAction = { kind: "continue", heading: "Continua", lessonTitle: "B", config: planned };
  assert.equal(recommend(cont, { online: true, due: 5 }), "lesson");
  assert.equal(recommend(cont, { online: true, due: 6 }), "review");
});

const sess = (config: SessionConfig, status: SessionSummary["status"] = "idle", lessonPath?: string): SessionSummary => ({
  id: "s1", config, title: "t", createdAt: "", updatedAt: "", status, lessonPath, usage: {} as SessionSummary["usage"],
});
const planned: SessionConfig = { kind: "teach", course: "c", target: { mode: "planned", title: "B" } };

test("findOpenSession: same planned title or same lesson file is reused, closed or other course is not", () => {
  assert.equal(findOpenSession([sess(planned)], planned)?.id, "s1");
  assert.equal(findOpenSession([sess(planned, "closed")], planned), undefined);
  assert.equal(findOpenSession([sess({ ...planned, course: "d" })], planned), undefined);
  const existing: SessionConfig = { kind: "teach", course: "c", target: { mode: "existing", lessonPath: "courses/c/lessons/B.html" } };
  assert.equal(findOpenSession([sess(planned, "running", "courses/c/lessons/B.html")], existing)?.id, "s1");
  assert.equal(findOpenSession([sess(planned)], { kind: "teach", course: "c", target: { mode: "free" } }), undefined);
});
