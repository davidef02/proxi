import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";
import { z } from "zod";
import type { ExerciseInfo, WriteExerciseInput } from "../../shared/protocol.ts";
import { badExercisePath, HINTS_FILE, INSTRUCTIONS_FILE, listExercises, readHints, runExercise, writeHints } from "../exercises.ts";
import type { ToolContext } from "../types.ts";
import { courseDir, fail, json, tool, toRel } from "./util.ts";

const NAME = /^\d{2}-[a-z0-9]+(?:-[a-z0-9]+)*$/;

const writeSchema = z.object({
  name: z.string().regex(NAME, 'must look like "13-provide-inject"').describe("Directory name under courses/<course>/exercises."),
  instructions: z.string().min(1).describe("Markdown shown on the exercise card; also written to ISTRUZIONI.md."),
  files: z.array(z.object({ path: z.string(), content: z.string() })).min(1).describe("Paths relative to the exercise dir (.vue .js .ts .json .css .md)."),
  hints: z.array(z.string().min(1)).min(2).max(4).optional().describe("Progressive hints, weakest first (nudge, concept/API, near-solution). Italian markdown. The learner reveals them one at a time."),
  final: z.boolean().optional().describe("True for the lesson's final semi-guided exercise."),
});

const runSchema = z.object({ name: z.string().regex(NAME, 'must look like "13-provide-inject"') });

// Hash on LF-normalised text: an editor or git autocrlf must not make an untouched file look learner-edited.
const sha = (t: string | Buffer) => crypto.createHash("sha256").update(t.toString("utf-8").replace(/\r\n/g, "\n")).digest("hex");
const key = (p: string) => path.posix.normalize(p).replace(/^\.\//, "");
const TRACK = ".app-written.json";

function readTrack(root: string): Record<string, string> {
  try {
    const raw = JSON.parse(fs.readFileSync(path.join(root, TRACK), "utf-8")) as Record<string, string>;
    return Object.fromEntries(Object.entries(raw).map(([k, v]) => [key(k), v]));
  } catch {
    return {};
  }
}

const exercisesDir = (ctx: ToolContext) => path.join(courseDir(ctx), "exercises");

/** Replace/insert `info` in the session exercise list. */
function upsert(ctx: ToolContext, info: ExerciseInfo) {
  const list = ctx.getExercises().filter((e) => e.name !== info.name);
  list.push(info);
  list.sort((a, b) => a.name.localeCompare(b.name));
  ctx.setExercises(list);
  ctx.emit({ id: `exercise:${info.name}`, kind: "exercise", exercise: info });
}

export const writeExerciseTool = tool<WriteExerciseInput>({
  name: "write_exercise",
  description: "Write a coding exercise (files plus instructions) into courses/<course>/exercises/<name>/ and show it to the learner, who solves it in the app's built-in editor and runs the tests there (results reach you as \"Esito test\" messages).",
  schema: writeSchema as unknown as z.ZodType<WriteExerciseInput>,
  writes: true,
  async run(input, ctx) {
    const root = path.join(exercisesDir(ctx), input.name);
    const errs: string[] = [];
    const seen = new Set<string>();
    for (const f of input.files) {
      const bad = badExercisePath(f.path);
      if (bad) errs.push(bad);
      else if (path.relative(root, path.resolve(root, f.path)).startsWith("..")) errs.push(`path "${f.path}" escapes the exercise dir`);
      else if (f.path === INSTRUCTIONS_FILE) errs.push(`${INSTRUCTIONS_FILE} is written by the tool from \`instructions\``);
      else if (seen.has(f.path)) errs.push(`duplicate path "${f.path}"`);
      seen.add(f.path);
    }
    if (errs.length) return fail(`Invalid files: ${errs.join("; ")}. Fix and call again.`);

    // Never destroy the learner's work: overwrite only files absent or still as the app last wrote them.
    const track = readTrack(root);
    for (const f of input.files) {
      const target = path.resolve(root, f.path);
      if (!fs.existsSync(target)) continue;
      const cur = fs.readFileSync(target);
      if (sha(cur) !== track[key(f.path)] && sha(cur) !== sha(f.content)) {
        return fail(`Il file ${f.path} è stato modificato dal learner (o non è stato scritto da questa app): non sovrascriverlo, scegli un nuovo nome di esercizio (NN-...). Nothing was written.`);
      }
    }
    for (const f of input.files) {
      const target = path.resolve(root, f.path);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, f.content, "utf-8");
      track[key(f.path)] = sha(f.content);
    }
    fs.writeFileSync(path.join(root, TRACK), JSON.stringify(track, null, 2) + "\n", "utf-8");
    fs.writeFileSync(path.join(root, INSTRUCTIONS_FILE), input.instructions.trimEnd() + "\n", "utf-8");
    // The hints file is app-owned and mutated by the learner's clicks, so it is not sha-tracked; `revealed` survives a rewrite unless the hints changed.
    const prev = readHints(root);
    const hints = input.hints ?? [];
    const same = !!prev && JSON.stringify(prev.hints) === JSON.stringify(hints);
    const persist = hints.length > 0 || !!input.final;
    if (persist) writeHints(root, { hints, final: !!input.final, revealed: same ? prev!.revealed : 0 });
    else if (prev) fs.rmSync(path.join(root, HINTS_FILE));

    const listed = (await listExercises(ctx.dataRoot, ctx.config.course)).find((e) => e.name === input.name);
    upsert(ctx, {
      course: ctx.config.course,
      name: input.name,
      dir: toRel(ctx.dataRoot, root),
      files: (listed?.files ?? input.files.map((f) => f.path)).filter((f) => f !== TRACK),
      instructions: input.instructions,
      ...(persist ? { hints, hintsRevealed: same ? prev!.revealed : 0, final: !!input.final } : {}),
    });
    return json({ dir: toRel(ctx.dataRoot, root), note: "Esercizio aperto nell'editor dell'app: il learner lavora lì e gli esiti arrivano come messaggi \"Esito test …\"." });
  },
});

export const runExerciseTool = tool<{ name: string }>({
  name: "run_exercise",
  description: "Run the tests of one exercise and return passed/failed with failure messages. The result is also shown to the learner.",
  schema: runSchema,
  async run(input, ctx) {
    const res = await runExercise(ctx.dataRoot, ctx.config.course, input.name);
    const existing = ctx.getExercises().find((e) => e.name === input.name);
    const instructions = existing?.instructions ?? readInstructions(path.join(exercisesDir(ctx), input.name));
    upsert(ctx, {
      ...existing,
      course: ctx.config.course,
      name: input.name,
      dir: toRel(ctx.dataRoot, path.join(exercisesDir(ctx), input.name)),
      files: existing?.files ?? [],
      instructions,
      lastRun: res,
    });
    let text: string;
    if (res.error) text = `Could not run the tests: ${res.error}`;
    else {
      text = `${res.ok ? "PASS" : "FAIL"}: ${res.passed}/${res.total} passed, ${res.failed} failed (${res.durationMs} ms).`;
      for (const f of res.failures) text += `\n- ${f.name}: ${f.message}`;
    }
    return { text: text.length > 4000 ? text.slice(0, 4000) + "\n...(truncated)" : text, isError: res.error ? true : undefined };
  },
});

function readInstructions(dir: string): string | undefined {
  try {
    return fs.readFileSync(path.join(dir, INSTRUCTIONS_FILE), "utf-8");
  } catch {
    return undefined;
  }
}
