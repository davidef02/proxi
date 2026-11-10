import { z } from "zod";
import type { DemoInput } from "../../../shared/protocol.ts";
import type { JobRole, JobTool } from "../../types.ts";
import { runDemoPage } from "../render.ts";
import { checkDemo } from "../sanitize.ts";
import { errorText, fail, readPrompt } from "./util.ts";

/** Per-job draft; lives in memory only for the minutes a job runs. */
const drafts = new Map<string, { html: string; passed: boolean }>();

const writeDemo: JobTool<{ html: string }> = {
  name: "write_demo",
  description: "Save the demo as one self-contained HTML file (replaces the previous one). No external network resources.",
  schema: z.object({ html: z.string().min(1).describe("The complete HTML document with inline CSS and JS.") }),
  async run({ html }, ctx) {
    const errs = checkDemo(html);
    if (errs.length) return fail(`HTML rifiutato: ${errs.join("; ")}. Rendilo autosufficiente e richiama write_demo.`);
    drafts.set(ctx.jobId, { html, passed: false });
    ctx.setActivity("scrive la demo");
    return { text: `Demo salvata (${html.length} caratteri). Chiama run_demo per provarla.` };
  },
};

const runDemo: JobTool<Record<string, never>> = {
  name: "run_demo",
  description: "Load the current demo headless for about 1.5 s; returns console errors, page errors and a screenshot.",
  schema: z.object({}),
  async run(_i, ctx) {
    const d = drafts.get(ctx.jobId);
    if (!d) return fail("Nessuna demo: chiama prima write_demo.");
    ctx.setActivity("prova la demo");
    try {
      const r = await runDemoPage(d.html);
      d.passed = r.errors.length === 0;
      const images = [{ data: r.png.toString("base64"), mimeType: "image/png" }];
      return d.passed
        ? { text: "PASSATO: nessun errore. Guarda lo screenshot; se la demo ti convince, submit_demo.", images }
        : { text: `FALLITO, ${r.errors.length} errori:\n- ${r.errors.join("\n- ")}\nCorreggi con write_demo e riprova.`, isError: true, images };
    } catch (e) {
      return fail(`Prova non riuscita: ${errorText(e)}`);
    }
  },
};

const submitDemo = (title: string): JobTool<{ notes?: string }> => ({
  name: "submit_demo",
  description: "Deliver the demo. Allowed only when the latest run_demo passed with zero errors.",
  schema: z.object({ notes: z.string().optional().describe("Italian: what to try, in two or three sentences.") }),
  async run({ notes }, ctx) {
    const d = drafts.get(ctx.jobId);
    if (!d) return fail("Nessuna demo da consegnare.");
    if (!d.passed) return fail("Non si consegna ciò che non è stato provato: l'ultima versione non ha ancora superato run_demo senza errori.");
    drafts.delete(ctx.jobId);
    ctx.setActivity("consegna la demo");
    ctx.finish({ kind: "demo", title, html: d.html, notes: notes?.trim() || undefined });
    return { text: "Consegnata. Chiudi qui con una riga." };
  },
});

export const demoRole: JobRole<DemoInput> = {
  kind: "demo",
  grant: [],
  maxTurns: 24,
  systemPrompt: () => readPrompt("demo.md"),
  firstMessage: (i) => `Costruisci questa demo, titolo "${i.title}":\n${i.brief}`,
  tools: (input) => [writeDemo, runDemo, submitDemo(input.title)],
  cleanup: (jobId) => void drafts.delete(jobId),
};
