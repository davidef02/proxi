import { z } from "zod";
import { syncToAnkiConnect } from "../../../extensions/anki-sync.ts";
import type { ToolContext } from "../types.ts";
import { fail, json, lessonAbs, tool, toRel } from "./util.ts";

/** Sync the whole lesson deck; never throws. `LEARNER_ANKI_URL` overrides AnkiConnect's address (tests). */
export async function syncLesson(ctx: ToolContext): Promise<string> {
  const abs = lessonAbs(ctx);
  if (!abs) return "No lesson artifact: nothing to sync.";
  try {
    const r = await syncToAnkiConnect({ lessonPath: abs, baseDir: ctx.dataRoot, ankiUrl: process.env.LEARNER_ANKI_URL || undefined });
    const tsv = r.tsvExportPath ? ` TSV: ${toRel(ctx.dataRoot, r.tsvExportPath)}.` : "";
    if (!r.synced) return `Anki offline or sync incomplete (${r.warning ?? r.error ?? "unknown"}); cards kept in the deck TSV.${tsv}`;
    return `Anki deck "${r.deckName}": ${r.addedCount} added, ${r.duplicateCount} duplicates skipped, ${r.failedCount} failed (${r.totalCards} cards).${tsv}`;
  } catch (e) {
    return `Anki sync failed: ${e instanceof Error ? e.message : e}`;
  }
}

export const syncAnkiTool = tool<Record<string, never>>({
  name: "sync_anki",
  description: "Sync the lesson's Anki cards (only exists when the learner opted in). Safe to call repeatedly: duplicates are skipped.",
  schema: z.object({}) as unknown as z.ZodType<Record<string, never>>,
  async run(_input, ctx) {
    if (!ctx.config.ankiOptIn) return fail("The learner did not opt in to Anki: no cards.");
    if (!ctx.getLessonPath()) return fail("No lesson artifact yet: nothing to sync.");
    const summary = await syncLesson(ctx);
    ctx.emit({ kind: "notice", level: "info", text: summary });
    return json({ summary });
  },
});
