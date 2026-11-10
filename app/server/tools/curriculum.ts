import * as fs from "node:fs";
import * as path from "node:path";
import { z } from "zod";
import { planChapter, readCurriculum } from "../../../extensions/chapter.ts";
import { lessonSlug } from "../../../extensions/slug.ts";
import type { Decision, ProposeCurriculumInput } from "../../shared/protocol.ts";
import { slugOfLesson } from "../lesson.ts";
import { CANCELLED, fail, isCancelled, json, KEBAB, tool } from "./util.ts";

const schema = z.object({
  chapter: z.string().min(1).describe("Chapter (course) title."),
  slug: z.string().describe("Course directory name under courses/, kebab-case."),
  destination: z.string().min(1).describe("1-2 sentences: where the learner ends up."),
  mission: z.string().optional().describe("Full MISSION.md (Why / Success looks like / Constraints / Out of scope). Required when the course directory is new."),
  lessons: z
    .array(z.object({ title: z.string().min(1).describe("Names the insight, not the topic."), plannedNodes: z.number().int().positive().optional(), slug: z.string().regex(KEBAB).optional().describe("Legacy only: file slug of an already-taught lesson whose name differs from its title. Normally omit.") }))
    .min(1),
});

export const proposeCurriculumTool = tool<ProposeCurriculumInput>({
  name: "propose_curriculum",
  description: "Show the learner the destination and ordered lesson list and wait for approval (blocks); on approval writes curriculum.json (and MISSION.md for a new course).",
  schema: schema as unknown as z.ZodType<ProposeCurriculumInput>,
  writes: true,
  async run(input, ctx) {
    if (!KEBAB.test(input.slug)) return fail(`slug "${input.slug}" must be kebab-case (lowercase letters, digits, hyphens).`);
    if (input.slug !== ctx.config.course) return fail(`slug "${input.slug}" must be this session's course "${ctx.config.course}".`);
    // Re-planning: keep explicit slugs of existing entries (matched by title) and never orphan a taught lesson.
    const old = readCurriculum({ baseDir: ctx.dataRoot }).chapters.find((c) => c.slug === input.slug)?.lessons ?? [];
    const lessons = input.lessons.map((l) => {
      const given = l.slug;
      const prev = old.find((o) => o.title.trim() === l.title.trim());
      return { ...l, slug: given ?? (prev && prev.slug !== lessonSlug(l.title) ? prev.slug : undefined) };
    });
    const slugs = new Map<string, string>();
    for (const l of lessons) {
      const s = l.slug ?? lessonSlug(l.title);
      if (!s) return fail(`lesson title "${l.title}" produces an empty slug.`);
      const clash = slugs.get(s);
      if (clash) return fail(`lessons "${clash}" and "${l.title}" share the slug "${s}": give them distinct titles.`);
      slugs.set(s, l.title);
    }
    const dir = path.join(ctx.dataRoot, "courses", input.slug);
    const lessonsDir = path.join(dir, "lessons");
    const taught = new Set(fs.existsSync(lessonsDir) ? fs.readdirSync(lessonsDir).filter((f) => f.endsWith(".html")).map(slugOfLesson) : []);
    const dropped = old.filter((o) => taught.has(o.slug) && !slugs.has(o.slug));
    if (dropped.length) {
      return fail(`The new plan drops already taught lessons: ${dropped.map((o) => `"${o.title}"`).join(", ")}. Keep them with the exact same title (or their slug) in the same plan, then call again.`);
    }
    const missionFile = path.join(dir, "MISSION.md");
    if (!fs.existsSync(dir) && !input.mission?.trim()) return fail(`courses/${input.slug} is new: mission (the full MISSION.md) is required.`);

    let decision: Decision;
    try {
      decision = await ctx.request<Decision>({ kind: "curriculum", input });
    } catch (e) {
      if (isCancelled(e)) return fail(CANCELLED);
      throw e;
    }
    if (!decision.approved) return json({ approved: false, feedback: decision.feedback ?? "" });

    const notes: string[] = [];
    if (!fs.existsSync(dir)) {
      for (const d of ["lessons", "reference", "learning-records"]) fs.mkdirSync(path.join(dir, d), { recursive: true });
    }
    if (fs.existsSync(missionFile)) {
      if (input.mission?.trim()) notes.push("MISSION.md already exists and was left untouched.");
    } else if (input.mission?.trim()) {
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(missionFile, input.mission.trim() + "\n", "utf-8");
    }
    planChapter({ chapter: input.chapter, slug: input.slug, lessons, baseDir: ctx.dataRoot });
    return json({ approved: true, course: input.slug, lessons: input.lessons.length, ...(notes.length ? { notes } : {}) });
  },
});
