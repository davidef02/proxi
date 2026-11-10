import { z } from "zod";
import type { CognitiveNodePayload } from "../../../extensions/html-lesson.ts";
import { htmlToText } from "../graph/extract.ts";
import { readLesson, readLessonHtml } from "../lesson.ts";
import { fail, tool } from "./util.ts";

interface ReadLessonInput {
  lessonPath: string;
  nodeIds?: string[];
}

const MAX_PROSE = 40_000;

function nodeText(n: CognitiveNodePayload): string {
  const parts = [`## ${n.nodeId}: ${n.title} (${n.tier})`];
  if (n.motivation) parts.push(`Perché: ${htmlToText(n.motivation)}`);
  parts.push(htmlToText(n.content ?? ""));
  for (const m of n.math ?? []) parts.push(`$$${m}$$`);
  for (const c of n.codeBlocks ?? []) parts.push(`${c.caption ? c.caption + "\n" : ""}\`\`\`${c.language}\n${c.code}\n\`\`\``);
  if (n.connection) parts.push(`Collegamento: ${htmlToText(n.connection)}`);
  if (n.quiz) {
    const right = n.quiz.correctAnswer ?? (n.quiz.correctIndex !== undefined ? n.quiz.options?.[n.quiz.correctIndex - 1] : undefined);
    parts.push(`Verifica: ${htmlToText(n.quiz.question)}${right ? `\nRisposta: ${htmlToText(right)}` : ""}`);
  }
  const card = n.ankiParams?.targetQuestion ?? n.ankiParams?.suggestedCloze;
  if (card) parts.push(`Carta Anki: ${card}`);
  return parts.filter(Boolean).join("\n\n");
}

/** A lesson as compact text (what was taught, in his vocabulary). The artifact HTML is mostly markup, styles and
 *  scripts, several times the tokens: reading it whole with Read was the biggest token sink of a practice session. */
export const readLessonTool = tool<ReadLessonInput>({
  name: "read_lesson",
  description:
    "Read a lesson artifact as compact text: per node its title, tier, prose, code, connection, check question and card. Pass nodeIds to read only those nodes. Use this instead of Read on lesson HTML.",
  schema: z.object({
    lessonPath: z.string().describe("courses/<course>/lessons/<NNNN-slug>.html"),
    nodeIds: z.array(z.string()).optional().describe("Only these nodes (nodeId); omit for the whole lesson."),
  }) as unknown as z.ZodType<ReadLessonInput>,
  async run(input, ctx) {
    let lesson;
    try {
      lesson = readLesson(ctx.dataRoot, input.lessonPath);
    } catch (e) {
      return fail(String((e as Error).message ?? e));
    }
    if (!lesson) {
      const raw = (() => { try { return readLessonHtml(ctx.dataRoot, input.lessonPath); } catch { return null; } })();
      if (raw === null) return fail(`No lesson at ${input.lessonPath}.`);
      return { text: htmlToText(/<body[^>]*>([\s\S]*)<\/body>/i.exec(raw)?.[1] ?? raw).slice(0, MAX_PROSE) };
    }
    const { metadata: m, nodes } = lesson.doc;
    const want = input.nodeIds?.length ? new Set(input.nodeIds) : null;
    const picked = want ? nodes.filter((n) => want.has(n.nodeId)) : nodes;
    const head = `# ${m.title}\nLezione ${m.lessonNumber} · ${lesson.rel}${m.targetGoal ? `\nObiettivo: ${m.targetGoal}` : ""}`;
    if (!nodes.length) {
      const raw = readLessonHtml(ctx.dataRoot, lesson.rel) ?? "";
      return { text: `${head}\n\n${htmlToText(/<body[^>]*>([\s\S]*)<\/body>/i.exec(raw)?.[1] ?? raw).slice(0, MAX_PROSE)}` };
    }
    const missing = want ? [...want].filter((id) => !nodes.some((n) => n.nodeId === id)) : [];
    const note = missing.length ? `\n\n(Nodi non trovati: ${missing.join(", ")}. Nodi della lezione: ${nodes.map((n) => n.nodeId).join(", ")})` : "";
    return { text: `${head}\n\n${picked.map(nodeText).join("\n\n")}${note}` };
  },
});
