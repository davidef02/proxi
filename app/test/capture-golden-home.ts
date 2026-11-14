// Captures the Node `home` CLI (extensions/homepage.ts) into crates/proxi/tests/fixtures/golden/home.json (A12).
// Regenerate only from Node, before the TypeScript is deleted.
import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import type { AddressInfo } from "node:net";
import os from "node:os";
import path from "node:path";
import { rootVariants } from "./capture-golden.ts";
import { REPO_ROOT } from "./harness.ts";

export const HOME_SCRIPT = path.join(REPO_ROOT, "extensions", "homepage.ts");
export const IT_STAMP_RE = /Dati Anki al \d{2}\/\d{2}\/\d{4}, \d{2}:\d{2}:\d{2}/g;

/** Replaces this run's temp paths and the one V8 message that cannot be replayed on another engine. */
export function normaliseText(text: string, dataRoots: string[]): string {
  let out = text;
  for (const root of [...dataRoots].sort((a, b) => b.length - a.length)) out = out.split(root).join("<dataRoot>");
  out = out.split("\\").join("/");
  return out.replace(/^(.*Malformed #lesson-data island: ).*$/gm, "$1<json-error>");
}

/** Replaces the Anki date stamp: the exact local time is the only volatile bytes in a generated homepage. */
export function normaliseFile(text: string): string {
  return text.replace(IT_STAMP_RE, "Dati Anki al <it-IT>");
}

interface HomeSeeds {
  [rel: string]: string;
}

interface HomeCase {
  name: string;
  seed: string;
  args: string[];
  anki?: Record<string, unknown>;
  result: { exitCode: number; stdout: string; stderr: string };
  ankiRequests?: unknown[];
  files: Record<string, string>;
}

interface HomeGolden {
  route: "home";
  seeds: Record<string, HomeSeeds>;
  cases: HomeCase[];
}

type SeedName = "board" | "empty" | "malformed";

interface CaseSpec {
  name: string;
  seed: SeedName;
  args: string[];
  anki?: Record<string, unknown>;
}

/** Writes a lesson artifact the way the retrofit and `lesson append` both do: data island last. */
function lesson(n: number, slug: string, title: string, tiers: string[] = [], extra: Record<string, unknown> = {}): string {
  const doc = {
    metadata: {
      lessonNumber: n,
      topic: title || slug,
      title,
      slug,
      dateCreated: "2026-09-01T00:00:00.000Z",
      dateModified: "2026-09-01T00:00:00.000Z",
      version: "1.0.0",
      ...extra,
    },
    nodes: tiers.map((tier, i) => ({
      nodeId: "n" + i,
      title: "Nodo " + i,
      tier,
      content: "...",
    })),
  };
  return (
    "<html><body><h1>" +
    title +
    '</h1>\n<script type="application/json" id="lesson-data">\n' +
    JSON.stringify(doc, null, 2) +
    "\n</script>\n</body></html>"
  );
}

function boardSeed(): HomeSeeds {
  const docker = {
    title: "Docker",
    slug: "docker",
    note: "tenuto",
    lessons: [
      { title: "Image e container", slug: "image-e-container" },
      { title: "Healthcheck", slug: "healthcheck", plannedNodes: 4 },
      { title: "Rete & volumi", slug: "rete-and-volumi" },
    ],
  };
  const probabilita = {
    title: "Probabilità",
    slug: "probabilita",
    lessons: [
      { title: "Eventi", slug: "eventi", plannedNodes: 1 },
      { title: "Varianza", slug: "varianza" },
    ],
  };
  const rust = {
    title: "Rust",
    slug: "rust",
    lessons: [
      { title: "Ownership", slug: "ownership" },
      { title: "Lifetimes", slug: "lifetimes", plannedNodes: 2 },
    ],
  };

  return {
    "curriculum.json": JSON.stringify({ chapters: [docker, probabilita] }, null, 2) + "\n",
    "plan.json": JSON.stringify({ chapters: [docker, probabilita, rust] }, null, 2) + "\n",
    "courses/docker/lessons/0001-image-e-container.html": lesson(
      1,
      "image-e-container",
      "Image e container",
      ["unconditional-truth", "axiom", "derivation", "bogus-tier"]
    ),
    "courses/docker/lessons/0003-rete-and-volumi.html": lesson(3, "rete-and-volumi", "Rete & volumi"),
    "courses/docker/lessons/0002-con spazio.html": lesson(2, "con-spazio", '<b>A & "B"</b> \'c\'', ["axiom"]),
    "courses/docker/lessons/0004-image-e-container-bis.html": lesson(
      4,
      "image-e-container",
      "Image e container bis",
      ["derivation"]
    ),
    "courses/docker/lessons/0005-rotta.html":
      '<html><body><h1>Rotta</h1>\n<script type="application/json" id="lesson-data">\n{ not json\n</script>\n</body></html>',
    "courses/docker/lessons/notes.html": "<html><body><h1>Note</h1></body></html>",
    "courses/docker/lessons/readme.txt": "Non e una lezione.\n",
    "courses/probabilita/lessons/0001-eventi.html": lesson(1, "eventi", "", ["axiom"], { topic: "Eventi" }),
    "courses/probabilita/lessons/0002-dadi.html": lesson(2, "dadi", "Dadi", ["axiom"]),
    "courses/probabilita/reference/eventi.html": "<html></html>",
    "courses/vuoto/MISSION.md": "# Vuoto\n",
    "corsi/rust/lessons/0001-ownership.html": lesson(1, "ownership", "Ownership", ["axiom", "derivation"]),
    "corsi/rust/lessons/0002-borrow.html": lesson(2, "borrow", "Borrow"),
  };
}

const SEEDS: Record<SeedName, HomeSeeds> = {
  board: boardSeed(),
  empty: {},
  malformed: { "curriculum.json": '{"chapters":{"docker":{}}}\n' },
};

const ANKI_DECK_NAMES = {
  result: {
    Default: 1,
    Proxi: 10,
    "Proxi::Docker": 11,
    "Proxi::Probabilità": 12,
    "Altro::Proxi": 13,
  },
  error: null,
};

const ANKI_DECK_STATS = {
  result: {
    "10": { deck_id: 10, name: "Proxi", new_count: 5, learn_count: 2, review_count: 7, total_in_deck: 0 },
    "11": { deck_id: 11, name: "Docker", new_count: 3, learn_count: 1, review_count: 4, total_in_deck: 20 },
    "12": { deck_id: 12, name: "Probabilità", new_count: 2, learn_count: 1, review_count: 3, total_in_deck: 3 },
  },
  error: null,
};

const CASES: CaseSpec[] = [
  { name: "generate-no-anki", seed: "board", args: ["generate", "--no-anki"] },
  { name: "generate-default-command", seed: "board", args: ["--no-anki"] },
  {
    name: "generate-options",
    seed: "board",
    args: ["--no-anki", "--dir", "<dataRoot>/corsi", "--curriculum", "plan.json", "--out", "board.html"],
  },
  { name: "generate-empty", seed: "empty", args: ["--no-anki"] },
  {
    name: "generate-anki",
    seed: "board",
    args: ["--anki-url", "<anki>"],
    anki: { deckNamesAndIds: ANKI_DECK_NAMES, getDeckStats: ANKI_DECK_STATS },
  },
  { name: "generate-anki-unreachable", seed: "board", args: ["--anki-url", "<closedAnki>"] },
  {
    name: "generate-anki-error",
    seed: "board",
    args: ["--anki-url", "<anki>"],
    anki: { deckNamesAndIds: { result: null, error: "collection is not available" } },
  },
  {
    name: "plan-new",
    seed: "board",
    args: [
      "plan",
      "--chapter",
      "  Rust & WebAssembly ",
      "--lessons-json",
      '[{"title":"Ownership","plannedNodes":3},{"title":"Città e borrow","slug":"Borrow Checker"}]',
    ],
  },
  {
    name: "plan-replace",
    seed: "board",
    args: ["plan", "--chapter", "Docker Avanzato", "--slug", "docker", "--lessons-json", '[{"title":"Compose"}]'],
  },
  {
    name: "plan-curriculum",
    seed: "board",
    args: ["plan", "--curriculum", "plan.json", "--chapter", "Rust", "--lessons-json", '[{"title":"Traits"}]'],
  },
  { name: "plan-empty-file", seed: "empty", args: ["plan", "--chapter", "Vue", "--lessons-json", "[]"] },
  {
    name: "plan-duplicate-slug",
    seed: "board",
    args: [
      "plan",
      "--chapter",
      "X",
      "--lessons-json",
      '[{"title":"Image e container"},{"title":"Image, e container!"}]',
    ],
  },
  { name: "plan-missing-chapter", seed: "board", args: ["plan", "--lessons-json", "[]"] },
  { name: "plan-missing-lessons", seed: "board", args: ["plan", "--chapter", "X"] },
  { name: "plan-not-array", seed: "board", args: ["plan", "--chapter", "X", "--lessons-json", '{"a":1}'] },
  {
    name: "plan-blank-title",
    seed: "board",
    args: ["plan", "--chapter", "X", "--lessons-json", '[{"title":"  "}]'],
  },
  { name: "plan-malformed", seed: "malformed", args: ["plan", "--chapter", "X", "--lessons-json", "[]"] },
  { name: "chapters", seed: "board", args: ["chapters"] },
  { name: "chapters-options", seed: "board", args: ["chapters", "--dir", "corsi", "--curriculum", "plan.json"] },
  { name: "chapters-empty", seed: "empty", args: ["chapters"] },
  { name: "chapters-malformed", seed: "malformed", args: ["chapters"] },
  { name: "help", seed: "empty", args: ["help"] },
  { name: "help-flag", seed: "board", args: ["plan", "-h"] },
  { name: "unknown-command", seed: "empty", args: ["frobnicate"] },
  { name: "unknown-option", seed: "empty", args: ["--bogus=3"] },
  { name: "unknown-short", seed: "empty", args: ["-hx"] },
  { name: "missing-value", seed: "empty", args: ["plan", "--chapter"] },
  { name: "ambiguous-value", seed: "empty", args: ["plan", "--chapter", "--slug", "x"] },
  { name: "boolean-with-value", seed: "empty", args: ["--no-anki=1"] },
  { name: "help-with-value", seed: "empty", args: ["--help=1"] },
];

/** Runs the real Node CLI, never in-process: the fake AnkiConnect server has to answer while it runs. */
function runHome(args: string[], dataRoot: string): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ["--import", "tsx", HOME_SCRIPT, ...args], {
      cwd: REPO_ROOT,
      env: { ...process.env, LEARNER_DATA_ROOT: dataRoot },
      shell: false,
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code) => resolve({ exitCode: code ?? 1, stdout, stderr }));
  });
}

