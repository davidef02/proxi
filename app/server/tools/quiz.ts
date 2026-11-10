import { z } from "zod";
import type { QuizAnswer, QuizInput, QuizResult } from "../../shared/protocol.ts";
import { slugOfLesson } from "../lesson.ts";
import { parseDagIds } from "../planstate.ts";
import { CANCELLED, fail, isCancelled, json, recomputePlan, tool } from "./util.ts";

const option = z.object({ label: z.string().min(1), value: z.string().min(1).optional() });

export const quizSchema = z.object({
  kind: z.enum(["probe", "predict", "check"]).describe("probe = level mapping; predict = before teaching a node; check = after teaching it"),
  nodeId: z.string().regex(/^N\d+$/).optional().describe('Plan node id such as "N3". Required for predict and check.'),
  question: z.string().min(1).describe("Exactly one question; markdown and LaTeX allowed."),
  options: z.array(option).min(2).max(6).describe("2 to 6 bare claims. Never add an I-dont-know option: the app adds it."),
  multiSelect: z.boolean().optional().describe("Set when more than one option is correct."),
  explanation: z.string().min(1).describe("Shown after answering, whether right or wrong."),
  correctAnswer: z.union([z.string(), z.array(z.string())]).describe("Option value(s) (the label when no value was given), not positions."),
  misconceptions: z.array(z.object({ value: z.string(), misconception: z.string() })).optional().describe("Per wrong option: the misconception it probes."),
});

const valueOf = (o: { label: string; value?: string }) => o.value ?? o.label;
const asArray = (a: string | string[]) => (Array.isArray(a) ? a : [a]);

/** Exact-set grading; dontKnow is never correct. Exported: sessions.ts grades submitted answers with it. */
export function gradeQuiz(input: QuizInput, answer: QuizAnswer): QuizResult {
  const correctSet = new Set(asArray(input.correctAnswer));
  const sel = new Set(answer.selected);
  const correct = !answer.dontKnow && sel.size === correctSet.size && [...sel].every((v) => correctSet.has(v));
  return { ...answer, correct };
}

function validate(input: QuizInput, dag: string | undefined): string[] {
  const errs: string[] = [];
  const values = input.options.map(valueOf);
  if (new Set(values).size !== values.length) errs.push("option values must be unique");
  const correct = asArray(input.correctAnswer);
  for (const c of correct) if (!values.includes(c)) errs.push(`correctAnswer "${c}" is not an option value`);
  if (new Set(correct).size > 1 && !input.multiSelect) errs.push("more than one correct answer requires multiSelect: true");
  for (const m of input.misconceptions ?? []) if (!values.includes(m.value)) errs.push(`misconception value "${m.value}" is not an option value`);
  if ((input.kind === "predict" || input.kind === "check") && !input.nodeId) errs.push(`kind "${input.kind}" requires nodeId`);
  if (input.nodeId && dag && !parseDagIds(dag).includes(input.nodeId)) {
    errs.push(`nodeId ${input.nodeId} is not in the approved plan (${parseDagIds(dag).join(", ")})`);
  }
  return errs;
}

/** Fisher-Yates permutation of 0..n-1. */
export function shuffled(n: number): number[] {
  const a = Array.from({ length: n }, (_, i) => i);
  for (let i = n - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
}

export const quizTool = tool<QuizInput>({
  name: "quiz",
  description:
    "Ask the learner one graded multiple-choice question as a clickable card; blocks until answered and returns the graded result with his confidence and optional note. Use for probes, predictions and checks.",
  schema: quizSchema as unknown as z.ZodType<QuizInput>,
  async run(input, ctx) {
    const errs = validate(input, ctx.getPlan()?.dag);
    if (errs.length) return fail(`Invalid quiz: ${errs.join("; ")}. Fix and call again.`);
    let answer: QuizAnswer;
    try {
      answer = await ctx.request<QuizAnswer>({ kind: "quiz", input, displayOrder: shuffled(input.options.length) });
    } catch (e) {
      if (isCancelled(e)) return fail(CANCELLED);
      throw e;
    }
    const result = gradeQuiz(input, answer);
    const lessonPath = ctx.getLessonPath();
    await ctx.logProgress({
      lessonSlug: lessonPath ? slugOfLesson(lessonPath) : undefined,
      nodeRefs: input.nodeId ? [input.nodeId] : [],
      kind: input.kind,
      question: input.question,
      correct: result.correct,
      dontKnow: result.dontKnow,
      confidence: result.confidence,
    });
    await recomputePlan(ctx);
    const label = (v: string) => input.options.find((x) => valueOf(x) === v)?.label ?? v;
    const out: Record<string, unknown> = {
      correct: result.correct,
      dontKnow: result.dontKnow,
      selected: result.selected.map(label),
      confidence: result.confidence,
    };
    if (result.note) out.note = result.note;
    if (answer.thinkingWhileReading) out.thinkingWhileReading = answer.thinkingWhileReading;
    if (!result.correct) out.correctAnswer = asArray(input.correctAnswer).map(label);
    return json(out);
  },
});
