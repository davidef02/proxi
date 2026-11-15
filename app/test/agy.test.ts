// The agy engine through the session API over HTTP/SSE only (no manager internals), so it runs against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import type { ChatItem, SessionState } from "../shared/protocol.ts";
import { fakeAgy, fakeClaude, startServer, type TestServer } from "./harness.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // closed port: never reach the real Anki

const SEED = {
  chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }],
};

function tempDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

/** A data folder with one course and one planned lesson. */
function seedData(): string {
  const data = tempDir("agy-data-");
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

/** Windows: a killed binary leaves its agy child holding the folder for a moment; retry instead of failing the teardown. */
async function removeTree(dir: string): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 1 });
      return;
    } catch (error) {
      if (attempt === 40) throw error;
      await new Promise((r) => setTimeout(r, 250));
    }
  }
}

describe("agy session over HTTP", () => {
  let root: string;
  let data: string;
  let server: TestServer;
  let id: string;
  let conv: string;

  const start = () => startServer({ repoRoot: root, dataRoot: data, agy: fakeAgy(), claude: fakeClaude() });
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
    root = tempDir("agy-repo-");
    data = seedData();
    fs.mkdirSync(path.join(root, ".learner"), { recursive: true });
    fs.writeFileSync(path.join(root, ".learner", "settings.json"), JSON.stringify({ provider: "agy" }));
    server = await start();
  });
  after(async () => {
    await server.close();
    await removeTree(root);
    await removeTree(data);
  });

  it("opens the lesson on agy: conversation token, agent file, state.json and usage", async () => {
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
    conv = /\((conv-\d+)\)/.exec(first)?.[1] ?? "";
    assert.ok(conv, "teacher answered with its conversation id: " + first);

    const scratch = path.join(root, ".learner", "sessions", id);
    assert.ok(fs.existsSync(path.join(scratch, ".agents", "agents", "ia-learner.md")));

    const saved = await waitFor(() => {
      try {
        const raw = JSON.parse(fs.readFileSync(path.join(scratch, "state.json"), "utf-8"));
        return raw.state?.status === "idle" && raw.resume === conv ? raw : undefined;
      } catch {
        return undefined; // not written yet
      }
    }, 3000);
    assert.equal(saved.provider, "agy");

    assert.ok((await session()).usage.outputTokens > 0);
  });

  it("answers the next message on the same conversation", async () => {
    await say("ancora qui");
    await waitFor(async () => {
      const s = await session();
      return s.status === "idle" && texts(s).filter((t) => t.includes(`(${conv})`)).length >= 2;
    });
  });

  it("keeps its engine after a settings switch and a restart", async () => {
    // The harness stops the binary by killing it, so the debounced state.json save (300 ms) must land
    // before the restart: wait for the two answers and the conversation, as a normal close would leave them.
    const scratch = path.join(root, ".learner", "sessions", id);
    await waitFor(() => {
      try {
        const raw = JSON.parse(fs.readFileSync(path.join(scratch, "state.json"), "utf-8"));
        const teacher = (raw.state?.items ?? []).filter((item: { kind?: string }) => item.kind === "teacher");
        return raw.state?.status === "idle" && raw.resume === conv && teacher.length >= 2 ? raw : undefined;
      } catch {
        return undefined; // not written yet
      }
    }, 3000);
    assert.equal((await post("/api/settings", { provider: "claude" })).status, 200);
    await server.close();
    server = await start();

    await say("dopo il cambio");
    await waitFor(async () => {
      const s = await session();
      return s.status === "idle" && texts(s).filter((t) => t.includes(`(${conv})`)).length >= 3;
    });
  });
});
