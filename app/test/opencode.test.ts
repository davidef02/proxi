// The opencode engine through the session API over HTTP/SSE only (no manager internals), so it runs against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import type { ChatItem, SessionState } from "../shared/protocol.ts";
import { fakeClaude, fakeOpencode, startServer, type TestServer } from "./harness.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // closed port: never reach the real Anki

const SEED = {
  chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }],
};

function tempDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

/** A data folder with one course and one planned lesson. */
function seedData(): string {
  const data = tempDir("opencode-data-");
  fs.writeFileSync(path.join(data, "curriculum.json"), JSON.stringify(SEED));
  fs.mkdirSync(path.join(data, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(data, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(data, "courses", "demo", "NOTES.md"), "# Notes\n");
  return data;
}

async function waitFor<T>(fn: () => T | undefined | false | Promise<T | undefined | false>, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

describe("opencode session over HTTP", () => {
  let root: string;
  let data: string;
  let server: TestServer;
  let id: string;
  let conv: string;

  // claude is faked too: a pinning bug must never reach the real one.
  const start = () => startServer({ repoRoot: root, dataRoot: data, opencode: fakeOpencode(), claude: fakeClaude() });
  const post = (p: string, body: unknown = {}) =>
    fetch(server.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const session = async (): Promise<SessionState> => {
    const r = await fetch(`${server.url}/api/sessions/${id}`);
    assert.equal(r.status, 200);
    return (await r.json()) as SessionState;
  };
  const idle = () => waitFor(async () => (await session()).status === "idle");
  const texts = (s: SessionState) => s.items.filter((i): i is Extract<ChatItem, { kind: "teacher" }> => i.kind === "teacher").map((t) => t.text);
  const say = async (text: string) => assert.equal((await post(`/api/sessions/${id}/message`, { text })).status, 200);

  before(async () => {
    root = tempDir("opencode-repo-");
    data = seedData();
    fs.mkdirSync(path.join(root, ".learner"), { recursive: true });
    fs.writeFileSync(path.join(root, ".learner", "settings.json"), JSON.stringify({ provider: "opencode" }));
    server = await start();
  });
  after(async () => {
    await server.close();
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(data, { recursive: true, force: true, maxRetries: 3 });
  });

  it("opens the lesson on opencode: session token, agent file, opencode.json and state.json", async () => {
    const res = await post("/api/sessions", {
      kind: "teach",
      course: "demo",
      ankiOptIn: false,
      target: { mode: "planned", title: "Prima lezione" },
    });
    assert.equal(res.status, 200);
    id = ((await res.json()) as { id: string }).id;
    await idle();

    const first = texts(await session())[0];
    conv = /\((ses-\d+)\)/.exec(first)?.[1] ?? "";
    assert.ok(conv, "teacher answered with its session id: " + first);

    const scratch = path.join(root, ".learner", "sessions", id);
    assert.ok(fs.existsSync(path.join(scratch, ".opencode", "agents", "ia-learner.md")));
    const config = JSON.parse(fs.readFileSync(path.join(scratch, "opencode.json"), "utf-8"));
    assert.match(config.mcp.servers.app.url, /\/mcp\/[^/]+$/);

    const saved = await waitFor(() => {
      try {
        const raw = JSON.parse(fs.readFileSync(path.join(scratch, "state.json"), "utf-8"));
        return raw.state?.status === "idle" && raw.resume === conv ? raw : undefined;
      } catch {
        return undefined; // not written yet
      }
    }, 3000);
    assert.equal(saved.provider, "opencode");
  });

  it("answers the next message on the same session", async () => {
    await say("ancora qui");
    await waitFor(async () => {
      const s = await session();
      return s.status === "idle" && texts(s).filter((t) => t.includes(`(${conv})`)).length >= 2;
    });
  });
});
