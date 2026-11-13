// Captures golden HTTP/SSE outputs from the Node server into crates/proxi/tests/fixtures/golden (A12). Regenerate only from Node, before the cutover.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { REPO_ROOT, startServer } from "./harness.ts";

export interface GoldenRequest {
  method: "GET" | "POST";
  path: string;
  body?: unknown;
  rawBody?: string;
}

export interface GoldenCase {
  name: string;
  request: GoldenRequest;
  response: { status: number; body: unknown };
  firstEvent?: unknown;
}

export interface GoldenFile {
  route: string;
  seed: Record<string, unknown>;
  /** Raw file contents written verbatim under the data folder (lessons, JSONL ledgers): the replay's input, never normalised. */
  dataSeed?: Record<string, string>;
  cases: GoldenCase[];
}

export const GOLDEN_DIR = path.join(REPO_ROOT, "crates", "proxi", "tests", "fixtures", "golden");
export const ISO_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/;

interface Roots {
  repoRoot: string[];
  dataRoot: string[];
}

/** Every form the server may report for `dir`: the resolved path and, when it exists, the native realpath. */
export function rootVariants(dir: string): string[] {
  const out = [path.resolve(dir)];
  if (fs.existsSync(dir)) {
    const real = fs.realpathSync.native(dir);
    if (!out.includes(real)) out.push(real);
  }
  return out;
}

/** Deep copy with the volatile bits replaced, so the fixture does not depend on this machine or this run. */
export function normalise(value: unknown, roots: Roots): unknown {
  if (typeof value === "string") {
    if (roots.repoRoot.includes(value)) return "<repoRoot>";
    if (roots.dataRoot.includes(value)) return "<dataRoot>";
    return ISO_RE.test(value) ? "<iso>" : value;
  }
  if (Array.isArray(value)) return value.map((v) => normalise(v, roots));
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [key, v] of Object.entries(value)) out[key] = normalise(v, roots);
    return out;
  }
  return value;
}

interface ScriptedCase {
  name: string;
  request: GoldenRequest;
  /** Open GET /api/events first and record the first data event the request triggers. */
  awaitEvent?: boolean;
}

interface Script {
  route: string;
  seed: Record<string, unknown>;
  /** Raw file contents written verbatim under the data folder (lessons, JSONL ledgers): the replay's input, never normalised. */
  dataSeed?: Record<string, string>;
  cases: ScriptedCase[];
}

// Low entropy and no `sk` prefix: neither gitleaks nor the JSON fixtures may read it as a key (L12/L13).
const KEY = "x".repeat(16) + "abcd";

const GRAPH_AT = "2026-01-01T00:00:00.000Z";

/** One graph/nodes.jsonl line: a proposition with one provenance point. `extra` lands after `provenance`. */
function nodeLine(id: string, name: string, statement: string, chatId: string, messageId: string, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({ id, name, statement, aliases: [], createdAt: GRAPH_AT, provenance: [{ chatId, messageId }], ...extra });
}

/** One graph/chains.jsonl line: a lesson-level assertion (empty messageId). */
function chainLine(id: string, premises: string[], conclusion: string, chatId: string): string {
  return JSON.stringify({ id, premises, conclusion, createdAt: GRAPH_AT, provenance: [{ chatId, messageId: "" }] });
}

/** A lesson artifact: the HTML shell around its #lesson-data island. */
function lessonHtml(metadata: Record<string, unknown>, nodes: unknown[]): string {
  return `<html><body><script type="application/json" id="lesson-data">${JSON.stringify({ metadata, nodes })}</script></body></html>`;
}

