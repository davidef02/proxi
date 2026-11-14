// Captures the Node `anki` CLI into crates/proxi/tests/fixtures/golden/anki-cli.json (A12). Regenerate only from Node, before the cutover.
import { spawn } from "node:child_process";
import fs from "node:fs";
import * as net from "node:net";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { MockAnki } from "../../tests/mock-anki.ts";
import { GOLDEN_DIR, rootVariants } from "./capture-golden.ts";
import { normaliseFile, normaliseStream, snapshot } from "./capture-lesson-golden.ts";
import { REPO_ROOT } from "./harness.ts";

export const ANKI_GOLDEN = "anki-cli";

/** The error the erroring mock server puts in AnkiConnect's `error` field. */
export const ANKI_ERROR = "collection is not available";

export interface AnkiCase {
  name: string;
  args: string[];
  stdin?: string;
  stderrPrefix?: string;
  expectExit: 0 | 1;
}

export interface AnkiGoldenCase {
  name: string;
  args: string[];
  stdin?: string;
  stderrPrefix?: string;
  stdout: string;
  stderr: string;
  exitCode: number;
  files: Record<string, string | null>;
}

export interface AnkiGolden {
  route: "anki-cli";
  ankiError: string;
  seed: Record<string, string>;
  cases: AnkiGoldenCase[];
}

/** The lesson artifact as the engine writes it: the document as one JSON island, raw around it. */
function islandLesson(island: string): string {
  return `<!doctype html>\n<html><body>\n  <script type="application/json" id="lesson-data">\n${island}\n  </script>\n</body></html>\n`;
}

/** Same shell with an upper-case tag and single quotes: the CLI's island regex is case-insensitive and accepts either quote. */
function upperIslandLesson(island: string): string {
  return `<!doctype html>\n<html><body>\n  <SCRIPT TYPE='application/json'  ID='lesson-data'>\n${island}\n  </SCRIPT>\n</body></html>\n`;
}

/** A bare shell whose island body is `body` verbatim: empty, blank and broken islands. */
function bareIslandLesson(body: string): string {
  return `<html><body><script type="application/json" id="lesson-data">${body}</script></body></html>`;
}

/** The course lesson island, raw: `1.0` and `1e21` make the capture pin V8's number printing. */
const COURSE_ISLAND = JSON.stringify({
  metadata: {
    topic: "Vue.js & Vetrine",
    title: "Reattività",
    slug: "reattivita",
    lessonNumber: "__float__",
    weight: "__big__",
  },
  nodes: [
    {
      nodeId: "n-basic",
      tier: "axiom",
      ankiParams: {
        targetQuestion: "Quanto vale $\\frac{d}{dx} x^2$?",
        targetFact: "\uFEFF  $$ 2x $$ sempre\tvero  ",
        tags: ["Reattività & Co", "vue-js-vetrine", "Già__Fatto"],
      },
    },
    {
      nodeId: "n-cloze",
      tier: "derivation",
      ankiParams: {
        suggestedCloze: "Il ref {{c1::\\(\\frac{a}{b}\\)}} traccia\tchi\r\nlo legge",
        clozeExtra: "Riga 1\nRiga 2 $x$\rfine",
      },
    },
    {
      nodeId: "n-cloze-bare",
      tier: "derivation",
      ankiParams: { suggestedCloze: "La formula {{c1:\\frac{a}{b}}} costa \\$5 e $y$" },
    },
    {
      nodeId: "n-both",
      tier: "axiom",
      ankiParams: { targetQuestion: "Q?", targetFact: "A.", suggestedCloze: "La risposta è {{c1::A}}." },
    },
    { title: "Senza id", ankiParams: { suggestedCloze: "Nodo {{c1::anonimo}}" } },
    { nodeId: "n-none", tier: "axiom" },
    { nodeId: "n-fact-only", tier: "axiom", ankiParams: { targetFact: "Solo fatto" } },
  ],
})
  .replace('"__float__"', "1.0")
  .replace('"__big__"', "1e21");

