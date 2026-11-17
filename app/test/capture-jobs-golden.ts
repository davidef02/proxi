// Captures the background-job surface from the Node server into crates/proxi/tests/fixtures/golden/jobs.json.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import {
  GOLDEN_DIR,
  normaliseSession,
  openEvents,
  rootVariants,
  SESSIONS_DATA_SEED,
  waitForStatus,
} from "./capture-golden.ts";
import { fakeClaude, removeTree, startServer } from "./harness.ts";

export const JOBS_GOLDEN = "jobs";

const SEED = { ".learner/settings.json": { autoMap: false } };
const CREATE = {
  kind: "teach",
  course: "demo",
  model: "haiku",
  ankiOptIn: false,
  target: { mode: "planned", title: "Prima lezione" },
} as const;
const BRIEF =
  "Vue 3.5 rende reattivo un ref leggendo .value dentro un effect: la dipendenza viene registrata al momento della lettura. Fonte primaria: la documentazione ufficiale.";
const SUBMIT = {
  tool: "submit_research",
  input: {
    brief: BRIEF,
    sources: [{ title: "Vue docs", url: "https://vuejs.org/guide/essentials/reactivity-fundamentals.html" }],
  },
};
const b64 = (value: unknown): string => Buffer.from(JSON.stringify(value)).toString("base64");
const jobScript = (steps: unknown[]): string => "FAKE:job:" + b64(steps);
const researchMessage = (question: string): string => `Verifica questo. FAKE:tool:research:${b64({ question })}`;
const MESSAGE = researchMessage("Cos'è un ref in Vue 3.5? " + jobScript([SUBMIT]));

interface JobCase {
  name: string;
  request: { method: "GET" | "POST"; path: string; headers?: Record<string, string>; body?: unknown };
  response: { status: number; body: unknown };
}

interface JobsGolden {
  route: typeof JOBS_GOLDEN;
  seed: typeof SEED;
  dataSeed: typeof SESSIONS_DATA_SEED;
  createBody: typeof CREATE;
  message: { text: string };
  statuses: string[];
  activities: string[];
  eventsCarryResult: boolean;
  job: unknown;
  list: unknown;
  persisted: unknown;
  mcpUrl: string;
  cases: JobCase[];
}

