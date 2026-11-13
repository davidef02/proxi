// /api/sync over plain HTTP, the GitHub device flow and the committed sync golden: identical against the Node server
// and the proxi binary (PROXI_TEST_SERVER).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { captureScript, GOLDEN_DIR, ISO_RE, SCRIPTS } from "./capture-golden.ts";
import { installFakeGithub, type FakeGithubSeed } from "./fake-github.ts";
import { startFakeServers, type FakeServers } from "./fake-servers.ts";
import { serverMode, startServer, type TestServer } from "./harness.ts";

const CURRICULUM = { chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }] };

const guard = { "x-learner-client": "1" };

interface SyncStatusBody {
  state: string;
  error?: string;
  provider: string;
  dataRoot: string;
  dataRootFromEnv: boolean;
  needsData: boolean;
  github: { connected: boolean; login?: string; repo?: string; pending?: { userCode: string; verificationUri: string; expiresAt: string }; error?: string };
  gdrive: { connected: boolean };
  conflictCount: number;
  trashCount: number;
}

interface Ctx {
  server: TestServer;
  repo: string;
  data: string;
}

interface SeedOptions {
  /** JSON files under the repo, written pretty like the server does. */
  repo?: Record<string, unknown>;
  /** Raw text under the data folder. */
  data?: Record<string, string>;
  /** Extra setup once both folders exist (old drive dirs and the like). */
  extra?: (repo: string, data: string) => void;
  github?: FakeGithubSeed;
}

let fakes: FakeServers;
const dirs: string[] = [];

before(async () => {
  fakes = await startFakeServers();
});

after(async () => {
  await fakes.close();
  for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
});

function writeJson(file: string, value: unknown): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value, null, 2) + "\n");
}

function writeText(file: string, text: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

/** A fresh repo + data folder and a server per test: seeds must not leak between cases. */
async function withServer(opts: SeedOptions, fn: (ctx: Ctx) => Promise<void>): Promise<void> {
  fakes.reset();
  installFakeGithub(fakes.github, opts.github ?? {});
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "api-sync-"));
  dirs.push(dir);
  const repo = path.join(dir, "repo");
  const data = path.join(dir, "data");
  fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
  fs.mkdirSync(data, { recursive: true });
  for (const [rel, value] of Object.entries(opts.repo ?? {})) writeJson(path.join(repo, rel), value);
  for (const [rel, text] of Object.entries(opts.data ?? {})) writeText(path.join(data, rel), text);
  opts.extra?.(repo, data);
  const server = await startServer({ repoRoot: repo, dataRoot: data, fakes: fakes.urls });
  try {
    await fn({ server, repo, data });
  } finally {
    await server.close();
  }
}