function listen(server: http.Server): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });
}

function close(server: http.Server): Promise<void> {
  return new Promise((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
}

/** A URL whose port was bound and released, so connecting to it is refused like a closed Anki. */
async function closedAnkiUrl(): Promise<string> {
  const server = http.createServer();
  await listen(server);
  const { port } = server.address() as AddressInfo;
  await close(server);
  return `http://127.0.0.1:${port}`;
}

function walkFiles(root: string, rel = ""): string[] {
  const out: string[] = [];
  for (const entry of fs.readdirSync(path.join(root, rel), { withFileTypes: true })) {
    const child = rel ? path.posix.join(rel, entry.name) : entry.name;
    if (entry.isDirectory()) out.push(...walkFiles(root, child));
    else out.push(child);
  }
  return out.sort();
}

function fakeAnki(spec: CaseSpec, requests: unknown[]): http.Server {
  const answers = spec.anki ?? {};
  return http.createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk: Buffer) => chunks.push(chunk));
    req.on("end", () => {
      const body = Buffer.concat(chunks).toString("utf-8");
      let parsed: unknown = null;
      try {
        parsed = JSON.parse(body);
      } catch {
        // An unreadable request still gets an answer: the golden records it as null.
      }
      requests.push(parsed);
      const action = (parsed as { action?: string } | null)?.action;
      const answer = action ? answers[action] : undefined;
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(answer ?? { result: null, error: "unsupported action" }));
    });
  });
}

