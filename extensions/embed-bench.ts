/**
 * embed-bench.ts — picks the local embedding model behind semantic concept search.
 *
 * Runs a few multilingual candidates over his own graph (graph/nodes.jsonl in the data
 * folder) with the queries in <dataRoot>/bench/embed-bench-queries.json, measuring size on
 * disk, recall@3 and median search time. The winner goes into
 * app/server/graph/embedding-model.json, and the report into REPORT_FILE (numbers only:
 * the queries and the concept names stay in the data folder).
 *
 * Models download into <CODE_ROOT>/.learner/models, the same dir the app uses, so the winner
 * is already in place when search needs it.
 *
 * Usage: npm run embed-bench [-- --no-write] [-- --queries <path>]
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { parseArgs } from "node:util";
import type { DataType, FeatureExtractionPipeline } from "@huggingface/transformers";
import { readSnapshot } from "../app/server/graph/store.ts";
import { main } from "./cli.ts";
import { CODE_ROOT, resolveDataRoot } from "./data-root.ts";

const MODELS_DIR = path.join(CODE_ROOT, ".learner", "models");
const REPORT_FILE = path.join(CODE_ROOT, "docs", "research", "07-embedding-benchmark.md");
const TOP_K = 3;
const MIN_RECALL = 0.9;
const MAX_MEDIAN_MS = 200;

interface Candidate {
  id: string;
  dtype: DataType;
  queryPrefix: string;
  passagePrefix: string;
}

/** Candidates to compare: edit this list to add or drop one. */
export const CANDIDATES: Candidate[] = [
  { id: "onnx-community/granite-embedding-97m-multilingual-r2-ONNX", dtype: "q8", queryPrefix: "", passagePrefix: "" },
  { id: "Xenova/multilingual-e5-small", dtype: "q8", queryPrefix: "query: ", passagePrefix: "passage: " },
  { id: "Xenova/paraphrase-multilingual-MiniLM-L12-v2", dtype: "q8", queryPrefix: "", passagePrefix: "" },
];

interface Concept {
  id: string;
  name: string;
  statement: string;
  aliases: string[];
}

interface BenchQuery {
  q: string;
  lang: "it" | "en";
  expect: string;
  /** Concept id of `expect`, resolved once so ranking compares ids. */
  expectId: string;
}

interface Hit {
  id: string;
  cosine: number;
}

interface Result {
  cand: Candidate;
  dtype: DataType;
  sizeMB: number;
  recall: number;
  medianMs: number;
  failsIt: number;
  failsEn: number;
  failed: BenchQuery[];
  thirdCosines: number[];
}

// keep in sync with app/server/graph/embeddings.ts conceptText
function conceptText(n: Pick<Concept, "name" | "statement" | "aliases">): string {
  return [n.name, n.statement, n.aliases.join(", ")].filter(Boolean).join(". ");
}

/** The same merged view the app embeds: a re-asserted node is benchmarked with its latest text. */
async function readConcepts(dataRoot: string): Promise<Concept[]> {
  const snap = await readSnapshot(dataRoot);
  return snap.propositions.map(({ id, name, statement, aliases }) => ({ id, name, statement, aliases }));
}

function readQueries(file: string, conceptId: Map<string, string>): BenchQuery[] {
  if (!fs.existsSync(file)) {
    throw new Error(`queries not found: ${file}\nwrite bench/embed-bench-queries.json in the data folder`);
  }
  const raw: unknown = JSON.parse(fs.readFileSync(file, "utf8"));
  if (!Array.isArray(raw) || raw.length === 0) throw new Error(`${file} must be a non-empty JSON array`);
  return raw.map((entry: any, i) => {
    const at = `${file}[${i}]`;
    if (typeof entry?.q !== "string" || !entry.q.trim()) throw new Error(`${at}.q must be a non-empty string`);
    if (entry.lang !== "it" && entry.lang !== "en") throw new Error(`${at}.lang must be "it" or "en"`);
    const id = conceptId.get(entry.expect);
    if (!id) throw new Error(`${at}.expect does not match any concept name: ${entry.expect}`);
    return { q: entry.q, lang: entry.lang, expect: entry.expect, expectId: id };
  });
}

async function loadExtractor(model: string, dtype: DataType): Promise<{ ex: FeatureExtractionPipeline; dtype: DataType }> {
  const { env, pipeline, LogLevel } = await import("@huggingface/transformers");
  env.cacheDir = MODELS_DIR;
  env.localModelPath = MODELS_DIR;
  env.allowRemoteModels = true;
  env.allowLocalModels = true;
  env.logLevel = LogLevel.ERROR;
  try {
    return { ex: await pipeline("feature-extraction", model, { dtype }), dtype };
  } catch (err: any) {
    // not every repo ships a q8 file: retry the full precision one
    if (dtype === "fp32") throw err;
    console.log(`  no ${dtype} weights (${err?.message ?? err}); retrying with fp32`);
    return { ex: await pipeline("feature-extraction", model, { dtype: "fp32" }), dtype: "fp32" };
  }
}

