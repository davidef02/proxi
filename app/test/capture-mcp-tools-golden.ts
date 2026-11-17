// Captures the ten tools ported from Node (#21) into crates/proxi/tests/fixtures/golden/mcp-tools.json (AC1-AC5):
// one server, a plan, a teach and a practice session, with the blocking cards answered through
// /api/sessions/:id/answer. Regenerate only from Node, before the cutover.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { GOLDEN_DIR, normaliseSession, rootVariants, SESSIONS_DATA_SEED, waitForStatus } from "./capture-golden.ts";
import { diff, mcpToken, post } from "./capture-mcp-golden.ts";
import { snapshot } from "./capture-lesson-golden.ts";
import { fakeClaude, REPO_ROOT, startServer } from "./harness.ts";

export const MCP_TOOLS_GOLDEN = "mcp-tools";

/** repo `.learner/settings.json`: no map job runs while the tools write. */
const SEED = { ".learner/settings.json": { autoMap: false } };

/** A course with an exercises package: write_exercise and run_exercise join the practice list. */
const EXERCISES_PACKAGE = '{\n  "name": "demo-exercises",\n  "private": true\n}\n';

/** The concept tools answer over the repo's fixture graph, read verbatim and written into the data root. */
function graphFixture(name: string): string {
  return fs.readFileSync(path.join(REPO_ROOT, "app", "test", "fixtures", "repo", "graph", name), "utf-8");
}

interface Answer {
  kind: "plan" | "curriculum";
  answer: { approved: boolean };
}

interface CaseDef {
  name: string;
  /** The JSON-RPC body, sent as is. */
  body: unknown;
  /** Blocking call: answer the card as soon as the session reports it pending. */
  answer?: Answer;
}

interface ToolCase {
  name: string;
  request: { method: "POST"; path: string; headers: Record<string, string>; body: unknown };
  answer?: Answer;
  response: { status: number; body: unknown };
  /** Data-folder changes of the call (deleted null, changed/added through normaliseFile); every tools/call case. */
  files?: Record<string, string | null>;
  /** The session after the call: lessonPath, plan, pendingRequestId, exercises and the tool items. */
  session?: unknown;
}

interface ToolSession {
  name: string;
  createBody: Record<string, unknown>;
  cases: ToolCase[];
}

interface ToolsGolden {
  route: "mcp-tools";
  seed: Record<string, unknown>;
  dataSeed: Record<string, string>;
  sessions: ToolSession[];
}

interface SessionDef {
  name: string;
  createBody: Record<string, unknown>;
  cases: CaseDef[];
}

/** The JSON-RPC ids run 1..n over the whole file: a single counter, in capture order. */
let nextId = 0;

const toolsList = (): CaseDef => ({ name: "tools-list", body: { jsonrpc: "2.0", id: ++nextId, method: "tools/list" } });

const call = (name: string, tool: string, args: unknown, answer?: Answer): CaseDef => ({
  name,
  body: { jsonrpc: "2.0", id: ++nextId, method: "tools/call", params: { name: tool, arguments: args } },
  ...(answer ? { answer } : {}),
});

/** Node's toolsFor order, one session per kind; the plan session runs first because it rewrites curriculum.json. */
const SESSIONS: SessionDef[] = [
  {
    name: "plan",
    createBody: { kind: "plan", course: "demo", model: "haiku", chapterTitle: "Demo" },
    cases: [
      toolsList(),
      call("propose-curriculum-bad-slug", "propose_curriculum", {
        chapter: "Demo",
        slug: "altro",
        destination: "x",
        lessons: [{ title: "A" }],
      }),
      call(
        "propose-curriculum",
        "propose_curriculum",
        {
          chapter: "Demo",
          slug: "demo",
          destination: "Arrivare in fondo.",
          mission: "# Mission nuova\n",
          lessons: [{ title: "Prima lezione", plannedNodes: 2 }, { title: "Seconda lezione" }],
        },
        { kind: "curriculum", answer: { approved: true } },
      ),
    ],
  },
  {
    name: "teach",
    createBody: { kind: "teach", course: "demo", model: "haiku", ankiOptIn: true, target: { mode: "planned", title: "Prima lezione" } },
    cases: [
      toolsList(),
      call("propose-plan-bad-dag", "propose_plan", { topic: "Demo", approach: "Prosa.", dag: 'graph TD\n  N1["x"]', goal: "capire" }),
      call(
        "propose-plan",
        "propose_plan",
        {
          topic: "Demo",
          approach: "Prosa.",
          dag: 'graph TD\n  N1["Nodo 1: la prima cosa"] --> N2["Nodo 2: la seconda"]',
          goal: "capire",
        },
        { kind: "plan", answer: { approved: true } },
      ),
    ],
  },
  {
    name: "practice",
    createBody: { kind: "practice", course: "demo", model: "haiku" },
    cases: [
      toolsList(),
      call("write-exercise", "write_exercise", {
        name: "01-demo",
        instructions: "Fai **questo**.",
        files: [
          { path: "Esercizio.vue", content: "<template/>\n" },
          { path: "Esercizio.test.js", content: "// t\n" },
        ],
      }),
      call("write-exercise-hints", "write_exercise", {
        name: "02-suggerimenti",
        instructions: "i",
        files: [{ path: "sub/x.ts", content: "1\n" }],
        hints: ["uno", "due"],
        final: true,
      }),
      call("write-exercise-bad", "write_exercise", { name: "demo" }),
      call("run-exercise-unknown", "run_exercise", { name: "03-assente" }),
      // NAME has no length cap: the directory is missing, so the error text itself overflows and gets truncated.
      call("run-exercise-truncated", "run_exercise", { name: "03-" + "a".repeat(4100) }),
      call("problem", "problem", { statement: "S", mode: "recall", nodeKeys: ["demo/x#n1"], modelSolution: "M" }),
      call("problem-again", "problem", { statement: "S", mode: "recall", nodeKeys: ["demo/x#n1"], modelSolution: "M" }),
      call("conclude", "conclude", { outcome: "solvedWithHints", verdict: "ok", rung: "R2", missClass: null, retest: false }),
      call("search-concepts", "search_concepts", { query: "passo" }),
      call("open-concept", "open_concept", { id: "n_fx_a" }),
      call("browse-concepts", "browse_concepts", {}),
    ],
  },
];

