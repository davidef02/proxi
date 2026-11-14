// Captures the Node `lesson` CLI into crates/proxi/tests/fixtures/golden/lesson-cli.json (A12). Regenerate only from Node, before the cutover.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { GOLDEN_DIR, rootVariants } from "./capture-golden.ts";
import { REPO_ROOT } from "./harness.ts";

export const LESSON_GOLDEN = "lesson-cli";

/** Files Node's refreshBoard writes at the data root: excluded from snapshots because D11 drops the board refresh. */
export const BOARD_FILES = ["index.html"];

export interface LessonCase {
  name: string;
  args: string[];
  stdin?: string;
  stderrPrefix?: string;
  expectExit: 0 | 1;
}

export interface LessonGoldenCase {
  name: string;
  args: string[];
  stdin?: string;
  stderrPrefix?: string;
  stdout: string;
  stderr: string;
  exitCode: number;
  files: Record<string, string | null>;
}

export interface LessonGolden {
  route: "lesson-cli";
  seed: Record<string, string>;
  cases: LessonGoldenCase[];
}

const ISO_RE = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z/g;
const GENERATED_RE = /Generated: \d{4}-\d{2}-\d{2}/g;

/** `<dataRoot>` for every form the CLI may print the temp folder in, then Windows separators to `/`. */
export function normaliseStream(text: string, roots: string[]): string {
  let out = text;
  for (const root of [...roots].sort((a, b) => b.length - a.length)) {
    // stdout is JSON (backslashes double-escaped), stderr is not: replace both spellings.
    out = out.split(JSON.stringify(root).slice(1, -1)).join("<dataRoot>");
    out = out.split(root).join("<dataRoot>");
  }
  return out.split("\\\\").join("/").split("\\").join("/");
}

/** The volatile parts of a lesson file: CRLF, the ISO stamps and the `Generated:` date. */
export function normaliseFile(text: string): string {
  return text.replace(/\r\n/g, "\n").replace(ISO_RE, "<iso>").replace(GENERATED_RE, "Generated: <date>");
}

const NODE_FILE = {
  nodeId: "n-file",
  title: "Codice & visuali",
  tier: "derivation",
  content: "Prima riga\n\n- punto *uno*\n- punto **due**\n\n<pre><code>let x = 1;\n  let y = 2;</code></pre>",
  codeBlocks: [{ language: "rust", code: 'fn main() { println!("<ciao>"); }', caption: "Esempio" }],
  visuals: [
    { type: "mermaid", content: "flowchart TD\n  A --> B", caption: "Grafo" },
    '<svg viewBox="0 0 4 4"><rect width="4" height="4"/></svg>',
    { type: "image", content: "img/a.png", alt: 'Figura "A"' },
  ],
  connection: "Collega a `n1`",
  ankiParams: { targetQuestion: "Cosa?", targetFact: "Questo.", deck: "Proxi::Demo" },
};

/** NODE_FILE plus a raw `extra` member so the file exercises V8 number formatting (`1.0`, `1e21`, `1e-7`). */
const NODE_FILE_JSON = JSON.stringify(NODE_FILE).slice(0, -1) + ',"extra":{"z":1.0,"a":[1e21,1e-7,0.5]}}';

const NODE_A = {
  nodeId: "n1",
  title: "Livelli </script> & co",
  tier: "unconditional-truth",
  motivation: "Perché **servono** livelli",
  content: "## Idea\nOgni livello *parla* col vicino.\n### Dettaglio\nUsa `TCP`.",
  math: ["E = mc^2", "$$\\frac{a}{b}$$"],
  quiz: {
    question: "Quale livello instrada?",
    options: ["Fisico", "Rete", "Applicazione"],
    correctIndex: 2,
    misconceptions: [{ optionIndex: 1, misconception: 'Il fisico "sposta" bit' }],
    explanation: "Il livello **rete** instrada.",
  },
  dependencies: ["n0", "n<x>"],
  ankiParams: { suggestedCloze: "Il livello {{c1::rete}} instrada" },
  weight: 1.5,
  capturedAt: "old",
};

