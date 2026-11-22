// GET/POST /api/update over plain HTTP: checks, refusals, signature verification and relaunch.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import type { GlobalEvent } from "../shared/protocol.ts";
import { FakeHttpServer } from "./fake-servers.ts";
import { fakeClaude, removeTree, startServer, type TestServer } from "./harness.ts";

type UpdateEvent = Extract<GlobalEvent, { type: "update" }>;

const SESSIONS_RUNNING =
  "C'è una sessione in corso. Chiudila o conferma per interromperla prima di aggiornare.";

const TEST_PUBKEY =
  "untrusted comment: minisign public key: E7620F1842B4E81F\n" +
  "RWTnYg8YQrToH4qI4910CfGV/VLbLTy6XXLKZwm/HZQSG/N0iAG0D29c\n";

const VALID_PREHASHED_SIG =
  "untrusted comment: signature from test key\n" +
  "RUTnYg8YQrToH2LzS+bFTfbIDD6txLKaxrgn2mAIaVDOQq1BoVE70sX5JCM35s0vY2o3eyth/YQkukF0k1JJ8sHrYOjC5YyqawQ=\n" +
  "trusted comment: timestamp:1556193335\tfile:proxi-setup.exe\tversion:9.9.9\n" +
  "hbAUuY3cSPxMMMUjEFw83lY0CZpEG9VJQWnuWpv7OgR14+JGy8rcnowXFM4eamZ3jQDjTGS5CT03JaUu17NYDA==\n";

const MISMATCHED_VERSION_SIG =
  "untrusted comment: signature from test key\n" +
  "RUTnYg8YQrToH2LzS+bFTfbIDD6txLKaxrgn2mAIaVDOQq1BoVE70sX5JCM35s0vY2o3eyth/YQkukF0k1JJ8sHrYOjC5YyqawQ=\n" +
  "trusted comment: timestamp:1556193335\tfile:proxi-setup.exe\tversion:9.9.8\n" +
  "xEimMOKoa7fGN6hwUL0kUsuMRZ6GZao7YBGQR18qL7BagPvV9vZVvE2P3ecAJXQI/BgaGwoZ5xlBqeyEEVG5Cw==\n";

const VALID_SIG_B64 = Buffer.from(VALID_PREHASHED_SIG, "utf8").toString("base64");
const MISMATCHED_SIG_B64 = Buffer.from(MISMATCHED_VERSION_SIG, "utf8").toString("base64");

function makeLatestJson(baseUrl: string, version = "9.9.9", notes = "Note di release", sigB64 = VALID_SIG_B64) {
  return {
    version,
    notes,
    platforms: {
      "windows-x86_64": {
        url: `${baseUrl}/proxi-setup.exe`,
        signature: sigB64,
      },
    },
  };
}

function seedDemoCourse(data: string): void {
  fs.writeFileSync(
    path.join(data, "curriculum.json"),
    JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }] }),
  );
  fs.mkdirSync(path.join(data, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(data, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(data, "courses", "demo", "NOTES.md"), "# Notes\n");
}

async function withServer(
  setupFake: (fake: FakeHttpServer) => void | Promise<void>,
  fn: (ctx: { server: TestServer; repo: string; data: string; fake: FakeHttpServer; fakeUrl: string }) => Promise<void>,
  options: { seedData?: boolean } = {},
): Promise<void> {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "api-update-"));
  const repo = path.join(tmp, "repo");
  const data = path.join(tmp, "data");
  fs.mkdirSync(repo, { recursive: true });
  fs.mkdirSync(data, { recursive: true });
  if (options.seedData) {
    seedDemoCourse(data);
  }
  const keyFile = path.join(tmp, "update_key.pub");
  fs.writeFileSync(keyFile, TEST_PUBKEY);

  const fake = new FakeHttpServer("releases");
  let server: TestServer | undefined;
  try {
    const fakeUrl = await fake.start();
    await setupFake(fake);
    server = await startServer({
      repoRoot: repo,
      dataRoot: data,
      fakes: { releases: `${fakeUrl}/latest.json` },
      updatePubkeyFile: keyFile,
      claude: fakeClaude(),
    });
    await fn({ server, repo, data, fake, fakeUrl });
  } finally {
    await server?.close();
    await fake.close();
    await removeTree(tmp);
  }
}

function post(server: TestServer, p: string, body?: unknown): Promise<Response> {
  const data = body === undefined ? undefined : JSON.stringify(body);
  return fetch(`${server.url}${p}`, {
    method: "POST",
    headers: {
      "x-learner-client": "1",
      ...(data === undefined ? {} : { "content-type": "application/json" }),
    },
    body: data,
  });
}

