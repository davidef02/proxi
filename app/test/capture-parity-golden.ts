// Captures the Node side of the live-parity suites into crates/proxi/tests/fixtures/golden (A12),
// before the Node deletion: data_cli.rs → data-cli.json, context_parity.rs → context.json.
// Run directly (not via package.json): node --import tsx app/test/capture-parity-golden.ts
// Regenerate only from Node; after the cutover the goldens are frozen.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { DATA_ROOT_ENV, resolveDataRoot } from "../../extensions/data-root.ts";
import { REPO_ROOT } from "./harness.ts";

const GOLDEN_DIR = path.join(REPO_ROOT, "crates", "proxi", "tests", "fixtures", "golden");

type Roots = Array<[string, string]>; // [path, placeholder]

// ---------------------------------------------------------------------------
// Shared normalisation: every temp root in all its spellings, then separators.
// ---------------------------------------------------------------------------

/** `<dir>` plus, when it exists, its native realpath: both forms may reach the output. */
function rootVariants(dir: string): string[] {
  const out = [path.resolve(dir)];
  if (fs.existsSync(dir)) {
    const real = fs.realpathSync.native(dir);
    if (!out.includes(real)) out.push(real);
  }
  return out;
}

function tagRoots(dir: string, tag: string): Roots {
  return rootVariants(dir).map((p) => [p, tag] as [string, string]);
}

/** Longest first: a root that is a prefix of another must be replaced by the deeper one. */
function normalise(text: string, roots: Roots): string {
  let out = text;
  for (const [root, tag] of [...roots].sort((a, b) => b[0].length - a[0].length)) {
    // stdout is JSON (backslashes double-escaped), stderr is not: replace both spellings.
    out = out.split(JSON.stringify(root).slice(1, -1)).join(tag);
    out = out.split(root).join(tag);
  }
  // CRLF → LF: the fixture embeds the real docs/agents files, whose line endings follow the checkout.
  return out.split("\\\\").join("/").split("\\").join("/").replace(/\r\n/g, "\n");
}

/** Deep normalisation of a JSON value (used for the context inputs). */
function normaliseValue(value: unknown, roots: Roots): unknown {
  if (typeof value === "string") return normalise(value, roots);
  if (Array.isArray(value)) return value.map((v) => normaliseValue(v, roots));
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [key, v] of Object.entries(value)) out[key] = normaliseValue(v, roots);
    return out;
  }
  return value;
}

// ---------------------------------------------------------------------------
// data-cli golden: every case of crates/proxi/tests/data_cli.rs.
// ---------------------------------------------------------------------------

interface DataCliCase {
  name: string;
  /** `proxi data <args>`; the where-resolver cases run the same `where` command. */
  args: string[];
  /** The child env, with `<home>`/`<dataRoot>`/`<repo>` placeholders the replay substitutes. */
  env: Record<string, string>;
  /** Written to `<repo>/.learner/config.json`; null → no config file. */
  config: { dataRoot: string } | null;
  stdout: string;
  stderr: string;
  exitCode: number;
}

interface DataCliGolden {
  route: "data-cli";
  cases: DataCliCase[];
}

const BASE_ENV: Record<string, string> = { HOME: "<home>", USERPROFILE: "<home>" };

