// /api/sync over plain HTTP: the GitHub and Google logins, conflicts, trash, the data folder and the committed
// goldens, identical against the Node server and the proxi binary (PROXI_TEST_SERVER).
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { DRIVE_SCOPE, GOOGLE_CLIENT_ID } from "../server/sync/gdrive.ts";
import { captureScript, GOLDEN_DIR, ISO_RE, SCRIPTS } from "./capture-golden.ts";
import { installFakeDrive, type FakeDrive, type FakeDriveSeed } from "./fake-drive.ts";
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
  gdrive: { connected: boolean; email?: string; pending?: boolean; error?: string };
  restartRequired?: boolean;
  conflictCount: number;
  trashCount: number;
}

interface Ctx {
  server: TestServer;
  repo: string;
  data: string;
  drive: FakeDrive;
}

interface SeedOptions {
  /** JSON files under the repo, written pretty like the server does. */
  repo?: Record<string, unknown>;
  /** Raw text under the data folder. */
  data?: Record<string, string>;
  /** Extra setup once both folders exist (old drive dirs and the like). */
  extra?: (repo: string, data: string) => void;
  github?: FakeGithubSeed;
  drive?: FakeDriveSeed;
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
  const drive = installFakeDrive(fakes.google, opts.drive ?? {});
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
    await fn({ server, repo, data, drive });
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

/** The loopback callback page of app/server/sync/service.ts, escaped the same way. */
function page(text: string): string {
  return (
    `<!doctype html><html lang="it"><head><meta charset="utf-8"><title>Proxi</title></head><body>` +
    `<h1>Proxi</h1><p>${text.replace(/[&<>"']/g, (ch) => `&#${ch.charCodeAt(0)};`)}</p></body></html>`
  );
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

describe("Google login", () => {
  it("PKCE login connects and keeps the refresh token (AC1)", async () => {
    await withServer({}, async ({ server, drive }) => {
      const started = await post(server, "/api/sync/google/start", {});
      assert.equal(started.status, 200);
      const { url } = (await started.json()) as { url: string };
      const u = new URL(url);
      assert.ok(u.pathname.endsWith("/o/oauth2/v2/auth"), `unexpected auth path: ${u.pathname}`);
      const redirect = `http://127.0.0.1:${server.port}/oauth/google/callback`;
      assert.equal(u.searchParams.get("client_id"), GOOGLE_CLIENT_ID);
      assert.equal(u.searchParams.get("redirect_uri"), redirect);
      assert.equal(u.searchParams.get("response_type"), "code");
      assert.equal(u.searchParams.get("scope"), DRIVE_SCOPE);
      const challenge = u.searchParams.get("code_challenge") ?? "";
      assert.equal(u.searchParams.get("code_challenge_method"), "S256");
      const state = u.searchParams.get("state") ?? "";
      assert.match(state, /^[0-9a-f]{32}$/);
      assert.equal(u.searchParams.get("access_type"), "offline");
      assert.equal(u.searchParams.get("prompt"), "consent");
      assert.equal((await status(server)).gdrive.pending, true);

      const callback = await fetch(`${server.url}/oauth/google/callback?code=abc&state=${state}`);
      assert.equal(callback.status, 200);
      assert.match(callback.headers.get("content-type") ?? "", /text\/html/);
      assert.equal(await callback.text(), page("Accesso a Google completato. Puoi chiudere questa scheda e tornare a Proxi."));

      const authForm = drive
        .mutations()
        .find((m) => m.path === "/token" && (m.body as { grant_type?: string }).grant_type === "authorization_code")?.body as
        | Record<string, string>
        | undefined;
      assert.ok(authForm, "the fake got no authorization_code token request");
      assert.equal(authForm.code, "abc");
      assert.equal(authForm.client_id, GOOGLE_CLIENT_ID);
      assert.equal(authForm.redirect_uri, redirect);
      assert.equal(createHash("sha256").update(authForm.code_verifier).digest("base64url"), challenge);

      const folder = drive.mutations().find((m) => m.method === "POST" && m.path === "/drive/v3/files");
      assert.deepEqual(folder?.body, { name: "proxi-data", mimeType: "application/vnd.google-apps.folder", appProperties: { proxi: "data" } });
      assert.equal(typeof drive.folderId(), "string");

      const connected = await waitFor(async () => {
        const body = await status(server);
        return body.gdrive.connected &&
          body.gdrive.email === "tester@example.com" &&
          body.provider === "gdrive" &&
          body.gdrive.pending === false &&
          body.state !== "syncing"
          ? body
          : undefined;
      });
      assert.equal(connected.provider, "gdrive");
      assert.equal(connected.gdrive.connected, true);
      assert.equal(connected.gdrive.email, "tester@example.com");
      assert.equal(connected.gdrive.pending, false);

      const again = await fetch(`${server.url}/oauth/google/callback?code=abc&state=${state}`);
      assert.equal(again.status, 400);
      assert.match(again.headers.get("content-type") ?? "", /text\/html/);
      assert.equal(await again.text(), page("Richiesta di accesso non valida o scaduta: riprova da Proxi."));

      const disconnected = (await (await post(server, "/api/sync/disconnect", { provider: "gdrive" })).json()) as SyncStatusBody;
      assert.equal(disconnected.gdrive.connected, false);
      assert.equal(disconnected.provider, "local");
      const revoke = drive.mutations().find((m) => m.method === "POST" && m.path === "/revoke");
      assert.deepEqual(revoke?.body, { token: "rt-1" });
    });
  });

  it("a denied Google login answers Node's page (AC2)", async () => {
    await withServer({}, async ({ server, drive }) => {
      const { url } = (await (await post(server, "/api/sync/google/start", {})).json()) as { url: string };
      const state = new URL(url).searchParams.get("state") ?? "";
      const denied = await fetch(`${server.url}/oauth/google/callback?error=access_denied&state=${state}`);
      assert.equal(denied.status, 400);
      assert.equal(await denied.text(), page("Accesso a Google annullato."));
      assert.equal(drive.mutations().some((m) => m.path === "/token"), false, "a denied login must not reach the token endpoint");
      assert.equal((await status(server)).gdrive.connected, false);
    });
  });

  it("an unknown state answers 400 html (AC2)", async () => {
    await withServer({}, async ({ server }) => {
      const r = await fetch(`${server.url}/oauth/google/callback?state=x`);
      assert.equal(r.status, 400);
      assert.match(r.headers.get("content-type") ?? "", /text\/html/);
      assert.equal(await r.text(), page("Richiesta di accesso non valida o scaduta: riprova da Proxi."));
    });
  });

  it("a revoked refresh token puts the reconnect message in the status (AC2)", async () => {
    await withServer(
      {
        repo: {
          ".learner/settings.json": { syncProvider: "gdrive" },
          ".learner/secrets.json": {
            v: 1,
            encrypted: false,
            data: JSON.stringify({ gdrive: { refreshToken: "rt-old", folderId: "folder-1", email: "tester@example.com" } }),
          },
        },
        drive: { folderId: "folder-1" },
      },
      async ({ server }) => {
        const r = await post(server, "/api/sync/run", {});
        assert.equal(r.status, 200);
        const s = (await r.json()) as SyncStatusBody;
        assert.equal(s.state, "error");
        assert.equal(s.error, "Accesso a Google scaduto o revocato: riconnetti.");
      },
    );
  });

  it("a full sync against the fake Drive sends the Node golden requests (AC3)", async () => {
    const script = SCRIPTS.find((s) => s.route === "sync-gdrive");
    assert.ok(script, "no sync-gdrive script in capture-golden.ts");
    const file = await captureScript(script, serverMode());
    const golden = JSON.parse(fs.readFileSync(path.join(GOLDEN_DIR, "sync-gdrive.json"), "utf-8")) as { cases: unknown };
    assert.deepEqual(file.cases, golden.cases);
  });
});

describe("conflicts and trash", () => {
  const at = (ms: number) => new Date(ms).toISOString();

  it("conflict listing, validation and resolution keep the chosen side (AC4)", async () => {
    await withServer({ data: EXISTING_DATA }, async ({ server, repo, data }) => {
      const conflicts = path.join(repo, ".learner", "drive-conflicts", "courses", "demo");
      fs.mkdirSync(conflicts, { recursive: true });
      fs.writeFileSync(path.join(data, "courses", "demo", "NOTES.md"), "local notes");
      fs.writeFileSync(path.join(conflicts, "NOTES.md.remoto-1700000000000"), "remote notes");

      const list = (await (await fetch(`${server.url}/api/sync/conflicts`)).json()) as { id: string; rel: string; side: string; at: string }[];
      assert.deepEqual(list, [
        { id: "courses/demo/NOTES.md.remoto-1700000000000", rel: "courses/demo/NOTES.md", side: "remoto", at: "2023-11-14T22:13:20.000Z" },
      ]);
      assert.equal((await status(server)).conflictCount, 1);

      const id = list[0].id;
      const noHeader = await fetch(`${server.url}/api/sync/conflicts/resolve`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ id, choice: "remote" }),
      });
      assert.equal(noHeader.status, 403);

      const badChoice = await post(server, "/api/sync/conflicts/resolve", { id, choice: "mine" });
      assert.equal(badChoice.status, 400);
      assert.deepEqual(await badChoice.json(), { error: 'choice: Invalid option: expected one of "local"|"remote"|"both"' });

      const traversal = await post(server, "/api/sync/conflicts/resolve", { id: "../../secrets.json.locale-1", choice: "local" });
      assert.equal(traversal.status, 400);
      assert.deepEqual(await traversal.json(), { error: "Conflitto non valido." });

      const bothInvalid = await post(server, "/api/sync/conflicts/resolve", { id: "", choice: "mine" });
      assert.equal(bothInvalid.status, 400);
      assert.deepEqual(await bothInvalid.json(), {
        error: 'id: Too small: expected string to have >=1 characters; choice: Invalid option: expected one of "local"|"remote"|"both"',
      });

      const resolved = await post(server, "/api/sync/conflicts/resolve", { id, choice: "remote" });
      assert.equal(resolved.status, 200);
      assert.deepEqual(await resolved.json(), { ok: true });
      assert.equal(fs.readFileSync(path.join(data, "courses", "demo", "NOTES.md"), "utf8"), "remote notes");
      assert.deepEqual(await (await fetch(`${server.url}/api/sync/conflicts`)).json(), []);
      assert.equal((await status(server)).conflictCount, 0);

      // A legacy `.drive-` copy is a remote one.
      fs.mkdirSync(conflicts, { recursive: true });
      fs.writeFileSync(path.join(conflicts, "legacy.md.drive-1600000000000"), "drive notes");
      const legacy = (await (await fetch(`${server.url}/api/sync/conflicts`)).json()) as { id: string; rel: string; side: string; at: string }[];
      assert.deepEqual(legacy, [{ id: "courses/demo/legacy.md.drive-1600000000000", rel: "courses/demo/legacy.md", side: "remoto", at: at(1600000000000) }]);

      const kept = await post(server, "/api/sync/conflicts/resolve", { id: legacy[0].id, choice: "both" });
      assert.equal(kept.status, 200);
      assert.deepEqual(await kept.json(), { ok: true });
      assert.equal(fs.readFileSync(path.join(data, "conflitti", legacy[0].id), "utf8"), "drive notes");
      assert.deepEqual(await (await fetch(`${server.url}/api/sync/conflicts`)).json(), []);
    });
  });

