// Semantic search over the knowledge graph: a local multilingual embedding model, no network at query
// time. transformers is imported lazily so importing this module costs nothing.
// All transformers `env` writes and every pipeline load go through withLoadLock: `env` is process-global
// and the startup download (remote) can race a query load (offline).
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";
import type { GraphNode } from "../../shared/graph/contracts.ts";

/** Shared by the install script and this module: app/server/graph/embedding-model.json. */
export interface EmbeddingConfig {
  model: string;
  dtype: string;
  queryPrefix: string;
  passagePrefix: string;
  minScore: number;
}

export const EMBEDDING: EmbeddingConfig = JSON.parse(
  fs.readFileSync(new URL("./embedding-model.json", import.meta.url), "utf8"),
);

type Extractor = import("@huggingface/transformers").FeatureExtractionPipeline;
type DataType = import("@huggingface/transformers").DataType;

export interface RankedHit {
  id: string;
  cosine: number;
}

export type RankResult = { ok: true; hits: RankedHit[] } | { ok: false; reason: string };

type NodeText = Pick<GraphNode, "id" | "name" | "statement" | "aliases">;

export function modelsDir(repoRoot: string): string {
  return path.join(repoRoot, ".learner", "models");
}

/** Same check as scripts/fetch-embedding-model.mjs present(): config.json plus at least one onnx weight. */
export function modelPresent(repoRoot: string, cfg = EMBEDDING): boolean {
  try {
    const modelDir = path.join(modelsDir(repoRoot), cfg.model);
    const onnxDir = path.join(modelDir, "onnx");
    return (
      fs.existsSync(path.join(modelDir, "config.json")) &&
      fs.existsSync(onnxDir) &&
      fs.readdirSync(onnxDir).some((f) => f.endsWith(".onnx"))
    );
  } catch {
    return false;
  }
}

/** The text a concept is embedded as. A6: change the recipe only here. */
export function conceptText(n: Pick<GraphNode, "name" | "statement" | "aliases">): string {
  return [n.name, n.statement, n.aliases.join(", ")].filter(Boolean).join(". ");
}

const sha1 = (s: string): string => createHash("sha1").update(s).digest("hex");

/** Vector cache file. Model, dtype and a hash of both prefixes in the name, so a config change never reuses stale vectors. */
export function cacheFile(repoRoot: string, cfg: EmbeddingConfig): string {
  const prefixes = sha1(`${cfg.queryPrefix}\u0000${cfg.passagePrefix}`).slice(0, 8);
  const model = cfg.model.replace(/\//g, "__");
  return path.join(repoRoot, ".learner", "embeddings", `${model}__${cfg.dtype}__${prefixes}.json`);
}

/** Cache file -> sha1(passage text) -> vector. */
export function readCache(file: string, cfg: EmbeddingConfig): Record<string, number[]> {
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8")) as Partial<
      Record<keyof EmbeddingConfig, unknown> & { vectors?: Record<string, unknown> }
    >;
    if (
      parsed.model !== cfg.model ||
      parsed.dtype !== cfg.dtype ||
      parsed.queryPrefix !== cfg.queryPrefix ||
      parsed.passagePrefix !== cfg.passagePrefix
    ) {
      return {};
    }
    const out: Record<string, number[]> = {};
    for (const [k, v] of Object.entries(parsed.vectors ?? {})) if (Array.isArray(v)) out[k] = v as number[];
    return out;
  } catch {
    return {}; // missing or corrupt: start empty
  }
}

function writeCache(file: string, cfg: EmbeddingConfig, vectors: Record<string, number[]>): void {
  const body = JSON.stringify({
    model: cfg.model,
    dtype: cfg.dtype,
    queryPrefix: cfg.queryPrefix,
    passagePrefix: cfg.passagePrefix,
    vectors,
  });
  const tmp = `${file}.${process.pid}.tmp`;
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    try {
      fs.writeFileSync(tmp, body);
      fs.renameSync(tmp, file);
    } catch {
      // Windows: rename over a file another process holds open can fail: write in place instead.
      fs.rmSync(tmp, { force: true });
      fs.writeFileSync(file, body);
    }
  } catch {
    // best effort: the vectors stay in memory for this process
  }
}

let chain: Promise<unknown> = Promise.resolve();

/** One queue for every env write + pipeline load, so a download and a query load never see each other's env. */
export function withLoadLock<T>(fn: () => Promise<T>): Promise<T> {
  const run = chain.then(fn, fn);
  chain = run.catch(() => {});
  return run;
}

const extractors = new Map<string, Promise<Extractor>>();

