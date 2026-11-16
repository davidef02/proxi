// Captures the MCP surface (initialize, ping, tools/list, tools/call, write_node, finish_lesson) from the Node server
// into crates/proxi/tests/fixtures/golden/mcp.json (A12, AC1/AC2/AC6). Regenerate only from Node, before the cutover.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { appendNode, initLesson } from "../../extensions/html-lesson.ts";
import { GOLDEN_DIR, normaliseSession, rootVariants, SESSIONS_DATA_SEED, waitForStatus } from "./capture-golden.ts";
import { normaliseFile, snapshot } from "./capture-lesson-golden.ts";
import { fakeClaude, startServer } from "./harness.ts";

export const MCP_GOLDEN = "mcp";

/** Every tool the Rust port serves, in Node's order: the whole list, nothing filtered (D11/D10, #21). */
export const IN_SCOPE = [
  "quiz",
  "ask",
  "propose_plan",
  "write_node",
  "research",
  "visual",
  "demo",
  "finish_lesson",
  "read_lesson",
  "search_concepts",
  "open_concept",
  "browse_concepts",
];

/** The one lesson the golden continues: an existing-mode teach session reads it back from disk. */
const LESSON_REL = "courses/demo/lessons/0001-prima-lezione.html";

const DAG = 'graph TD\n  N1["Nodo 1: la prima cosa"] --> N2["Nodo 2: la seconda"]';

const CREATE = {
  kind: "teach",
  course: "demo",
  model: "haiku",
  ankiOptIn: false,
  target: { mode: "existing", lessonPath: LESSON_REL },
} as const;

/** repo `.learner/settings.json`: no map job runs while the golden closes its lesson. */
const SEED = { ".learner/settings.json": { autoMap: false } };

const FINISH_ARGS = {
  recordTitle: "Prima lezione: acquisita",
  summary: "Sa la prima cosa.",
  evidence: "Ha risposto bene.",
  implications: "Riprendere dalla seconda.",
  notesAddendum: "Preferisce esempi concreti.",
};

interface GoldenRequest {
  method: "POST";
  path: string;
  headers?: Record<string, string>;
  body?: unknown;
  /** Sent verbatim instead of `body`: the bad-json case has no valid JSON to encode. */
  rawBody?: string;
}

interface McpCase {
  name: string;
  request: GoldenRequest;
  response: { status: number; body: unknown };
  /** Data-folder changes of this call (deleted null, changed/added through normaliseFile): writing cases only. */
  files?: Record<string, string | null>;
  /** The session after the call, {lessonPath, plan, pendingRequestId, items} of kind node/finish: writing cases only. */
  session?: unknown;
}

interface McpGolden {
  route: "mcp";
  seed: Record<string, unknown>;
  dataSeed: Record<string, string>;
  createBody: typeof CREATE;
  inScope: readonly string[];
  cases: McpCase[];
}

interface CaseDef {
  name: string;
  /** The JSON-RPC body, sent as is. */
  body: string;
  /** Sent verbatim, not parsed: `{` must reach the server as invalid JSON. */
  raw?: boolean;
  /** Default `/mcp/<session>`; `<session>` is replaced with the created id. */
  path?: string;
  /** `false`: send no token (the 401 case). */
  token?: boolean;
}

const rpc = (id: number, method: string, params?: unknown): string =>
  JSON.stringify({ jsonrpc: "2.0", id, method, ...(params === undefined ? {} : { params }) });
const note = (method: string): string => JSON.stringify({ jsonrpc: "2.0", method });