  it("trash listing, validation and restore move files back and refuse to overwrite (AC4)", async () => {
    const ms = String(Date.now() - 60_000);
    const deletedAt = at(Number(ms));
    await withServer({ data: EXISTING_DATA }, async ({ server, repo, data }) => {
      const trashed = path.join(repo, ".learner", "trash", ms, "courses", "demo");
      fs.mkdirSync(trashed, { recursive: true });
      fs.writeFileSync(path.join(trashed, "gone.md"), "gone");
      fs.writeFileSync(path.join(trashed, "MISSION.md"), "old mission");

      const list = (await (await fetch(`${server.url}/api/sync/trash`)).json()) as { id: string; rel: string; deletedAt: string; size: number }[];
      assert.equal(list.length, 2);
      const byRel = new Map(list.map((e) => [e.rel, e]));
      assert.deepEqual(byRel.get("courses/demo/gone.md"), { id: `${ms}/courses/demo/gone.md`, rel: "courses/demo/gone.md", deletedAt, size: 4 });
      assert.deepEqual(byRel.get("courses/demo/MISSION.md"), {
        id: `${ms}/courses/demo/MISSION.md`,
        rel: "courses/demo/MISSION.md",
        deletedAt,
        size: 11,
      });
      const s = await status(server);
      assert.equal(s.trashCount, 2);
      assert.equal(s.conflictCount, 0);

      const gone = byRel.get("courses/demo/gone.md")!;
      const noHeader = await fetch(`${server.url}/api/sync/trash/restore`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ id: gone.id }),
      });
      assert.equal(noHeader.status, 403);

      const missing = await post(server, "/api/sync/trash/restore", {});
      assert.equal(missing.status, 400);
      assert.deepEqual(await missing.json(), { error: "id: Invalid input: expected string, received undefined" });

      const restored = await post(server, "/api/sync/trash/restore", { id: gone.id });
      assert.equal(restored.status, 200);
      assert.deepEqual(await restored.json(), { ok: true });
      assert.equal(fs.readFileSync(path.join(data, "courses", "demo", "gone.md"), "utf8"), "gone");
      assert.equal((await status(server)).trashCount, 1);

      const clash = byRel.get("courses/demo/MISSION.md")!;
      const refused = await post(server, "/api/sync/trash/restore", { id: clash.id });
      assert.equal(refused.status, 400);
      assert.deepEqual(await refused.json(), { error: "Esiste già un file con questo nome: courses/demo/MISSION.md" });
      assert.equal(fs.readFileSync(path.join(data, "courses", "demo", "MISSION.md"), "utf8"), "# Mission\n");
    });
  });
});

