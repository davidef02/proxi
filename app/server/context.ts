// System prompt assembly + opening message. The prompts live in app/prompts/*.md.
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import type { SessionConfig } from "../shared/protocol.ts";
import { readCurriculum, matchCurriculum, formatCurriculum } from "../../extensions/chapter.ts";
import { listLessons, readLesson } from "./lesson.ts";
import { parseDagIds } from "./planstate.ts";
import { buildKnowledgeMap } from "./knowledge.ts";
import { courseIndexBlock } from "./courseindex.ts";

const PROMPTS_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "prompts");

export interface BuildSystemPromptOptions {
  config: SessionConfig;
  repoRoot: string;
  /** Data folder (courses/, graph/, decks/, curriculum.json). Defaults to repoRoot. */
  dataRoot?: string;
  lessonPath?: string;
  sessionId?: string;
  today?: string;
}

function readIf(p: string): string | undefined {
  try {
    return fs.readFileSync(p, "utf-8").trim();
  } catch {
    return undefined;
  }
}

/** Wraps a file as <file path="...">, or "" when missing/empty. */
function fileBlock(root: string, rel: string): string {
  const text = readIf(path.join(root, rel));
  return text ? `<file path="${rel}">\n${text}\n</file>` : "";
}

function lastRecords(dataRoot: string, course: string, n: number): string[] {
  const dir = path.join(dataRoot, "courses", course, "learning-records");
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir)
    .filter((f) => /^\d{4}-.*\.md$/.test(f))
    .sort()
    .slice(-n)
    .map((f) => `courses/${course}/learning-records/${f}`);
}

function chapterPlan(dataRoot: string, course: string): string {
  try {
    const match = matchCurriculum(readCurriculum({ baseDir: dataRoot }), listLessons(dataRoot).map((l) => l.summary));
    const chapters = match.chapters.filter((c) => c.slug === course);
    const orphans = match.orphans.filter((l) => l.course === course);
    if (!chapters.length && !orphans.length) return "";
    return formatCurriculum({ chapters, taught: match.taught, orphans });
  } catch {
    return "";
  }
}

function exerciseNames(dataRoot: string, course: string): string[] {
  const dir = path.join(dataRoot, "courses", course, "exercises");
  if (!fs.existsSync(path.join(dir, "package.json"))) return [];
  return fs.readdirSync(dir).filter((d) => /^(\d{2}|zz)-/.test(d) && fs.statSync(path.join(dir, d)).isDirectory()).sort();
}

function courseContext(dataRoot: string, config: SessionConfig): string {
  const c = config.course;
  const parts: string[] = [];
  for (const rel of [`courses/${c}/MISSION.md`, `courses/${c}/NOTES.md`, `courses/${c}/GLOSSARY.md`]) {
    const b = fileBlock(dataRoot, rel);
    if (b) parts.push(b);
  }
  for (const rel of lastRecords(dataRoot, c, 3)) {
    const b = fileBlock(dataRoot, rel);
    if (b) parts.push(b);
  }
  const plan = chapterPlan(dataRoot, c);
  if (plan) parts.push(`<chapter-plan>\n${plan}\n</chapter-plan>`);
  const ex = exerciseNames(dataRoot, c);
  if (ex.length) parts.push(`<exercises course="${c}">\n${ex.join("\n")}\n</exercises>`);
  return `# Course context\n\nCourse directory: courses/${c}\n\n${parts.join("\n\n") || "(no course files yet)"}`;
}

/** Shown only when the data folder is not the code repo: tells the teacher where the course files are. */
function dataFolderBlock(dataRoot: string): string {
  return [
    "# Data folder",
    "",
    `Course files live in the data folder \`${dataRoot}\`, not in the current directory. Every \`courses/...\`, \`graph/...\` or \`curriculum.json\` path in these instructions is relative to it: pass absolute paths (\`${dataRoot}/courses/...\`) to Read, Glob and Grep.`,
  ].join("\n");
}

