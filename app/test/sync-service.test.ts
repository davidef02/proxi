import { test } from "node:test";
import assert from "node:assert/strict";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { readDataRootPointer } from "../../extensions/data-root.ts";
import { readSettings, writeSettings } from "../server/settings.ts";
import { SyncService } from "../server/sync/service.ts";
import { writeSecrets, type Secrets } from "../server/sync/secrets.ts";
import type { RemoteStat, SyncProvider } from "../server/sync/types.ts";

const sha1 = (b: Buffer) => crypto.createHash("sha1").update(b).digest("hex");
/** Stand-in for the desktop app's safeStorage cipher. */
const reversingCipher = {
  available: async () => true,
  encrypt: async (plain: string) => Buffer.from([...plain].reverse().join("")).toString("base64"),
  decrypt: async (b64: string) => [...Buffer.from(b64, "base64").toString()].reverse().join(""),
};
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

/** In-memory remote. Writes and removals are staged until commit(), like the GitHub provider. */
function remote(files: Record<string, string> = {}) {
  const store = new Map<string, { data: Buffer; mtimeMs: number }>(
    Object.entries(files).map(([k, v]) => [k, { data: Buffer.from(v), mtimeMs: 1_700_000_000_000 }]),
  );
  const staged = new Map<string, { data: Buffer; mtimeMs: number } | null>();
  const calls = { list: 0, read: 0, write: 0, remove: 0, commit: 0 };
  const provider: SyncProvider = {
    key: "mem:test",
    async list() {
      calls.list++;
      staged.clear();
      const out = new Map<string, RemoteStat>();
      for (const [rel, f] of store) out.set(rel, { size: f.data.length, mtimeMs: f.mtimeMs, token: sha1(f.data) });
      return out;
    },
    async read(rel) {
      calls.read++;
      const f = store.get(rel);
      if (!f) throw new Error(`missing ${rel}`);
      return f.data;
    },
    async write(rel, data, mtimeMs) {
      calls.write++;
      staged.set(rel, { data, mtimeMs });
      return { size: data.length, mtimeMs, token: sha1(data) };
    },
    async remove(rel) {
      calls.remove++;
      staged.set(rel, null);
    },
    async commit() {
      calls.commit++;
      for (const [rel, s] of staged) {
        if (s) store.set(rel, s);
        else store.delete(rel);
      }
      staged.clear();
    },
  };
  return { store, calls, provider };
}

function setup(o: { files?: Record<string, string>; fetch?: typeof fetch; sleep?: (ms: number) => Promise<void> } = {}) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-svc-"));
  const repo = path.join(tmp, "repo");
  const data = path.join(tmp, "data");
  fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
  fs.mkdirSync(data, { recursive: true });
  const rem = remote(o.files);
  const c = { pulled: 0, made: 0, relaunched: 0 };
  const svc = new SyncService({
    repoRoot: repo,
    dataRoot: data,
    dataRootFromEnv: false,
    getPort: () => 4321,
    fetch: o.fetch,
    sleep: o.sleep,
    relaunch: () => void c.relaunched++,
    onStatus: () => {},
    onPulled: () => void c.pulled++,
    makeProvider: async () => (c.made++, rem.provider),
  });
  const cleanup = () => fs.rmSync(tmp, { recursive: true, force: true });
  return { tmp, repo, data, rem, svc, c, cleanup };
}

async function connectGithub(repo: string, secrets: Secrets = {}) {
  await writeSecrets(repo, { github: { token: "t", login: "octo", repo: "octo/proxi-data", branch: "main" }, ...secrets });
  writeSettings(repo, { syncProvider: "github" });
}

test("setDataRoot: relative and in-repo paths are refused; the same path needs no restart; another path restarts", async () => {
  const t = setup();
  try {
    await assert.rejects(t.svc.setDataRoot("relative/dir"), (e: any) => e.status === 400);
    await assert.rejects(t.svc.setDataRoot(path.join(t.repo, "inside")), (e: any) => e.status === 400);

    const same = await t.svc.setDataRoot(t.data);
    assert.equal(same.restartRequired, undefined);
    assert.equal(fs.existsSync(path.join(t.data, "courses")), true);
    assert.equal(readDataRootPointer(t.repo), undefined);

    const other = path.join(t.tmp, "other");
    const s = await t.svc.setDataRoot(other);
    assert.equal(s.restartRequired, true);
    assert.equal(readDataRootPointer(t.repo), path.resolve(other));
    await new Promise((r) => setTimeout(r, 600));
    assert.equal(t.c.relaunched, 1);
  } finally {
    t.cleanup();
  }
});