async function embed(ex: FeatureExtractionPipeline, texts: string[]): Promise<number[][]> {
  const tensor = await ex(texts, { pooling: "mean", normalize: true });
  return tensor.tolist() as number[][];
}

function dot(a: number[], b: number[]): number {
  let sum = 0;
  for (let i = 0; i < a.length; i++) sum += a[i] * b[i];
  return sum;
}

function dirSize(dir: string): number {
  let total = 0;
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return 0;
  }
  for (const entry of entries) {
    const child = path.join(dir, entry.name);
    total += entry.isDirectory() ? dirSize(child) : fs.statSync(child).size;
  }
  return total;
}

function median(xs: number[]): number {
  if (xs.length === 0) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}

/** Nearest-rank percentile: good enough for a 30-odd sample comparison. */
function percentile(xs: number[], p: number): number {
  if (xs.length === 0) return 0;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.max(0, Math.floor(p * (s.length - 1))))];
}

async function runCandidate(cand: Candidate, concepts: Concept[], queries: BenchQuery[]): Promise<Result> {
  console.log(`\n${cand.id} (${cand.dtype}) — loading...`);
  const { ex, dtype } = await loadExtractor(cand.id, cand.dtype);
  try {
    const passages = await embed(ex, concepts.map((c) => cand.passagePrefix + conceptText(c)));
    const rank = async (query: string): Promise<Hit[]> => {
      const [vec] = await embed(ex, [cand.queryPrefix + query]);
      return concepts
        .map((c, i) => ({ id: c.id, cosine: dot(vec, passages[i]) }))
        .sort((a, b) => b.cosine - a.cosine)
        .slice(0, TOP_K);
    };

    await rank(queries[0].q); // warm-up: the first inference pays for the session build

    const times: number[] = [];
    const thirdCosines: number[] = [];
    const failed: BenchQuery[] = [];
    for (const q of queries) {
      // one search = embed the query + score every proposition + sort (corpus vectors excluded)
      const t0 = performance.now();
      const hits = await rank(q.q);
      times.push(performance.now() - t0);
      if (hits.some((h) => h.id === q.expectId)) thirdCosines.push(hits[TOP_K - 1].cosine);
      else failed.push(q);
    }

    const sizeMB = dirSize(path.join(MODELS_DIR, cand.id)) / (1024 * 1024);
    return {
      cand,
      dtype,
      sizeMB,
      recall: (queries.length - failed.length) / queries.length,
      medianMs: median(times),
      failsIt: failed.filter((q) => q.lang === "it").length,
      failsEn: failed.filter((q) => q.lang === "en").length,
      failed,
      thirdCosines,
    };
  } finally {
    await ex.dispose().catch(() => {});
  }
}

interface Winner {
  result: Result;
  passed: boolean;
}

/** Same-size models happen (the two Xenova repos ship identical file sizes): recall decides. */
const sizeTenths = (r: Result) => Math.round(r.sizeMB * 10);

function pickWinner(results: Result[]): Winner {
  const passing = results.filter((r) => r.recall >= MIN_RECALL && r.medianMs < MAX_MEDIAN_MS);
  if (passing.length > 0) {
    const best = passing.reduce((a, b) =>
      sizeTenths(b) < sizeTenths(a) || (sizeTenths(b) === sizeTenths(a) && b.recall > a.recall) ? b : a
    );
    return { result: best, passed: true };
  }
  const best = results.reduce((a, b) =>
    b.recall > a.recall || (b.recall === a.recall && sizeTenths(b) < sizeTenths(a)) ? b : a
  );
  return { result: best, passed: false };
}

function table(rows: string[][]): string {
  return rows.map((r) => `| ${r.join(" | ")} |`).join("\n");
}

function printTable(results: Result[]): void {
  const rows = results.map((r) => [
    r.cand.id,
    r.dtype,
    r.sizeMB.toFixed(1),
    (r.recall * 100).toFixed(1),
    r.medianMs.toFixed(1),
    r.recall >= MIN_RECALL && r.medianMs < MAX_MEDIAN_MS ? "yes" : "no",
  ]);
  console.log("\n" + table([["model", "dtype", "size MB", "recall@3", "median ms", "passes"], ...rows]));
}