/** The six `where` resolver cases: Node's side is `resolveDataRoot(repo, env, home)`. */
function whereCases(roots: Roots, repo: string, home: string, data: string): DataCliCase[] {
  const setConfig = (dataRoot: string | null): void => {
    if (dataRoot === null) {
      fs.rmSync(path.join(repo, ".learner"), { recursive: true, force: true, maxRetries: 3 });
      return;
    }
    const learner = path.join(repo, ".learner");
    fs.mkdirSync(learner, { recursive: true });
    fs.writeFileSync(path.join(learner, "config.json"), JSON.stringify({ dataRoot }) + "\n");
  };
  const run = (env: Record<string, string>): Pick<DataCliCase, "stdout" | "stderr" | "exitCode"> => {
    const { dataRoot, source } = resolveDataRoot(repo, env, home);
    return { stdout: `${dataRoot} (${source})\n`, stderr: "", exitCode: 0 };
  };
  const record = (
    name: string,
    env: Record<string, string>,
    config: { dataRoot: string } | null,
    resolved: Pick<DataCliCase, "stdout" | "stderr" | "exitCode">,
  ): DataCliCase => ({
    name,
    args: ["where"],
    env,
    config,
    stdout: normalise(resolved.stdout, roots),
    stderr: normalise(resolved.stderr, roots),
    exitCode: resolved.exitCode,
  });

  return [
    record(
      "where_env_absolute_matches_node",
      { ...BASE_ENV, LEARNER_DATA_ROOT: "<dataRoot>", LEARNER_REPO_ROOT: "<repo>" },
      null,
      run({ [DATA_ROOT_ENV]: data }),
    ),
    record(
      "where_env_relative_matches_node",
      { ...BASE_ENV, LEARNER_DATA_ROOT: "rel/dir", LEARNER_REPO_ROOT: "<repo>" },
      null,
      run({ [DATA_ROOT_ENV]: "rel/dir" }),
    ),
    record(
      "where_env_padded_matches_node",
      { ...BASE_ENV, LEARNER_DATA_ROOT: "  <dataRoot>  ", LEARNER_REPO_ROOT: "<repo>" },
      null,
      run({ [DATA_ROOT_ENV]: `  ${data}  ` }),
    ),
    record(
      "where_config_pointer_matches_node",
      { ...BASE_ENV, LEARNER_REPO_ROOT: "<repo>" },
      { dataRoot: "<dataRoot>" },
      (() => {
        setConfig(data);
        return run({});
      })(),
    ),
    record(
      "where_config_pointer_relative_falls_back_to_default",
      { ...BASE_ENV, LEARNER_REPO_ROOT: "<repo>" },
      { dataRoot: "rel/dir" },
      (() => {
        setConfig("rel/dir");
        return run({});
      })(),
    ),
    record(
      "where_default_matches_node",
      { ...BASE_ENV, LEARNER_REPO_ROOT: "<repo>" },
      null,
      (() => {
        setConfig(null);
        return run({});
      })(),
    ),
  ];
}

/** The ten `data` CLI cases: Node's side is the real `extensions/data-root.ts` CLI. */
function cliCases(roots: Roots, repo: string, home: string, data: string): DataCliCase[] {
  const argSets: Array<[string, string[]]> = [
    ["cli_where_matches_node", ["where"]],
    ["cli_help_matches_node", ["help"]],
    ["cli_no_args_matches_node", []],
    ["cli_unknown_command_matches_node", ["nope"]],
    ["cli_unknown_long_option_matches_node", ["where", "--foo"]],
    ["cli_unknown_long_option_with_value_matches_node", ["where", "--foo=bar"]],
    ["cli_unknown_short_option_matches_node", ["-x"]],
    ["cli_unknown_short_option_group_matches_node", ["-abc"]],
    ["cli_double_dash_positional_matches_node", ["where", "--", "--zz"]],
    ["cli_double_dash_unknown_command_matches_node", ["--", "-abc"]],
  ];
  const env = { ...process.env, HOME: home, USERPROFILE: home, LEARNER_DATA_ROOT: data, LEARNER_REPO_ROOT: repo };
  return argSets.map(([name, args]) => {
    const run = spawnSync(process.execPath, ["--import", "tsx", path.join(REPO_ROOT, "extensions", "data-root.ts"), ...args], {
      cwd: REPO_ROOT,
      env,
      input: "",
      encoding: "utf-8",
    });
    if (run.error) throw run.error;
    return {
      name,
      args,
      env: { ...BASE_ENV, LEARNER_DATA_ROOT: "<dataRoot>", LEARNER_REPO_ROOT: "<repo>" },
      config: null,
      stdout: normalise(run.stdout, roots),
      stderr: normalise(run.stderr, roots),
      exitCode: run.status ?? -1,
    };
  });
}