function post(server: TestServer, p: string, body: unknown): Promise<Response> {
  return fetch(`${server.url}${p}`, {
    method: "POST",
    headers: { ...guard, "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function status(server: TestServer): Promise<SyncStatusBody> {
  const r = await fetch(`${server.url}/api/sync`);
  assert.equal(r.status, 200);
  return (await r.json()) as SyncStatusBody;
}

/** Same poll as integration.test.ts, sync or async predicate. */
async function waitFor<T>(fn: () => T | undefined | false | Promise<T | undefined | false>, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Minimal SSE client: `ready` resolves when the headers arrive, `close` aborts the stream. */
function sse(url: string) {
  const events: { type: string; status?: SyncStatusBody }[] = [];
  const ac = new AbortController();
  const ready = fetch(url, { signal: ac.signal });
  void ready
    .then(async (res) => {
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
    })
    .catch(() => {});
  return { events, ready, close: () => ac.abort() };
}

const EXISTING_DATA = { "curriculum.json": JSON.stringify(CURRICULUM) + "\n", "courses/demo/MISSION.md": "# Mission\n" };

describe("GET/POST /api/sync", () => {
  it("GET /api/sync returns the status shape for a local provider on an existing data folder", async () => {
    await withServer({ data: EXISTING_DATA }, async ({ server, data }) => {
      const s = await status(server);
      assert.equal(s.provider, "local");
      assert.equal(s.dataRoot, data);
      assert.equal(s.needsData, false);
      assert.equal(s.state, "idle");
      assert.equal(s.dataRootFromEnv, false);
      assert.equal(s.conflictCount, 0);
      assert.equal(s.trashCount, 0);
      assert.equal(s.github.connected, false);
      assert.equal(s.gdrive.connected, false);
    });
  });

  it("POST /api/sync/run needs the x-learner-client header", async () => {
    await withServer({ data: EXISTING_DATA }, async ({ server }) => {
      const noHeader = await fetch(`${server.url}/api/sync/run`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
      assert.equal(noHeader.status, 403);
      const r = await post(server, "/api/sync/run", {});
      assert.equal(r.status, 200);
      assert.equal(((await r.json()) as SyncStatusBody).provider, "local");
    });
  });

  it("POST /api/sync/run accepts confirmDeletions and rejects a non-boolean", async () => {
    await withServer({ data: EXISTING_DATA }, async ({ server }) => {
      assert.equal((await post(server, "/api/sync/run", { confirmDeletions: true })).status, 200);
      assert.equal((await post(server, "/api/sync/run", { confirmDeletions: "yes" })).status, 400);
    });
  });

  it("POST /api/sync/disconnect validates its provider", async () => {
    await withServer({ data: EXISTING_DATA }, async ({ server }) => {
      assert.equal((await post(server, "/api/sync/disconnect", { provider: "dropbox" })).status, 400);
      const d = await post(server, "/api/sync/disconnect", { provider: "github" });
      assert.equal(d.status, 200);
      assert.equal(((await d.json()) as SyncStatusBody).provider, "local");
    });
  });

  it("a fresh install writes no pointer, leaves old settings untouched and reports needsData", async () => {
    let settingsBefore = "";
    await withServer(
      {
        extra: (repo) => {
          const old = path.join(path.dirname(repo), "old-drive");
          writeText(path.join(old, "courses", "demo", "MISSION.md"), "# Mission\n");
          settingsBefore = JSON.stringify({ driveSync: true, driveFolder: old, autoMap: false });
          writeText(path.join(repo, ".learner", "settings.json"), settingsBefore);
        },
      },
      async ({ server, repo, data }) => {
        assert.equal(fs.existsSync(path.join(repo, ".learner", "config.json")), false);
        assert.equal(fs.readFileSync(path.join(repo, ".learner", "settings.json"), "utf8"), settingsBefore);
        const s = await status(server);
        assert.equal(s.needsData, true);
        assert.equal(path.resolve(s.dataRoot), path.resolve(data));
      },
    );
  });
});

describe("GitHub device flow", () => {
  it("github/start shows the device code and sends the client id; cancel clears it (AC6)", async () => {
    await withServer({ repo: { ".learner/settings.json": { githubClientId: "gh-client" } } }, async ({ server }) => {
      const started = (await (await post(server, "/api/sync/github/start", {})).json()) as SyncStatusBody;
      const pending = started.github.pending;
      assert.ok(pending, "no pending device code");
      assert.match(pending.expiresAt, ISO_RE);
      assert.deepEqual(
        { ...pending, expiresAt: "<iso>" },
        { userCode: "WXYZ-1234", verificationUri: "https://github.com/login/device", expiresAt: "<iso>" },
      );

      const device = fakes.github.requests.find((r) => r.method === "POST" && r.path === "/login/device/code");
      assert.ok(device, "the fake got no device-code request");
      assert.deepEqual(JSON.parse(device.body), { client_id: "gh-client", scope: "repo" });

      const cancelled = (await (await post(server, "/api/sync/github/cancel", {})).json()) as SyncStatusBody;
      assert.equal(cancelled.github.pending, undefined);
      assert.equal(cancelled.github.error, undefined);
    });
  });

  it("github/start with a device-code error reports it in the status (AC6)", async () => {
    await withServer({ github: { deviceError: { error: "unauthorized_client", error_description: "bad client" } } }, async ({ server }) => {
      const r = await post(server, "/api/sync/github/start", {});
      assert.equal(r.status, 200);
      const s = (await r.json()) as SyncStatusBody;
      assert.equal(s.github.error, "bad client");
    });
  });

  it("a completed device login creates the private repo and connects (AC6, AC1)", async () => {
    await withServer({ github: { repo: "none", deviceToken: "t".repeat(12) } }, async ({ server, repo }) => {
      assert.equal((await post(server, "/api/sync/github/start", {})).status, 200);
      const s = await waitFor(async () => {
        const body = await status(server);
        return body.provider === "github" ? body : undefined;
      }, 10_000);
      assert.equal(s.github.connected, true);
      assert.equal(s.github.login, "tester");
      assert.equal(s.github.repo, "tester/proxi-data");
      const created = fakes.github.requests.find((r) => r.method === "POST" && r.path === "/user/repos");
      assert.ok(created, "the fake got no POST /user/repos");
      assert.equal((JSON.parse(created.body) as { private?: boolean }).private, true);
      if (server.mode === "binary") assert.equal(fs.existsSync(path.join(repo, ".learner", "secrets.json")), false);
    });
  });

  it("disconnect removes the GitHub secret (AC7, AC3)", async () => {
    await withServer(
      {
        repo: {
          ".learner/settings.json": { syncProvider: "local" },
          ".learner/secrets.json": {
            v: 1,
            encrypted: false,
            data: JSON.stringify({ github: { token: "t".repeat(12), login: "tester", repo: "tester/proxi-data", branch: "main" } }),
          },
        },
      },
      async ({ server, repo }) => {
        const connected = await status(server);
        assert.equal(connected.github.connected, true);
        assert.equal(connected.provider, "local");
        if (server.mode === "binary") assert.equal(fs.existsSync(path.join(repo, ".learner", "secrets.json")), false);

        const disconnected = (await (await post(server, "/api/sync/disconnect", { provider: "github" })).json()) as SyncStatusBody;
        assert.equal(disconnected.github.connected, false);
        assert.equal(disconnected.provider, "local");

        assert.equal((await post(server, "/api/settings", { syncProvider: "github" })).status, 200);
        const run = (await (await post(server, "/api/sync/run", {})).json()) as SyncStatusBody;
        assert.equal(run.state, "error");
        assert.equal(run.error, "Non connesso: premi Accedi.");
      },
    );
  });

  it("github/cancel emits a sync event", async () => {
    await withServer({}, async ({ server }) => {
      const stream = sse(`${server.url}/api/events`);
      try {
        assert.equal((await stream.ready).status, 200);
        assert.equal((await post(server, "/api/sync/github/cancel", {})).status, 200);
        const event = await waitFor(() => stream.events[0]);
        assert.equal(event.type, "sync");
        assert.equal(event.status?.provider, "local");
      } finally {
        stream.close();
      }
    });
  });
});

it("a full sync against the fake GitHub sends the Node golden commits (AC5)", async () => {
  const script = SCRIPTS.find((s) => s.route === "sync");
  assert.ok(script, "no sync script in capture-golden.ts");
  const file = await captureScript(script, serverMode());
  const golden = JSON.parse(fs.readFileSync(path.join(GOLDEN_DIR, "sync.json"), "utf-8")) as { cases: unknown };
  assert.deepEqual(file.cases, golden.cases);
});
