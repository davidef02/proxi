import * as fs from "node:fs";
import * as path from "node:path";
import { z } from "zod";
import { compileReference } from "../../../extensions/html-lesson.ts";
import { slugify } from "../../../extensions/slug.ts";
import type { FinishLessonInput, FinishResult } from "../../shared/protocol.ts";
import { listSources, startMapJob } from "../graph/extract.ts";
import { readSettings } from "../settings.ts";
import { syncLesson } from "./anki.ts";
import type { ToolContext } from "../types.ts";
import { lessonMarker } from "../lesson.ts";
import { courseDir, currentLesson, fail, json, tool, toRel } from "./util.ts";

/** Map trigger: a lesson just closed. Failures never fail the close; they become a notice. */
function autoMap(ctx: ToolContext, abs: string | undefined) {
  if (!abs || ctx.config.kind !== "teach" || !readSettings(ctx.repoRoot).autoMap) return;
  try {
    startMapJob(ctx.jobs, listSources(ctx.dataRoot), `${ctx.config.course}/${path.basename(abs, ".html")}`);
  } catch (e) {
    ctx.emit({ kind: "notice", level: "info", text: `Mappa non aggiornata automaticamente: ${(e as Error).message}` });
  }
}

const schema = z.object({
  recordTitle: z.string().min(1).describe("What became true, e.g. 'Il contratto del componente: acquisito'."),
  summary: z.string().min(1).describe("What he can now do."),
  evidence: z.string().min(1).describe("What he actually did: quiz results, slips, quotes (markdown)."),
  implications: z.string().min(1).describe("What the next session must do differently (markdown)."),
  notesAddendum: z.string().optional().describe("What was learned about teaching him; appended to NOTES.md."),
});

export const finishLessonTool = tool<FinishLessonInput>({
  name: "finish_lesson",
  description: "Close the lesson: compile the reference sheet, write the learning record (for the teacher), optionally append to NOTES.md and sync Anki. If the lesson already has its record, only the reference sheet is refreshed (and Anki resynced).",
  schema: schema as unknown as z.ZodType<FinishLessonInput>,
  writes: true,
  async run(input, ctx) {
    const course = courseDir(ctx);
    const lesson = currentLesson(ctx);
    const abs = lesson?.abs;
    if (ctx.config.kind === "teach" && !abs) return fail("No lesson artifact yet: nothing to finish. Get the plan approved with propose_plan first.");

    const recDir = path.join(course, "learning-records");
    fs.mkdirSync(recDir, { recursive: true });
    // The record's last line names the lesson (empty for plan/practice sessions), so a second finish_lesson
    // for it only refreshes the reference sheet. The Lesson module owns the "closed" rule.
    const marker = lessonMarker(lesson?.summary.slug ?? "");
    const done = lesson?.record;

    const result: FinishResult = { recordPath: "", notesUpdated: false };
    let recordPath: string = done ?? "";
    let recordContent: string | undefined = undefined;

    if (!done) {
      const body =
        `# ${input.recordTitle}\n\n${input.summary}\n\n## Evidence\n\n${input.evidence}\n\n## Implications\n\n${input.implications}\n` +
        `\n${marker}\n`;
      recordContent = body;
      const lessonNo = abs ? Number(/^(\d{4})-/.exec(path.basename(abs))?.[1]) || 0 : 0;
      let recFile = "";
      for (let tries = 0; !recFile; tries++) {
        const files = fs.readdirSync(recDir);
        const max = files.reduce((m, f) => Math.max(m, /^(\d{4})-/.test(f) ? Number(f.slice(0, 4)) : 0), 0);
        const n = tries === 0 && lessonNo && !files.some((f) => f.startsWith(String(lessonNo).padStart(4, "0") + "-")) ? lessonNo : max + 1;
        const cand = path.join(recDir, `${String(n).padStart(4, "0")}-${slugify(input.recordTitle) || "record"}.md`);
        try {
          fs.writeFileSync(cand, body, { encoding: "utf-8", flag: "wx" });
          recFile = cand;
        } catch (e) {
          if ((e as NodeJS.ErrnoException).code !== "EEXIST" || tries > 20) throw e;
        }
      }
      recordPath = toRel(ctx.dataRoot, recFile);
    }
    result.recordPath = recordPath;

    if (abs) {
      result.lessonPath = toRel(ctx.dataRoot, abs);
      const ref = await compileReference({
        lessonPath: abs,
        baseDir: ctx.dataRoot,
        outputDir: path.join(course, "reference"),
        learningRecordPath: recordPath ? path.resolve(ctx.dataRoot, recordPath) : undefined,
        learningRecordContent: recordContent,
      });
      result.referencePath = toRel(ctx.dataRoot, ref.filePath);
    }

    if (done) {
      if (ctx.config.ankiOptIn && abs) result.anki = await syncLesson(ctx);
      autoMap(ctx, abs);
      ctx.emit({ kind: "finish", result });
      return json({ ...result, message: "already closed: reference refreshed" + (result.anki ? ", Anki resynced" : "") });
    }

    const add = input.notesAddendum?.trim();
    if (add) {
      const notes = path.join(course, "NOTES.md");
      const prev = fs.existsSync(notes) ? fs.readFileSync(notes, "utf-8").trimEnd() : "";
      if (!prev.includes(add)) {
        fs.writeFileSync(notes, (prev ? prev + "\n\n" : "") + add + "\n", "utf-8");
        result.notesUpdated = true;
      }
    }

    if (ctx.config.ankiOptIn && abs) result.anki = await syncLesson(ctx);
    autoMap(ctx, abs);
    ctx.emit({ kind: "finish", result });
    return json(result);
  },
});