const LOOSE_ISLAND = JSON.stringify({
  metadata: { title: "Entropia", slug: "entropia" },
  nodes: [
    {
      nodeId: "n1",
      tier: "axiom",
      ankiParams: { targetQuestion: "Che cos'è l'entropia?", targetFact: "Una misura del disordine." },
    },
  ],
});

const STANDALONE_ISLAND = JSON.stringify({
  metadata: { topic: "Entropia", slug: "microstati" },
  nodes: [{ nodeId: "n1", tier: "axiom", ankiParams: { targetQuestion: "Quanti microstati?", targetFact: "$W$ li conta." } }],
});

const UPPER_ISLAND = JSON.stringify({
  metadata: { topic: "Maiuscole", slug: "upper" },
  nodes: [{ nodeId: "n1", tier: "axiom", ankiParams: { suggestedCloze: "Il tag si legge {{c1::case-insensitive}}" } }],
});

/** Raw text, never through the lesson writer: the CLI's island reader must cope with the engine's own output. */
export const SEED: Record<string, string> = {
  "curriculum.json": JSON.stringify({ chapters: [{ title: "Vue.js & Vetrine", slug: "vue-vetrine", lessons: [] }] }),
  "courses/vue-vetrine/lessons/0001-reattivita.html": islandLesson(COURSE_ISLAND),
  "lessons-sciolte/0001-entropia.html": islandLesson(LOOSE_ISLAND),
  "courses/lezioni-singole/lessons/0001-microstati.html": islandLesson(STANDALONE_ISLAND),
  "broken/upper.html": upperIslandLesson(UPPER_ISLAND),
  "broken/no-island.html": "<html><body>niente</body></html>",
  "broken/empty-island.html": bareIslandLesson(""),
  "broken/blank-island.html": bareIslandLesson("   "),
  "broken/bad-json.html": bareIslandLesson("{ nope"),
};

const L = "courses/vue-vetrine/lessons/0001-reattivita.html";

const NODES = JSON.stringify({
  topic: "Algebra lineare",
  nodes: [
    {
      nodeId: "n-eigen",
      tier: "axiom",
      ankiParams: { targetQuestion: "Quanto vale $\\lambda$?", targetFact: "Lo spettro\tcompleto" },
    },
    {
      nodeId: "n-det",
      tier: "derivation",
      ankiParams: { suggestedCloze: "Il determinante {{c1::\\(A\\)}} è nullo" },
    },
  ],
});

const CARDS = JSON.stringify({
  topic: "Ignorato",
  cards: [
    { id: "c1", nodeId: "x", type: "basic", deck: "D", front: "Riga\tuno\r\ndue", back: "\uFEFF retro ", tags: ["a", null, "b"] },
    { id: "c2", type: "cloze", text: "Il {{c1::ciclo}}", tags: ["c"] },
    { id: "c3", type: "altro", tags: ["z"] },
  ],
});