const SEED: Record<string, string> = {
  "payloads/node-file.json": NODE_FILE_JSON,
  "payloads/extra.html": "<html></html>",
  "courses/demo/lessons/0003-preesistente.html": "<html></html>",
};

const L = "courses/demo/lessons/0004-il-modello-osi.html";
const INIT_OUT_DIR = ["init", "--topic", "Reti", "--title", "Il modello OSI", "--goal", "Capire i livelli", "--out-dir", "./courses/demo/lessons/"];

export const CASES: LessonCase[] = [
  {
    name: "init-default",
    args: [
      "init",
      "--topic",
      "Probabilità & Statistica",
      "--title",
      "Probabilità: è “davvero” 50%? Ça va!",
      "--description",
      "Perché il caso <non> è caos",
      "--dag",
      'flowchart TD\n  A["Caso"] --> B["Legge"]',
    ],
    expectExit: 0,
  },
  { name: "init-out-dir", args: INIT_OUT_DIR, expectExit: 0 },
  { name: "init-duplicate", args: [...INIT_OUT_DIR], expectExit: 1 },
  {
    name: "init-slug-flag",
    args: ["init", "--topic", "Slug", "--slug", "  Già__Fatto -- Ok  ", "--description", "", "--out-dir", "courses/demo/lessons"],
    expectExit: 0,
  },
  { name: "init-base-dir", args: ["init", "--topic", "Base", "--base-dir", "courses/base"], expectExit: 0 },
  { name: "init-flag-as-value", args: ["init", "--topic", "--title", "--out-dir", "courses/flag"], expectExit: 0 },
  { name: "init-missing-topic", args: ["init", "--title", "Senza topic"], expectExit: 1 },
  { name: "init-empty-topic", args: ["init", "--topic", ""], expectExit: 1 },
  { name: "append-data", args: ["append", "--lesson", L, "--data", JSON.stringify(NODE_A)], expectExit: 0 },
  { name: "append-node-file", args: ["append", "--lesson", L, "--node-file", "payloads/node-file.json"], expectExit: 0 },
  {
    name: "append-stdin-slug",
    args: ["append", "--slug", "probabilita-e-davvero-50-ca-va", "--stdin"],
    stdin: JSON.stringify({
      nodeId: "s1",
      title: "Passi",
      tier: "axiom",
      content: "   ",
      steps: [
        { stepId: "p1", title: "Primo", content: "Uno **forte**", math: ["a+b"] },
        { stepId: "p2", content: "Due" },
      ],
    }),
    expectExit: 0,
  },
  {
    name: "append-replace",
    args: ["append", "--file", L, "--node-json", JSON.stringify({ nodeId: "n1", title: "Livelli rivisti", tier: "derivation", content: "Nuovo testo" })],
    expectExit: 0,
  },
  {
    name: "append-empty-data-falls-through",
    args: ["append", "--lesson", L, "--data", "", "--stdin"],
    stdin: JSON.stringify({ nodeId: "n3", title: "Da stdin", tier: "derivation", content: "x" }),
    expectExit: 0,
  },
  { name: "append-no-payload", args: ["append", "--lesson", L], expectExit: 1 },
  { name: "append-missing-lesson", args: ["append", "--lesson", "courses/demo/lessons/9999-nope.html", "--data", '{"nodeId":"x"}'], expectExit: 1 },
  { name: "append-no-target", args: ["append", "--data", "{}"], expectExit: 1 },
  { name: "append-slug-prefix", args: ["append", "--slug", "probabilita", "--data", "{}"], expectExit: 1 },
  { name: "append-missing-node-file", args: ["append", "--lesson", L, "--node-file", "payloads/missing.json"], stderrPrefix: "Error: ENOENT: no such file or directory, open '", expectExit: 1 },
  { name: "append-invalid-json", args: ["append", "--lesson", L, "--data", "{"], stderrPrefix: "Error: Invalid JSON payload: ", expectExit: 1 },
  { name: "list-default", args: ["list"], expectExit: 0 },
  { name: "list-dir", args: ["list", "--dir", "payloads"], expectExit: 0 },
  { name: "list-missing", args: ["list", "--dir", "nope"], expectExit: 0 },
  { name: "usage-unknown", args: ["frobnicate"], expectExit: 0 },
  { name: "usage-none", args: [], expectExit: 0 },
];