const CASES: CaseDef[] = [
  {
    name: "initialize",
    body: rpc(1, "initialize", { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "t", version: "1" } }),
  },
  { name: "initialize-default", body: rpc(2, "initialize") },
  { name: "ping", body: rpc(3, "ping") },
  { name: "notification", body: note("notifications/initialized") },
  { name: "unknown-method", body: rpc(4, "foo/bar") },
  { name: "unknown-notification", body: note("foo/bar") },
  { name: "invalid-request", body: JSON.stringify({ jsonrpc: "2.0", id: 5 }) },
  { name: "batch", body: `[${rpc(6, "ping")},${note("notifications/initialized")},${rpc(7, "foo/bar")}]` },
  { name: "empty-batch", body: "[]" },
  { name: "unknown-tool", body: rpc(9, "tools/call", { name: "nope", arguments: {} }) },
  { name: "invalid-args", body: rpc(10, "tools/call", { name: "ask", arguments: {} }) },
  { name: "tools-list", body: rpc(8, "tools/list") },
  { name: "unauthorized", body: rpc(1, "ping"), token: false },
  { name: "unknown-session", body: rpc(1, "ping"), path: "/mcp/00000000-0000-4000-8000-000000000000" },
  { name: "bad-json", body: "{", raw: true },
  {
    name: "write-node",
    body: rpc(11, "tools/call", {
      name: "write_node",
      arguments: {
        planNode: "N2",
        node: { nodeId: "seconda", title: "La seconda", tier: "derivation", summary: "Segue.", content: "<p>segue dalla prima</p>" },
      },
    }),
  },
  { name: "finish", body: rpc(12, "tools/call", { name: "finish_lesson", arguments: FINISH_ARGS }) },
  { name: "finish-again", body: rpc(13, "tools/call", { name: "finish_lesson", arguments: FINISH_ARGS }) },
];

const WRITES = new Set(["write-node", "finish", "finish-again"]);

/** The seed lesson, built with Node's own init/append and pinned to fixed stamps so every capture is byte-identical. */
async function seedLesson(): Promise<string> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-mcp-lesson-"));
  try {
    const lessonsDir = path.join(dir, "courses", "demo", "lessons");
    const init = await initLesson({
      topic: "Demo",
      title: "Prima lezione",
      dagMermaid: DAG,
      targetGoal: "capire",
      lessonsDir,
      baseDir: dir,
    });
    await appendNode({
      lessonPath: init.filePath,
      node: { nodeId: "prima-cosa", title: "La prima cosa", tier: "axiom", summary: "Una cosa vera.", content: "<p>vera</p>" },
      baseDir: dir,
    });
    if (path.basename(init.filePath) !== path.basename(LESSON_REL)) {
      throw new Error(`the seed lesson is ${path.basename(init.filePath)}, not ${path.basename(LESSON_REL)}`);
    }
    // Pinned text: the checkout's line endings and the two stamps are the only volatile bits, so every capture is byte-identical.
    return fs
      .readFileSync(init.filePath, "utf-8")
      .replace(/\r\n/g, "\n")
      .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z/g, "2026-01-01T00:00:00.000Z")
      .replace(/Generated: \d{4}-\d{2}-\d{2}/g, "Generated: 2026-01-01");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  }
}

/** Changed, added and deleted files between two snapshots, text normalised; the progress log never joins the golden. */
export function diff(before: Map<string, string>, after: Map<string, string>): Record<string, string | null> {
  const out: Record<string, string | null> = {};
  const names = [...new Set([...before.keys(), ...after.keys()])].sort();
  for (const name of names) {
    if (name === "courses/demo/progress.jsonl") continue;
    const old = before.get(name);
    const now = after.get(name);
    if (now === undefined) out[name] = null;
    else if (old !== now) out[name] = normaliseFile(now);
  }
  return out;
}

export async function post(base: string, pathname: string, body: string, token?: string): Promise<{ status: number; text: string }> {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers["x-learner-token"] = token;
  const res = await fetch(base + pathname, { method: "POST", headers, body });
  return { status: res.status, text: await res.text() };
}

