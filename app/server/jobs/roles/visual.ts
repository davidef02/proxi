import { z } from "zod";
import type { VisualInput } from "../../../shared/protocol.ts";
import type { JobRole, JobTool } from "../../types.ts";
import { renderSvgPng } from "../render.ts";
import { checkSvg } from "../sanitize.ts";
import { errorText, fail, readPrompt } from "./util.ts";

/** Per-job draft; lives in memory only for the minutes a job runs. */
const drafts = new Map<string, { svg: string; rendered: boolean }>();

const writeSvg: JobTool<{ svg: string }> = {
  name: "write_svg",
  description: "Save the SVG draft (replaces the previous one). Rejected when it contains scripts, foreignObject, on* attributes, external references or a non-svg root.",
  schema: z.object({ svg: z.string().min(1).describe("The complete <svg>…</svg> document.") }),
  async run({ svg }, ctx) {
    const errs = checkSvg(svg);
    if (errs.length) return fail(`SVG rifiutato: ${errs.join("; ")}. Correggi e richiama write_svg.`);
    drafts.set(ctx.jobId, { svg: svg.trim(), rendered: false });
    ctx.setActivity("scrive l'SVG");
    return { text: `Bozza salvata (${svg.length} caratteri). Chiama render_svg per guardarla.` };
  },
};

const renderSvg: JobTool<Record<string, never>> = {
  name: "render_svg",
  description: "Render the current draft to a PNG and return it as an image, so you can look at it.",
  schema: z.object({}),
  async run(_i, ctx) {
    const d = drafts.get(ctx.jobId);
    if (!d) return fail("Nessuna bozza: chiama prima write_svg.");
    ctx.setActivity("render SVG");
    try {
      const png = await renderSvgPng(d.svg);
      d.rendered = true;
      return { text: "Ecco il rendering della bozza corrente su fondo scuro. Guardalo: se qualcosa non va, correggi con write_svg; altrimenti submit_visual.", images: [{ data: png.toString("base64"), mimeType: "image/png" }] };
    } catch (e) {
      return fail(`Rendering non riuscito: ${errorText(e)}`);
    }
  },
};

const submitVisual: JobTool<{ caption?: string }> = {
  name: "submit_visual",
  description: "Deliver the current draft. Requires that the latest draft has been rendered and looked at.",
  schema: z.object({ caption: z.string().optional().describe("One Italian sentence under the figure.") }),
  async run({ caption }, ctx) {
    const d = drafts.get(ctx.jobId);
    if (!d) return fail("Nessuna bozza da consegnare.");
    if (!d.rendered) return fail("L'ultima bozza non è stata ancora guardata: chiama render_svg, controlla l'immagine e poi consegna.");
    drafts.delete(ctx.jobId);
    ctx.setActivity("consegna la figura");
    ctx.finish({ kind: "visual", svg: d.svg, caption: caption?.trim() || undefined });
    return { text: "Consegnata. Chiudi qui con una riga." };
  },
};

export const visualRole: JobRole<VisualInput> = {
  kind: "visual",
  grant: [],
  maxTurns: 14,
  systemPrompt: () => readPrompt("visual.md"),
  firstMessage: (i) => `Disegna questa figura:\n${i.brief}`,
  tools: () => [writeSvg, renderSvg, submitVisual],
  cleanup: (jobId) => void drafts.delete(jobId),
};