const SCRIPTS: Script[] = [
  {
    route: "settings",
    seed: {},
    cases: [
      { name: "get-defaults", request: { method: "GET", path: "/api/settings" } },
      { name: "post-api-key", request: { method: "POST", path: "/api/settings", body: { anthropicApiKey: KEY } } },
      { name: "get-after-post", request: { method: "GET", path: "/api/settings" } },
      { name: "post-nested", request: { method: "POST", path: "/api/settings", body: { effort: { teacher: "high" }, agyModels: { teacher: "t" } } } },
      { name: "post-invalid-json", request: { method: "POST", path: "/api/settings", rawBody: "{" } },
      { name: "post-unknown-key", request: { method: "POST", path: "/api/settings", body: { nope: 1 } } },
    ],
  },
  {
    route: "limits",
    seed: { ".learner/limits.json": { fiveHour: { utilization: 0.42, resetsAt: 1700000000 }, at: "2026-11-12T10:00:00.000Z" } },
    cases: [
      { name: "get-seeded", request: { method: "GET", path: "/api/limits" } },
      { name: "post-provider-agy", request: { method: "POST", path: "/api/settings", body: { provider: "agy" } } },
      { name: "get-after-agy", request: { method: "GET", path: "/api/limits" } },
    ],
  },
  {
    route: "events",
    seed: {},
    cases: [
      { name: "limits-off", request: { method: "POST", path: "/api/settings", body: { provider: "opencode" } }, awaitEvent: true },
    ],
  },
  {
    route: "graph",
    seed: {},
    dataSeed: {
      "curriculum.json": JSON.stringify({ chapters: [{ title: "Corso Demo", slug: "demo", lessons: [] }] }),
      "courses/demo/lessons/0001-t.html": lessonHtml(
        { lessonNumber: 1, topic: "T", title: "Tema", slug: "t", dateCreated: "", dateModified: "", version: "1" },
        [
          { nodeId: "alpha", title: "Alfa", tier: "axiom", content: "x" },
          { nodeId: "beta", title: "Beta", tier: "derivation", content: "x", dependencies: ["alpha", "ghost"] },
          { nodeId: "gamma", title: "Gamma", tier: "derivation", content: "x", dependencies: ["beta"] },
        ],
      ),
      "courses/demo/lessons/0002-u.html": lessonHtml(
        { lessonNumber: 2, topic: "Topic U", title: "", slug: "u", dateCreated: "", dateModified: "", version: "1" },
        [{ nodeId: "delta", title: "Delta", tier: "axiom", content: "x" }],
      ),
      "courses/demo/lessons/notes.html": "<html><body>appunti</body></html>",
      "courses/demo/progress.jsonl": [
        JSON.stringify({ at: "2026-01-02T00:00:00Z", sessionId: "s", course: "demo", lessonSlug: "t", nodeRefs: ["alpha"], kind: "check", correct: true, confidence: 20 }),
        JSON.stringify({ at: "2026-01-02T00:00:00Z", sessionId: "s", course: "demo", lessonSlug: "t", nodeRefs: ["beta"], kind: "check", correct: false }),
        JSON.stringify({ at: "2026-01-02T00:00:00Z", sessionId: "s", course: "demo", lessonSlug: "u", nodeRefs: ["N1"], kind: "check", correct: true, confidence: 90 }),
        JSON.stringify({ kind: "review", source: "anki", sessionId: "anki", course: "demo", lessonSlug: "t", nodeRefs: ["demo/t#gamma"], cardId: 7, reviewId: 100, ease: 3, interval: 30, lapses: 0, at: "2026-01-03T00:00:00Z" }),
        "not json",
      ]
        .map((line) => line + "\n")
        .join(""),
      "graph/nodes.jsonl": [
        nodeLine("n_a", "A", "A vale.", "demo/0001-t", "alpha"),
        nodeLine("n_b", "B", "B vale.", "demo/0001-t", "beta"),
        nodeLine("n_c", "C", "C vale.", "demo/0001-t", "gamma"),
        nodeLine("n_g", "G", "G vale.", "demo/0001-t", "alpha", { note: "kept" }),
        nodeLine("n_d", "D", "D vale.", "demo/0002-u", "delta"),
        nodeLine("n_e", "E", "E vale.", "other/0001-x", ""),
        nodeLine("n_f", "F", "F vale.", "other/0001-x", ""),
        JSON.stringify({ id: "n_a", name: "A bis", statement: "A riformulato", aliases: ["A2"], createdAt: GRAPH_AT, provenance: [{ chatId: "demo/0002-u", messageId: "delta" }] }),
        "{not json",
        '{"id":"n_bad","name":"x"}',
      ]
        .map((line) => line + "\n")
        .join(""),
      "graph/chains.jsonl": [
        chainLine("arg_abc", ["n_a", "n_b"], "n_c", "demo/0001-t"),
        chainLine("arg_cd", ["n_c"], "n_d", "demo/0002-u"),
        chainLine("arg_dup", ["n_b", "n_a"], "n_c", "other/0001-x"),
        chainLine("arg_ef", ["n_e"], "n_f", "other/0001-x"),
        chainLine("arg_dangling", ["n_missing"], "n_a", "other/0001-x"),
        chainLine("arg_gad", ["n_g", "n_a"], "n_d", "demo/0002-u"),
        '{"id":"arg_cut","premises":["n_a"]', // unterminated: a crashed append, kept verbatim
      ].join("\n"),
      "graph/positions.json": JSON.stringify({ n_a: { x: 1, y: 1 } }),
    },
    cases: [
      { name: "get-map", request: { method: "GET", path: "/api/map" } },
      { name: "get-graph", request: { method: "GET", path: "/api/graph" } },
      {
        name: "post-positions",
        request: {
          method: "POST",
          path: "/api/graph/positions",
          body: { positions: { n_a: { x: 5, y: 6.5 }, arg_ef: { x: 1, y: 2 }, ghost: { x: 1, y: 2 }, n_c: { x: "7", y: 1 } } },
        },
      },
      { name: "get-graph-after-positions", request: { method: "GET", path: "/api/graph" } },
      { name: "post-positions-missing", request: { method: "POST", path: "/api/graph/positions", body: {} } },
      { name: "post-positions-invalid-json", request: { method: "POST", path: "/api/graph/positions", rawBody: "{" } },
      { name: "delete-node", request: { method: "POST", path: "/api/graph/nodes/n_b/delete", body: {} }, awaitEvent: true },
      { name: "get-graph-after-delete", request: { method: "GET", path: "/api/graph" } },
      { name: "delete-chain", request: { method: "POST", path: "/api/graph/nodes/arg_gad/delete", body: {} } },
      { name: "delete-invalid-id", request: { method: "POST", path: "/api/graph/nodes/x_1/delete", body: {} } },
      { name: "get-map-after", request: { method: "GET", path: "/api/map" } },
    ],
  },
];

