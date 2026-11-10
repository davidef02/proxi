import { z } from "zod";
import { appendNode, type CognitiveNodePayload } from "../../../extensions/html-lesson.ts";
import type { WriteNodeInput } from "../../shared/protocol.ts";
import { readNodeLines } from "../graph/store.ts";
import { currentLesson, fail, json, lessonProgress, recomputePlan, tool, toRel } from "./util.ts";

// Authored HTML goes straight into the artifact: refuse active content.
// An on<event>= attribute only counts inside a real tag: escaped code samples (&lt;button onClick=...) are inert text.
// Only real tags (<letter...>) outside <pre>/<code> content are inspected: prose like "x < y" or "javascript: un URL" passes.
const TAG = /<[a-zA-Z](?:"[^"]*"|'[^']*'|[^>"'])*>/g;
export function unsafeHtml(v: string): boolean {
  if (/<(script|iframe)/i.test(v)) return true;
  const stripped = v.replace(/(<(pre|code)\b[^>]*>)[\s\S]*?(<\/\2>)/gi, "$1$3");
  return (stripped.match(TAG) ?? []).some((t) => /[\s"'/]on[a-z]+\s*=/i.test(t) || /javascript:/i.test(t));
}

const codeBlock = z.object({
  language: z.string().optional(),
  code: z.string(),
  caption: z.string().optional(),
});

const visual = z.object({
  type: z.enum(["svg-inline", "image", "mermaid"]),
  content: z.string(),
  caption: z.string().optional(),
  alt: z.string().optional(),
});

const step = z.object({
  stepId: z.string().min(1),
  title: z.string().optional(),
  content: z.string().refine((v) => !unsafeHtml(v), "unsafe HTML rejected"),
  math: z.array(z.string()).optional(),
  codeBlocks: z.array(codeBlock).optional(),
  visuals: z.array(visual).optional(),
});

const quizCheckpoint = z
  .object({
    question: z.string().min(1),
    options: z.array(z.string()).optional(),
    correctAnswer: z.string().optional().describe("The text of the correct option; must equal options[correctIndex - 1]."),
    correctIndex: z.number().int().optional().describe("1-based index into options."),
    explanation: z.string().min(1),
    misconceptions: z.array(z.object({ optionIndex: z.number().int(), misconception: z.string() })).optional().describe("optionIndex is 1-based."),
  })
  .superRefine((q, ctx) => {
    const n = q.options?.length ?? 0;
    if (n && q.correctIndex === undefined) ctx.addIssue({ code: "custom", path: ["correctIndex"], message: "required when options are given (1-based)" });
    if (n && q.correctAnswer === undefined) ctx.addIssue({ code: "custom", path: ["correctAnswer"], message: "required when options are given: the text of the correct option" });
    if (n && q.correctIndex !== undefined && (q.correctIndex < 1 || q.correctIndex > n)) {
      ctx.addIssue({ code: "custom", path: ["correctIndex"], message: `must be an integer from 1 to ${n} (1-based)` });
    } else if (n && q.correctIndex !== undefined && q.correctAnswer !== undefined && q.options![q.correctIndex - 1] !== q.correctAnswer) {
      ctx.addIssue({ code: "custom", path: ["correctAnswer"], message: "must equal options[correctIndex - 1]" });
    }
    q.misconceptions?.forEach((m, i) => {
      if (n && (m.optionIndex < 1 || m.optionIndex > n)) {
        ctx.addIssue({ code: "custom", path: ["misconceptions", i, "optionIndex"], message: `must be from 1 to ${n} (1-based)` });
      }
    });
  });

const ankiParams = z
  .object({
    targetQuestion: z.string().optional(),
    targetFact: z.string().optional(),
    suggestedCloze: z.string().optional(),
    clozeExtra: z.string().optional(),
    deck: z.string().optional(),
    tags: z.array(z.string()).optional(),
  })
  .superRefine((a, ctx) => {
    if (!!a.targetQuestion !== !!a.targetFact) ctx.addIssue({ code: "custom", message: "targetQuestion and targetFact go together" });
    if ((a.targetQuestion || a.targetFact) && a.suggestedCloze) ctx.addIssue({ code: "custom", message: "use either targetQuestion+targetFact or suggestedCloze, never both" });
    if (a.suggestedCloze && !/\{\{c\d+::/.test(a.suggestedCloze)) ctx.addIssue({ code: "custom", path: ["suggestedCloze"], message: "must contain a cloze like {{c1::...}}" });
  });

const anchor = z.object({
  id: z.string().regex(/^n_\S+$/, 'a concept id like "n_…" from search_concepts'),
  name: z.string().min(1),
});

/** Prose and concept names are compared the same way: lowercased, without emphasis, trimmed and with single spaces. */
const normalise = (s: string) => s.replace(/[`*_]/g, "").replace(/\s+/g, " ").trim().toLowerCase();

/** What the learner reads, normalised: where an anchor's name shows up in the prose. */
function proseText(n: CognitiveNodePayload): string {
  const parts: string[] = [n.title];
  if (n.motivation) parts.push(n.motivation);
  if (n.content) parts.push(n.content);
  if (n.connection) parts.push(n.connection);
  for (const s of n.steps ?? []) for (const v of Object.values(s)) if (typeof v === "string") parts.push(v);
  return normalise(parts.join("\n").replace(/<[^>]*>/g, " "));
}

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Live concepts whose name the node's prose already uses: the anchors it should have declared. */
function namedConcepts(node: CognitiveNodePayload, nodes: readonly { id: string; name: string }[]): { id: string; name: string }[] {
  const prose = proseText(node);
  return nodes
    .filter((n) => {
      const name = normalise(n.name);
      return name.length >= 4 && new RegExp(`(^|[^a-z0-9])${escapeRe(name)}([^a-z0-9]|$)`).test(prose);
    })
    .slice(0, 3);
}

export const nodeSchema = z.object({
  nodeId: z.string().regex(/^[a-z0-9]+(?:-[a-z0-9]+)*$/, "kebab-case slug").describe("Unique in the lesson; writing the same nodeId again replaces the node."),
  title: z.string().min(1),
  tier: z.enum(["unconditional-truth", "axiom", "derivation"]),
  summary: z.string().optional().describe("Concise 1-2 sentence distilled claim for the Sintesi reference sheet."),
  motivation: z.string().optional().describe("Authored HTML or prose."),
  content: z.string().optional().describe("Authored HTML or prose: the Establish step."),
  steps: z.array(step).optional(),
  math: z.array(z.string()).optional(),
  codeBlocks: z.array(codeBlock).optional(),
  visuals: z.array(z.union([visual, z.string()])).optional(),
  dependencies: z.array(z.string()).optional(),
  anchors: z.array(anchor).optional().describe("Known concepts this idea builds on: id and name from search_concepts/open_concept/browse_concepts. Name them in the prose too."),
  connection: z.string().optional(),
  quiz: quizCheckpoint.optional(),
  ankiParams: ankiParams.optional().describe("Only when the learner opted in AND passed the check for this node: write the node first without it, then rewrite the same nodeId with ankiParams after the pass."),
});

const payload = nodeSchema;

export const writeNodeSchema = z
  .object({
    planNode: z.string().regex(/^N\d+$/, 'must look like "N3"').describe("The plan node this realises."),
    node: nodeSchema,
  })
  .superRefine(({ node }, ctx) => {
    if ((!node.steps || node.steps.length === 0) && (!node.content || !node.content.trim())) {
      ctx.addIssue({ code: "custom", path: ["node", "content"], message: "content is required when steps are not provided" });
    }
    const fields: [string, string | undefined][] = [
      ["summary", node.summary],
      ["motivation", node.motivation],
      ["content", node.content],
      ["connection", node.connection],
      ["quiz.question", node.quiz?.question],
      ["quiz.explanation", node.quiz?.explanation],
      ...(node.visuals ?? []).map((v, i): [string, string] => [`visuals[${i}]`, typeof v === "string" ? v : v.content]),
      ...(node.steps ?? []).flatMap((s, sIdx) => [
        ...(s.visuals ?? []).map((v, i): [string, string] => [`steps[${sIdx}].visuals[${i}]`, v.content]),
      ]),
    ];
    for (const [name, v] of fields) {
      if (v && unsafeHtml(v)) ctx.addIssue({ code: "custom", path: ["node", name], message: "active HTML (<script, <iframe, on*= handlers, javascript:) is not allowed" });
    }
  });

export const writeNodeTool = tool<WriteNodeInput>({
  name: "write_node",
  description: "Write (or replace, same nodeId) one node into the lesson artifact and show it to the learner. Requires an approved plan. Every new idea carries `anchors` (n_… ids and names from the concept tools) and its prose names those concepts.",
  schema: writeNodeSchema as unknown as z.ZodType<WriteNodeInput>,
  writes: true,
  async run(input, ctx) {
    if (!ctx.getLessonPath()) return fail("No lesson artifact yet: get the plan approved with propose_plan first.");
    const lesson = currentLesson(ctx);
    if (!lesson) return fail("The lesson artifact is missing or has no data island.");
    const { abs, doc } = lesson;

    const liveNodes = readNodeLines(ctx.dataRoot);
    const prev = doc.nodes.find((n) => n.nodeId === input.node.nodeId);
    if (input.node.anchors?.length) {
      const live = new Set(liveNodes.map((n) => n.id));
      // a rewrite may resend an anchor whose concept was deleted since: it just degrades to text
      const kept = new Set((prev?.anchors ?? []).map((a) => a.id));
      const unknown = input.node.anchors.map((a) => a.id).filter((id) => !live.has(id) && !kept.has(id));
      if (unknown.length) return fail(`anchors: ${unknown.join(", ")} not in the concept map. Use ids returned by search_concepts/open_concept/browse_concepts, or drop the anchor.`);
    }

    const node = { ...input.node } as CognitiveNodePayload;
    if (node.steps && node.steps.length > 0 && (!node.content || !node.content.trim())) {
      node.content = node.steps.map((s) => s.content).join("\n\n");
    }
    const notes: string[] = [];
    if (!node.anchors?.length) {
      // the prose is the source of truth: if it names a live concept, the anchor is missing, not optional
      const named = namedConcepts(node, liveNodes);
      if (named.length) {
        notes.push(`anchors missing: the prose names ${named.map((n) => `${n.name} (${n.id})`).join(", ")}. Resend this node with anchors to link it on the map.`);
      } else if (prev?.anchors?.length) {
        notes.push(`anchors dropped: this rewrite of ${node.nodeId} carries none; resend its anchors or they are lost.`);
      }
    } else {
      // an anchor the prose never names is invisible to the learner: ask for the same sentence with the stored name
      const prose = proseText(node);
      const liveNames = new Map(liveNodes.map((n) => [n.id, n.name]));
      // a kept anchor whose concept was deleted falls back to the name the tutor sent: it is plain text now
      const unnamed = node.anchors
        .map((a) => ({ id: a.id, name: liveNames.get(a.id) ?? a.name }))
        .filter((a) => !prose.includes(normalise(a.name)));
      if (unnamed.length) {
        notes.push(`anchors not named in the prose: ${unnamed.map((a) => `${a.name} (${a.id})`).join(", ")}. The prose must name each anchored concept verbatim; resend this node with it.`);
      }
    }
    if (node.ankiParams) {
      if (!ctx.config.ankiOptIn) {
        delete node.ankiParams;
        notes.push("ankiParams removed: the learner did not opt in to Anki.");
      } else {
        const latest = (await lessonProgress(ctx)).filter((r) => r.kind === "check" && r.nodeRefs.includes(input.planNode)).at(-1);
        if (!latest || latest.correct !== true || latest.dontKnow) {
          delete node.ankiParams;
          notes.push("ankiParams removed: no passed check for this node yet. ankiParams kept after the check passes: call write_node again with ankiParams after a passed check, then sync_anki.");
        }
      }
    }
    const existing = doc.nodes.some((n) => n.nodeId === node.nodeId);
    if (!existing && input.planNode !== `N${doc.nodes.length + 1}`) {
      notes.push(`warning: this is node ${doc.nodes.length + 1} of the lesson but planNode is ${input.planNode}; nodes are expected in plan order (N${doc.nodes.length + 1}).`);
    }

    const res = await appendNode({ lessonPath: abs, node, baseDir: ctx.dataRoot });
    const lessonPath = toRel(ctx.dataRoot, abs);
    ctx.emit({ id: `node:${node.nodeId}`, kind: "node", nodeId: node.nodeId, planNode: input.planNode, title: node.title, lessonPath, node });
    ctx.emitEvent({ type: "lesson", lessonPath, nodeId: node.nodeId });
    await recomputePlan(ctx);
    return json({ ok: true, nodeCount: res.nodeCount, ...(notes.length ? { notes } : {}) });
  },
});