async function waitFor<T>(fn: () => T | undefined | false | Promise<T | undefined | false>, ms = 15_000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const value = await fn();
    if (value) return value;
    if (Date.now() > end) throw new Error("job golden wait timed out");
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

async function captureJob(outDir: string): Promise<string> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-jobs-"));
  const repo = path.join(dir, "repo");
  const data = path.join(dir, "data");
  fs.mkdirSync(repo);
  fs.mkdirSync(data);
  for (const [rel, content] of Object.entries(SEED)) {
    const file = path.join(repo, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, JSON.stringify(content, null, 2) + "\n");
  }
  for (const [rel, text] of Object.entries(SESSIONS_DATA_SEED)) {
    const file = path.join(data, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  }

  const roots = { repoRoot: rootVariants(repo), dataRoot: rootVariants(data) };
  const server = await startServer({ repoRoot: repo, dataRoot: data, mode: "node", claude: fakeClaude() });
  const events = openEvents(server.url);
  try {
    await events.ready;
    const created = await fetch(`${server.url}/api/sessions`, {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "application/json" },
      body: JSON.stringify(CREATE),
    });
    const createBody = (await created.json()) as { id?: string };
    if (!created.ok || !createBody.id) throw new Error(`POST /api/sessions: ${created.status}`);
    const sessionId = createBody.id;
    await waitForStatus(server.url, sessionId, "idle");

    const sent = await fetch(`${server.url}/api/sessions/${sessionId}/message`, {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "application/json" },
      body: JSON.stringify({ text: MESSAGE }),
    });
    if (!sent.ok) throw new Error(`POST /api/sessions/${sessionId}/message: ${sent.status}`);

    const job = await waitFor(async () => {
      const response = await fetch(`${server.url}/api/jobs`);
      const list = (await response.json()) as Array<{ id: string }>;
      return list[0];
    });
    const done = await waitFor(async () => {
      const response = await fetch(`${server.url}/api/jobs/${job.id}`);
      const value = (await response.json()) as { status?: string; delivered?: boolean };
      return value.status === "done" && value.delivered ? value : false;
    });
    await waitForStatus(server.url, sessionId, "idle");

    const listResponse = await fetch(`${server.url}/api/jobs`);
    const list = await listResponse.json();
    const persisted = JSON.parse(fs.readFileSync(path.join(repo, ".learner", "jobs", `${job.id}.json`), "utf-8"));
    const mcp = JSON.parse(fs.readFileSync(path.join(repo, ".learner", "jobs", job.id, "mcp.json"), "utf-8"));
    const jobEvents = events.events.filter(
      (event) => (event as { type?: string }).type === "job" && (event as { job?: { id?: string } }).job?.id === job.id,
    ) as Array<{ job: { status: string; activity: string; result?: unknown } }>;
    const statuses = jobEvents
      .map((event) => event.job.status)
      .filter((status, index, array) => index === 0 || array[index - 1] !== status);
    const activities = [...new Set(jobEvents.map((event) => event.job.activity).filter(Boolean))].sort();
    const normalise = (value: unknown) => normaliseSession(value, roots, sessionId);

    const token = mcp.mcpServers.job.headers["x-learner-token"] as string;
    const request = async (method: "GET" | "POST", pathname: string, body?: unknown, headers?: Record<string, string>) => {
      const response = await fetch(server.url + pathname, {
        method,
        headers: {
          ...(method === "POST" ? { "content-type": "application/json", "x-learner-client": "1" } : {}),
          ...headers,
        },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      return { status: response.status, body: await response.json() };
    };
    const rpc = { jsonrpc: "2.0", id: 1, method: "tools/list" };
    const cases: JobCase[] = [
      { name: "unknown-job", request: { method: "GET", path: "/api/jobs/nope" }, response: await request("GET", "/api/jobs/nope") },
      {
        name: "unknown-cancel",
        request: { method: "POST", path: "/api/jobs/nope/cancel", headers: { "x-learner-client": "1" }, body: {} },
        response: await request("POST", "/api/jobs/nope/cancel", {}),
      },
      {
        name: "cancel-settled",
        request: { method: "POST", path: "/api/jobs/<job>/cancel", headers: { "x-learner-client": "1" }, body: {} },
        response: await request("POST", `/api/jobs/${job.id}/cancel`, {}),
      },
      {
        name: "mcp-unauthorized",
        request: { method: "POST", path: "/mcp/job/<job>", body: rpc },
        response: await request("POST", `/mcp/job/${job.id}`, rpc),
      },
      {
        name: "mcp-unknown",
        request: { method: "POST", path: "/mcp/job/nope", headers: { "x-learner-token": "<token>" }, body: rpc },
        response: await request("POST", "/mcp/job/nope", rpc, { "x-learner-token": token }),
      },
      {
        name: "settled-mcp",
        request: { method: "POST", path: "/mcp/job/<job>", headers: { "x-learner-token": "<token>" }, body: rpc },
        response: await request("POST", `/mcp/job/${job.id}`, rpc, { "x-learner-token": token }),
      },
    ];
    const file: JobsGolden = {
      route: JOBS_GOLDEN,
      seed: SEED,
      dataSeed: SESSIONS_DATA_SEED,
      createBody: CREATE,
      message: { text: MESSAGE },
      statuses,
      activities,
      eventsCarryResult: jobEvents.some((event) => event.job.result !== undefined),
      job: normalise(done),
      list: normalise(list),
      persisted: normalise(persisted),
      mcpUrl: normaliseSession(mcp.mcpServers.job.url.replace(`${server.port}`, "<port>"), roots, sessionId) as string,
      cases: cases.map((entry) => ({
        ...entry,
        request: {
          ...entry.request,
          path: entry.request.path.replace(job.id, "<job>"),
          headers: entry.request.headers && Object.fromEntries(
            Object.entries(entry.request.headers).map(([key, value]) => [key, value === token ? "<token>" : value]),
          ),
        },
        response: { status: entry.response.status, body: normalise(entry.response.body) },
      })),
    };
    fs.mkdirSync(outDir, { recursive: true });
    const target = path.join(outDir, `${JOBS_GOLDEN}.json`);
    fs.writeFileSync(target, JSON.stringify(file, null, 2) + "\n");
    return target;
  } finally {
    events.close();
    await server.close();
    await removeTree(dir);
  }
}

export async function captureJobsGolden(outDir: string = GOLDEN_DIR): Promise<string> {
  return captureJob(outDir);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const at = process.argv.indexOf("--out");
  const outDir = at === -1 ? GOLDEN_DIR : path.resolve(process.argv[at + 1] ?? GOLDEN_DIR);
  captureJobsGolden(outDir)
    .then((file) => console.log(file))
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exit(1);
    });
}