test("Google: googleStart builds a PKCE URL, a bad state is refused, a valid callback stores the token", async () => {
  const fake: typeof fetch = async (input) => {
    const url = String(input);
    if (url.startsWith("https://oauth2.googleapis.com/token")) return json({ access_token: "at", refresh_token: "rt", expires_in: 3600 });
    if (url.includes("/drive/v3/files") && url.includes("mimeType")) return json({ files: [{ id: "folder1" }] });
    if (url.includes("/drive/v3/about")) return json({ user: { emailAddress: "a@example.com" } });
    return new Response("unhandled", { status: 500 });
  };
  const t = setup({ fetch: fake });
  try {
    writeSettings(t.repo, { googleClientId: "test-client" });
    const { url } = await t.svc.googleStart();
    const u = new URL(url);
    assert.ok(u.searchParams.get("code_challenge"));
    assert.equal(u.searchParams.get("code_challenge_method"), "S256");
    assert.equal(u.searchParams.get("redirect_uri"), "http://127.0.0.1:4321/oauth/google/callback");
    const state = u.searchParams.get("state")!;
    assert.ok(state);

    const bad = await t.svc.googleCallback({ code: "x", state: "not-issued" });
    assert.equal(bad.status, 400);
    assert.match(bad.html, /scaduta/);
    assert.match(bad.html, /<title>Proxi<\/title>/);

    const ok = await t.svc.googleCallback({ code: "abc", state });
    assert.equal(ok.status, 200);
    assert.match(ok.html, /completato/);
    assert.match(ok.html, /<h1>Proxi<\/h1>/);
    assert.match(ok.html, /tornare a Proxi/);
    const again = await t.svc.googleCallback({ code: "abc", state });
    assert.equal(again.status, 400, "a state is one-time");

    const s = await t.svc.status();
    assert.equal(s.gdrive.connected, true);
    assert.equal(s.gdrive.email, "a@example.com");
    assert.equal(s.provider, "gdrive");
  } finally {
    t.cleanup();
  }
});

