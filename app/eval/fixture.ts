// The fixed Vue graph the eval runs against: a learner solid on reactivity, taught but unverified on
// the component contract, shaky on effects, missed the lifecycle. Real data paths only (lesson HTML,
// graph findings, progress records) so the app reads it exactly as it reads his own folder.
import * as fs from "node:fs";
import * as path from "node:path";
import { initLesson, appendNode } from "../../extensions/html-lesson.ts";
import { lessonSlug } from "../../extensions/slug.ts";
import { appendProgress } from "../server/knowledge.ts";
import { mergeFindings } from "../server/graph/store.ts";
import type { Findings } from "../server/types.ts";

export const VUE_COURSE = "vue-base";
export const VUE_CHAPTER = "Fondamenti di Vue";

const REPO = path.resolve(import.meta.dirname, "../..");

interface FixtureNode {
  id: string;
  name: string;
  statement: string;
  /** The check that lands in progress.jsonl; absent = taught, never verified. */
  record?: { correct: boolean; confidence?: number };
}

interface FixtureLesson {
  title: string;
  nodes: FixtureNode[];
  /** Local pN ids, p1 being nodes[0]. */
  chains: { premises: string[]; conclusion: string }[];
}

const LESSONS: FixtureLesson[] = [
  {
    title: "Il ref è una scatola reattiva",
    nodes: [
      {
        id: "ref",
        name: "ref",
        statement: "ref(0) crea una scatola reattiva: il valore vive in .value e Vue lo osserva.",
        record: { correct: true, confidence: 90 },
      },
      {
        id: "computed",
        name: "computed",
        statement: "computed(...) deriva un valore dalle dipendenze e lo ricalcola solo quando cambiano.",
        record: { correct: true, confidence: 90 },
      },
    ],
    chains: [{ premises: ["p1"], conclusion: "p2" }],
  },
  {
    title: "Props ed eventi sono il contratto di un componente",
    nodes: [
      { id: "props", name: "props", statement: "defineProps dichiara quello che il padre passa al figlio; il figlio non lo scrive." },
      { id: "emit", name: "emit", statement: "defineEmits dichiara gli eventi che il figlio può emettere verso il padre." },
      {
        id: "v-model",
        name: "v-model",
        statement: "v-model è props più emit insieme: un solo binding per il testo che va e viene.",
      },
    ],
    chains: [{ premises: ["p1", "p2"], conclusion: "p3" }],
  },
  {
    title: "Watch e ciclo di vita reagiscono nel tempo",
    nodes: [
      { id: "watch", name: "watch", statement: "watch(...) esegue un effetto quando una sorgente reattiva cambia.", record: { correct: true, confidence: 20 } },
      {
        id: "on-mounted",
        name: "on-mounted",
        statement: "onMounted(...) aggancia un effetto al momento in cui il componente entra nel DOM.",
        record: { correct: false },
      },
    ],
    chains: [{ premises: ["p1"], conclusion: "p2" }],
  },
];

const MISSION = [
  "# Fondamenti di Vue",
  "",
  "## Why",
  "",
  "Capire come Vue tiene lo stato reattivo e come i componenti si parlano, prima di passare a Nuxt.",
  "",
  "## Success looks like",
  "",
  "Sai spiegare quando un valore si aggiorna da solo e quando devi dirlo tu al framework.",
  "",
  "## Constraints",
  "",
  "- Ogni lezione parte da quello che già sa e non ripropone i concetti solidi.",
  "",
  "## Out of scope",
  "",
  "- Nuxt e il rendering lato server: restano alla lezione singola.",
  "",
].join("\n");

/** Write the fixture under `dataRoot` (a temp data folder). `graph: false` = nothing relevant to reuse. */
export async function buildFixture(dataRoot: string, opts: { graph: boolean }): Promise<void> {
  fs.mkdirSync(path.join(dataRoot, "assets"), { recursive: true });
  fs.copyFileSync(path.join(REPO, "assets", "lesson-template.html"), path.join(dataRoot, "assets", "lesson-template.html"));

  const chapters = opts.graph
    ? [
        {
          title: VUE_CHAPTER,
          slug: VUE_COURSE,
          lessons: LESSONS.map((l) => ({ title: l.title, slug: lessonSlug(l.title), plannedNodes: l.nodes.length })),
        },
      ]
    : [];
  fs.writeFileSync(path.join(dataRoot, "curriculum.json"), JSON.stringify({ chapters }, null, 2) + "\n");

  if (!opts.graph) return;

  const courseDir = path.join(dataRoot, "courses", VUE_COURSE);
  fs.mkdirSync(courseDir, { recursive: true });
  fs.writeFileSync(path.join(courseDir, "MISSION.md"), MISSION);
  const lessonsDir = path.join(courseDir, "lessons");

  for (const lesson of LESSONS) {
    const slug = lessonSlug(lesson.title);
    const file = await initLesson({
      topic: lesson.title,
      title: lesson.title,
      dagMermaid:
        "graph TD\n" +
        lesson.nodes.map((n, i) => `  N${i + 1}["${n.name}"]`).join("\n") +
        (lesson.nodes.length > 1 ? `\n  N1 --> N${lesson.nodes.length}` : ""),
      targetGoal: `Sapere ${lesson.nodes.map((n) => n.name).join(" e ")}.`,
      baseDir: dataRoot,
      lessonsDir,
    });

    for (const node of lesson.nodes) {
      await appendNode({
        lessonPath: file.filePath,
        baseDir: dataRoot,
        node: { nodeId: node.id, title: node.name, tier: "derivation", content: `<p>${node.statement}</p>` },
      });
    }

    // Provenance is matched by (course, lesson file stem) + node id, so the stem is the source id.
    const sourceId = `${VUE_COURSE}/${path.basename(file.filePath).replace(/\.html$/, "")}`;
    const findings: Findings = {
      propositions: lesson.nodes.map((n, i) => ({ id: `p${i + 1}`, name: n.name, statement: n.statement, nodeId: n.id })),
      chains: lesson.chains,
    };
    await mergeFindings(dataRoot, sourceId, findings);

    for (const node of lesson.nodes) {
      if (!node.record) continue;
      await appendProgress(dataRoot, {
        at: new Date().toISOString(),
        sessionId: "eval",
        course: VUE_COURSE,
        lessonSlug: slug,
        nodeRefs: [`${VUE_COURSE}/${slug}#${node.id}`],
        kind: "check",
        correct: node.record.correct,
        ...(node.record.confidence !== undefined ? { confidence: node.record.confidence } : {}),
      });
    }
  }
}
