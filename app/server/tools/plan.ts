import * as path from "node:path";
import { z } from "zod";
import { initLesson } from "../../../extensions/html-lesson.ts";
import type { Decision, ProposePlanInput } from "../../shared/protocol.ts";
import { revisePlan } from "../lesson.ts";
import { derivePlanState, validateDagLabels } from "../planstate.ts";
import { CANCELLED, courseDir, currentLesson, fail, isCancelled, json, lessonProgress, tool, toRel } from "./util.ts";

export const proposePlanSchema = z.object({
  topic: z.string().min(1).describe("Lesson topic; also names the Anki deck."),
  title: z.string().min(1).optional().describe("Only for a free lesson: the title, naming the insight. Ignored when the lesson is planned or existing."),
  approach: z.string().min(1).describe("The approach in prose (markdown)."),
  dag: z.string().min(1).describe('Mermaid "graph TD". Every node: N1["Nodo 1: complete Italian clause"]; unconditional truths at the roots, goal at the sink.'),
  goal: z.string().min(1).describe("The learner's goal for the lesson."),
});

export const proposePlanTool = tool<ProposePlanInput>({
  name: "propose_plan",
  description:
    "Show the learner the lesson approach and dependency DAG and wait for approval (blocks). On approval the lesson artifact is created and nodes can be written with write_node.",
  schema: proposePlanSchema as unknown as z.ZodType<ProposePlanInput>,
  writes: true,
  async run(input, ctx) {
    const target = ctx.config.target ?? { mode: "free" as const };
    const errs = validateDagLabels(input.dag);
    if (errs.length) return fail(`Invalid DAG:\n${errs.join("\n")}\nFix and call again.`);
    if (target.mode === "free" && !input.title) return fail("This is a free lesson: propose_plan needs a title (the insight the lesson teaches).");

    let decision: Decision;
    try {
      decision = await ctx.request<Decision>({ kind: "plan", input });
    } catch (e) {
      if (isCancelled(e)) return fail(CANCELLED);
      throw e;
    }
    if (!decision.approved) return json({ approved: false, feedback: decision.feedback ?? "" });

    if (!ctx.getLessonPath()) {
      if (target.mode === "existing") {
        ctx.setLessonPath(target.lessonPath);
      } else {
        // The planned title goes in verbatim: the board joins plan entry and artifact by its slug.
        const title = target.mode === "planned" ? target.title : input.title!;
        const res = await initLesson({
          topic: input.topic,
          title,
          dagMermaid: input.dag,
          targetGoal: input.goal,
          baseDir: ctx.dataRoot,
          lessonsDir: path.join(courseDir(ctx), "lessons"),
        });
        ctx.setLessonPath(toRel(ctx.dataRoot, res.filePath));
      }
      ctx.emitEvent({ type: "lesson", lessonPath: ctx.getLessonPath()! });
    }
    // Approved revision of an existing lesson: the artifact's DAG and goal follow the new plan.
    await revisePlan(ctx.dataRoot, ctx.getLessonPath()!, input);
    ctx.setPlan(
      derivePlanState({ topic: input.topic, goal: input.goal, dag: input.dag, approved: true }, currentLesson(ctx)?.doc.nodes.length ?? 0, await lessonProgress(ctx)),
    );
    return json({ approved: true, lessonPath: ctx.getLessonPath() });
  },
});
