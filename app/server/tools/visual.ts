import { z } from "zod";
import type { VisualInput } from "../../shared/protocol.ts";
import { ack, label, tool } from "./util.ts";

const schema = z.object({
  brief: z.string().min(1).describe("Self-contained description of the figure: what to show, the labels (Italian), the relations. The visual-maker sees nothing else of the lesson."),
});

export const visualTool = tool<VisualInput>({
  name: "visual",
  description:
    "Start a background subagent that draws one SVG figure and checks it by rendering it. Returns at once with a job id; the figure appears in the chat when ready.",
  schema: schema as unknown as z.ZodType<VisualInput>,
  async run(input, ctx) {
    return ack(ctx.jobs.start("visual", input, { title: `Figura: ${label(input.brief)}` }));
  },
});