function captureDataCli(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-data-cli-"));
  try {
    const repo = path.join(dir, "repo");
    const home = path.join(dir, "home");
    const data = path.join(dir, "data");
    for (const sub of [repo, home, data]) fs.mkdirSync(sub);
    // The relative-env cases resolve against the cwd, which the Rust suite pins to the repo root.
    process.chdir(REPO_ROOT);
    const roots: Roots = [...tagRoots(REPO_ROOT, "<repoRoot>"), ...tagRoots(repo, "<repo>"), ...tagRoots(home, "<home>"), ...tagRoots(data, "<dataRoot>")];
    const cases = [...whereCases(roots, repo, home, data), ...cliCases(roots, repo, home, data)];
    const golden: DataCliGolden = { route: "data-cli", cases };
    fs.mkdirSync(GOLDEN_DIR, { recursive: true });
    const target = path.join(GOLDEN_DIR, "data-cli.json");
    fs.writeFileSync(target, JSON.stringify(golden, null, 2) + "\n"); // always LF, never os.EOL
    return target;
  } finally {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  }
}

// ---------------------------------------------------------------------------
// context golden: every case of crates/proxi/tests/context_parity.rs cases().
// ---------------------------------------------------------------------------

const TITLE = "Il titolo pianificato della lezione due";
const LESSON = "courses/demo/lessons/0001-lezione-uno.html";
const DAG = "graph TD\n  N1[\"Nodo 1: Prima cosa\"] --> N2[\"Nodo 2: Seconda cosa\"]\n  N2 --> N3[\"Nodo 3: Terza cosa\"]";
const TODAY = "2026-10-27";

interface ContextCase {
  name: string;
  /** The prompt-dump input, with `<repo>`/`<dataRoot>` placeholders the replay substitutes. */
  input: Record<string, unknown>;
  /** Prove the fixture was actually read: the replay re-asserts them against the golden prompt. */
  markers: string[];
  prompt: string;
  opening: string;
}

interface ContextGolden {
  route: "context";
  cases: ContextCase[];
}

function write(root: string, rel: string, text: string): void {
  const file = path.join(root, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

const lessonIsland = (doc: unknown) => `<script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script>`;

/** One temp code repo (reference docs) and a separate temp data folder with a full course. */
function buildFixture(): { repo: string; data: string; inputs: string; cleanup: () => void } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-context-"));
  const repo = path.join(dir, "repo");
  const data = path.join(dir, "data");
  const inputs = path.join(dir, "inputs");
  for (const sub of [repo, data, inputs]) fs.mkdirSync(sub);

  for (const name of ["pedagogy.md", "anki-cards.md"]) {
    write(repo, path.join("docs", "agents", name), fs.readFileSync(path.join(REPO_ROOT, "docs", "agents", name), "utf-8"));
  }

  write(
    data,
    "curriculum.json",
    JSON.stringify({
      chapters: [
        {
          title: "Demo",
          slug: "demo",
          lessons: [
            { title: "Lezione uno", slug: "lezione-uno", plannedNodes: 3 },
            { title: TITLE, slug: "il-titolo-pianificato-della-lezione-due", plannedNodes: 4 },
          ],
        },
        {
          title: "Altro",
          slug: "altro",
          lessons: [{ title: "Lezione tre", slug: "lezione-tre", plannedNodes: 2 }],
        },
      ],
    }),
  );

  write(data, "courses/demo/MISSION.md", "# Mission\nMISSION-MARKER");
  write(data, "courses/demo/NOTES.md", "# Notes\nNOTES-MARKER");
  write(data, "courses/demo/GLOSSARY.md", "# Glossario\nGLOSSARY-MARKER");
  for (let n = 1; n <= 4; n++) {
    write(data, `courses/demo/learning-records/000${n}-x${n}.md`, `# rec ${n}\nREC-${n}`);
  }
  write(data, "courses/demo/exercises/package.json", "{}");
  fs.mkdirSync(path.join(data, "courses/demo/exercises/01-a"));
  fs.mkdirSync(path.join(data, "courses/demo/exercises/zz-b"));

  const demo = {
    metadata: {
      lessonNumber: 1,
      topic: "Demo topic",
      title: "Lezione uno",
      slug: "lezione-uno",
      dateCreated: "2026-09-01T08:00:00.000Z",
      dateModified: "2026-09-01T08:00:00.000Z",
      initialDag: DAG,
      targetGoal: "Capire la prima cosa",
      version: "1.0.0",
    },
    nodes: [{ nodeId: "prima-cosa", title: "Prima cosa", tier: "unconditional-truth", content: "<p>x</p>" }],
  };
  write(data, LESSON, lessonIsland(demo));
  const demoProgress = {
    at: "2026-09-01T08:00:00.000Z",
    sessionId: "s1",
    course: "demo",
    lessonSlug: "lezione-uno",
    nodeRefs: ["demo/lezione-uno#prima-cosa"],
    kind: "review",
    source: "anki",
    reviewId: 1,
    cardId: 1,
    ease: 1,
    lapses: 2,
    interval: 0,
  };
  write(data, "courses/demo/progress.jsonl", `${JSON.stringify(demoProgress)}\n`);

  const orphan = {
    metadata: {
      lessonNumber: 1,
      topic: "Tema orfano",
      title: "Lezione orfana",
      slug: "lezione-orfana",
      dateCreated: "2026-09-02T08:00:00.000Z",
      dateModified: "2026-09-02T08:00:00.000Z",
      initialDag: "graph TD\n  N1[\"Nodo 1: Nodo orfano\"] --> N2[\"Nodo 2: Coda orfana\"]",
      targetGoal: "Capire l'orfano",
      version: "1.0.0",
    },
    nodes: [{ nodeId: "nodo-orfano", title: "Nodo orfano", tier: "axiom", content: "<p>y</p>" }],
  };
  write(data, "courses/orfano/lessons/0001-lezione-orfana.html", lessonIsland(orphan));
  const orphanProgress = {
    at: "2026-09-02T08:00:00.000Z",
    sessionId: "s3",
    course: "orfano",
    lessonSlug: "lezione-orfana",
    nodeRefs: ["orfano/lezione-orfana#nodo-orfano"],
    kind: "check",
    correct: true,
    confidence: 80,
  };
  write(data, "courses/orfano/progress.jsonl", `${JSON.stringify(orphanProgress)}\n`);

  // common::write_node_lines: the graph store the routes and the CLIs accept.
  const graph = path.join(data, "graph");
  fs.mkdirSync(graph);
  const nodeLine = (id: string, name: string, statement: string) =>
    JSON.stringify({ id, name, statement, aliases: [], createdAt: "2026-01-01T00:00:00.000Z", provenance: [] });
  fs.writeFileSync(
    path.join(graph, "nodes.jsonl"),
    [nodeLine("n_prima", "Concetto primo", "La prima proposizione del grafo."), nodeLine("n_seconda", "Concetto secondo", "La seconda proposizione del grafo.")].join("\n") + "\n",
  );
  fs.writeFileSync(path.join(graph, "chains.jsonl"), "");

  return { repo, data, inputs, cleanup: () => fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 }) };
}