function getExtractor(repoRoot: string, cfg: EmbeddingConfig): Promise<Extractor> {
  const key = `${modelsDir(repoRoot)}|${cfg.model}|${cfg.dtype}`;
  let p = extractors.get(key);
  if (!p) {
    p = withLoadLock(async () => {
      const { env, pipeline, LogLevel } = await import("@huggingface/transformers");
      const dir = modelsDir(repoRoot);
      env.cacheDir = dir;
      env.localModelPath = dir;
      env.allowRemoteModels = false;
      env.allowLocalModels = true;
      env.logLevel = LogLevel.ERROR;
      return await pipeline("feature-extraction", cfg.model, {
        dtype: cfg.dtype as DataType,
        local_files_only: true,
      });
    });
    p.catch(() => extractors.delete(key)); // a failed load is retried on the next call
    extractors.set(key, p);
  }
  return p;
}

function dot(a: number[], b: number[] | undefined): number {
  if (!b) return 0;
  let sum = 0;
  for (let i = 0; i < a.length && i < b.length; i++) sum += a[i] * b[i];
  return sum;
}

const vectorCaches = new Map<string, Record<string, number[]>>();

/**
 * Cosine of the query against every node, in input order. Filtering, sorting and the cap belong to the
 * caller. Resolves `ok: false` for a missing model or any load/inference error.
 */
export async function rankSemantic(
  repoRoot: string,
  query: string,
  nodes: NodeText[],
  opts: { cfg?: EmbeddingConfig; cacheFile?: string } = {},
): Promise<RankResult> {
  const cfg = opts.cfg ?? EMBEDDING;
  if (!modelPresent(repoRoot, cfg)) return { ok: false, reason: "model missing" };
  try {
    const extractor = await getExtractor(repoRoot, cfg);

    const file = opts.cacheFile ?? cacheFile(repoRoot, cfg);
    const cached = vectorCaches.get(file);
    const vectors = cached ?? readCache(file, cfg);
    if (!cached) vectorCaches.set(file, vectors);

    const keys = nodes.map((n) => sha1(cfg.passagePrefix + conceptText(n)));
    // one batch for the texts the cache does not have yet
    const missing = new Map<string, string>();
    for (let i = 0; i < nodes.length; i++) {
      if (!vectors[keys[i]]) missing.set(keys[i], cfg.passagePrefix + conceptText(nodes[i]));
    }
    if (missing.size > 0) {
      const out = await extractor([...missing.values()], { pooling: "mean", normalize: true });
      const rows = out.tolist() as number[][];
      let i = 0;
      for (const key of missing.keys()) vectors[key] = rows[i++];
      writeCache(file, cfg, vectors);
    }

    const q = await extractor([cfg.queryPrefix + query], { pooling: "mean", normalize: true });
    const qv = (q.tolist() as number[][])[0];
    return { ok: true, hits: nodes.map((n, i) => ({ id: n.id, cosine: dot(qv, vectors[keys[i]]) })) };
  } catch (err) {
    return { ok: false, reason: err instanceof Error ? err.message : String(err) };
  }
}

const downloads = new Map<string, Promise<boolean>>();

/** Download the model if it is missing. Single-flight, never rejects. */
export function ensureModel(repoRoot: string, cfg = EMBEDDING): Promise<boolean> {
  const key = `${modelsDir(repoRoot)}|${cfg.model}|${cfg.dtype}`;
  let p = downloads.get(key);
  if (!p) {
    p = download(repoRoot, cfg);
    downloads.set(key, p);
    void p.then((ok) => {
      if (!ok) downloads.delete(key); // a failed download may be retried later
    });
  }
  return p;
}

async function download(repoRoot: string, cfg: EmbeddingConfig): Promise<boolean> {
  if (modelPresent(repoRoot, cfg)) return true;
  try {
    await withLoadLock(async () => {
      const { env, pipeline, LogLevel } = await import("@huggingface/transformers");
      const dir = modelsDir(repoRoot);
      env.cacheDir = dir;
      env.localModelPath = dir;
      env.logLevel = LogLevel.ERROR;
      env.allowRemoteModels = true; // only for this load
      try {
        const extractor = await pipeline("feature-extraction", cfg.model, { dtype: cfg.dtype as DataType });
        await extractor.dispose?.();
      } finally {
        env.allowRemoteModels = false;
      }
    });
    return true;
  } catch {
    return false;
  }
}

/** Fill the vector cache in the background. An optimisation only: correctness never depends on it. */
export async function warmIndex(repoRoot: string, nodes: NodeText[]): Promise<void> {
  try {
    await rankSemantic(repoRoot, "", nodes);
  } catch {
    /* warming is optional */
  }
}
