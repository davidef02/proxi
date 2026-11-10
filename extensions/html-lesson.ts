/**
 * html-lesson.ts — Standalone HTML Lesson Engine & Distilled Reference Compiler
 *
 * Part of the Proxi teaching engine.
 * Supports programmatic TS imports, CLI execution, and cross-agent interoperability.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { lessonSlug, slugify } from "./slug.ts";
import { enterDataRoot } from "./data-root.ts";
import { APP_NAME, DECK_ROOT } from "../app/shared/identity.mjs";

// ────────────────────────────────────────────────────────────────────────────
// Types & Data Contracts
// ────────────────────────────────────────────────────────────────────────────

export type PedagogicalTier = "unconditional-truth" | "axiom" | "derivation";

export interface SintesiNode {
  nodeId: string;
  number: number;
  title: string;
  tier: PedagogicalTier;
  summary: string;
  keyFormula?: string;
  keyCode?: { language?: string; code: string; caption?: string };
}

export interface SintesiMisconception {
  description: string;
  sourceNode?: string;
}

export interface SintesiDocument {
  title: string;
  topic?: string;
  slug: string;
  initialDag?: string;
  nodes: SintesiNode[];
  formulas: { math: string; sourceNode: string }[];
  misconceptions: SintesiMisconception[];
  evidence?: string;
}

export interface LessonMetadata {
  lessonNumber: number;
  topic: string;
  title: string;
  slug: string;
  description?: string;
  dateCreated: string;
  dateModified: string;
  initialDag?: string;
  targetGoal?: string;
  prerequisites?: string[];
  tags?: string[];
  version: string;
}

export interface QuizDistractorMisconception {
  optionIndex: number;
  misconception: string;
}

export interface QuizCheckpointData {
  question: string;
  options?: string[];
  correctAnswer?: string;
  correctIndex?: number;
  explanation: string;
  misconceptions?: QuizDistractorMisconception[];
}

export interface NodeVisual {
  type: "svg-inline" | "image" | "mermaid";
  content: string;
  caption?: string;
  alt?: string;
}

export interface NodeCodeBlock {
  language: string;
  code: string;
  caption?: string;
}

export interface CognitiveStepPayload {
  stepId: string;
  title?: string;
  content: string; // Authored HTML or markdown prose
  math?: string[];
  codeBlocks?: { language?: string; code: string; caption?: string }[];
  visuals?: ({ type: "svg-inline" | "image" | "mermaid"; content: string; caption?: string; alt?: string } | string)[];
}

export interface CognitiveNodePayload {
  nodeId: string;
  title: string;
  tier: PedagogicalTier;
  /** Concise distilled claim for the Sintesi / reference cheat sheet. */
  summary?: string;
  motivation?: string;
  content: string;
  math?: string[];
  codeBlocks?: NodeCodeBlock[];
  visuals?: (NodeVisual | string)[];
  steps?: CognitiveStepPayload[];
  dependencies?: string[];
  /** Known graph concepts (global "n_…" ids) this idea builds on; the name survives if the id is deleted. */
  anchors?: { id: string; name: string }[];
  connection?: string;
  quiz?: QuizCheckpointData;
  ankiParams?: {
    /** Italian question for the Basic card front. No question, no Basic card. */
    targetQuestion?: string;
    targetFact?: string;
    suggestedCloze?: string;
    /** One short line for the cloze "Back Extra". Never the node prose. */
    clozeExtra?: string;
    deck?: string;
    tags?: string[];
  };
  capturedAt?: string;
}

export interface FullLessonDocument {
  metadata: LessonMetadata;
  nodes: CognitiveNodePayload[];
}

export interface InitLessonOptions {
  topic: string;
  title?: string;
  slug?: string;
  description?: string;
  dagMermaid?: string;
  initialDag?: string;
  targetGoal?: string;
  baseDir?: string;
  lessonsDir?: string;
  templatePath?: string;
}

export interface InitLessonResult {
  filePath: string;
  lessonNumber: number;
  slug: string;
}

export interface AppendNodeOptions {
  lessonPath?: string;
  filepath?: string;
  slug?: string;
  node: CognitiveNodePayload;
  baseDir?: string;
  lessonsDir?: string;
}

export interface AppendNodeResult {
  filePath: string;
  lessonPath: string;
  filepath: string;
  nodeCount: number;
  nodeId: string;
}

export interface CompileReferenceOptions {
  lessonPath?: string;
  lessonFilepath?: string;
  slug?: string;
  baseDir?: string;
  outputDir?: string;
  lessonsDir?: string;
  learningRecordPath?: string;
  learningRecordContent?: string;
}

export interface CompileReferenceResult {
  filePath: string;
  filepath: string;
  slug: string;
  unconditionalTruthCount: number;
  axiomsCount: number;
  formulaCount: number;
  nodeCount: number;
  derivationCount?: number;
  misconceptionsCount?: number;
  document?: SintesiDocument;
}

// ────────────────────────────────────────────────────────────────────────────
// Utilities: Numbering, Slugs & HTML Formatting
// ────────────────────────────────────────────────────────────────────────────

// The slug rule lives in slug.ts and nowhere else: the plan and the taught artifact must
// slug the same string identically, or the homepage join turns every lesson into an orphan.
export { lessonSlug, slugify };

/**
 * Reads the embedded lesson document out of a lesson artifact's data island.
 * Returns null when the HTML carries no island — a hand-authored lesson, not an artifact.
 */
export function parseLessonDocument(rawHtml: string): FullLessonDocument | null {
  const dataMatch = new RegExp('<script type="application/json" id="lesson-data">([\\s\\S]*?)</script>').exec(rawHtml);
  if (!dataMatch) return null;
  try {
    return JSON.parse(dataMatch[1].trim()) as FullLessonDocument;
  } catch (err: any) {
    // Distinct from "no island": the artifact claims to be one and isn't readable. Callers that
    // scan many lessons report the file and move on; a single one must not hide the rest.
    throw new Error("Malformed #lesson-data island: " + err.message);
  }
}