function lessonBlock(dataRoot: string, lessonPath: string): string {
  let lesson;
  try {
    lesson = readLesson(dataRoot, lessonPath);
  } catch {
    /* bad path or malformed island */
  }
  if (!lesson) return `Lesson artifact: ${lessonPath} (unreadable)`;
  const { doc } = lesson;
  const m = doc.metadata;
  const ids = m.initialDag ? parseDagIds(m.initialDag) : [];
  const nodes = doc.nodes.map((n, i) => `- N${i + 1}: nodeId "${n.nodeId}", "${n.title}" (${n.tier})`);
  const next = ids[doc.nodes.length] ?? `N${doc.nodes.length + 1}`;
  const done = lesson.dagNodes > 0 && lesson.pendingNodes === 0;
  return [
    `Lesson artifact: ${lessonPath} (lesson number ${m.lessonNumber})`,
    `Topic: ${m.topic}`,
    `Title: ${m.title}`,
    `Goal: ${m.targetGoal ?? "(none)"}`,
    `Plan DAG (initialDag):\n${m.initialDag ?? "(none)"}`,
    `Captured nodes (${doc.nodes.length}${ids.length ? ` of ${ids.length} planned` : ""}):\n${nodes.join("\n") || "(none)"}`,
    done ? "All planned nodes are captured: go to Phase 4." : `First missing plan node: ${next}. Resume there.`,
  ].join("\n");
}

const OPEN_PICKED = "Opening: he picked these on the map. Start at once with a problem on them; do not ask him to choose a topic.";
const OPEN_TOOLS =
  "Opening: nothing picked on the map. Before your first message call mcp__app__browse_concepts on this course and open the concepts that look fragile or mancato with mcp__app__open_concept. Then ask him, in one short question, whether he wants exercises on a topic of his choice or on those weak concepts, naming up to three by name; if none is fragile or mancato, ask which topic he wants or whether you should choose. Wait for his answer before posing a problem.";

/** Propositions picked on the map: name, statement and the lessons they come from. */
async function propositionsBlock(dataRoot: string, ids: string[]): Promise<string> {
  const { readSnapshot } = await import("./graph/store.ts");
  const snap: any = await readSnapshot(dataRoot);
  const sessions: Record<string, string> = snap.sessions ?? {};
  const lines = ids.map((id) => {
    const n = (snap.nodes ?? []).find((x: any) => x.id === id);
    if (!n) return `- ${id} (not found in the map)`;
    const from = [...new Set<string>((n.provenance ?? []).map((p: any) => sessions[p.chatId] ?? p.chatId))].join("; ");
    return `- ${n.name} | ${n.statement}${from ? ` | lessons: ${from}` : ""}`;
  });
  return "Propositions selected on the map to practise (true statements; build the practice around them):\n" + lines.join("\n");
}

async function practiceBlock(dataRoot: string, config: SessionConfig): Promise<string> {
  if (config.practicePropositions?.length) {
    const props = await propositionsBlock(dataRoot, config.practicePropositions);
    return props + "\n\n" + OPEN_PICKED;
  }
  const map = await buildKnowledgeMap(dataRoot);
  const course = map.nodes.filter((n) => n.course === config.course);
  const sel = config.practiceNodes?.length
    ? config.practiceNodes.map((k) => course.find((n) => n.key === k) ?? map.nodes.find((n) => n.key === k)).filter((n) => !!n)
    : [];
  if (config.practiceNodes?.length && !sel.length) {
    return (
      "Selected nodes (keys only, not found in the map):\n" +
      config.practiceNodes.map((k) => `- ${k}`).join("\n") +
      "\n\n" +
      OPEN_PICKED
    );
  }
  // No pick: the bounded lesson-node list gives the problem tool valid keys; the tutor picks from it.
  const list = sel.length ? sel : course.slice(0, 80);
  const head = sel.length ? "Selected nodes to practise" : "Course nodes (pass the key as problem.nodeKeys)";
  const base =
    `${head}:\n` +
    (list.map((n) => `- key "${n.key}" | ${n.title} | status ${n.status} | lesson ${n.lessonTitle} (${n.lessonPath})`).join("\n") || "(the map is empty)");
  return base + "\n\n" + (sel.length ? OPEN_PICKED : OPEN_TOOLS);
}