/** Same poll as integration.test.ts. */
async function waitFor<T>(fn: () => T | undefined | false | Promise<T | undefined | false>, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Rejects when `p` has not settled within `ms`; clears its timer either way. */
async function within(p: Promise<void>, ms: number, what: string): Promise<void> {
  let timer: NodeJS.Timeout | undefined;
  try {
    await Promise.race([
      p,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${what} within ${ms} ms`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** Minimal SSE client: `ready` resolves when the headers arrive, `close` aborts the stream. */
function sse(url: string) {
  const events: GlobalEvent[] = [];
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

describe("/api/update", () => {
  it("GET /api/update starts idle", async () => {
    await withServer(
      () => {},
      async ({ server }) => {
        const res = await fetch(`${server.url}/api/update`);
        assert.equal(res.status, 200);
        assert.deepEqual(await res.json(), { state: "idle", activeSessions: 0 });
      },
    );
  });

  it("check: up to date", async () => {
    await withServer(
      (fake) => {
        fake.on("GET", "/latest.json", {
          body: makeLatestJson(fake.url, "0.1.0"),
        });
      },
      async ({ server }) => {
        const res = await post(server, "/api/update/check");
        assert.equal(res.status, 200);
        const body = (await res.json()) as Record<string, unknown>;
        assert.equal(body.state, "up_to_date");
        assert.equal(body.activeSessions, 0);
        assert.deepEqual(Object.keys(body), ["state", "checkedAt", "activeSessions"]);
        assert.ok(!Number.isNaN(Date.parse(body.checkedAt as string)));
      },
    );
  });

  it("check: available version and notes", async () => {
    await withServer(
      (fake) => {
        fake.on("GET", "/latest.json", {
          body: makeLatestJson(fake.url, "9.9.9", "Note di release"),
        });
      },
      async ({ server }) => {
        const res = await post(server, "/api/update/check");
        assert.equal(res.status, 200);
        const body = (await res.json()) as Record<string, unknown>;
        assert.equal(body.state, "available");
        assert.equal(body.version, "9.9.9");
        assert.equal(body.notes, "Note di release");
        assert.equal(body.activeSessions, 0);
        assert.deepEqual(Object.keys(body), ["state", "version", "notes", "checkedAt", "activeSessions"]);
        assert.ok(!Number.isNaN(Date.parse(body.checkedAt as string)));
      },
    );
  });

  it("check: an unreachable releases URL returns idle", async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "api-update-unreachable-"));
    const repo = path.join(tmp, "repo");
    const data = path.join(tmp, "data");
    fs.mkdirSync(repo, { recursive: true });
    fs.mkdirSync(data, { recursive: true });
    const keyFile = path.join(tmp, "update_key.pub");
    fs.writeFileSync(keyFile, TEST_PUBKEY);
    let server: TestServer | undefined;
    try {
      server = await startServer({
        repoRoot: repo,
        dataRoot: data,
        fakes: { releases: "http://127.0.0.1:1/latest.json" },
        updatePubkeyFile: keyFile,
      });
      const res = await post(server, "/api/update/check");
      assert.equal(res.status, 200);
      const body = (await res.json()) as Record<string, unknown>;
      assert.equal(body.state, "idle");
      assert.equal(body.activeSessions, 0);
    } finally {
      await server?.close();
      await removeTree(tmp);
    }
  });

  it("check: pending marker present reports available", async () => {
    await withServer(
      () => {},
      async ({ server, repo }) => {
        const targetDir = path.join(repo, "target", "release");
        fs.mkdirSync(targetDir, { recursive: true });
        fs.writeFileSync(
          path.join(targetDir, "proxi.update-pending"),
          JSON.stringify({ installer: path.join(targetDir, "proxi-setup-9.9.9.exe"), version: "9.9.9" }),
        );

        const res = await post(server, "/api/update/check");
        assert.equal(res.status, 200);
        const body = (await res.json()) as Record<string, unknown>;
        assert.equal(body.state, "available");
        assert.equal(body.version, "9.9.9");
      },
    );
  });

  it("apply: refuses a tampered signature", async () => {
    await withServer(
      (fake) => {
        fake.on("GET", "/latest.json", {
          body: makeLatestJson(fake.url, "9.9.9", "Note di release"),
        });
        fake.on("GET", "/proxi-setup.exe", {
          headers: { "content-type": "application/octet-stream" },
          body: "Test",
        });
      },
      async ({ server, repo }) => {
        const res = await post(server, "/api/update/apply", {});
        assert.equal(res.status, 500);
        const body = (await res.json()) as { error: string };
        assert.match(body.error, /Firma dell'installer non valida/);

        const markerPath = path.join(repo, "target", "release", "proxi.update-pending");
        assert.ok(!fs.existsSync(markerPath));
        const installerPath = path.join(repo, "target", "release", "proxi-setup-9.9.9.exe");
        assert.ok(!fs.existsSync(installerPath));

        const statusRes = await fetch(`${server.url}/api/update`);
        assert.equal(statusRes.status, 200);
        const statusBody = (await statusRes.json()) as Record<string, unknown>;
        assert.equal(statusBody.state, "error");
        assert.match(statusBody.error as string, /Firma dell'installer non valida/);
      },
    );
  });

  it("apply: refuses same or older version", async () => {
    await withServer(
      (fake) => {
        fake.on("GET", "/latest.json", {
          body: makeLatestJson(fake.url, "0.1.0", "Release 0.1.0"),
        });
        fake.on("GET", "/proxi-setup.exe", {
          headers: { "content-type": "application/octet-stream" },
          body: "test",
        });
      },
      async ({ server, repo }) => {
        const res = await post(server, "/api/update/apply", {});
        assert.equal(res.status, 500);
        const body = (await res.json()) as { error: string };
        assert.match(body.error, /non è più recente della versione corrente/);

        const markerPath = path.join(repo, "target", "release", "proxi.update-pending");
        assert.ok(!fs.existsSync(markerPath));
      },
    );
  });

  it("apply: refuses a signature with mismatched version", async () => {
    await withServer(
      (fake) => {
        fake.on("GET", "/latest.json", {
          body: makeLatestJson(fake.url, "9.9.9", "Release notes", MISMATCHED_SIG_B64),
        });
        fake.on("GET", "/proxi-setup.exe", {
          headers: { "content-type": "application/octet-stream" },
          body: "test",
        });
      },
      async ({ server, repo }) => {
        const res = await post(server, "/api/update/apply", {});
        assert.equal(res.status, 500);
        const body = (await res.json()) as { error: string };
        assert.match(body.error, /Firma dell'installer non valida/);

        const markerPath = path.join(repo, "target", "release", "proxi.update-pending");
        assert.ok(!fs.existsSync(markerPath));
      },
    );
  });

  it("apply: downloads, verifies, writes marker and relaunches", async () => {
    await withServer(
      (fake) => {
        fake.on("GET", "/latest.json", {
          body: makeLatestJson(fake.url, "9.9.9", "Note di release"),
        });
        fake.on("GET", "/proxi-setup.exe", {
          headers: { "content-type": "application/octet-stream" },
          body: "test",
        });
      },
      async ({ server, repo }) => {
        const stream = sse(`${server.url}/api/events`);
        try {
          assert.equal((await stream.ready).status, 200);

          const res = await post(server, "/api/update/apply", { force: true });
          assert.equal(res.status, 200);
          assert.deepEqual(await res.json(), { ok: true, relaunching: true });

          const states = await waitFor(() => {
            const seen = stream.events.filter((e): e is UpdateEvent => e.type === "update").map((e) => e.status.state);
            return seen.includes("relaunching") ? seen : undefined;
          });
          assert.ok(states.includes("updating"), `states: ${states.join(",")}`);
          assert.equal(states.at(-1), "relaunching");
        } finally {
          stream.close();
        }

        const markerPath = path.join(repo, "target", "release", "proxi.update-pending");
        assert.ok(fs.existsSync(markerPath), "marker file exists");
        const marker = JSON.parse(fs.readFileSync(markerPath, "utf8")) as { installer: string; version: string };
        assert.equal(marker.version, "9.9.9");
        assert.ok(fs.existsSync(marker.installer), "installer file exists");
        assert.equal(fs.readFileSync(marker.installer, "utf8"), "test");

        await within(server.exited!, 5000, "the binary exits");
      },
    );
  });

  it("apply: refuses a live session without force, then force succeeds", async () => {
    await withServer(
      (fake) => {
        fake.on("GET", "/latest.json", {
          body: makeLatestJson(fake.url, "9.9.9", "Note di release"),
        });
        fake.on("GET", "/proxi-setup.exe", {
          headers: { "content-type": "application/octet-stream" },
          body: "test",
        });
      },
      async ({ server }) => {
        const sessionRes = await post(server, "/api/sessions", {
          kind: "practice",
          course: "demo",
          model: "haiku",
        });
        assert.equal(sessionRes.status, 200);
        const { id } = (await sessionRes.json()) as { id: string };

        const sessionStatus = async () => {
          const res = await fetch(`${server.url}/api/sessions/${id}`);
          return ((await res.json()) as { status: string }).status;
        };
        await waitFor(async () => (await sessionStatus()) === "idle");

        const msgRes = await post(server, `/api/sessions/${id}/message`, { text: "FAKE:slow:500" });
        assert.equal(msgRes.status, 200);

        await waitFor(async () => (await sessionStatus()) === "running");

        const updateRes = await fetch(`${server.url}/api/update`);
        const updateBody = (await updateRes.json()) as { activeSessions: number };
        assert.equal(updateBody.activeSessions, 1);

        const conflictRes = await post(server, "/api/update/apply", {});
        assert.equal(conflictRes.status, 409);
        const conflictBody = (await conflictRes.json()) as { error: string; code: string };
        assert.equal(conflictBody.code, "sessions_running");
        assert.equal(conflictBody.error, SESSIONS_RUNNING);

        const forceRes = await post(server, "/api/update/apply", { force: true });
        assert.equal(forceRes.status, 200);
        assert.deepEqual(await forceRes.json(), { ok: true, relaunching: true });

        await within(server.exited!, 5000, "the binary exits");
      },
      { seedData: true },
    );
  });
});