/** Escapes a string for literal use inside a RegExp. */
function escapeRegExp(str: string): string {
  return str.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Resolves a lesson artifact by slug inside a lessons directory. The match is anchored on the whole
 * `NNNN-<slug>.html` name: an unanchored substring match resolved `--slug come-nginx` to
 * `0009-come-nginx-trova-il-backend.html` and then rewrote that file.
 */
export function findLessonBySlug(lessonsDir: string, slug: string): string | undefined {
  if (!fs.existsSync(lessonsDir)) return undefined;
  const pattern = new RegExp("^\\d+-" + escapeRegExp(slug) + "\\.html$");
  const match = fs.readdirSync(lessonsDir).sort().find((f) => pattern.test(f));
  return match ? path.join(lessonsDir, match) : undefined;
}

/**
 * Serializes a lesson document into its data island. `<` is escaped to its JSON unicode form — still valid JSON,
 * but a node whose content contains `</script>` can no longer terminate the island early and make
 * the artifact unreadable to the board, the append path and the Anki sync.
 */
export function renderDataIsland(doc: FullLessonDocument): string {
  return "<script type=\"application/json\" id=\"lesson-data\">\n" +
    JSON.stringify(doc, null, 2).replace(/</g, "\\u003c") + "\n  </script>";
}

export function escapeHtml(str: string): string {
  if (!str) return "";
  return str
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

export function parseMarkdownProse(markdown: string): string {
  if (!markdown) return "";
  const lines = markdown.split("\n");
  const rendered = [];
  let inList = false;
  // <pre> nesting depth: a single line may close one block and open the next, so a boolean
  // flipped on "</pre>" would drop out of code one block too early.
  let preDepth = 0;
  const preDelta = (s: string) =>
    (s.match(/<pre[\s>]/gi) || []).length - (s.match(/<\/pre>/gi) || []).length;

  for (let i = 0; i < lines.length; i++) {
    // Inside <pre> every line is code: keep it verbatim, whitespace included, no <p> wrapping.
    if (preDepth > 0) {
      rendered.push(lines[i]);
      preDepth += preDelta(lines[i]);
      continue;
    }

    const line = lines[i].trim();

    // A line that starts with a tag, or opens a <pre> mid-line, is already HTML (authored
    // content): pass it through. Wrapping it in <p> would put a block inside a paragraph.
    if (line.startsWith("<") || /<pre[\s>]/i.test(line)) {
      if (inList) {
        rendered.push("</ul>");
        inList = false;
      }
      rendered.push(lines[i]);
      preDepth += preDelta(line);
      continue;
    }

    if (!line) {
      if (inList) {
        rendered.push("</ul>");
        inList = false;
      }
      continue;
    }

    if (line.startsWith("- ") || line.startsWith("* ")) {
      if (!inList) {
        rendered.push("<ul class=\"prose-list\">");
        inList = true;
      }
      const itemContent = formatInlineMarkdown(line.slice(2));
      rendered.push("  <li>" + itemContent + "</li>");
      continue;
    }

    if (inList) {
      rendered.push("</ul>");
      inList = false;
    }

    if (line.startsWith("### ")) {
      rendered.push("<h4>" + formatInlineMarkdown(line.slice(4)) + "</h4>");
    } else if (line.startsWith("## ")) {
      rendered.push("<h3>" + formatInlineMarkdown(line.slice(3)) + "</h3>");
    } else {
      rendered.push("<p>" + formatInlineMarkdown(line) + "</p>");
    }
  }

  if (inList) rendered.push("</ul>");
  return rendered.join("\n");
}

function formatInlineMarkdown(text: string): string {
  return text
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(/\*(.+?)\*/g, "<em>$1</em>")
    .replace(/`([^`]+)`/g, "<code>$1</code>");
}

export function getNextLessonNumber(lessonsDir: string): number {
  if (!fs.existsSync(lessonsDir)) {
    return 1;
  }
  const files = fs.readdirSync(lessonsDir);
  let maxNum = 0;
  for (const f of files) {
    const match = /^(\d{4})-[^.]+\.html$/.exec(f);
    if (match) {
      const num = parseInt(match[1], 10);
      if (num > maxNum) maxNum = num;
    }
  }
  return maxNum + 1;
}

export function formatLessonFilename(num: number, slug: string): string {
  const padded = String(num).padStart(4, "0");
  return padded + "-" + slug + ".html";
}

// ────────────────────────────────────────────────────────────────────────────
// Template Loader
// ────────────────────────────────────────────────────────────────────────────

function loadTemplate(baseDir: string, templatePath?: string): string {
  let moduleDir = "";
  try {
    moduleDir = path.dirname(fileURLToPath(import.meta.url));
  } catch {
    moduleDir = process.cwd();
  }

  const possiblePaths = [
    templatePath,
    templatePath ? path.resolve(baseDir, templatePath) : undefined,
    path.resolve(baseDir, "assets", "lesson-template.html"),
    path.resolve(process.cwd(), "assets", "lesson-template.html"),
    path.resolve(moduleDir, "..", "assets", "lesson-template.html"),
  ].filter((p): p is string => Boolean(p));

  for (const p of possiblePaths) {
    if (fs.existsSync(p)) {
      return fs.readFileSync(p, "utf-8");
    }
  }

  throw new Error("Could not locate assets/lesson-template.html");
}

// ────────────────────────────────────────────────────────────────────────────
// Node HTML Renderer
// ────────────────────────────────────────────────────────────────────────────

function renderMathHtml(math?: string[]): string {
  if (!math || math.length === 0) return "";
  return "<div class=\"node-math-container\">" +
    math.map((m) => {
      const isDisplay = m.startsWith("$$") && m.endsWith("$$");
      const formatted = isDisplay ? m : "$$ " + m + " $$";
      return "<div class=\"math-card\">" + formatted + "</div>";
    }).join("\n") +
    "</div>";
}

function renderCodeHtml(codeBlocks?: { language?: string; code: string; caption?: string }[]): string {
  if (!codeBlocks || codeBlocks.length === 0) return "";
  return codeBlocks.map((cb) =>
    "<div class=\"code-container\">" +
    (cb.caption ? "<div class=\"code-caption\">" + escapeHtml(cb.caption) + "</div>" : "") +
    "<pre><code class=\"language-" + escapeHtml(cb.language || "") + "\">" + escapeHtml(cb.code) + "</code></pre>" +
    "</div>"
  ).join("\n");
}

function renderVisualHtml(visuals?: ({ type: "svg-inline" | "image" | "mermaid"; content: string; caption?: string; alt?: string } | string)[]): string {
  if (!visuals || visuals.length === 0) return "";
  return visuals.map((v) => {
    if (typeof v === "string") {
      if (v.trim().startsWith("<svg")) {
        return "<figure class=\"svg-figure\">" + v + "</figure>";
      }
      return "<figure class=\"svg-figure\"><img src=\"" + escapeHtml(v) + "\" alt=\"Visual\"/></figure>";
    }
    if (v.type === "svg-inline") {
      return "<figure class=\"svg-figure\">" + v.content + (v.caption ? "<figcaption>" + escapeHtml(v.caption) + "</figcaption>" : "") + "</figure>";
    } else if (v.type === "mermaid") {
      return "<div class=\"diagram-container\"><pre class=\"mermaid\">" + v.content + "</pre>" + (v.caption ? "<div class=\"diagram-title\">" + escapeHtml(v.caption) + "</div>" : "") + "</div>";
    } else {
      return "<figure class=\"svg-figure\"><img src=\"" + escapeHtml(v.content) + "\" alt=\"" + escapeHtml(v.alt || "") + "\"/>" + (v.caption ? "<figcaption>" + escapeHtml(v.caption) + "</figcaption>" : "") + "</figure>";
    }
  }).join("\n");
}

export function renderNodeHtml(node: CognitiveNodePayload, index: number): string {
  const isAxiom = node.tier === "unconditional-truth" || node.tier === "axiom";
  const tierClass = isAxiom ? "badge-axiom" : "badge-derivation";
  const tierLabel = isAxiom ? "Unconditional Truth" : "Derivation";

  const mathHtml = renderMathHtml(node.math);
  const codeHtml = renderCodeHtml(node.codeBlocks);
  const visualHtml = renderVisualHtml(node.visuals);

  let quizHtml = "";
  if (node.quiz) {
    const q = node.quiz;
    let optionsHtml = "";
    if (q.options && q.options.length > 0) {
      // correctIndex is 1-based, matching the "1. / 2. / 3." the learner sees. An out-of-range
      // value used to mark nothing as correct, silently — the artifact then taught the quiz wrong.
      if (q.correctIndex !== undefined && (q.correctIndex < 1 || q.correctIndex > q.options.length)) {
        throw new Error(
          "quiz.correctIndex is 1-based: got " + q.correctIndex + " for " + q.options.length + " options"
        );
      }
      optionsHtml = "<ul class=\"quiz-options\">" +
        q.options.map((opt, i) => {
          const isCorrect = q.correctIndex !== undefined ? i + 1 === q.correctIndex : false;
          const letter = String.fromCharCode(65 + i);
          const misconceptionObj = q.misconceptions?.find((m) => m.optionIndex === i + 1);
          const misconceptionAttr = misconceptionObj ? " data-misconception=\"" + escapeHtml(misconceptionObj.misconception) + "\"" : "";
          return "<li class=\"quiz-option-item\">" +
            "<button type=\"button\" class=\"quiz-option quiz-option-btn" + (isCorrect ? " is-correct-target" : "") + "\" data-index=\"" + (i + 1) + "\"" + misconceptionAttr + ">" +
            "<span class=\"quiz-option-letter\">" + letter + "</span>" +
            "<span class=\"quiz-option-text\">" + escapeHtml(opt) + "</span>" +
            "<span class=\"quiz-option-status\" aria-hidden=\"true\"></span>" +
            "</button>" +
            "</li>";
        }).join("\n") +
        "</ul>";
    } else if (q.correctAnswer) {
      optionsHtml = "<ul class=\"quiz-options\"><li class=\"quiz-option-item\"><button type=\"button\" class=\"quiz-option quiz-option-btn is-correct-target\" data-index=\"1\"><span class=\"quiz-option-letter\">A</span><span class=\"quiz-option-text\">" + escapeHtml(q.correctAnswer) + "</span><span class=\"quiz-option-status\" aria-hidden=\"true\"></span></button></li></ul>";
    }

    const explanationContent = q.explanation ? parseMarkdownProse(q.explanation) : "";

    quizHtml = "<div class=\"step-quiz\" data-quiz-interactive=\"true\">" +
      "<div class=\"step-label\" style=\"color: var(--step-quiz-text);\">🎯 Diagnostic Checkpoint</div>" +
      "<div class=\"quiz-question\">" + parseMarkdownProse(q.question) + "</div>" +
      (q.options && q.options.length > 1 ? "<div class=\"quiz-instruction\">Seleziona un'opzione per verificare la comprensione:</div>" : "") +
      optionsHtml +
      "<div class=\"quiz-feedback-banner\" style=\"display: none;\" role=\"alert\">" +
      "<div class=\"quiz-feedback-title\"></div>" +
      "<div class=\"quiz-feedback-detail\"></div>" +
      "</div>" +
      "<div class=\"quiz-action-bar\" style=\"display: none;\">" +
      "<button type=\"button\" class=\"quiz-retry-btn\">🔄 Riprova il Quiz</button>" +
      "</div>" +
      "<details class=\"quiz-reveal\">" +
      "<summary>Reveal Answer & Diagnostic Reasoning</summary>" +
      "<div class=\"quiz-explanation\">" +
      (q.correctAnswer ? "<strong>Correct Claim:</strong> " + escapeHtml(q.correctAnswer) + "<br/>" : "") +
      explanationContent +
      "</div>" +
      "</details>" +
      "</div>";
  }

  const motivateBlock = node.motivation
    ? "<div class=\"step-motivate\"><div class=\"step-label\">⚡ Motivating Tension</div>" + parseMarkdownProse(node.motivation) + "</div>"
    : "";

  const connectBlock = node.connection
    ? "<div class=\"step-connect\"><div class=\"step-label\">🔗 Connessione Concettuale</div>" + parseMarkdownProse(node.connection) + "</div>"
    : (node.dependencies && node.dependencies.length > 0)
    ? "<div class=\"step-connect\"><div class=\"step-label\">🔗 Connessione Concettuale</div><p>Si collega a: " + node.dependencies.map(d => "<code>" + escapeHtml(d) + "</code>").join(", ") + "</p></div>"
    : "";

  const captureBlock = "<footer class=\"step-capture\">" +
    "<span>Captured in Deck: <code>" + escapeHtml(node.ankiParams?.deck || `${DECK_ROOT}::Topic`) + "</code></span>" +
    // Mirrors generateCardsFromNode: the question card wins, the cloze is the fallback.
    "<span>Card Type: " + (node.ankiParams?.targetQuestion && node.ankiParams.targetFact ? "Basic Front/Back"
      : node.ankiParams?.suggestedCloze ? "Cloze" : "nessuna") + "</span>" +
    "</footer>";

  let coreContentHtml = "";
  if (node.steps && node.steps.length > 0) {
    const stepsRendered = node.steps.map((s) => {
      const sMath = renderMathHtml(s.math);
      const sCode = renderCodeHtml(s.codeBlocks);
      const sVisual = renderVisualHtml(s.visuals);
      return "<div class=\"node-step\" id=\"step-" + escapeHtml(s.stepId) + "\" data-step-id=\"" + escapeHtml(s.stepId) + "\">" +
        "<div class=\"step-header\">" +
        "<span class=\"step-id\">" + escapeHtml(s.stepId) + "</span>" +
        (s.title ? "<h3 class=\"step-title\">" + escapeHtml(s.title) + "</h3>" : "") +
        "</div>" +
        "<div class=\"step-content prose\">" +
        parseMarkdownProse(s.content) +
        "</div>" +
        (sMath ? "\n" + sMath : "") +
        (sCode ? "\n" + sCode : "") +
        (sVisual ? "\n" + sVisual : "") +
        "</div>";
    }).join("\n");

    coreContentHtml = "<div class=\"node-steps\">\n" + stepsRendered + "\n</div>" + mathHtml + codeHtml + visualHtml;
  } else {
    coreContentHtml = "<div class=\"step-establish prose\">" +
      parseMarkdownProse(node.content || "") +
      "</div>" +
      mathHtml +
      codeHtml +
      visualHtml;
  }

  return "<article class=\"cognitive-node\" id=\"node-" + escapeHtml(node.nodeId) + "\" data-tier=\"" + escapeHtml(node.tier) + "\">" +
    "<header class=\"node-header\">" +
    "<div class=\"node-title-group\">" +
    "<span class=\"node-number\">Node " + String(index + 1).padStart(2, "0") + "</span>" +
    "<h2 class=\"node-title\">" + escapeHtml(node.title) + "</h2>" +
    "</div>" +
    "<span class=\"badge " + tierClass + "\">" + tierLabel + "</span>" +
    "</header>" +
    motivateBlock +
    coreContentHtml +
    connectBlock +
    quizHtml +
    captureBlock +
    "</article>";
}

// ────────────────────────────────────────────────────────────────────────────
// Core Operations
// ────────────────────────────────────────────────────────────────────────────

export async function initLesson(options: InitLessonOptions): Promise<InitLessonResult> {
  const baseDir = options.baseDir || process.cwd();
  const lessonsDir = options.lessonsDir || path.resolve(baseDir, "lessons");

  if (!fs.existsSync(lessonsDir)) {
    fs.mkdirSync(lessonsDir, { recursive: true });
  }

  const lessonNumber = getNextLessonNumber(lessonsDir);
  const title = options.title || options.topic;
  // The slug comes from the TITLE: the plan only knows the title, so slugging the topic
  // would orphan the artifact whenever topic and title differ.
  const slug = options.slug ? slugify(options.slug) : lessonSlug(title);
  const filename = formatLessonFilename(lessonNumber, slug);
  const filePath = path.join(lessonsDir, filename);

  // Slug is the join identity within a course. A second artifact with the same slug is invisible
  // on the board (the plan entry claims only one of them), so refuse it at creation.
  const twin = findLessonBySlug(lessonsDir, slug);
  if (twin) {
    throw new Error(
      "A lesson with slug \"" + slug + "\" already exists in this course: " + path.basename(twin) +
      ". Reuse it, or give this lesson a distinct title."
    );
  }

  const rawTemplate = loadTemplate(baseDir, options.templatePath);

  const now = new Date().toISOString();
  const dateStr = now.slice(0, 10);
  const dag = options.dagMermaid || options.initialDag || "";

  const metadata: LessonMetadata = {
    lessonNumber,
    topic: options.topic,
    title,
    slug,
    description: options.description,
    dateCreated: now,
    dateModified: now,
    initialDag: dag,
    targetGoal: options.targetGoal,
    version: "1.0.0",
  };

  const dagSection = dag
    ? "<section class=\"diagram-container\" id=\"dagContainer\">" +
      "<div class=\"diagram-title\">Curriculum Dependency Graph</div>" +
      "<pre class=\"mermaid\">\n" + dag + "\n</pre>" +
      "</section>"
    : "";

  const doc: FullLessonDocument = {
    metadata,
    nodes: [],
  };

  const dataIsland = renderDataIsland(doc);

  let html = rawTemplate
    .replace(/<!-- LESSON_TITLE_SLOT -->/g, () => escapeHtml(title))
    .replace(/<!-- APP_NAME_SLOT -->/g, () => APP_NAME)
    .replace(/<!-- LESSON_GOAL_SLOT -->/g, () => escapeHtml(options.description || options.targetGoal || ""))
    .replace(/<!-- LESSON_DATE_SLOT -->/g, () => "Generated: " + dateStr)
    .replace(/<!-- LESSON_NUMBER_SLOT -->/g, () => String(lessonNumber).padStart(4, "0"))
    .replace(/<!-- DAG_CONTAINER_SLOT -->/g, () => dagSection)
    .replace(/<!-- DATA_ISLAND_SLOT -->/g, () => dataIsland);

  fs.writeFileSync(filePath, html, "utf-8");

  return { filePath, lessonNumber, slug };
}

export async function appendNode(options: AppendNodeOptions): Promise<AppendNodeResult> {
  const baseDir = options.baseDir || process.cwd();
  let filePath = options.lessonPath || options.filepath;
  const lessonsDir = options.lessonsDir || path.resolve(baseDir, "lessons");

  if (!filePath && options.slug) filePath = findLessonBySlug(lessonsDir, options.slug);

  if (!filePath || !fs.existsSync(filePath)) {
    throw new Error("Lesson file not found: " + (filePath || options.slug));
  }

  const rawHtml = fs.readFileSync(filePath, "utf-8");
  const doc = parseLessonDocument(rawHtml);
  if (!doc) {
    throw new Error("Corrupted lesson file: missing #lesson-data island in " + filePath);
  }

  const node = options.node;
  node.capturedAt = new Date().toISOString();
  if (node.steps && node.steps.length > 0 && (!node.content || !node.content.trim())) {
    node.content = node.steps.map((s) => s.content).join("\n\n");
  }

  const existingIdx = doc.nodes.findIndex((n) => n.nodeId === node.nodeId);
  if (existingIdx >= 0) {
    doc.nodes[existingIdx] = node;
  } else {
    doc.nodes.push(node);
  }

  doc.metadata.dateModified = new Date().toISOString();

  const nodeIndex = existingIdx >= 0 ? existingIdx : doc.nodes.length - 1;
  const nodeHtml = renderNodeHtml(node, nodeIndex);

  let updatedHtml = rawHtml;
  if (existingIdx >= 0) {
    // The markup holds the id escaped, and a nodeId like `n1(v2)` is a live regex if interpolated
    // raw: the lookup would miss and the else-branch would inject a second copy of the node.
    const nodeRegex = new RegExp("<article class=\"cognitive-node\" id=\"node-" + escapeRegExp(escapeHtml(node.nodeId)) + "\"[\\s\\S]*?<\\/article>", "m");
    if (nodeRegex.test(rawHtml)) {
      updatedHtml = rawHtml.replace(nodeRegex, () => nodeHtml.trim());
    } else {
      updatedHtml = rawHtml.replace("<!-- NODES_INJECTION_POINT -->", () => nodeHtml.trim() + "\n      <!-- NODES_INJECTION_POINT -->");
    }
  } else {
    updatedHtml = rawHtml.replace("<!-- NODES_INJECTION_POINT -->", () => nodeHtml.trim() + "\n      <!-- NODES_INJECTION_POINT -->");
  }

  const updatedDataIsland = renderDataIsland(doc);
  updatedHtml = updatedHtml.replace(new RegExp('<script type="application/json" id="lesson-data">[\\s\\S]*?</script>'), () => updatedDataIsland);

  fs.writeFileSync(filePath, updatedHtml, "utf-8");

  return {
    filePath,
    lessonPath: filePath,
    filepath: filePath,
    nodeCount: doc.nodes.length,
    nodeId: node.nodeId,
  };
}

export function renderFullLesson(doc: FullLessonDocument, rawTemplate: string): string {
  const metadata = doc.metadata;
  const title = metadata.title || metadata.topic;
  const dateStr = (metadata.dateCreated || new Date().toISOString()).slice(0, 10);
  const dag = metadata.initialDag || "";

  const dagSection = dag
    ? "<section class=\"diagram-container\" id=\"dagContainer\">\n" +
      "      <div class=\"diagram-title\">Curriculum Dependency Graph</div>\n" +
      "      <pre class=\"mermaid\">\n" + dag + "\n</pre>\n" +
      "    </section>"
    : "";

  const nodesHtml = doc.nodes.map((node, index) => renderNodeHtml(node, index)).join("\n      ");
  const dataIsland = renderDataIsland(doc);

  let html = rawTemplate
    .replace(/<!-- LESSON_TITLE_SLOT -->/g, () => escapeHtml(title))
    .replace(/<!-- APP_NAME_SLOT -->/g, () => APP_NAME)
    .replace(/<!-- LESSON_GOAL_SLOT -->/g, () => escapeHtml(metadata.description || metadata.targetGoal || ""))
    .replace(/<!-- LESSON_DATE_SLOT -->/g, () => "Generated: " + dateStr)
    .replace(/<!-- LESSON_NUMBER_SLOT -->/g, () => String(metadata.lessonNumber).padStart(4, "0"))
    .replace(/<!-- DAG_CONTAINER_SLOT -->/g, () => dagSection)
    .replace(/<!-- NODES_INJECTION_POINT -->/g, () => nodesHtml + "\n      <!-- NODES_INJECTION_POINT -->")
    .replace(/<!-- DATA_ISLAND_SLOT -->/g, () => dataIsland);

  return html;
}

export async function rebuildLesson(options: { lessonPath?: string; slug?: string; baseDir?: string; lessonsDir?: string; templatePath?: string }): Promise<{ filePath: string; nodeCount: number }> {
  const baseDir = options.baseDir || process.cwd();
  const lessonsDir = options.lessonsDir || path.resolve(baseDir, "lessons");
  let filePath = options.lessonPath;
  if (!filePath && options.slug) filePath = findLessonBySlug(lessonsDir, options.slug);

  if (!filePath || !fs.existsSync(filePath)) {
    throw new Error("Lesson not found: " + (filePath || options.slug));
  }

  const rawHtml = fs.readFileSync(filePath, "utf-8");
  const doc = parseLessonDocument(rawHtml);
  if (!doc) {
    throw new Error("Corrupted lesson file: missing #lesson-data island in " + filePath);
  }

  const rawTemplate = loadTemplate(baseDir, options.templatePath);
  const newHtml = renderFullLesson(doc, rawTemplate);
  fs.writeFileSync(filePath, newHtml, "utf-8");

  return { filePath, nodeCount: doc.nodes.length };
}

/** Patches a lesson's metadata (e.g. an approved plan revision) and re-renders the artifact from its data island. */
export async function updateLessonMetadata(options: { lessonPath: string; baseDir?: string; patch: Partial<LessonMetadata> }): Promise<void> {
  const doc = parseLessonDocument(fs.readFileSync(options.lessonPath, "utf-8"));
  if (!doc) throw new Error("Corrupted lesson file: missing #lesson-data island in " + options.lessonPath);
  doc.metadata = { ...doc.metadata, ...options.patch, dateModified: new Date().toISOString() };
  fs.writeFileSync(options.lessonPath, renderFullLesson(doc, loadTemplate(options.baseDir || process.cwd())), "utf-8");
}

export function parseReferenceDocument(rawHtml: string): SintesiDocument | null {
  const match = /<script type="application\/json" id="reference-data">([\s\S]*?)<\/script>/.exec(rawHtml);
  if (!match) return null;
  try {
    return JSON.parse(match[1]);
  } catch {
    return null;
  }
}

export function extractDistilledClaim(node: CognitiveNodePayload): string {
  if (node.summary && node.summary.trim()) {
    return node.summary.trim();
  }
  if (node.ankiParams?.targetFact && node.ankiParams.targetFact.trim()) {
    return node.ankiParams.targetFact.trim();
  }
  const content = node.content || "";
  let caveatMatch = /<strong>\s*La verit[àa] da tenere senza caveat\.?\s*<\/strong>\s*([^<]+)/i.exec(content);
  if (!caveatMatch && node.steps && node.steps.length > 0) {
    caveatMatch = /<strong>\s*La verit[àa] da tenere senza caveat\.?\s*<\/strong>\s*([^<]+)/i.exec(node.steps[0].content || "");
  }
  if (caveatMatch && caveatMatch[1].trim()) {
    return caveatMatch[1].trim();
  }

  const rawSource = (node.steps && node.steps.length > 0 && node.steps[0].content && node.steps[0].content.trim())
    ? node.steps[0].content
    : content;

  const pMatch = /<p>([\s\S]*?)<\/p>/i.exec(rawSource);
  const rawText = pMatch ? pMatch[1] : rawSource;
  if (rawText && rawText.trim()) {
    const text = rawText.replace(/<[^>]+>/g, "").replace(/\s+/g, " ").trim();
    const sentences = text.split(/(?<=[.!?])\s+/);
    if (sentences && sentences.length > 0 && sentences[0].length > 0) {
      return sentences.slice(0, 2).join(" ").trim();
    }
    return text.slice(0, 200).trim();
  }
  return node.title;
}

export function extractKeySnippet(node: CognitiveNodePayload): {
  keyFormula?: string;
  keyCode?: { language?: string; code: string; caption?: string };
} {
  let keyFormula: string | undefined = undefined;
  if (node.math && node.math.length > 0) {
    keyFormula = node.math[0];
  } else if (node.steps && node.steps.length > 0) {
    for (const step of node.steps) {
      if (step.math && step.math.length > 0) {
        keyFormula = step.math[0];
        break;
      }
    }
  }

  let keyCode: { language?: string; code: string; caption?: string } | undefined = undefined;
  if (node.codeBlocks && node.codeBlocks.length > 0) {
    keyCode = node.codeBlocks[0];
  } else if (node.steps && node.steps.length > 0) {
    for (const step of node.steps) {
      if (step.codeBlocks && step.codeBlocks.length > 0) {
        keyCode = step.codeBlocks[0];
        break;
      }
    }
  } else {
    const codeMatch = /<pre><code(?:\s+class="language-([^"]+)")?>([\s\S]*?)<\/code><\/pre>/i.exec(node.content);
    const fenceMatch = /```(\w+)?\n([\s\S]*?)```/.exec(node.content);
    if (codeMatch) {
      const language = codeMatch[1] || "typescript";
      const rawCode = codeMatch[2]
        .replace(/&lt;/g, "<")
        .replace(/&gt;/g, ">")
        .replace(/&amp;/g, "&")
        .replace(/&#39;/g, "'")
        .replace(/&quot;/g, '"');
      const lines = rawCode.split("\n");
      const truncated = lines.length > 12 ? lines.slice(0, 10).join("\n") + "\n// ..." : rawCode;
      keyCode = { language, code: truncated.trim() };
    } else if (fenceMatch) {
      const language = fenceMatch[1] || "typescript";
      const rawCode = fenceMatch[2];
      const lines = rawCode.split("\n");
      const truncated = lines.length > 12 ? lines.slice(0, 10).join("\n") + "\n// ..." : rawCode;
      keyCode = { language, code: truncated.trim() };
    }
  }

  return { keyFormula, keyCode };
}

export function findLearningRecordForLesson(lessonPath: string, slug: string): string | null {
  const dir = path.dirname(path.resolve(lessonPath));
  const recDir = path.resolve(dir, "..", "learning-records");
  if (!fs.existsSync(recDir)) return null;

  const files = fs.readdirSync(recDir).filter((f) => /^\d{4}-.*\.md$/.test(f)).sort();
  const markerRegex = new RegExp(`<!--\\s*lesson:\\s*${slug}\\s*-->`);
  for (const f of files) {
    const full = path.join(recDir, f);
    try {
      const c = fs.readFileSync(full, "utf-8");
      if (markerRegex.test(c)) return full;
    } catch {}
  }
  const numMatch = /^(\d{4})-/.exec(path.basename(lessonPath));
  if (numMatch) {
    const prefix = numMatch[1] + "-";
    const hit = files.find((f) => f.startsWith(prefix));
    if (hit) return path.join(recDir, hit);
  }
  return null;
}

export function extractMisconceptionsFromRecord(recordMarkdown: string): SintesiMisconception[] {
  const norm = recordMarkdown.replace(/\r\n/g, "\n");
  const match = /(?:^|\n)## Evidence\s*\n([\s\S]*?)(?=\n##|\n<!--|$)/.exec(norm);
  if (!match) return [];
  const text = match[1].trim();
  const misconceptions: SintesiMisconception[] = [];

  const parseItem = (desc: string): SintesiMisconception => {
    const nodeMatch = /(?:Nodo|Node)\s+(\d+|[a-zA-Z0-9-]+)/i.exec(desc);
    return {
      description: desc,
      sourceNode: nodeMatch ? `Nodo ${nodeMatch[1]}` : undefined,
    };
  };

  const lines = text.split("\n").map((l) => l.trim());
  const bulletLines = lines.filter((l) => l.startsWith("- ") || l.startsWith("* "));
  if (bulletLines.length > 0) {
    for (const b of bulletLines) {
      misconceptions.push(parseItem(b.replace(/^[-*]\s+/, "")));
    }
  } else {
    const sentences = text.replace(/<[^>]+>/g, "").split(/(?<=[.!?])\s+/);
    for (const s of sentences) {
      const clean = s.replace(/\s+/g, " ").trim();
      if (/errore|difficolt|scambia|confond|buchi|slip|sbagli|attento|non colleg|contato|non risposto/i.test(clean)) {
        misconceptions.push(parseItem(clean));
      }
    }
    if (misconceptions.length === 0 && sentences.length > 0) {
      misconceptions.push(parseItem(sentences[0].replace(/\s+/g, " ").trim()));
    }
  }
  return misconceptions;
}

export function buildSintesiDocument(
  doc: FullLessonDocument,
  options: {
    learningRecordContent?: string;
    learningRecordPath?: string;
    lessonPath?: string;
    baseDir?: string;
  } = {}
): SintesiDocument {
  let recordMarkdown = options.learningRecordContent;
  if (!recordMarkdown && options.learningRecordPath && fs.existsSync(options.learningRecordPath)) {
    recordMarkdown = fs.readFileSync(options.learningRecordPath, "utf-8");
  }
  if (!recordMarkdown && options.lessonPath) {
    const recPath = findLearningRecordForLesson(options.lessonPath, doc.metadata.slug);
    if (recPath && fs.existsSync(recPath)) {
      recordMarkdown = fs.readFileSync(recPath, "utf-8");
    }
  }

  const misconceptions = recordMarkdown ? extractMisconceptionsFromRecord(recordMarkdown) : [];

  const nodes: SintesiNode[] = doc.nodes.map((n, idx) => {
    const summary = extractDistilledClaim(n);
    const { keyFormula, keyCode } = extractKeySnippet(n);
    return {
      nodeId: n.nodeId,
      number: idx + 1,
      title: n.title,
      tier: n.tier,
      summary,
      keyFormula,
      keyCode,
    };
  });

  const allFormulas: { math: string; sourceNode: string }[] = [];
  for (const n of doc.nodes) {
    if (n.math) {
      for (const m of n.math) {
        allFormulas.push({ math: m, sourceNode: n.title });
      }
    }
    if (n.steps) {
      for (const s of n.steps) {
        if (s.math) {
          for (const m of s.math) {
            allFormulas.push({ math: m, sourceNode: n.title });
          }
        }
      }
    }
  }

  return {
    title: doc.metadata.title || doc.metadata.topic,
    topic: doc.metadata.topic,
    slug: doc.metadata.slug,
    initialDag: doc.metadata.initialDag,
    nodes,
    formulas: allFormulas,
    misconceptions,
  };
}

export async function compileReference(options: CompileReferenceOptions): Promise<CompileReferenceResult> {
  const baseDir = options.baseDir || process.cwd();
  let filePath = options.lessonPath || options.lessonFilepath;
  const lessonsDir = options.lessonsDir || path.resolve(baseDir, "lessons");
  const outDir = options.outputDir || path.resolve(baseDir, "reference");

  if (!filePath && options.slug) filePath = findLessonBySlug(lessonsDir, options.slug);

  if (!filePath || !fs.existsSync(filePath)) {
    throw new Error("Cannot compile reference: lesson file not found (" + filePath + ")");
  }

  const rawHtml = fs.readFileSync(filePath, "utf-8");
  const doc = parseLessonDocument(rawHtml);
  if (!doc) {
    throw new Error("Corrupted lesson file: missing #lesson-data in " + filePath);
  }
  if (!fs.existsSync(outDir)) {
    fs.mkdirSync(outDir, { recursive: true });
  }

  const outPath = path.join(outDir, doc.metadata.slug + ".html");

  const sintesiDoc = buildSintesiDocument(doc, {
    learningRecordContent: options.learningRecordContent,
    learningRecordPath: options.learningRecordPath,
    lessonPath: filePath,
    baseDir,
  });

  const unconditionalTruths = doc.nodes.filter((n) => n.tier === "unconditional-truth" || n.tier === "axiom");
  const derivations = doc.nodes.filter((n) => n.tier === "derivation");

  const tierBadge = (tier: PedagogicalTier) => {
    if (tier === "derivation") return '<span class="badge badge-deriv">Derivazione</span>';
    if (tier === "axiom") return '<span class="badge badge-truth">Assioma</span>';
    return '<span class="badge badge-truth">Verità incondizionata</span>';
  };

  const nodesHtml = sintesiDoc.nodes.length > 0
    ? sintesiDoc.nodes.map((n) => {
        const numStr = String(n.number).padStart(2, "0");
        let extraSnippet = "";
        if (n.keyFormula) {
          const isDisplay = n.keyFormula.startsWith("$$") && n.keyFormula.endsWith("$$");
          const formatted = isDisplay ? n.keyFormula : "$$ " + n.keyFormula + " $$";
          extraSnippet += '<div class="formula-card"><div>' + formatted + '</div></div>';
        }
        if (n.keyCode) {
          extraSnippet += '<div class="code-card"><pre><code>' + escapeHtml(n.keyCode.code) + '</code></pre>' +
            (n.keyCode.caption ? '<small class="code-caption">' + escapeHtml(n.keyCode.caption) + '</small>' : '') +
            '</div>';
        }
        return (
          '<div class="node-card tier-' + n.tier + '">\n' +
          '  <div class="node-card-header">\n' +
          '    <div class="node-card-title-group">\n' +
          '      <span class="node-card-num">Nodo ' + numStr + '</span>\n' +
          '      <h3 class="node-card-title">' + escapeHtml(n.title) + '</h3>\n' +
          '    </div>\n' +
          '    ' + tierBadge(n.tier) + '\n' +
          '  </div>\n' +
          '  <div class="node-card-claim">' + parseMarkdownProse(n.summary) + '</div>\n' +
          '  ' + extraSnippet + '\n' +
          '</div>'
        );
      }).join("\n")
    : '<p>Nessun nodo registrato.</p>';

  const formulasHtml = sintesiDoc.formulas.length > 0
    ? sintesiDoc.formulas.map((f) => {
        const isDisplay = f.math.startsWith("$$") && f.math.endsWith("$$");
        const formatted = isDisplay ? f.math : "$$ " + f.math + " $$";
        return (
          '<div class="formula-card">\n' +
          '  <div>' + formatted + '</div>\n' +
          '  <small style="color: #64748b;">Fonte: ' + escapeHtml(f.sourceNode) + '</small>\n' +
          '</div>'
        );
      }).join("\n")
    : '<p>Nessuna formula registrata.</p>';

  const misconceptionsHtml = sintesiDoc.misconceptions.length > 0
    ? '<div class="card misconceptions-card">\n' +
      '  <h2>Errori e dubbi emersi nella sessione</h2>\n' +
      '  <ul class="misconceptions-list">\n' +
      sintesiDoc.misconceptions.map(m =>
        '<li>' + (m.sourceNode ? '<strong>' + escapeHtml(m.sourceNode) + ': </strong>' : '') + escapeHtml(m.description) + '</li>'
      ).join("\n") +
      '  </ul>\n' +
      '</div>\n'
    : '';

  const refHtml = "<!DOCTYPE html>\n" +
"<html lang=\"it\" data-theme=\"light\">\n" +
"<head>\n" +
"  <meta charset=\"UTF-8\">\n" +
"  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n" +
"  <title>Sintesi: " + escapeHtml(sintesiDoc.title) + " | " + APP_NAME + "</title>\n" +
"  <link rel=\"stylesheet\" href=\"https://cdn.jsdelivr.net/npm/katex@0.16.11/dist/katex.min.css\" crossorigin=\"anonymous\">\n" +
"  <script defer src=\"https://cdn.jsdelivr.net/npm/katex@0.16.11/dist/katex.min.js\" crossorigin=\"anonymous\"></script>\n" +
"  <script defer src=\"https://cdn.jsdelivr.net/npm/katex@0.16.11/dist/contrib/auto-render.min.js\" crossorigin=\"anonymous\"></script>\n" +
"  <script src=\"https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.min.js\"></script>\n" +
"  <style>\n" +
"    :root {\n" +
"      --font-body: Charter, Georgia, serif, -apple-system, sans-serif;\n" +
"      --font-ui: -apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, sans-serif;\n" +
"      --font-mono: ui-monospace, \"SF Mono\", monospace;\n" +
"      --bg: #ffffff;\n" +
"      --fg: #0f172a;\n" +
"      --card-bg: #f8fafc;\n" +
"      --border: #cbd5e1;\n" +
"      --accent: #1e40af;\n" +
"      --truth: #047857;\n" +
"      --truth-bg: #ecfdf5;\n" +
"      --derive: #2563eb;\n" +
"      --derive-bg: #eff6ff;\n" +
"      --warn: #b45309;\n" +
"      --warn-bg: #fffbeb;\n" +
"    }\n" +
"    body { background: var(--bg); color: var(--fg); font-family: var(--font-body); margin: 0; padding: 2.5rem 1.5rem; line-height: 1.6; }\n" +
"    .ref-container { max-width: 960px; margin: 0 auto; }\n" +
"    header { margin-bottom: 2rem; border-bottom: 2px solid var(--accent); padding-bottom: 0.75rem; }\n" +
"    h1 { font-family: var(--font-ui); font-size: 2.2rem; margin: 0 0 0.5rem 0; color: var(--fg); }\n" +
"    .subtitle { font-size: 1rem; color: #64748b; margin: 0; }\n" +
"    .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 1.5rem; margin-bottom: 2rem; }\n" +
"    .card { background: var(--card-bg); border: 1px solid var(--border); border-radius: 8px; padding: 1.5rem; margin-bottom: 1.5rem; }\n" +
"    .card h2 { font-family: var(--font-ui); font-size: 1.25rem; margin-top: 0; color: var(--accent); border-bottom: 1px solid var(--border); padding-bottom: 0.5rem; }\n" +
"    .node-card { background: #ffffff; border: 1px solid var(--border); border-radius: 8px; padding: 1.25rem; margin-bottom: 1rem; }\n" +
"    .node-card.tier-unconditional-truth, .node-card.tier-axiom { border-left: 4px solid var(--truth); }\n" +
"    .node-card.tier-derivation { border-left: 4px solid var(--derive); }\n" +
"    .node-card-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; margin-bottom: 0.75rem; }\n" +
"    .node-card-title-group { display: flex; flex-direction: column; gap: 0.25rem; }\n" +
"    .node-card-num { font-family: var(--font-mono); font-size: 0.75rem; color: #64748b; text-transform: uppercase; letter-spacing: 0.05em; }\n" +
"    .node-card-title { font-family: var(--font-ui); font-size: 1.1rem; margin: 0; color: var(--fg); }\n" +
"    .badge { font-family: var(--font-ui); font-size: 0.7rem; font-weight: 600; padding: 0.2rem 0.5rem; border-radius: 4px; white-space: nowrap; }\n" +
"    .badge-truth { background: var(--truth-bg); color: var(--truth); }\n" +
"    .badge-deriv { background: var(--derive-bg); color: var(--derive); }\n" +
"    .node-card-claim { font-size: 0.95rem; line-height: 1.5; color: #334155; margin-bottom: 0.75rem; }\n" +
"    .formula-card { background: #f8fafc; border: 1px solid var(--border); border-radius: 4px; padding: 0.75rem; margin-bottom: 0.75rem; text-align: center; }\n" +
"    .code-card { background: #0f172a; color: #f8fafc; border-radius: 4px; padding: 0.75rem 1rem; margin-top: 0.5rem; overflow-x: auto; }\n" +
"    .code-card pre { margin: 0; font-family: var(--font-mono); font-size: 0.85rem; }\n" +
"    .code-caption { display: block; color: #94a3b8; font-size: 0.75rem; margin-top: 0.25rem; }\n" +
"    .diagram-container { background: #ffffff; border: 1px solid var(--border); border-radius: 8px; padding: 1.5rem; margin-bottom: 1.5rem; }\n" +
"    .diagram-container h2 { font-family: var(--font-ui); font-size: 1.25rem; margin-top: 0; color: var(--accent); border-bottom: 1px solid var(--border); padding-bottom: 0.5rem; }\n" +
"    .misconceptions-card { border-left: 4px solid var(--warn); background: var(--warn-bg); }\n" +
"    .misconceptions-card h2 { color: var(--warn); }\n" +
"    .misconceptions-list { margin: 0.5rem 0 0 1.25rem; padding: 0; font-size: 0.9rem; }\n" +
"    .misconceptions-list li { margin-bottom: 0.35rem; }\n" +
"    @media print {\n" +
"      body { padding: 0; }\n" +
"      .card, .node-card, .diagram-container { border: 1px solid #000; break-inside: avoid; }\n" +
"    }\n" +
"  </style>\n" +
"</head>\n" +
"<body>\n" +
"  <script type=\"application/json\" id=\"reference-data\">" + JSON.stringify(sintesiDoc) + "</script>\n" +
"  <main class=\"ref-container\">\n" +
"    <header>\n" +
"      <h1>Sintesi: " + escapeHtml(sintesiDoc.title) + "</h1>\n" +
"      <p class=\"subtitle\">Sintesi rapida per il ripasso: concetti chiave, derivazioni, grafo ed inventario formule.</p>\n" +
"    </header>\n" +
"\n" +
"    <section class=\"card sintesi-nodes\">\n" +
"      <h2>Nodi concettuali</h2>\n" +
"      " + nodesHtml + "\n" +
"    </section>\n" +
"\n" +
    (doc.metadata.initialDag ?
"    <div class=\"diagram-container\">\n" +
"      <h2>Grafo delle dipendenze</h2>\n" +
"      <pre class=\"mermaid\">" + doc.metadata.initialDag + "</pre>\n" +
"    </div>\n" : "") +
"\n" +
    misconceptionsHtml +
"\n" +
"    <div class=\"card\">\n" +
"      <h2>Inventario formule</h2>\n" +
"      " + formulasHtml + "\n" +
"    </div>\n" +
"  </main>\n" +
"\n" +
"  <script>\n" +
"    document.addEventListener(\"DOMContentLoaded\", function() {\n" +
"      if (typeof renderMathInElement === \"function\") {\n" +
"        renderMathInElement(document.body, {\n" +
"          delimiters: [\n" +
"            {left: \"$$\", right: \"$$\", display: true},\n" +
"            {left: \"$\", right: \"$\", display: false},\n" +
"            {left: \"\\(\", right: \"\\)\", display: false},\n" +
"            {left: \"\\[\", right: \"\\]\", display: true}\n" +
"          ],\n" +
"          throwOnError: false\n" +
"        });\n" +
"      }\n" +
"      if (typeof mermaid !== \"undefined\") {\n" +
"        mermaid.initialize({\n" +
"          startOnLoad: true,\n" +
"          fontFamily: '-apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, sans-serif'\n" +
"        });\n" +
"      }\n" +
"    });\n" +
"  </script>\n" +
"</body>\n" +
"</html>";

  fs.writeFileSync(outPath, refHtml, "utf-8");

  return {
    filePath: outPath,
    filepath: outPath,
    slug: doc.metadata.slug,
    unconditionalTruthCount: unconditionalTruths.length,
    axiomsCount: unconditionalTruths.length,
    formulaCount: sintesiDoc.formulas.length,
    nodeCount: doc.nodes.length,
    derivationCount: derivations.length,
    misconceptionsCount: sintesiDoc.misconceptions.length,
    document: sintesiDoc,
  };
}


// ────────────────────────────────────────────────────────────────────────────
// CLI Subcommand Dispatcher
// ────────────────────────────────────────────────────────────────────────────

// index.html is derived from the lesson files, so every CLI write regenerates it: a board that
// waits for a manual `npm run home` is a board that lags. Anki closed → render without card
// counts (the board says so) rather than skip. Dynamic import: homepage.ts imports this module.
async function refreshBoard(): Promise<void> {
  const { generateHomepage } = await import("./homepage.ts");
  const res = await generateHomepage().catch(() => generateHomepage({ noAnki: true }));
  console.error(`[home] ${res.filePath} rigenerato${res.ankiOnline ? "" : " senza Anki"}`);
}

export async function runCli(args: string[]): Promise<void> {
  const command = args[0];

  function getFlag(flag: string): string | undefined {
    const idx = args.indexOf(flag);
    if (idx !== -1 && idx + 1 < args.length) {
      return args[idx + 1];
    }
    return undefined;
  }

  try {
    if (command === "init") {
      const topic = getFlag("--topic");
      if (!topic) throw new Error("Missing required parameter: --topic");
      const title = getFlag("--title");
      const slug = getFlag("--slug");
      const description = getFlag("--description");
      const dag = getFlag("--dag");
      const targetGoal = getFlag("--goal");
      const baseDir = getFlag("--base-dir");
      const lessonsDir = getFlag("--out-dir");

      const res = await initLesson({ topic, title, slug, description, dagMermaid: dag, targetGoal, baseDir, lessonsDir });
      console.log(JSON.stringify(res, null, 2));
      await refreshBoard();
    } else if (command === "append") {
      const file = getFlag("--lesson") || getFlag("--file");
      const slug = getFlag("--slug");
      const dataStr = getFlag("--data") || getFlag("--node-json");
      const nodeFile = getFlag("--node-file");
      const isStdin = args.includes("--stdin");

      let payloadStr = "";
      if (dataStr) {
        payloadStr = dataStr;
      } else if (nodeFile) {
        payloadStr = fs.readFileSync(nodeFile, "utf-8");
      } else if (isStdin) {
        payloadStr = fs.readFileSync(0, "utf-8");
      } else {
        throw new Error("Must provide --data, --node-json, --node-file, or --stdin for append payload");
      }

      let nodePayload: CognitiveNodePayload;
      try {
        nodePayload = JSON.parse(payloadStr);
      } catch (err: any) {
        throw new Error("Invalid JSON payload: " + err.message);
      }

      const res = await appendNode({ lessonPath: file, slug, node: nodePayload });
      console.log(JSON.stringify(res, null, 2));
      await refreshBoard();
    } else if (command === "compile-ref" || command === "reference") {
      const file = getFlag("--lesson") || getFlag("--file");
      const slug = getFlag("--slug");
      const outDir = getFlag("--out-dir");
      const baseDir = getFlag("--base-dir");
      const record = getFlag("--record");

      const res = await compileReference({ lessonPath: file, slug, outputDir: outDir, baseDir, learningRecordPath: record });
      console.log(JSON.stringify(res, null, 2));
      await refreshBoard();
    } else if (command === "rebuild") {
      const file = getFlag("--lesson") || getFlag("--file");
      const slug = getFlag("--slug");
      const baseDir = getFlag("--base-dir");
      const lessonsDir = getFlag("--out-dir") || getFlag("--dir");

      const res = await rebuildLesson({ lessonPath: file, slug, baseDir, lessonsDir });
      console.log(JSON.stringify(res, null, 2));
    } else if (command === "list") {
      const lessonsDir = getFlag("--dir") || "./lessons";
      if (!fs.existsSync(lessonsDir)) {
        console.log(JSON.stringify([], null, 2));
        return;
      }
      const files = fs.readdirSync(lessonsDir).filter((f) => f.endsWith(".html"));
      console.log(JSON.stringify(files, null, 2));
    } else {
      console.log("Usage: html-lesson.ts <command> [options]");
    }
  } catch (err: any) {
    console.error("Error: " + err.message);
    process.exit(1);
  }
}

// Guard for direct execution
const isDirectCli = () => {
  if (!process.argv || !process.argv[1]) return false;
  const script = process.argv[1].replace(/\\/g, "/");
  return (script.endsWith("/html-lesson.ts") || script.endsWith("/html-lesson.js") || script.endsWith("html-lesson")) && !script.includes(".test.");
};

if (typeof process !== "undefined" && isDirectCli()) {
  const cliArgs = process.argv.slice(2);
  enterDataRoot();
  runCli(cliArgs);
}
