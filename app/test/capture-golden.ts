// Captures golden HTTP/SSE outputs from the server into crates/proxi/tests/fixtures/golden (A12). The scripted
// helpers live in ./golden-scripts.ts; this file keeps the sessions capture and re-exports the shared names.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { captureHome } from "./capture-golden-home.ts";
import { fakeAgy, fakeClaude, fakeOpencode, REPO_ROOT, startServer } from "./harness.ts";
import {
  captureScript,
  GOLDEN_DIR,
  ISO_RE,
  normalise,
  rootVariants,
  SCRIPTS,
  send,
  SESSIONS_DATA_SEED,
  waitForStatus,
  type GoldenCase,
  type GoldenFile,
  type GoldenRequest,
  type Script,
  openEvents,
} from "./golden-scripts.ts";

export { captureScript, GOLDEN_DIR, ISO_RE, normalise, openEvents, rootVariants, SCRIPTS, SESSIONS_DATA_SEED, waitForStatus };
export type { GoldenCase, GoldenFile, GoldenRequest, Script };

interface Roots {
  repoRoot: string[];
  dataRoot: string[];
}

/**
 * Like `normalise`, plus the session ids a run invents. In every string, in this order: the captured
 * session id becomes `<session>`, every other UUID `<uuid>`, the fake engine's `msg_<pid>_` /
 * `toolu_fake_<pid>_` counters lose the pid, agy's `conv-<pid>` and opencode's `ses-<pid>` too, as do
 * agy's `a<pid>_<run>_<step>` step ids, then the whole-string root and ISO rules of `normalise`.
 * The Rust replay mirrors these rules.
 */
export function normaliseSession(value: unknown, roots: Roots, sessionId: string): unknown {
  if (typeof value === "string") {
    const replaced = value
      .split(sessionId).join("<session>")
      .replace(SESSION_UUID_RE, "<uuid>")
      .replace(/msg_\d+_/g, "msg_<pid>_")
      .replace(/toolu_fake_\d+_/g, "toolu_fake_<pid>_")
      .replace(/conv-\d+/g, "conv-<pid>")
      .replace(/ses-\d+/g, "ses-<pid>")
      .replace(/:a\d+_\d+_/g, ":a<pid>_<run>_");
    if (roots.repoRoot.includes(replaced)) return "<repoRoot>";
    if (roots.dataRoot.includes(replaced)) return "<dataRoot>";
    return ISO_RE.test(replaced) ? "<iso>" : replaced;
  }
  if (Array.isArray(value)) return value.map((v) => normaliseSession(v, roots, sessionId));
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [key, v] of Object.entries(value)) out[key] = normaliseSession(v, roots, sessionId);
    return out;
  }
  return value;
}

const SESSION_UUID_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g;

/** The one session the golden captures: teach, planned, no Anki question. */
const SESSIONS_CREATE = {
  kind: "teach",
  course: "demo",
  model: "haiku",
  ankiOptIn: false,
  target: { mode: "planned", title: "Prima lezione" },
} as const;

/**
 * Captures the session core (AC1/AC4): the opening turn, a message turn with its whole SSE stream,
 * get, list and the saved `state.json`. `engine` picks the provider the session is created with.
 * Seeds are raw; only the output is normalised.
 */