/** Every file under `data` as `<dir>/<name>` → text, board files at the root excluded. */
export function snapshot(data: string): Map<string, string> {
  const out = new Map<string, string>();
  const walk = (dir: string, prefix: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full, rel);
      else if (entry.isFile() && (prefix || !BOARD_FILES.includes(entry.name))) out.set(rel, fs.readFileSync(full, "utf-8"));
    }
  };
  walk(data, "");
  return out;
}

/** Runs CASES against a fresh temp data folder and writes `<outDir>/lesson-cli.json`; returns its path. */
export async function captureLessonGolden(outDir: string = GOLDEN_DIR): Promise<string> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-lesson-"));
  try {
    const data = path.join(dir, "data");
    fs.mkdirSync(data);
    for (const [rel, content] of Object.entries(SEED)) {
      const file = path.join(data, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, content);
    }
    const roots = rootVariants(data);
    const cases: LessonGoldenCase[] = [];
    for (const c of CASES) {
      const before = snapshot(data);
      // No shell (L15): a Windows path with spaces must reach the script unchanged.
      const run = spawnSync(process.execPath, ["--import", "tsx", path.join(REPO_ROOT, "extensions", "html-lesson.ts"), ...c.args], {
        cwd: REPO_ROOT,
        env: { ...process.env, LEARNER_DATA_ROOT: data },
        input: c.stdin ?? "",
        encoding: "utf-8",
      });
      if (run.error) throw run.error;
      // Node appends `[home] ...` after a successful write; D11 drops the board refresh, so the golden must not pin it.
      const stderr = run.stderr.split("\n").filter((line) => !line.startsWith("[home] ")).join("\n");
      if (run.status !== c.expectExit) {
        throw new Error(`${c.name}: exit ${run.status}, expected ${c.expectExit}\nstdout: ${run.stdout}\nstderr: ${run.stderr}`);
      }
      if (c.stderrPrefix !== undefined && !stderr.startsWith(c.stderrPrefix)) {
        throw new Error(`${c.name}: stderr does not start with ${JSON.stringify(c.stderrPrefix)}: ${JSON.stringify(stderr)}`);
      }
      const after = snapshot(data);
      const files: Record<string, string | null> = {};
      for (const name of [...new Set([...before.keys(), ...after.keys()])].sort()) {
        const old = before.get(name);
        const now = after.get(name);
        if (now === undefined) files[name] = null;
        else if (old !== now) files[name] = normaliseFile(now);
      }
      cases.push({
        name: c.name,
        args: c.args,
        ...(c.stdin !== undefined ? { stdin: c.stdin } : {}),
        ...(c.stderrPrefix !== undefined ? { stderrPrefix: c.stderrPrefix } : {}),
        stdout: normaliseStream(run.stdout, roots),
        // A stderrPrefix means the tail is platform-specific (Node's ENOENT prints the resolved path
        // on Windows, the argument as typed on POSIX): the golden pins only the prefix.
        stderr: normaliseStream(c.stderrPrefix ?? stderr, roots),
        exitCode: c.expectExit,
        files,
      });
    }
    fs.mkdirSync(outDir, { recursive: true });
    const target = path.join(outDir, `${LESSON_GOLDEN}.json`);
    // The seed and every case's args/stdin stay raw: they are the replay's input. Only outputs are normalised.
    const file: LessonGolden = { route: LESSON_GOLDEN, seed: SEED, cases };
    fs.writeFileSync(target, JSON.stringify(file, null, 2) + "\n"); // always LF, never os.EOL
    return target;
  } finally {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const at = process.argv.indexOf("--out");
  const outDir = at === -1 ? GOLDEN_DIR : path.resolve(process.argv[at + 1] ?? GOLDEN_DIR);
  captureLessonGolden(outDir)
    .then((file) => console.log(file))
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exit(1);
    });
}
