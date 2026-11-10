import { z } from "zod";
import type { DemoInput } from "../../shared/protocol.ts";
import { ack, label, tool } from "./util.ts";

const schema = z.object({
  title: z.string().min(1).describe("Short Italian title of the demo."),
  brief: z.string().min(1).describe("Self-contained description of what the learner can manipulate and what it should make visible. The demo-builder sees nothing else of the lesson."),
});

export const demoTool = tool<DemoInput>({
  name: "demo",
  description:
    "Start a background subagent that builds one interactive single-file HTML demo and tests it headless. Returns at once with a job id; the demo appears in the chat when ready.",
  schema: schema as unknown as z.ZodType<DemoInput>,
  async run(input, ctx) {
    return ack(ctx.jobs.start("demo", input, { title: `Demo: ${label(input.title)}` }));
  },
});