const teachPlanned = (anki: boolean, model: string): Record<string, unknown> => ({
  kind: "teach",
  course: "demo",
  model,
  target: { mode: "planned", title: TITLE },
  ankiOptIn: anki,
});
const teachExisting = (): Record<string, unknown> => ({
  kind: "teach",
  course: "demo",
  model: "sonnet",
  target: { mode: "existing", lessonPath: LESSON },
  ankiOptIn: false,
});
const practice = (extra: Record<string, unknown>): Record<string, unknown> => ({
  kind: "practice",
  course: "demo",
  model: "haiku",
  practiceNodes: [],
  ...extra,
});

interface CaseDef {
  name: string;
  config: Record<string, unknown>;
  dataRoot: boolean;
  lessonPath?: string;
  lessonTitle?: string;
  markers: string[];
}

function contextCaseDefs(): CaseDef[] {
  return [
    { name: "teach-planned-anki", config: teachPlanned(true, "sonnet"), dataRoot: true, markers: ["Anki opt-in: Sì", TITLE, "REC-2", "anki-cards.md"] },
    { name: "teach-planned-no-anki", config: teachPlanned(false, "haiku"), dataRoot: true, markers: ["Anki opt-in: No", "<chapter-plan>"] },
    { name: "teach-existing", config: teachExisting(), dataRoot: true, lessonPath: LESSON, lessonTitle: "Lezione uno", markers: ["Captured nodes (1 of 3 planned)", "Capire la prima cosa"] },
    { name: "teach-existing-target-path", config: teachExisting(), dataRoot: true, lessonTitle: "Lezione uno", markers: ["Captured nodes (1 of 3 planned)", "Capire la prima cosa"] },
    {
      name: "teach-free-standalone",
      config: { kind: "teach", course: "lezioni-singole", model: "sonnet", target: { mode: "free" }, ankiOptIn: false, note: "  Voglio partire dalle basi.  " },
      dataRoot: true,
      markers: ["Learner's note: Voglio partire dalle basi."],
    },
    {
      name: "plan-existing",
      config: { kind: "plan", course: "demo", model: "sonnet", chapterTitle: "Demo", note: "Partiamo dal piano." },
      dataRoot: true,
      markers: ["<chapter-plan>", "GLOSSARY-MARKER", "<exercises course=\"demo\">", "01-a", "zz-b"],
    },
    { name: "plan-new", config: { kind: "plan", course: "nuovo", model: "opus", chapterTitle: "Nuovo corso" }, dataRoot: true, markers: ["is new: provide a full mission"] },
    { name: "practice-no-pick", config: practice({}), dataRoot: true, markers: ["- key \"demo/lezione-uno#prima-cosa\""] },
    { name: "practice-nodes", config: practice({ practiceNodes: ["demo/lezione-uno#prima-cosa"] }), dataRoot: true, markers: ["Selected nodes to practise"] },
    { name: "practice-unknown-nodes", config: practice({ practiceNodes: ["demo/lezione-uno#sparito"] }), dataRoot: true, markers: ["not found in the map"] },
    { name: "practice-propositions", config: practice({ practicePropositions: ["n_prima", "n_mancante"] }), dataRoot: true, markers: ["Concetto primo", "La prima proposizione del grafo.", "n_mancante (not found in the map)"] },
    { name: "teach-planned-repo-as-data", config: teachPlanned(false, "sonnet"), dataRoot: false, markers: ["The knowledge graph is empty"] },
  ];
}

