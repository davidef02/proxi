import { z } from "zod";
import type { ResearchInput } from "../../shared/protocol.ts";
import { ack, label, tool } from "./util.ts";

const schema = z.object({
  question: z.string().min(1).describe("The precise question to verify, in the learner's language."),
  context: z.string().optional().describe("What the lesson is about and what version/constraints matter; the researcher sees nothing else."),
});

export const researchTool = tool<ResearchInput>({
  name: "research",
  description:
    "Start a background web research subagent (WebSearch/WebFetch). Returns at once with a job id; the sourced brief arrives later as a message from the app.",
  schema: schema as unknown as z.ZodType<ResearchInput>,
  async run(input, ctx) {
    return ack(ctx.jobs.start("research", input, { title: `Ricerca: ${label(input.question)}` }));
  },
});
