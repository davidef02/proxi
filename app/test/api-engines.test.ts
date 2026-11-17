// The engines' model lists and their missing-executable errors over HTTP only, so it runs against the
// Node server and the proxi binary. Bodies are compared as text: the JSON key order is part of the contract.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { OPENCODE_MODELS_FALLBACK } from "../shared/protocol.ts";
import type { SessionState } from "../shared/protocol.ts";
import { startServer, type StartOptions, type TestServer } from "./harness.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // closed port: never reach the real Anki

const SEED = {
  chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }],
};

function tempDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

/** A data folder with one course and one planned lesson. */
function seedData(): string {
  const data = tempDir("api-engines-data-");
  fs.writeFileSync(path.join(data, "curriculum.json"), JSON.stringify(SEED));
  fs.mkdirSync(path.join(data, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(data, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(data, "courses", "demo", "NOTES.md"), "# Notes\n");
  return data;
}

/** The engine a new session pins, read from `.learner/settings.json`. */
function seedProvider(repo: string, provider: string): void {
  fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
  fs.writeFileSync(path.join(repo, ".learner", "settings.json"), JSON.stringify({ provider }));
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

const scratch: string[] = [];

/** The path of an executable that was never written: its name is part of the spawn error. */
function missingBin(prefix: string, file: string): string {
  const dir = tempDir(prefix);
  scratch.push(dir);
  return path.join(dir, file);
}

const MISSING_OPENCODE = process.platform === "win32" ? "missing-opencode.exe" : "missing-opencode";

describe("engines over HTTP", () => {
  let root: string;
  let data: string;
  let server: TestServer;
  let modelsScript: string;

  const A_LIST = 'if (process.argv.includes("models")) process.stdout.write("model-a\\tA\\nmodel-b\\tB\\n");\n';
  const Z_LIST = 'if (process.argv.includes("models")) process.stdout.write("model-z\\tZ\\n");\n';

  const createTeach = async (srv: TestServer): Promise<string> => {
    const res = await fetch(srv.url + "/api/sessions", {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "application/json" },
      body: JSON.stringify({
        kind: "teach",
        course: "demo",
        model: "haiku",
        ankiOptIn: false,
        target: { mode: "planned", title: "Prima lezione" },
      }),
    });
    assert.equal(res.status, 200);
    return ((await res.json()) as { id: string }).id;
  };

  /** lastError of a session whose opening turn never gets an answer. */
  const lastErrorOf = async (
    provider: string,
    bins: Pick<StartOptions, "agy" | "opencode" | "claude">,
  ): Promise<string> => {
    const repo = tempDir(`api-engines-${provider}-`);
    seedProvider(repo, provider);
    const srv = await startServer({ repoRoot: repo, dataRoot: data, ...bins });
    try {
      const id = await createTeach(srv);
      const failed = await waitFor(async () => {
        const state = (await (await fetch(`${srv.url}/api/sessions/${id}`)).json()) as SessionState;
        return state.status === "error" ? state : undefined;
      });
      return failed.lastError ?? "";
    } finally {
      await srv.close();
      fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
    }
  };

  before(async () => {
    root = tempDir("api-engines-repo-");
    data = seedData();
    modelsScript = path.join(root, "agy-models.mjs");
    fs.writeFileSync(modelsScript, A_LIST);
    server = await startServer({
      repoRoot: root,
      dataRoot: data,
      agy: { cmd: process.execPath, args: [modelsScript] },
      opencode: { cmd: missingBin("api-engines-bins-", MISSING_OPENCODE) },
      claude: { cmd: missingBin("api-engines-bins-", "missing-claude.exe") },
    });
  });
  after(async () => {
    await server.close();
    for (const dir of scratch) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(data, { recursive: true, force: true, maxRetries: 3 });
  });

  it("serves the agy model list once per server", async () => {
    const first = await (await fetch(server.url + "/api/agy/models")).text();
    assert.equal(first, JSON.stringify(["model-a", "model-b"]));

    // The list is cached: the rewritten script is never run.
    fs.writeFileSync(modelsScript, Z_LIST);
    assert.equal(await (await fetch(server.url + "/api/agy/models")).text(), first);
  });

  it("falls back to the static opencode list when the engine is missing", async () => {
    const res = await fetch(server.url + "/api/opencode/models");
    assert.equal(res.status, 200);
    assert.equal(await res.text(), JSON.stringify(OPENCODE_MODELS_FALLBACK));
  });

  it("an agy executable that does not exist fails like Node", async () => {
    const missing = missingBin("api-engines-agy-bin-", "missing-agy");
    assert.equal(
      await lastErrorOf("agy", { agy: { cmd: missing } }),
      `teacher process exited (null): spawn error: spawn ${missing} ENOENT`,
    );
  });

  it("an opencode executable that does not exist fails like Node", async () => {
    const missing = missingBin("api-engines-opencode-bin-", MISSING_OPENCODE);
    assert.equal(await lastErrorOf("opencode", { opencode: { cmd: missing } }), "Errore del docente (error)");
  });

  it("a claude executable that does not exist fails like Node", async () => {
    const missing = missingBin("api-engines-claude-bin-", "missing-claude.exe");
    assert.equal(
      await lastErrorOf("claude", { claude: { cmd: missing } }),
      `teacher process exited (null): spawn error: spawn ${missing} ENOENT`,
    );
  });
});
