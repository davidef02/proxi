import { z } from "zod";
import type { AskAnswer, AskInput } from "../../shared/protocol.ts";
import { CANCELLED, fail, isCancelled, json, tool } from "./util.ts";

export const askSchema = z.object({
  question: z.string().min(1).describe("Exactly one question."),
  details: z.string().optional(),
  options: z
    .array(z.object({ label: z.string().min(1), value: z.string().min(1).optional(), description: z.string().optional() }))
    .optional()
    .describe("Omit for a free-text answer. An Altro free-text row is always added."),
  multiSelect: z.boolean().optional(),
});

export const askTool = tool<AskInput>({
  name: "ask",
  description: "Ask the learner an ungraded question (goal, preference, fork) as a card; blocks until answered. Use quiz for anything with a right answer.",
  schema: askSchema as unknown as z.ZodType<AskInput>,
  async run(input, ctx) {
    const values = (input.options ?? []).map((o) => o.value ?? o.label);
    if (new Set(values).size !== values.length) return fail("Invalid ask: option values must be unique.");
    let a: AskAnswer;
    try {
      a = await ctx.request<AskAnswer>({ kind: "ask", input });
    } catch (e) {
      if (isCancelled(e)) return fail(CANCELLED);
      throw e;
    }
    const labels = a.selected.map((v) => input.options?.find((o) => (o.value ?? o.label) === v)?.label ?? v);
    return json(a.other ? { selected: labels, other: a.other } : { selected: labels });
  },
});