/** Polls the session until the card the blocking tool just pushed is pending; the answer needs its live requestId. */
async function pendingCard(base: string, id: string, ms = 10_000): Promise<string> {
  const end = Date.now() + ms;
  for (;;) {
    const res = await fetch(`${base}/api/sessions/${id}`);
    if (res.ok) {
      const state = (await res.json()) as { pendingRequestId?: string | null };
      if (state.pendingRequestId) return state.pendingRequestId;
    }
    if (Date.now() > end) throw new Error(`session ${id} shows no pending card within ${ms} ms`);
    await new Promise((r) => setTimeout(r, 50));
  }
}

/** Creates one session and waits for its opening turn: the tools are listed after it. */
async function create(base: string, body: Record<string, unknown>): Promise<string> {
  const res = await fetch(`${base}/api/sessions`, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const created = (await res.json()) as { id?: string };
  if (!res.ok || !created.id) throw new Error(`POST /api/sessions: ${res.status} ${JSON.stringify(created)}`);
  await waitForStatus(base, created.id, "idle");
  return created.id;
}

/** One blocking call: send it, answer the card, then read the reply. */
async function exchange(
  base: string,
  pathname: string,
  body: string,
  token: string,
  answer: Answer,
  sessionId: string,
): Promise<{ status: number; text: string }> {
  const pending = post(base, pathname, body, token);
  const requestId = await pendingCard(base, sessionId);
  const res = await fetch(`${base}/api/sessions/${sessionId}/answer`, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: JSON.stringify({ requestId, ...answer }),
  });
  if (!res.ok) throw new Error(`POST /api/sessions/:id/answer: ${res.status} ${await res.text()}`);
  return pending;
}

const ITEM_KINDS = new Set(["plan", "curriculum", "exercise", "problem", "notice"]);

/** Runs the script against a fresh temp repo + data folder and writes `<outDir>/mcp-tools.json`; returns its path. */
export async function captureMcpToolsGolden(outDir: string = GOLDEN_DIR): Promise<string> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-mcp-tools-"));
  const repo = path.join(dir, "repo");
  const data = path.join(dir, "data");
  fs.mkdirSync(repo);
  fs.mkdirSync(data);
  const dataSeed: Record<string, string> = {
    ...SESSIONS_DATA_SEED,
    "courses/demo/exercises/package.json": EXERCISES_PACKAGE,
    "graph/nodes.jsonl": graphFixture("nodes.jsonl"),
    "graph/chains.jsonl": graphFixture("chains.jsonl"),
  };
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
  const server = await startServer({ repoRoot: repo, dataRoot: data, claude: fakeClaude() });
  try {
    const sessions: ToolSession[] = [];
    let token: string | undefined;
    for (const def of SESSIONS) {
      const id = await create(server.url, def.createBody);
      token ??= mcpToken(repo, id);
      const cases: ToolCase[] = [];
      for (const c of def.cases) {
        const pathname = `/mcp/${id}`;
        const body = JSON.stringify(c.body);
        // Only a tools/call writes: the list cases touch no file and no session slice.
        const before = (c.body as { method?: string }).method === "tools/call" ? snapshot(data) : undefined;
        const sent = c.answer ? await exchange(server.url, pathname, body, token, c.answer, id) : await post(server.url, pathname, body, token);
        const reply: unknown = sent.text === "" ? "" : normaliseSession(JSON.parse(sent.text), roots, id);
        const out: ToolCase = {
          name: c.name,
          request: {
            method: "POST",
            path: "/mcp/<session>",
            headers: { "content-type": "application/json", "x-learner-token": "<token>" },
            body: c.body,
          },
          ...(c.answer ? { answer: c.answer } : {}),
          response: { status: sent.status, body: reply },
        };
        if (before) {
          // The 300 ms board debounce must be over before the after-snapshot.
          await new Promise((r) => setTimeout(r, 1000));
          out.files = diff(before, snapshot(data));
          const got = await fetch(`${server.url}/api/sessions/${id}`);
          const state = (await got.json()) as {
            lessonPath?: string;
            plan?: unknown;
            pendingRequestId?: string | null;
            exercises?: unknown[];
            items: Array<{ kind: string }>;
          };
          out.session = {
            lessonPath: normaliseSession(state.lessonPath ?? null, roots, id),
            plan: normaliseSession(state.plan ?? null, roots, id),
            pendingRequestId: normaliseSession(state.pendingRequestId ?? null, roots, id),
            exercises: normaliseSession(state.exercises ?? [], roots, id),
            items: (normaliseSession(state.items, roots, id) as Array<{ kind: string }>).filter((i) => ITEM_KINDS.has(i.kind)),
          };
        }
        cases.push(out);
      }
      sessions.push({ name: def.name, createBody: def.createBody, cases });
    }

    fs.mkdirSync(outDir, { recursive: true });
    const target = path.join(outDir, `${MCP_TOOLS_GOLDEN}.json`);
    const file: ToolsGolden = { route: MCP_TOOLS_GOLDEN, seed: SEED, dataSeed, sessions };
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
  captureMcpToolsGolden(outDir)
    .then((file) => console.log(file))
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exit(1);
    });
}