function captureContext(): string {
  const fixture = buildFixture();
  try {
    const roots: Roots = [
      ...tagRoots(REPO_ROOT, "<repoRoot>"),
      ...tagRoots(fixture.repo, "<repo>"),
      ...tagRoots(fixture.data, "<dataRoot>"),
      ...tagRoots(fixture.inputs, "<inputs>"),
    ];
    const cases: ContextCase[] = [];
    for (const def of contextCaseDefs()) {
      const input: Record<string, unknown> = {
        config: def.config,
        repoRoot: fixture.repo,
        ...(def.dataRoot ? { dataRoot: fixture.data } : {}),
        ...(def.lessonPath ? { lessonPath: def.lessonPath } : {}),
        today: TODAY,
        ...(def.lessonTitle ? { lessonTitle: def.lessonTitle } : {}),
      };
      const file = path.join(fixture.inputs, `${def.name}.json`);
      fs.writeFileSync(file, JSON.stringify(input));
      const run = spawnSync(process.execPath, ["--import", "tsx", path.join(REPO_ROOT, "app", "test", "prompt-dump.ts"), file], {
        cwd: REPO_ROOT,
        encoding: "utf-8",
      });
      if (run.error) throw run.error;
      if (run.status !== 0) {
        throw new Error(`${def.name}: prompt-dump failed:\n${run.stderr}`);
      }
      const dumped = JSON.parse(run.stdout) as { prompt: string; opening: string };
      // A fixture both sides read as empty would compare equal and prove nothing.
      if (!dumped.prompt.includes("# Course index") || !dumped.prompt.includes("# This session")) {
        throw new Error(`${def.name}: prompt did not read the fixture:\n${dumped.prompt}`);
      }
      for (const marker of def.markers) {
        if (!dumped.prompt.includes(marker)) {
          throw new Error(`${def.name}: prompt lacks ${JSON.stringify(marker)}:\n${dumped.prompt}`);
        }
      }
      cases.push({
        name: def.name,
        input: normaliseValue(input, roots) as Record<string, unknown>,
        markers: def.markers,
        prompt: normalise(dumped.prompt, roots),
        opening: normalise(dumped.opening, roots),
      });
    }
    const golden: ContextGolden = { route: "context", cases };
    fs.mkdirSync(GOLDEN_DIR, { recursive: true });
    const target = path.join(GOLDEN_DIR, "context.json");
    fs.writeFileSync(target, JSON.stringify(golden, null, 2) + "\n"); // always LF, never os.EOL
    return target;
  } finally {
    fixture.cleanup();
  }
}

function main(): void {
  console.log(captureDataCli());
  console.log(captureContext());
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main();
  } catch (error: unknown) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
