import * as path from "node:path";
import type { ProgressRecord } from "../../shared/protocol.ts";
import { lessonAbs as resolveLesson, readLesson, slugOfLesson, type Lesson } from "../lesson.ts";
import { readProgress } from "../knowledge.ts";
import { derivePlanState } from "../planstate.ts";
import type { AppTool, ToolContext, ToolOutcome } from "../types.ts";

export const KEBAB = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

export const json = (o: unknown): ToolOutcome => ({ text: JSON.stringify(o) });
export const fail = (text: string): ToolOutcome => ({ text, isError: true });
export const CANCELLED = "The learner dismissed the question (session interrupted).";
export const isCancelled = (e: unknown) => e instanceof Error && e.message === "cancelled";

/** Erases the input type so heterogeneous tools fit in one AppTool[]. */
export const tool = <I>(t: AppTool<I>): AppTool => t as unknown as AppTool;

export const toRel = (dataRoot: string, abs: string) => path.relative(dataRoot, abs).split(path.sep).join("/");

/** courses/<course> as an absolute path; the course name is validated (it comes from config). */
export function courseDir(ctx: Pick<ToolContext, "dataRoot" | "config">, course = ctx.config.course): string {
  if (!KEBAB.test(course)) throw new Error(`Invalid course directory name "${course}".`);
  return path.join(ctx.dataRoot, "courses", course);
}

/** Absolute path of the session's lesson artifact, or undefined (the Lesson module owns the path rule). */
export function lessonAbs(ctx: ToolContext): string | undefined {
  const rel = ctx.getLessonPath();
  return rel ? resolveLesson(ctx.dataRoot, rel) : undefined;
}

/** The session's lesson, read through the Lesson module; null without a path, a file or a data island. */
export function currentLesson(ctx: ToolContext): Lesson | null {
  const rel = ctx.getLessonPath();
  return rel ? readLesson(ctx.dataRoot, rel) : null;
}

export async function lessonProgress(ctx: ToolContext): Promise<ProgressRecord[]> {
  const p = ctx.getLessonPath();
  if (!p) return [];
  const slug = slugOfLesson(p);
  return (await readProgress(ctx.dataRoot, ctx.config.course)).filter((r) => r.lessonSlug === slug);
}

/** Re-derive PlanState from the artifact node count and the progress log; no-op without a plan. */
export async function recomputePlan(ctx: ToolContext): Promise<void> {
  const plan = ctx.getPlan();
  if (!plan) return;
  ctx.setPlan(derivePlanState(plan, currentLesson(ctx)?.doc.nodes.length ?? 0, await lessonProgress(ctx)));
}

/** Short single-line label for a job title. */
export const label = (s: string, n = 60) => {
  const one = s.replace(/\s+/g, " ").trim();
  return one.length > n ? one.slice(0, n - 1) + "…" : one;
};

/** Ack returned by the dispatch tools (research / visual / demo): the result arrives later as a message. */
export const ack = (job: { id: string }): ToolOutcome => ({
  text: `Avviato in background (job ${job.id}): il risultato arriverà come messaggio. Continua la lezione.`,
});