export async function captureSessions(
  engine: "claude" | "agy" | "opencode" = "claude",
): Promise<SessionGolden> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "golden-sessions-"));
  const repo = path.join(dir, "repo");
  const data = path.join(dir, "data");
  fs.mkdirSync(repo);
  fs.mkdirSync(data);
  // The provider is a setting the server reads at create: agy and opencode need it seeded.
  const seed: Record<string, unknown> = engine === "claude" ? {} : { ".learner/settings.json": { provider: engine } };
  for (const [rel, content] of Object.entries(seed)) {
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
  const server = await startServer({
    repoRoot: repo,
    dataRoot: data,
    claude: fakeClaude(),
    ...(engine === "agy" ? { agy: fakeAgy() } : {}),
    ...(engine === "opencode" ? { opencode: fakeOpencode() } : {}),
  });
  try {
    const created = await send(server.url, { method: "POST", path: "/api/sessions", body: SESSIONS_CREATE });
    const createBody = JSON.parse(await created.text()) as { id: string };
    if (!created.ok) throw new Error(`POST /api/sessions: ${created.status} ${JSON.stringify(createBody)}`);
    const sessionId = createBody.id;
    await waitForStatus(server.url, sessionId, "idle");

    const stream = openEvents(server.url, `/api/sessions/${sessionId}/events`);
    let message: GoldenCase;
    try {
      await stream.ready;
      await stream.first(5000);
      const snapshotEnd = stream.events.length;
      const sent = await send(server.url, {
        method: "POST",
        path: `/api/sessions/${sessionId}/message`,
        body: { text: "Ciao" },
      });
      const sentBody = JSON.parse(await sent.text());
      if (!sent.ok) throw new Error(`POST /api/sessions/${sessionId}/message: ${sent.status} ${JSON.stringify(sentBody)}`);
      await stream.until((events) => events.slice(snapshotEnd).some(isIdleStatus), 15_000);
      message = {
        name: "message",
        request: { method: "POST", path: "/api/sessions/<session>/message", body: { text: "Ciao" } },
        response: { status: sent.status, body: normaliseSession(sentBody, roots, sessionId) },
        events: normaliseSession(stream.events, roots, sessionId) as unknown[],
      };
    } finally {
      stream.close();
    }

    const got = await send(server.url, { method: "GET", path: `/api/sessions/${sessionId}` });
    const gotBody = JSON.parse(await got.text());
    const listed = await send(server.url, { method: "GET", path: "/api/sessions" });
    const listBody = JSON.parse(await listed.text());
    await server.close();
    const stateFile = JSON.parse(
      fs.readFileSync(path.join(repo, ".learner", "sessions", sessionId, "state.json"), "utf-8"),
    );
    return {
      route: engine === "claude" ? "sessions" : `sessions-${engine}`,
      seed,
      dataSeed: SESSIONS_DATA_SEED,
      cases: [
        {
          name: "create",
          request: { method: "POST", path: "/api/sessions", body: SESSIONS_CREATE },
          response: { status: created.status, body: normaliseSession(createBody, roots, sessionId) },
        },
        message,
        {
          name: "get",
          request: { method: "GET", path: "/api/sessions/<session>" },
          response: { status: got.status, body: normaliseSession(gotBody, roots, sessionId) },
        },
        {
          name: "list",
          request: { method: "GET", path: "/api/sessions" },
          response: { status: listed.status, body: normaliseSession(listBody, roots, sessionId) },
        },
      ],
      stateFile: normaliseSession(stateFile, roots, sessionId),
    };
  } finally {
    await server.close();
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  }
}

const isIdleStatus = (event: unknown): boolean => {
  const e = event as { type?: string; status?: string };
  return e.type === "status" && e.status === "idle";
};

export interface SessionGolden extends GoldenFile {
  stateFile: unknown;
}

/** Writes `<outDir>/<route>.json` for every scripted route and returns the file paths. Always Node mode. */
export async function captureGolden(outDir: string = GOLDEN_DIR): Promise<string[]> {
  fs.mkdirSync(outDir, { recursive: true });
  const written: string[] = [];
  for (const script of SCRIPTS) {
    const target = path.join(outDir, `${script.route}.json`);
    const file = await captureScript(script);
    fs.writeFileSync(target, JSON.stringify(file, null, 2) + "\n"); // always LF, never os.EOL
    written.push(target);
  }
  const sessions = path.join(outDir, "sessions.json");
  fs.writeFileSync(sessions, JSON.stringify(await captureSessions(), null, 2) + "\n");
  written.push(sessions);
  for (const engine of ["agy", "opencode"] as const) {
    const target = path.join(outDir, `sessions-${engine}.json`);
    fs.writeFileSync(target, JSON.stringify(await captureSessions(engine), null, 2) + "\n");
    written.push(target);
  }
  written.push(await captureHome(outDir));
  return written;
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