test("Google callback without the desktop cipher refuses to overwrite an encrypted secrets file", async () => {
  const fake: typeof fetch = async (input) => {
    const url = String(input);
    if (url.startsWith("https://oauth2.googleapis.com/token")) return json({ access_token: "at", refresh_token: "rt-new", expires_in: 3600 });
    if (url.includes("/drive/v3/files") && url.includes("mimeType")) return json({ files: [{ id: "folder1" }] });
    if (url.includes("/drive/v3/about")) return json({ user: { emailAddress: "a@example.com" } });
    return new Response("unhandled", { status: 500 });
  };
  const t = setup({ fetch: fake });
  try {
    // The desktop app encrypted the GitHub token. Plain Node has no cipher to read it back.
    await writeSecrets(t.repo, { github: { token: "gho_desktop", login: "octo", repo: "octo/proxi-data", branch: "main" } }, reversingCipher);
    const secretsFile = path.join(t.repo, ".learner", "secrets.json");
    const before = fs.readFileSync(secretsFile, "utf-8");

    writeSettings(t.repo, { googleClientId: "test-client" });
    const state = new URL((await t.svc.googleStart()).url).searchParams.get("state")!;
    const r = await t.svc.googleCallback({ code: "abc", state });
    assert.equal(r.status, 400);
    assert.match(r.html, /cifrati dall(&#39;|')app desktop/);
    assert.equal(fs.readFileSync(secretsFile, "utf-8"), before, "the encrypted GitHub token is untouched");
    const s = await t.svc.status();
    assert.match(s.gdrive.error ?? "", /cifrati dall'app desktop/);
    assert.equal(s.gdrive.connected, false);
    assert.equal(s.provider, "local");
    assert.equal(readSettings(t.repo).syncProvider, "local", "the provider does not switch to gdrive");
  } finally {
    t.cleanup();
  }
});

test("a conflict keeps both versions; the conflict list resolves it and the choice reaches the remote", async () => {
  const t = setup({ files: { "courses/c.md": "base" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    // Both sides edit the same file; the remote copy is newer, so it wins and the local bytes go to the conflicts.
    fs.writeFileSync(path.join(t.data, "courses", "c.md"), "local-edit");
    const past = new Date(Date.now() - 60_000);
    fs.utimesSync(path.join(t.data, "courses", "c.md"), past, past);
    t.rem.store.set("courses/c.md", { data: Buffer.from("remote-edit"), mtimeMs: Date.now() });
    const r = await t.svc.run();
    assert.equal(r.conflicts, 1);
    assert.equal(r.conflictCount, 1);
    assert.equal(fs.readFileSync(path.join(t.data, "courses", "c.md"), "utf8"), "remote-edit");

    const [c] = t.svc.conflicts();
    assert.equal(c.rel, "courses/c.md");
    assert.equal(c.side, "locale");
    assert.equal(fs.readFileSync(path.join(t.repo, ".learner", "drive-conflicts", c.id), "utf8"), "local-edit");

    assert.deepEqual(await t.svc.resolveConflict(c.id, "local"), { ok: true });
    assert.equal(fs.readFileSync(path.join(t.data, "courses", "c.md"), "utf8"), "local-edit");
    assert.equal(t.svc.conflicts().length, 0);
    assert.equal((await t.svc.status()).conflictCount, 0);

    const next = await t.svc.run();
    assert.equal(next.pushed, 1);
    assert.equal(t.rem.store.get("courses/c.md")?.data.toString(), "local-edit");
  } finally {
    t.cleanup();
  }
});

test("resolveConflict and restore map a bad id or a missing copy to a 400", async () => {
  const t = setup();
  try {
    await assert.rejects(t.svc.resolveConflict("../../x.locale-1", "local"), (e: any) => e.status === 400);
    await assert.rejects(t.svc.resolveConflict("courses/x.md.locale-1", "local"), (e: any) => e.status === 400);
    await assert.rejects(t.svc.restore("nope"), (e: any) => e.status === 400);
  } finally {
    t.cleanup();
  }
});

test("a remote deletion moves the local copy to the trash; restore brings it back and the next sync re-uploads it", async () => {
  const t = setup({ files: { "courses/a/x.md": "x", "courses/a/k1.md": "k", "courses/a/k2.md": "k", "courses/a/k3.md": "k" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    t.rem.store.delete("courses/a/x.md");
    const r = await t.svc.run();
    assert.equal(r.state, "idle");
    assert.equal(r.deleted, 1);
    assert.equal(r.trashCount, 1);
    assert.equal(fs.existsSync(path.join(t.data, "courses", "a", "x.md")), false);

    const [entry] = t.svc.trash();
    assert.equal(entry.rel, "courses/a/x.md");
    assert.deepEqual(await t.svc.restore(entry.id), { ok: true });
    assert.equal(fs.readFileSync(path.join(t.data, "courses", "a", "x.md"), "utf8"), "x");
    assert.equal((await t.svc.status()).trashCount, 0);

    const again = await t.svc.run();
    assert.equal(again.pushed, 1);
    assert.equal(t.rem.store.get("courses/a/x.md")?.data.toString(), "x");
  } finally {
    t.cleanup();
  }
});

test("disconnect gdrive revokes the refresh token, drops it and keeps a GitHub connection", async () => {
  const revoked: string[] = [];
  const fake: typeof fetch = async (input, init) => {
    const url = String(input);
    if (url.startsWith("https://oauth2.googleapis.com/revoke")) {
      revoked.push(url + " " + String(init?.body ?? ""));
      return new Response("", { status: 200 });
    }
    return new Response("unhandled", { status: 500 });
  };
  const t = setup({ fetch: fake });
  try {
    await connectGithub(t.repo, { gdrive: { refreshToken: "rt-1", folderId: "f", email: "a@example.com" } });
    writeSettings(t.repo, { syncProvider: "gdrive" });
    const s = await t.svc.disconnect("gdrive");
    assert.equal(revoked.length, 1);
    assert.match(revoked[0], /rt-1/);
    assert.equal(s.gdrive.connected, false);
    assert.equal(s.github.connected, true);
    assert.equal(s.provider, "local");
  } finally {
    t.cleanup();
  }
});

test("setDataRoot is refused with 409 when LEARNER_DATA_ROOT sets the folder", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-svc-env-"));
  try {
    const repo = path.join(tmp, "repo");
    fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
    const svc = new SyncService({ repoRoot: repo, dataRoot: path.join(tmp, "data"), dataRootFromEnv: true, getPort: () => 1, onStatus: () => {}, onPulled: () => {} });
    await assert.rejects(svc.setDataRoot(path.join(tmp, "other")), (e: any) => e.status === 409);
    assert.equal(readDataRootPointer(repo), undefined);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("resolveConflict and restore refresh the board", async () => {
  const t = setup({ files: { "courses/c.md": "base" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    fs.writeFileSync(path.join(t.data, "courses", "c.md"), "local-edit");
    const past = new Date(Date.now() - 60_000);
    fs.utimesSync(path.join(t.data, "courses", "c.md"), past, past);
    t.rem.store.set("courses/c.md", { data: Buffer.from("remote-edit"), mtimeMs: Date.now() });
    await t.svc.run();
    const [c] = t.svc.conflicts();
    const before = t.c.pulled;
    await t.svc.resolveConflict(c.id, "local");
    assert.equal(t.c.pulled, before + 1);
  } finally {
    t.cleanup();
  }
});

test("restoring a trashed file refreshes the board", async () => {
  const t = setup({ files: { "courses/a.md": "a", "courses/b.md": "b", "courses/c.md": "c", "courses/d.md": "d" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    t.rem.store.delete("courses/a.md");
    await t.svc.run();
    const [entry] = t.svc.trash();
    const before = t.c.pulled;
    await t.svc.restore(entry.id);
    assert.equal(t.c.pulled, before + 1);
    assert.equal(fs.readFileSync(path.join(t.data, "courses", "a.md"), "utf8"), "a");
  } finally {
    t.cleanup();
  }
});