/** Replays every `home` case against the real Node CLI and writes `<outDir>/home.json`. */
export async function captureHome(outDir: string): Promise<string> {
  const closed = await closedAnkiUrl();
  const cases: HomeCase[] = [];

  for (const spec of CASES) {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "golden-home-"));
    const data = path.join(tmp, "data");
    const requests: unknown[] = [];
    let server: http.Server | undefined;
    try {
      fs.mkdirSync(data, { recursive: true });
      for (const [rel, content] of Object.entries(SEEDS[spec.seed])) {
        const file = path.join(data, rel);
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, content, "utf-8");
      }

      let ankiUrl = "";
      if (spec.anki) {
        server = fakeAnki(spec, requests);
        await listen(server);
        ankiUrl = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
      }

      const args = spec.args.map((arg) =>
        arg
          .split("<dataRoot>").join(path.resolve(data))
          .split("<anki>").join(ankiUrl)
          .split("<closedAnki>").join(closed)
      );
      const result = await runHome(args, data);

      const files: Record<string, string> = {};
      for (const rel of walkFiles(data)) {
        const content = fs.readFileSync(path.join(data, rel), "utf-8");
        if (SEEDS[spec.seed][rel] === content) continue;
        files[rel] = normaliseFile(content);
      }

      const entry: HomeCase = {
        name: spec.name,
        seed: spec.seed,
        args: spec.args,
        result: {
          exitCode: result.exitCode,
          stdout: normaliseText(result.stdout, rootVariants(data)),
          stderr: normaliseText(result.stderr, rootVariants(data)),
        },
        files,
      };
      if (spec.anki) {
        entry.anki = spec.anki;
        entry.ankiRequests = requests;
      }
      cases.push(entry);
    } finally {
      if (server) await close(server);
      fs.rmSync(tmp, { recursive: true, force: true, maxRetries: 3 });
    }
  }

  fs.mkdirSync(outDir, { recursive: true });
  const target = path.join(outDir, "home.json");
  const golden: HomeGolden = { route: "home", seeds: SEEDS, cases };
  fs.writeFileSync(target, JSON.stringify(golden, null, 2) + "\n", "utf-8");
  return target;
}