describe("data folder", () => {
  it("data-root validates, writes the pointer and asks for a restart (AC5)", async () => {
    await withServer({ data: EXISTING_DATA }, async ({ server, repo, data }) => {
      const config = path.join(repo, ".learner", "config.json");

      const relative = await post(server, "/api/sync/data-root", { dataRoot: "relative/dir" });
      assert.equal(relative.status, 400);
      assert.deepEqual(await relative.json(), { error: "La cartella dei dati deve essere un percorso assoluto." });

      const inside = await post(server, "/api/sync/data-root", { dataRoot: path.join(repo, "inside") });
      assert.equal(inside.status, 400);
      assert.deepEqual(await inside.json(), { error: "La cartella dei dati non può stare dentro il codice di Proxi (né contenerlo)." });

      const missing = await post(server, "/api/sync/data-root", {});
      assert.equal(missing.status, 400);
      assert.deepEqual(await missing.json(), { error: "dataRoot: Invalid input: expected string, received undefined" });

      const tooLong = await post(server, "/api/sync/data-root", { dataRoot: "x".repeat(501) });
      assert.equal(tooLong.status, 400);
      assert.deepEqual(await tooLong.json(), { error: "dataRoot: Too big: expected string to have <=500 characters" });
      assert.equal(fs.existsSync(config), false, "invalid input must not write the pointer");

      const same = await post(server, "/api/sync/data-root", { dataRoot: data });
      assert.equal(same.status, 200);
      const sameBody = (await same.json()) as SyncStatusBody;
      assert.equal(sameBody.dataRoot, data);
      assert.equal(sameBody.restartRequired, undefined);
      assert.equal(fs.existsSync(config), false, "the current data folder needs no pointer");
      assert.equal(fs.statSync(path.join(data, "courses")).isDirectory(), true);

      const other = path.join(path.dirname(data), "other-data");
      const moved = await post(server, "/api/sync/data-root", { dataRoot: other });
      assert.equal(moved.status, 200);
      const movedBody = (await moved.json()) as SyncStatusBody;
      assert.equal(movedBody.dataRoot, data, "the running server keeps its data folder");
      assert.equal(movedBody.restartRequired, true);
      assert.equal(fs.statSync(path.join(other, "courses")).isDirectory(), true);
      assert.equal(fs.readFileSync(config, "utf8"), JSON.stringify({ dataRoot: path.resolve(other) }, null, 2) + "\n");
      assert.equal((await status(server)).restartRequired, true);
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