export const CASES: AnkiCase[] = [
  { name: "help", args: ["--help"], expectExit: 0 },
  { name: "usage-none", args: [], expectExit: 0 },
  { name: "unknown", args: ["frobnicate"], expectExit: 1 },
  { name: "ping-online", args: ["ping", "--anki-url", "<ankiUrl>"], expectExit: 0 },
  { name: "ping-error", args: ["ping", "--anki-url", "<errorUrl>"], expectExit: 0 },
  { name: "ping-timeout", args: ["ping", "--anki-url", "<slowUrl>", "--timeout", "200"], expectExit: 0 },
  { name: "ping-offline", args: ["ping", "--anki-url", "<deadUrl>", "--timeout", "1000"], expectExit: 0 },
  { name: "extract-course", args: ["extract", "--lesson", L], expectExit: 0 },
  { name: "extract-loose", args: ["extract", "--lesson", "lessons-sciolte/0001-entropia.html"], expectExit: 0 },
  { name: "extract-standalone", args: ["extract", "--lesson", "courses/lezioni-singole/lessons/0001-microstati.html"], expectExit: 0 },
  { name: "extract-upper", args: ["extract", "--lesson", "broken/upper.html"], expectExit: 0 },
  { name: "extract-missing-flag", args: ["extract"], expectExit: 1 },
  {
    name: "extract-not-found",
    args: ["extract", "--lesson", "broken/nope.html"],
    stderrPrefix: "Fatal error: Error: Lesson file not found at path: broken/nope.html\n",
    expectExit: 1,
  },
  {
    name: "extract-no-island",
    args: ["extract", "--lesson", "broken/no-island.html"],
    stderrPrefix: "Fatal error: Error: Could not find embedded #lesson-data JSON block in broken/no-island.html\n",
    expectExit: 1,
  },
  {
    name: "extract-empty-island",
    args: ["extract", "--lesson", "broken/empty-island.html"],
    stderrPrefix: "Fatal error: Error: Could not find embedded #lesson-data JSON block in broken/empty-island.html\n",
    expectExit: 1,
  },
  {
    name: "extract-blank-island",
    args: ["extract", "--lesson", "broken/blank-island.html"],
    stderrPrefix: "Fatal error: Error: Failed to parse lesson-data JSON in broken/blank-island.html: ",
    expectExit: 1,
  },
  {
    name: "extract-bad-json",
    args: ["extract", "--lesson", "broken/bad-json.html"],
    stderrPrefix: "Fatal error: Error: Failed to parse lesson-data JSON in broken/bad-json.html: ",
    expectExit: 1,
  },
  { name: "export-lesson", args: ["export", "--lesson", L], expectExit: 0 },
  { name: "export-lesson-wins", args: ["export", "--data", CARDS, "--lesson", "lessons-sciolte/0001-entropia.html"], expectExit: 0 },
  { name: "export-data-nodes", args: ["export", "--data", NODES, "--out", "decks/from-data.tsv"], expectExit: 0 },
  { name: "export-stdin-nodes", args: ["export", "--stdin", "--out", "decks/from-stdin.tsv"], stdin: NODES, expectExit: 0 },
  { name: "export-data-cards", args: ["export", "--data", CARDS, "--topic", "Algoritmi", "--slug", "algo"], expectExit: 0 },
  {
    name: "export-cloze-only",
    args: [
      "export",
      "--data",
      JSON.stringify({ topic: "Fisica/Meccanica", cards: [{ id: "k", type: "cloze", text: "a {{c1::b}}", extra: "x\ty", tags: [] }] }),
    ],
    expectExit: 0,
  },
  {
    name: "export-nodes-win",
    args: [
      "export",
      "--data",
      JSON.stringify({ topic: "Proxi::Già", nodes: [], cards: [{ id: "q", type: "basic", front: "f", back: "b", tags: [] }] }),
    ],
    expectExit: 0,
  },
  {
    name: "export-empty-data-falls-through",
    args: ["export", "--data", "", "--stdin"],
    stdin: JSON.stringify({ topic: "Da stdin", cards: [] }),
    expectExit: 0,
  },
  { name: "export-base-dir", args: ["export", "--data", JSON.stringify({ topic: "Base" }), "--base-dir", "out"], expectExit: 0 },
  { name: "export-no-payload", args: ["export"], expectExit: 0 },
  { name: "export-invalid-data", args: ["export", "--data", "{"], stderrPrefix: "Fatal error: SyntaxError: ", expectExit: 1 },
  {
    name: "export-invalid-stdin-with-lesson",
    args: ["export", "--stdin", "--lesson", L],
    stdin: "nope",
    stderrPrefix: "Fatal error: SyntaxError: ",
    expectExit: 1,
  },
];

/** Real URLs back to their placeholders (longest first, so no URL is a prefix of another), then the paths and the timings. */
export function normaliseAnkiStream(text: string, urls: Record<string, string>, roots: string[]): string {
  let out = text;
  for (const [placeholder, url] of Object.entries(urls).sort((a, b) => b[1].length - a[1].length)) {
    out = out.split(url).join(placeholder);
  }
  return normaliseStream(out, roots).replace(/\((\d+)ms\)/g, "(<ms>ms)");
}

interface NodeRun {
  code: number | null;
  stdout: string;
  stderr: string;
}

