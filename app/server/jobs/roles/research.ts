import { z } from "zod";
import type { ResearchInput } from "../../../shared/protocol.ts";
import type { JobRole, JobTool } from "../../types.ts";
import { fail, readPrompt } from "./util.ts";

const schema = z.object({
  brief: z.string().min(80).describe("Markdown in Italian with the sections ## Sintesi, ## Risultati, ## Fonti, ## Lacune."),
  sources: z
    .array(z.object({ title: z.string().min(1), url: z.string().url() }))
    .min(1)
    .describe("Every page the brief relies on."),
});

const submit: JobTool<z.infer<typeof schema>> = {
  name: "submit_research",
  description: "Deliver the finished research brief and its sources. Call it exactly once, as the last step.",
  schema,
  async run(input, ctx) {
    const bad = input.sources.find((s) => !/^https?:\/\//i.test(s.url));
    if (bad) return fail(`La fonte "${bad.title}" non ha un URL http(s): correggi e richiama.`);
    ctx.setActivity("consegna la ricerca");
    ctx.finish({ kind: "research", brief: input.brief.trim(), sources: input.sources });
    return { text: "Consegnato. Chiudi qui con una riga." };
  },
};

export const researchRole: JobRole<ResearchInput> = {
  kind: "research",
  grant: ["web"],
  maxTurns: 20,
  systemPrompt: () => readPrompt("research.md"),
  firstMessage: (i) => `Domanda da verificare:\n${i.question}` + (i.context ? `\n\nContesto della lezione:\n${i.context}` : ""),
  tools: () => [submit],
};