async function sessionBlock(opts: BuildSystemPromptOptions): Promise<string> {
  const { config } = opts;
  const data = opts.dataRoot ?? opts.repoRoot;
  const lines = [
    "# This session",
    "",
    `Kind: ${config.kind}`,
    `Course: ${config.course}`,
    `Today: ${opts.today ?? new Date().toISOString().slice(0, 10)}`,
    `Model: ${config.model}`,
  ];
  if (config.kind === "teach") {
    const t = config.target;
    if (t?.mode === "planned") {
      lines.push(`Target: planned lesson. Title (use it as is, character for character, never reword): "${t.title}"`);
    } else if (t?.mode === "existing") {
      lines.push("Target: existing lesson, resume it.");
    } else {
      lines.push("Target: free lesson outside the plan; propose the title in propose_plan.");
    }
    lines.push(
      config.ankiOptIn
        ? "Anki opt-in: Sì (already asked by the UI; cards are authored and synced)."
        : "Anki opt-in: No (already asked by the UI; never author ankiParams, never mention cards)."
    );
    const lp = opts.lessonPath ?? (t?.mode === "existing" ? t.lessonPath : undefined);
    if (lp) lines.push("", lessonBlock(data, lp));
  } else if (config.kind === "plan") {
    lines.push(`Chapter title: ${config.chapterTitle ?? "(not given)"}`);
    const exists = fs.existsSync(path.join(data, "courses", config.course));
    lines.push(
      exists
        ? `Course directory courses/${config.course} exists${fs.existsSync(path.join(data, "courses", config.course, "MISSION.md")) ? " and has a MISSION.md: do not rewrite it" : ""}.`
        : `Course directory courses/${config.course} is new: provide a full mission.`
    );
  } else {
    lines.push("", await practiceBlock(data, config));
  }
  lines.push("", await courseIndexBlock(data));
  if (config.note?.trim()) lines.push("", `Learner's note: ${config.note.trim()}`);
  return lines.join("\n");
}

export async function buildSystemPrompt(opts: BuildSystemPromptOptions): Promise<string> {
  const { config, repoRoot } = opts;
  const data = opts.dataRoot ?? repoRoot;
  const parts: string[] = [readIf(path.join(PROMPTS_DIR, "base.md")) ?? "", readIf(path.join(PROMPTS_DIR, `${config.kind}.md`)) ?? ""];
  const refs: string[] = [];
  if (config.kind !== "practice") refs.push(fileBlock(repoRoot, "docs/agents/pedagogy.md"));
  if (config.kind === "teach" && config.ankiOptIn) refs.push(fileBlock(repoRoot, "docs/agents/anki-cards.md"));
  const refText = refs.filter(Boolean).join("\n\n");
  if (refText) {
    parts.push(
      "# Reference documents\n\nWritten for other runtimes. Where they mention `npm` commands, subagents, rendering or render-check tools, `ask_user_question`, or asking the Anki opt-in, the app tools and the `# This session` block override them (`mcp__app__ask`, WebSearch/WebFetch, the write/finish tools; the opt-in is already decided by the UI). The interactive quiz card is shuffled, but the mirror quiz in a node artifact is not: vary the position of the correct option. Everything else applies.\n\n" +
        refText
    );
  }
  const separate = path.resolve(data) !== path.resolve(repoRoot);
  parts.push([separate && opts.dataRoot ? dataFolderBlock(data) : "", courseContext(data, config)].filter(Boolean).join("\n\n"));
  parts.push(await sessionBlock(opts));
  return parts.filter(Boolean).join("\n\n---\n\n") + "\n";
}

export function openingMessage(config: SessionConfig, lessonTitle?: string): string {
  const note = config.note?.trim() ?? "";
  let head: string;
  if (config.kind === "teach") {
    const t = config.target;
    head =
      t?.mode === "planned"
        ? `Iniziamo la lezione "${t.title}".`
        : t?.mode === "existing"
          ? `Riprendiamo la lezione${lessonTitle ? ` "${lessonTitle}"` : ""} da dove l'abbiamo lasciata.`
          : `Iniziamo una lezione libera sul corso ${config.course}.`;
  } else if (config.kind === "plan") {
    head = `Pianifichiamo il corso "${config.chapterTitle ?? config.course}".`;
  } else {
    head = "Facciamo esercizi.";
  }
  return note ? `${head} ${note}` : head;
}