/** Writes `<outDir>/<route>.json` for every scripted route and returns the file paths. Always Node mode. */
export async function captureGolden(outDir: string = GOLDEN_DIR): Promise<string[]> {
  fs.mkdirSync(outDir, { recursive: true });
  const written: string[] = [];
  for (const script of SCRIPTS) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), `golden-${script.route}-`));
    const repo = path.join(dir, "repo");
    const data = path.join(dir, "data");
    fs.mkdirSync(repo);
    fs.mkdirSync(data);
    for (const [rel, content] of Object.entries(script.seed)) {
      const file = path.join(repo, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, JSON.stringify(content, null, 2) + "\n");
    }
    for (const [rel, content] of Object.entries(script.dataSeed ?? {})) {
      const file = path.join(data, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, content);
    }
    const roots = { repoRoot: rootVariants(repo), dataRoot: rootVariants(data) };
    const server = await startServer({ repoRoot: repo, dataRoot: data, mode: "node" });
    try {
      const cases: GoldenCase[] = [];
      for (const c of script.cases) cases.push(await captureCase(server.url, c, roots));
      const target = path.join(outDir, `${script.route}.json`);
      // The seed is the replay's input, so it stays raw; only the captured responses are normalised.
      const file: GoldenFile = { route: script.route, seed: script.seed, ...(script.dataSeed ? { dataSeed: script.dataSeed } : {}), cases };
      fs.writeFileSync(target, JSON.stringify(file, null, 2) + "\n"); // always LF, never os.EOL
      written.push(target);
    } finally {
      await server.close();
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
    }
  }
  return written;
}

async function captureCase(base: string, c: ScriptedCase, roots: Roots): Promise<GoldenCase> {
  const events = c.awaitEvent ? openEvents(base) : undefined;
  try {
    if (events) await events.ready;
    const res = await send(base, c.request);
    const body = normalise(JSON.parse(await res.text()), roots);
    const out: GoldenCase = { name: c.name, request: c.request, response: { status: res.status, body } };
    // Node stamps the event with the current time: normalise it like any other field (app/server/main.ts:293).
    if (events) out.firstEvent = normalise(await events.first(5000), roots);
    return out;
  } finally {
    events?.close();
  }
}

function send(base: string, request: GoldenRequest): Promise<Response> {
  if (request.method === "GET") return fetch(base + request.path);
  return fetch(base + request.path, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: request.rawBody ?? JSON.stringify(request.body),
  });
}

/** SSE client on /api/events: `ready` resolves once the headers arrive, `first` waits for the first data event. */
function openEvents(base: string) {
  const events: unknown[] = [];
  const ac = new AbortController();
  let started!: () => void;
  let failed!: (error: unknown) => void;
  const ready = new Promise<void>((resolve, reject) => {
    started = resolve;
    failed = reject;
  });
  void (async () => {
    const res = await fetch(`${base}/api/events`, { signal: ac.signal });
    if (!res.ok) throw new Error(`GET /api/events: ${res.status}`);
    started();
    const dec = new TextDecoder();
    let buf = "";
    for await (const chunk of res.body as unknown as AsyncIterable<Uint8Array>) {
      buf += dec.decode(chunk, { stream: true });
      let i: number;
      while ((i = buf.indexOf("\n\n")) >= 0) {
        const block = buf.slice(0, i);
        buf = buf.slice(i + 2);
        const data = block.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join("");
        if (data) events.push(JSON.parse(data));
      }
    }
  })().catch(failed);
  return {
    ready,
    async first(ms: number): Promise<unknown> {
      const end = Date.now() + ms;
      for (;;) {
        if (events.length > 0) return events[0];
        if (Date.now() > end) throw new Error(`no event on /api/events within ${ms} ms`);
        await new Promise((r) => setTimeout(r, 25));
      }
    },
    close: () => ac.abort(),
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const at = process.argv.indexOf("--out");
  const outDir = at === -1 ? GOLDEN_DIR : path.resolve(process.argv[at + 1] ?? GOLDEN_DIR);
  captureGolden(outDir)
    .then((files) => {
      for (const file of files) console.log(file);
    })
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exit(1);
    });
}
