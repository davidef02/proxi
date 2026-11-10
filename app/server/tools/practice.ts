import { z } from "zod";
import type { ConcludeInput, ProblemInput } from "../../shared/protocol.ts";
import type { ToolContext } from "../types.ts";
import { fail, tool } from "./util.ts";

// ponytail: the open problem lives in memory per ToolContext (one per session); a server restart
// forgets it and the teacher simply opens a new problem.
const open = new WeakMap<ToolContext, { id: string; input: ProblemInput }>();

const problemSchema = z.object({
  statement: z.string().min(1).describe("Markdown shown to the learner."),
  mode: z.enum(["recall", "discrimination", "composition"]),
  nodeKeys: z.array(z.string()).min(1).describe("MapNode keys this problem targets."),
  modelSolution: z.string().min(1).describe("Hidden from the learner until conclude."),
});

const concludeSchema = z.object({
  outcome: z.enum(["solved", "solvedWithHints", "movedOn", "surrendered"]),
  verdict: z.string().min(1),
  rung: z.enum(["R0", "R1", "R2", "R3", "R4", "R5"]).nullable().describe("Highest hint rung used, or null."),
  missClass: z.enum(["cue", "discrimination", "execution", "careless", "structural"]).nullable().describe("Why he missed, or null when solved cleanly."),
  retest: z.boolean().describe("Whether to bring this node back later."),
});

export const problemTool = tool<ProblemInput>({
  name: "problem",
  description: "Show the learner a practice problem card. Non-blocking: coach his attempt in chat, then call conclude. Only one problem may be open.",
  schema: problemSchema as unknown as z.ZodType<ProblemInput>,
  async run(input, ctx) {
    if (open.has(ctx)) return fail("A problem is already open: coach it to the end and call conclude before opening another.");
    const id = ctx.emit({ kind: "problem", input });
    open.set(ctx, { id, input });
    return { text: "Problem shown. Wait for the learner's attempt in chat; coach with the hint ladder; call conclude when done." };
  },
});

export const concludeTool = tool<ConcludeInput>({
  name: "conclude",
  description: "Close the open practice problem with the outcome, highest hint rung, miss class and whether to retest. Free practice: nothing is recorded toward mastery.",
  schema: concludeSchema as unknown as z.ZodType<ConcludeInput>,
  async run(input, ctx) {
    const p = open.get(ctx);
    if (!p) return fail("No open problem: call problem first.");
    ctx.emit({ id: p.id, kind: "problem", input: p.input, conclusion: input });
    open.delete(ctx);
    return { text: "Concluded." };
  },
});