/** Runs the script against a fresh temp repo + data folder and writes `<outDir>/mcp.json`; returns its path. */
export async function captureMcpGolden(outDir: string = GOLDEN_DIR): Promise<string> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-mcp-"));
  const repo = path.join(dir, "repo");
  const data = path.join(dir, "data");
  fs.mkdirSync(repo);
  fs.mkdirSync(data);
  const dataSeed = { ...SESSIONS_DATA_SEED, [LESSON_REL]: await seedLesson() };
  for (const [rel, content] of Object.entries(SEED)) {
    const file = path.join(repo, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, JSON.stringify(content, null, 2) + "\n");
  }
  for (const [rel, text] of Object.entries(dataSeed)) {
    const file = path.join(data, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  }
  const roots = { repoRoot: rootVariants(repo), dataRoot: rootVariants(data) };
  const server = await startServer({ repoRoot: repo, dataRoot: data, mode: "node", claude: fakeClaude() });
  try {
    const token = server.token;
    if (!token) throw new Error("the Node test server reports no MCP token");
    const created = await fetch(`${server.url}/api/sessions`, {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "application/json" },
      body: JSON.stringify(CREATE),
    });
    const createBody = (await created.json()) as { id?: string };
    if (!created.ok || !createBody.id) throw new Error(`POST /api/sessions: ${created.status} ${JSON.stringify(createBody)}`);
    const id = createBody.id;
    await waitForStatus(server.url, id, "idle");

    const cases: McpCase[] = [];
    for (const c of CASES) {
      const before = WRITES.has(c.name) ? snapshot(data) : undefined;
      const pathname = (c.path ?? "/mcp/<session>").replace("<session>", id);
      const sent = await post(server.url, pathname, c.body, c.token === false ? undefined : token);
      let body: unknown = sent.text === "" ? "" : normaliseSession(JSON.parse(sent.text), roots, id);
      if (c.name === "tools-list") {
        const r = body as { result: { tools: Array<{ name: string }> } };
        // Every Node tool is ported now: the whole list must be IN_SCOPE, in Node's order. Nothing may hide silently.
        const names = r.result.tools.map((t) => t.name);
        const unexpected = names.filter((name) => !IN_SCOPE.includes(name));
        if (unexpected.length) throw new Error(`tools-list: Node lists unported tools: ${unexpected.join(", ")}`);
        if (names.join(",") !== IN_SCOPE.join(",")) {
          throw new Error(`tools-list: Node order ${names.join(", ")} differs from IN_SCOPE ${IN_SCOPE.join(", ")}`);
        }
        body = { ...r, result: { ...r.result, tools: r.result.tools.filter((t) => IN_SCOPE.includes(t.name)) }, filteredTo: IN_SCOPE };
      } else if (c.name === "unknown-tool") {
        // The Node list names the out-of-scope tools: pin only the error and its prefix (D11).
        const text = String((body as { result: { content: Array<{ text: string }> } }).result.content[0]?.text);
        const prefix = 'Unknown tool "nope". Available:';
        if (!text.startsWith(prefix)) throw new Error(`unknown-tool: ${JSON.stringify(text)}`);
        body = { isError: true, textStartsWith: prefix };
      }
      const out: McpCase = {
        name: c.name,
        request: {
          method: "POST",
          path: c.path ?? "/mcp/<session>",
          headers: { "content-type": "application/json", ...(c.token === false ? {} : { "x-learner-token": "<token>" }) },
          ...(c.raw ? { rawBody: c.body } : { body: JSON.parse(c.body) }),
        },
        response: { status: sent.status, body },
      };
      if (before) {
        // The 300 ms board debounce must be over before the after-snapshot.
        await new Promise((r) => setTimeout(r, 1000));
        out.files = diff(before, snapshot(data));
        const got = await fetch(`${server.url}/api/sessions/${id}`);
        const state = (await got.json()) as { lessonPath?: string; plan?: unknown; pendingRequestId?: string | null; items: Array<{ kind: string }> };
        out.session = {
          lessonPath: normaliseSession(state.lessonPath ?? null, roots, id),
          plan: normaliseSession(state.plan ?? null, roots, id),
          pendingRequestId: normaliseSession(state.pendingRequestId ?? null, roots, id),
          items: (normaliseSession(state.items, roots, id) as Array<{ kind: string }>).filter(
            (i) => i.kind === "node" || i.kind === "finish",
          ),
        };
      }
      cases.push(out);
    }

    fs.mkdirSync(outDir, { recursive: true });
    const target = path.join(outDir, `${MCP_GOLDEN}.json`);
    const file: McpGolden = { route: MCP_GOLDEN, seed: SEED, dataSeed, createBody: CREATE, inScope: IN_SCOPE, cases };
    fs.writeFileSync(target, JSON.stringify(file, null, 2) + "\n"); // always LF, never os.EOL
    return target;
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const at = process.argv.indexOf("--out");
  const outDir = at === -1 ? GOLDEN_DIR : path.resolve(process.argv[at + 1] ?? GOLDEN_DIR);
  captureMcpGolden(outDir)
    .then((file) => console.log(file))
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exit(1);
    });
}
