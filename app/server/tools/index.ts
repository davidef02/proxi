import * as fs from "node:fs";
import * as path from "node:path";
import type { SessionConfig } from "../../shared/protocol.ts";
import type { AppTool } from "../types.ts";
import { askTool } from "./ask.ts";
import { syncAnkiTool } from "./anki.ts";
import { browseConceptsTool, openConceptTool, searchConceptsTool } from "./concepts.ts";
import { proposeCurriculumTool } from "./curriculum.ts";
import { runExerciseTool, writeExerciseTool } from "./exercise.ts";
import { finishLessonTool } from "./finish.ts";
import { readLessonTool } from "./lessontext.ts";
import { writeNodeTool } from "./node.ts";
import { proposePlanTool } from "./plan.ts";
import { concludeTool, problemTool } from "./practice.ts";
import { quizTool } from "./quiz.ts";
import { demoTool } from "./demo.ts";
import { researchTool } from "./research.ts";
import { visualTool } from "./visual.ts";
import { coursesChanged } from "../lesson.ts";
import { KEBAB } from "./util.ts";

export { refreshBoard } from "./board.ts";
export { gradeQuiz } from "./quiz.ts";

/** The write seam: a successful run of a `writes` tool raises coursesChanged (board + app refetch) in one place. */
const raising = (t: AppTool): AppTool =>
  t.writes
    ? {
        ...t,
        async run(input, ctx) {
          const out = await t.run(input, ctx);
          if (!out.isError) coursesChanged(ctx.dataRoot);
          return out;
        },
      }
    : t;

/** The tools a session of this kind may call. Exercise tools only when the course has an exercises package. */
export function toolsFor(config: SessionConfig, dataRoot: string): AppTool[] {
  return [...toolsOf(config, dataRoot), readLessonTool, searchConceptsTool, openConceptTool, browseConceptsTool].map(raising);
}

function toolsOf(config: SessionConfig, dataRoot: string): AppTool[] {
  const hasExercises =
    KEBAB.test(config.course) && fs.existsSync(path.join(dataRoot, "courses", config.course, "exercises", "package.json"));
  const exercise = hasExercises ? [writeExerciseTool, runExerciseTool] : [];
  switch (config.kind) {
    case "plan":
      return [quizTool, askTool, proposeCurriculumTool, researchTool];
    case "practice":
      return [quizTool, askTool, problemTool, concludeTool, researchTool, visualTool, ...exercise];
    default:
      return [quizTool, askTool, proposePlanTool, writeNodeTool, researchTool, visualTool, demoTool, ...(config.ankiOptIn ? [syncAnkiTool] : []), ...exercise, finishLessonTool];
  }
}