/** Async on purpose: the mock servers live in this process, so a sync spawn would block them. No shell (L15). */
function runNode(args: string[], stdin: string, env: NodeJS.ProcessEnv): Promise<NodeRun> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ["--import", "tsx", path.join(REPO_ROOT, "extensions", "anki-sync.ts"), ...args], {
      cwd: REPO_ROOT,
      env,
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf-8").on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.setEncoding("utf-8").on("data", (chunk: string) => {
      stderr += chunk;
    });
    child.once("error", reject);
    child.once("close", (code) => resolve({ code, stdout, stderr }));
    child.stdin.end(stdin);
  });
}

/** A loopback URL with nothing listening: bound and closed before the CLI runs, so a connect is refused. */
async function deadUrl(): Promise<string> {
  const server = net.createServer();
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("no port for the dead Anki URL");
  await new Promise<void>((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
  return `http://127.0.0.1:${address.port}`;
}

/** Runs CASES against a fresh temp data folder and writes `<outDir>/anki-cli.json`; returns its path. */
export async function captureAnkiGolden(outDir: string = GOLDEN_DIR): Promise<string> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-anki-"));
  const online = new MockAnki();
  const erroring = new MockAnki();
  const slow = new MockAnki();
  try {
    const data = path.join(dir, "data");
    fs.mkdirSync(data);
    for (const [rel, content] of Object.entries(SEED)) {
      const file = path.join(data, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, content);
    }
    const roots = rootVariants(data);

    const onlineUrl = await online.start();
    erroring.responseError = ANKI_ERROR;
    const errorUrl = await erroring.start();
    slow.shouldTimeout = true;
    const slowUrl = await slow.start();
    const urls: Record<string, string> = {
      "<ankiUrl>": onlineUrl,
      "<errorUrl>": errorUrl,
      "<slowUrl>": slowUrl,
      "<deadUrl>": await deadUrl(),
    };

    const env: NodeJS.ProcessEnv = { ...process.env, LEARNER_DATA_ROOT: data };
    // The CLI reads only --anki-url; a stray LEARNER_ANKI_URL would point the offline cases at a live server.
    delete env.LEARNER_ANKI_URL;

    const cases: AnkiGoldenCase[] = [];
    for (const c of CASES) {
      const before = snapshot(data);
      const args = c.args.map((arg) => urls[arg] ?? arg);
      const run = await runNode(args, c.stdin ?? "", env);
      if (run.code !== c.expectExit) {
        throw new Error(`${c.name}: exit ${run.code}, expected ${c.expectExit}\nstdout: ${run.stdout}\nstderr: ${run.stderr}`);
      }
      if (c.stderrPrefix !== undefined && !run.stderr.startsWith(c.stderrPrefix)) {
        throw new Error(`${c.name}: stderr does not start with ${JSON.stringify(c.stderrPrefix)}: ${JSON.stringify(run.stderr)}`);
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
        stdout: normaliseAnkiStream(run.stdout, urls, roots),
        // A stderrPrefix means the tail is platform-specific (the stack, V8's JSON wording): pin only the prefix.
        stderr: normaliseAnkiStream(c.stderrPrefix ?? run.stderr, urls, roots),
        exitCode: c.expectExit,
        files,
      });
    }

    fs.mkdirSync(outDir, { recursive: true });
    const target = path.join(outDir, `${ANKI_GOLDEN}.json`);
    // The seed and every case's args/stdin stay raw: they are the replay's input. Only outputs are normalised.
    const file: AnkiGolden = { route: ANKI_GOLDEN, ankiError: ANKI_ERROR, seed: SEED, cases };
    fs.writeFileSync(target, JSON.stringify(file, null, 2) + "\n"); // always LF, never os.EOL
    return target;
  } finally {
    await online.stop();
    await erroring.stop();
    await slow.stop();
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const at = process.argv.indexOf("--out");
  const outDir = at === -1 ? GOLDEN_DIR : path.resolve(process.argv[at + 1] ?? GOLDEN_DIR);
  captureAnkiGolden(outDir)
    .then((file) => console.log(file))
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exit(1);
    });
}