function report(results: Result[], winner: Winner, concepts: number, queries: BenchQuery[], minScore: number): string {
  const it = queries.filter((q) => q.lang === "it").length;
  const rows = results.map((r) => [
    r.cand.id,
    r.dtype,
    r.sizeMB.toFixed(1),
    (r.recall * 100).toFixed(1),
    r.medianMs.toFixed(1),
    r.recall >= MIN_RECALL && r.medianMs < MAX_MEDIAN_MS ? "yes" : "no",
  ]);
  const failures = results.map((r) => `- \`${r.cand.id}\` — it: ${r.failsIt}, en: ${r.failsEn}`).join("\n");
  return [
    "# Local embedding model benchmark",
    "",
    `Date: ${new Date().toISOString().slice(0, 10)}`,
    `Corpus: ${concepts} concepts from the learner's graph`,
    `Queries: ${queries.length} (${it} it, ${queries.length - it} en)`,
    "",
    table([
      ["model", "dtype", "size MB", "recall@3", "median ms", "passes"],
      ["---", "---", "---:", "---:", "---:", "---"],
      ...rows,
    ]),
    "",
    `Winner: ${winner.result.cand.id} (${winner.result.dtype})`,
    ...(winner.passed
      ? []
      : [`no candidate passed (no model reached recall@3 >= 90% with a median under 200 ms); the best recall wins, ties broken by size.`]),
    "",
    "Rule: the smallest model on disk with recall@3 >= 90% and a median search under 200 ms wins (equal sizes: higher recall); if none qualifies, the highest recall wins, ties broken by size.",
    "",
    "Failed queries per model:",
    failures,
    "",
    `minScore: ${minScore.toFixed(2)} (10th percentile of the 3rd-hit cosine on correct queries, rounded down to 0.05, floor 0.2)`,
    "",
  ].join("\n");
}

const USAGE = `Usage: npm run embed-bench [-- --no-write] [-- --queries <path>]

  Compares the embedding candidates in extensions/embed-bench.ts over the graph in the
  data folder and writes ${path.relative(CODE_ROOT, REPORT_FILE).split(path.sep).join("/")}.

  --no-write       print the table only, write nothing
  --queries <path> query file (default <dataRoot>/bench/embed-bench-queries.json)`;

export async function runCli(argv: string[]): Promise<number> {
  try {
    const { values } = parseArgs({
      args: argv,
      options: {
        queries: { type: "string" },
        "no-write": { type: "boolean" },
        help: { type: "boolean", short: "h" },
      },
    });
    if (values.help) {
      console.log(USAGE);
      return 0;
    }

    const { dataRoot } = resolveDataRoot();
    const queriesFile = values.queries
      ? path.resolve(dataRoot, values.queries)
      : path.join(dataRoot, "bench", "embed-bench-queries.json");
    const concepts = await readConcepts(dataRoot);
    if (concepts.length === 0) throw new Error(`no concepts in ${path.join(dataRoot, "graph", "nodes.jsonl")}`);
    const queries = readQueries(queriesFile, new Map(concepts.map((c) => [c.name, c.id])));
    const it = queries.filter((q) => q.lang === "it").length;
    console.log(`${concepts.length} concepts, ${queries.length} queries (${it} it, ${queries.length - it} en) from ${queriesFile}`);

    const results: Result[] = [];
    for (const cand of CANDIDATES) results.push(await runCandidate(cand, concepts, queries));

    printTable(results);
    const winner = pickWinner(results);
    console.log(`\nWinner: ${winner.result.cand.id} (${winner.result.dtype})`);
    if (!winner.passed) console.log("no candidate passed");

    for (const r of results) {
      if (r.failed.length === 0) continue;
      console.log(`\nMissed by ${r.cand.id}:`);
      for (const q of r.failed) console.log(`  [${q.lang}] ${q.q} -> ${q.expect}`);
    }

    const thirds = winner.result.thirdCosines;
    // 3rd-hit cosine on the queries the winner answers: the floor under its own top hits
    const p10 = percentile(thirds, 0.1);
    const minScore = Math.round(Math.max(0.2, Math.floor(p10 / 0.05) * 0.05) * 100) / 100;
    console.log(
      `\n3rd-hit cosine (${thirds.length} correct): min ${percentile(thirds, 0).toFixed(3)}, p10 ${p10.toFixed(3)}, median ${median(thirds).toFixed(3)}`
    );
    console.log(`minScore: ${minScore.toFixed(2)}`);

    if (values["no-write"]) {
      console.log("\n--no-write: report not written");
      return 0;
    }
    fs.mkdirSync(path.dirname(REPORT_FILE), { recursive: true });
    fs.writeFileSync(REPORT_FILE, report(results, winner, concepts.length, queries, minScore));
    console.log(`\nReport: ${REPORT_FILE}`);
    return 0;
  } catch (err: any) {
    console.error("Error: " + (err?.message ?? err));
    return 1;
  }
}

main(import.meta.url, runCli);
