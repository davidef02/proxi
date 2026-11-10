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
import { RemoteMovedError, type RemoteStat, type SyncProvider } from "../server/sync/types.ts";

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

async function waitFor(cond: () => Promise<boolean>, ms = 2000) {
  const end = Date.now() + ms;
  while (!(await cond())) {
    if (Date.now() > end) throw new Error("timeout waiting for condition");
    await new Promise((r) => setTimeout(r, 5));
  }
}

test("a fresh data folder needs data", async () => {
  const t = setup();
  try {
    const s = await t.svc.status();
    assert.equal(s.needsData, true);
    assert.equal(s.provider, "local");
  } finally {
    t.cleanup();
  }
});

test("first connect pulls the remote into an empty data folder; the next pass moves nothing", async () => {
  const t = setup({ files: { "courses/demo/MISSION.md": "# demo\n", "curriculum.json": "{}\n" } });
  try {
    await connectGithub(t.repo);
    const first = await t.svc.run();
    assert.equal(first.state, "idle");
    assert.equal(first.pulled, 2);
    assert.equal(fs.readFileSync(path.join(t.data, "courses", "demo", "MISSION.md"), "utf8"), "# demo\n");
    assert.equal(first.needsData, false);
    assert.equal(t.c.pulled, 1);

    const before = { ...t.rem.calls };
    const second = await t.svc.run();
    assert.equal(second.pulled, 0);
    assert.equal(second.pushed, 0);
    assert.equal(t.rem.calls.read, before.read);
    assert.equal(t.rem.calls.write, before.write);
    assert.equal(t.rem.calls.commit, before.commit);
    assert.equal(t.c.pulled, 1);
  } finally {
    t.cleanup();
  }
});

test("the local provider makes no provider calls", async () => {
  const t = setup({ files: { "curriculum.json": "{}\n" } });
  try {
    const s = await t.svc.run();
    assert.equal(s.provider, "local");
    assert.equal(s.state, "idle");
    assert.equal(t.c.made, 0);
    assert.deepEqual(t.rem.calls, { list: 0, read: 0, write: 0, remove: 0, commit: 0 });
  } finally {
    t.cleanup();
  }
});

test("a remote that is not connected reports it and touches nothing", async () => {
  const t = setup({ files: { "curriculum.json": "{}\n" } });
  try {
    writeSettings(t.repo, { syncProvider: "gdrive" });
    const s = await t.svc.run();
    assert.equal(s.state, "error");
    assert.equal(s.error, "Non connesso: premi Accedi.");
    assert.equal(t.c.made, 0);
  } finally {
    t.cleanup();
  }
});

test("deletions over 30% stop the pass until confirmed", async () => {
  const t = setup({ files: { "courses/a.md": "a", "courses/b.md": "b", "courses/c.md": "c", "courses/d.md": "d" } });
  try {
    await connectGithub(t.repo);
    assert.equal((await t.svc.run()).pulled, 4);
    fs.rmSync(path.join(t.data, "courses", "a.md"));
    fs.rmSync(path.join(t.data, "courses", "b.md"));

    const held = await t.svc.run();
    assert.equal(held.state, "confirm");
    assert.deepEqual(held.pendingDeletions, { count: 2, total: 4 });
    assert.equal(t.rem.store.size, 4);

    const done = await t.svc.run({ confirmDeletions: true });
    assert.equal(done.state, "idle");
    assert.equal(done.deleted, 2);
    assert.deepEqual([...t.rem.store.keys()].sort(), ["courses/c.md", "courses/d.md"]);
  } finally {
    t.cleanup();
  }
});

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

test("GitHub: githubStart polls the device flow, sets up the repo and switches the provider to github", async () => {
  let polls = 0;
  const fake: typeof fetch = async (input) => {
    const url = String(input);
    if (url === "https://github.com/login/device/code")
      return json({ device_code: "dc", user_code: "ABCD-1234", verification_uri: "https://github.com/login/device", expires_in: 900, interval: 1 });
    if (url === "https://github.com/login/oauth/access_token") return json(++polls < 2 ? { error: "authorization_pending" } : { access_token: "gho_x" });
    if (url === "https://api.github.com/user") return json({ login: "octo" });
    if (url === "https://api.github.com/repos/octo/proxi-data")
      return json({ private: true, full_name: "octo/proxi-data", default_branch: "main" });
    return new Response("unhandled", { status: 500 });
  };
  const t = setup({ fetch: fake, sleep: async () => {} });
  try {
    writeSettings(t.repo, { githubClientId: "gh-client" });
    const started = await t.svc.githubStart();
    assert.equal(started.github.pending?.userCode, "ABCD-1234");
    await waitFor(async () => (await t.svc.status()).provider === "github");
    const s = await t.svc.status();
    assert.equal(s.github.connected, true);
    assert.equal(s.github.login, "octo");
    assert.equal(s.github.repo, "octo/proxi-data");
    assert.equal(s.github.pending, undefined);
    assert.equal(polls, 2);
  } finally {
    t.cleanup();
  }
});

test("GitHub connect without the desktop cipher refuses to overwrite an encrypted secrets file", async () => {
  let polls = 0;
  const fake: typeof fetch = async (input) => {
    const url = String(input);
    if (url === "https://github.com/login/device/code")
      return json({ device_code: "dc", user_code: "ABCD-1234", verification_uri: "https://github.com/login/device", expires_in: 900, interval: 1 });
    if (url === "https://github.com/login/oauth/access_token") return json(++polls < 2 ? { error: "authorization_pending" } : { access_token: "gho_x" });
    if (url === "https://api.github.com/user") return json({ login: "octo" });
    if (url === "https://api.github.com/repos/octo/proxi-data")
      return json({ private: true, full_name: "octo/proxi-data", default_branch: "main" });
    return new Response("unhandled", { status: 500 });
  };
  const t = setup({ fetch: fake, sleep: async () => {} });
  try {
    // The desktop app encrypted the Google token. Plain Node has no cipher to read it back.
    const desktopCipher = {
      available: async () => true,
      encrypt: async (plain: string) => Buffer.from([...plain].reverse().join("")).toString("base64"),
      decrypt: async (b64: string) => [...Buffer.from(b64, "base64").toString()].reverse().join(""),
    };
    await writeSecrets(t.repo, { gdrive: { refreshToken: "rt", folderId: "f1", email: "a@b.c" } }, desktopCipher);
    const secretsFile = path.join(t.repo, ".learner", "secrets.json");
    const before = fs.readFileSync(secretsFile, "utf-8");
    assert.equal(JSON.parse(before).encrypted, true);

    writeSettings(t.repo, { githubClientId: "gh-client" });
    await t.svc.githubStart();
    await waitFor(async () => !!(await t.svc.status()).github.error);
    const s = await t.svc.status();
    assert.match(s.github.error ?? "", /cifrati dall'app desktop/);
    assert.equal(s.github.connected, false);
    assert.equal(fs.readFileSync(secretsFile, "utf-8"), before, "the encrypted Google token is untouched");
    assert.equal(JSON.parse(fs.readFileSync(secretsFile, "utf-8")).encrypted, true);
    assert.equal(readSettings(t.repo).syncProvider, "local", "the provider does not switch to github");
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

test("GitHub connect with a cipher that can no longer decrypt the file replaces it (nothing readable is lost)", async () => {
  // The safeStorage key changed (new OS profile, reset keyring) or the file is corrupt: no cipher anywhere can read
  // it, so refusing to overwrite would leave the learner unable to ever connect again (Disconnetti is hidden while
  // nothing is connected).
  let polls = 0;
  const fake: typeof fetch = async (input) => {
    const url = String(input);
    if (url === "https://github.com/login/device/code")
      return json({ device_code: "dc", user_code: "ABCD-1234", verification_uri: "https://github.com/login/device", expires_in: 900, interval: 1 });
    if (url === "https://github.com/login/oauth/access_token") return json(++polls < 2 ? { error: "authorization_pending" } : { access_token: "gho_x" });
    if (url === "https://api.github.com/user") return json({ login: "octo" });
    if (url === "https://api.github.com/repos/octo/proxi-data")
      return json({ private: true, full_name: "octo/proxi-data", default_branch: "main" });
    return new Response("unhandled", { status: 500 });
  };
  // The old file was encrypted with a key this process no longer has. The current key reads what it writes itself.
  const brokenKey = {
    available: async () => true,
    encrypt: async (plain: string) => `current:${plain}`,
    decrypt: async (b64: string) => {
      if (!b64.startsWith("current:")) throw new Error("Error while decrypting the ciphertext provided to safeStorage.decryptString.");
      return b64.slice("current:".length);
    },
  };
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-svc-"));
  const repo = path.join(tmp, "repo");
  const data = path.join(tmp, "data");
  fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
  fs.mkdirSync(data, { recursive: true });
  const svc = new SyncService({
    repoRoot: repo,
    dataRoot: data,
    dataRootFromEnv: false,
    getPort: () => 4321,
    fetch: fake,
    sleep: async () => {},
    cipher: brokenKey,
    relaunch: () => {},
    onStatus: () => {},
    onPulled: () => {},
    makeProvider: async () => remote().provider,
  });
  try {
    await writeSecrets(repo, { gdrive: { refreshToken: "rt", folderId: "f1" } }, reversingCipher);
    writeSettings(repo, { githubClientId: "gh-client" });
    await svc.githubStart();
    await waitFor(async () => {
      const s = await svc.status();
      return s.provider === "github" || !!s.github.error;
    });
    const s = await svc.status();
    assert.equal(s.github.error, undefined);
    assert.equal(s.provider, "github");
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("GitHub connect replaces a malformed plaintext secrets file", async () => {
  let polls = 0;
  const fake: typeof fetch = async (input) => {
    const url = String(input);
    if (url === "https://github.com/login/device/code")
      return json({ device_code: "dc", user_code: "ABCD-1234", verification_uri: "https://github.com/login/device", expires_in: 900, interval: 1 });
    if (url === "https://github.com/login/oauth/access_token") return json(++polls < 2 ? { error: "authorization_pending" } : { access_token: "gho_x" });
    if (url === "https://api.github.com/user") return json({ login: "octo" });
    if (url === "https://api.github.com/repos/octo/proxi-data")
      return json({ private: true, full_name: "octo/proxi-data", default_branch: "main" });
    return new Response("unhandled", { status: 500 });
  };
  const t = setup({ fetch: fake, sleep: async () => {} });
  try {
    // A write cut short (the Windows in-place fallback in writeSecrets) leaves a truncated file. Nothing in it is
    // readable, so a new connect must be able to replace it.
    fs.writeFileSync(path.join(t.repo, ".learner", "secrets.json"), '{"v":1,"encrypted":false,"data":"{\\"gdri');
    writeSettings(t.repo, { githubClientId: "gh-client" });
    await t.svc.githubStart();
    await waitFor(async () => {
      const s = await t.svc.status();
      return s.provider === "github" || !!s.github.error;
    });
    const s = await t.svc.status();
    assert.equal(s.github.error, undefined);
    assert.equal(s.github.connected, true);
  } finally {
    t.cleanup();
  }
});

test("a pass retried after the remote moved still refreshes the board for the files attempt 1 pulled", async () => {
  const t = setup({ files: { "courses/a.md": "a" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    const refreshed = t.c.pulled;
    // Attempt 1 pulls b.md and pushes c.md; its commit finds the ref moved. Attempt 2 sees b.md already on disk.
    t.rem.store.set("courses/b.md", { data: Buffer.from("b"), mtimeMs: Date.now() - 5_000 });
    fs.writeFileSync(path.join(t.data, "courses", "c.md"), "c");
    const commit = t.rem.provider.commit.bind(t.rem.provider);
    let moved = true;
    t.rem.provider.commit = async (message) => {
      if (moved) {
        moved = false;
        throw new RemoteMovedError("ref moved");
      }
      return commit(message);
    };
    const r = await t.svc.run();
    assert.equal(r.state, "idle");
    assert.equal(r.pulled, 1);
    assert.equal(r.pushed, 1);
    assert.equal(t.c.pulled, refreshed + 1, "onPulled refreshes the board and index.html");
    assert.equal(t.rem.store.get("courses/c.md")?.data.toString(), "c");
  } finally {
    t.cleanup();
  }
});

test("disconnect deletes the token and resets the provider to local", async () => {
  const t = setup();
  try {
    await connectGithub(t.repo);
    assert.equal((await t.svc.status()).github.connected, true);
    const s = await t.svc.disconnect("github");
    assert.equal(s.github.connected, false);
    assert.equal(s.provider, "local");
    assert.equal(fs.existsSync(path.join(t.repo, ".learner", "secrets.json")), false);
  } finally {
    t.cleanup();
  }
});

test("the status view reports the last run and the conflict and trash counts", async () => {
  const t = setup({ files: { "curriculum.json": "{}\n" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    const s = await t.svc.status();
    assert.equal(s.state, "idle");
    assert.equal(s.pulled, 1);
    assert.equal(s.conflictCount, 0);
    assert.equal(s.trashCount, 0);
    assert.equal(s.dataRoot, t.data);
    assert.equal(s.dataRootFromEnv, false);
  } finally {
    t.cleanup();
  }
});

/** Every file name under `dir`, recursively (forward slashes). */
function walk(dir: string, rel = ""): string[] {
  if (!fs.existsSync(path.join(dir, rel))) return [];
  return fs.readdirSync(path.join(dir, rel)).flatMap((n) => {
    const r = rel ? `${rel}/${n}` : n;
    return fs.statSync(path.join(dir, r)).isDirectory() ? walk(dir, r) : [r];
  });
}

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

test("each run purges trash older than 30 days and keeps newer trash", async () => {
  const t = setup();
  try {
    const trash = path.join(t.repo, ".learner", "trash");
    const old = String(Date.now() - 31 * 24 * 60 * 60 * 1000);
    const fresh = String(Date.now() - 24 * 60 * 60 * 1000);
    for (const ms of [old, fresh]) {
      fs.mkdirSync(path.join(trash, ms, "courses"), { recursive: true });
      fs.writeFileSync(path.join(trash, ms, "courses", "f.md"), ms);
    }
    await t.svc.run(); // local provider: the purge still runs
    assert.equal(fs.existsSync(path.join(trash, old)), false);
    assert.equal(fs.existsSync(path.join(trash, fresh, "courses", "f.md")), true);
  } finally {
    t.cleanup();
  }
});

test("tokens land in <repo>/.learner/secrets.json, never under the data folder; plaintext reports encrypted false", async () => {
  const fake: typeof fetch = async (input) => {
    const url = String(input);
    if (url.startsWith("https://oauth2.googleapis.com/token")) return json({ access_token: "at", refresh_token: "rt-SECRET", expires_in: 3600 });
    if (url.includes("/drive/v3/files") && url.includes("mimeType")) return json({ files: [{ id: "folder1" }] });
    if (url.includes("/drive/v3/about")) return json({ user: { emailAddress: "a@example.com" } });
    return new Response("unhandled", { status: 500 });
  };
  const t = setup({ fetch: fake });
  try {
    writeSettings(t.repo, { googleClientId: "test-client" });
    const state = new URL((await t.svc.googleStart()).url).searchParams.get("state")!;
    assert.equal((await t.svc.googleCallback({ code: "abc", state })).status, 200);
    const s = await t.svc.status();
    assert.equal(s.encrypted, false);
    assert.match(fs.readFileSync(path.join(t.repo, ".learner", "secrets.json"), "utf8"), /rt-SECRET/);
    for (const f of walk(t.data)) assert.doesNotMatch(fs.readFileSync(path.join(t.data, f), "utf8"), /rt-SECRET/, f);
    for (const f of walk(path.join(t.repo, ".learner"))) {
      if (f === "secrets.json") continue;
      assert.doesNotMatch(fs.readFileSync(path.join(t.repo, ".learner", f), "utf8"), /rt-SECRET/, f);
    }
  } finally {
    t.cleanup();
  }
});

test("with a cipher, status reports encrypted and the token is not stored in clear", async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-svc-cipher-"));
  try {
    const repo = path.join(tmp, "repo");
    const data = path.join(tmp, "data");
    fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
    fs.mkdirSync(data, { recursive: true });
    const cipher = {
      available: async () => true,
      encrypt: async (p: string) => Buffer.from(p.split("").reverse().join("")).toString("base64"),
      decrypt: async (b: string) => Buffer.from(b, "base64").toString().split("").reverse().join(""),
    };
    const svc = new SyncService({ repoRoot: repo, dataRoot: data, dataRootFromEnv: false, getPort: () => 1, cipher, onStatus: () => {}, onPulled: () => {} });
    await writeSecrets(repo, { github: { token: "gho_CLEAR_TOKEN", login: "octo", repo: "octo/proxi-data", branch: "main" } }, cipher);
    writeSettings(repo, { syncProvider: "github" });
    const s = await svc.status();
    assert.equal(s.encrypted, true);
    assert.equal(s.github.connected, true);
    assert.equal(s.github.login, "octo");
    assert.doesNotMatch(fs.readFileSync(path.join(repo, ".learner", "secrets.json"), "utf8"), /gho_CLEAR_TOKEN/);

    // The same file without the cipher (plain Node): not connected, with the desktop-app hint.
    const plain = new SyncService({ repoRoot: repo, dataRoot: data, dataRootFromEnv: false, getPort: () => 1, onStatus: () => {}, onPulled: () => {} });
    const p = await plain.status();
    assert.equal(p.encrypted, false);
    assert.equal(p.github.connected, false);
    assert.match(p.github.error ?? "", /desktop/);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
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

test("disconnecting a provider that is not the active one keeps the active provider", async () => {
  const t = setup({ fetch: async () => new Response("", { status: 200 }) });
  try {
    await connectGithub(t.repo, { gdrive: { refreshToken: "rt-1", folderId: "f" } });
    const s = await t.svc.disconnect("gdrive");
    assert.equal(s.provider, "github");
    assert.equal(s.github.connected, true);
    assert.equal(s.gdrive.connected, false);
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

test("concurrent run() calls share one pass", async () => {
  const t = setup({ files: { "curriculum.json": "{}\n" } });
  try {
    await connectGithub(t.repo);
    const [a, b] = await Promise.all([t.svc.run(), t.svc.run()]);
    assert.equal(a, b);
    assert.equal(t.rem.calls.list, 1);
  } finally {
    t.cleanup();
  }
});

test("a provider error lands in the status and does not save the manifest", async () => {
  const t = setup({ files: { "curriculum.json": "{}\n" } });
  try {
    await connectGithub(t.repo);
    t.rem.provider.list = async () => {
      throw new Error("rete assente");
    };
    const s = await t.svc.run();
    assert.equal(s.state, "error");
    assert.equal(s.error, "rete assente");
    assert.equal(fs.existsSync(path.join(t.repo, ".learner", "sync-manifests")), false);
  } finally {
    t.cleanup();
  }
});

/** A second service over the same repo (same .learner/ manifests and conflicts) and the same remote, with another data folder. */
function otherDataRoot(t: ReturnType<typeof setup>, dir: string) {
  return new SyncService({
    repoRoot: t.repo,
    dataRoot: dir,
    dataRootFromEnv: false,
    getPort: () => 4321,
    onStatus: () => {},
    onPulled: () => void t.c.pulled++,
    makeProvider: async () => t.rem.provider,
  });
}

test("switching the data folder never pushes an older copy or removes remote files", async () => {
  const t = setup({ files: { "courses/c.md": "remote" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    const older = path.join(t.tmp, "older");
    fs.mkdirSync(path.join(older, "courses"), { recursive: true });
    fs.writeFileSync(path.join(older, "courses", "c.md"), "older-copy");
    const past = new Date(Date.now() - 3_600_000);
    fs.utimesSync(path.join(older, "courses", "c.md"), past, past);

    const before = { ...t.rem.calls };
    const s1 = await otherDataRoot(t, older).run();
    assert.equal(s1.state, "idle");
    assert.equal(s1.pushed, 0);
    assert.equal(s1.deleted, 0);
    assert.equal(t.rem.calls.write, before.write);
    assert.equal(t.rem.calls.remove, before.remove);
    assert.equal(t.rem.store.get("courses/c.md")?.data.toString(), "remote");
    assert.equal(fs.readFileSync(path.join(older, "courses", "c.md"), "utf8"), "remote");

    const empty = path.join(t.tmp, "empty");
    fs.mkdirSync(empty, { recursive: true });
    const s2 = await otherDataRoot(t, empty).run();
    assert.equal(s2.state, "idle");
    assert.equal(s2.deleted, 0);
    assert.equal(s2.pendingDeletions, undefined);
    assert.equal(t.rem.calls.remove, before.remove);
    assert.equal(t.rem.store.has("courses/c.md"), true);
    assert.equal(fs.readFileSync(path.join(empty, "courses", "c.md"), "utf8"), "remote");
  } finally {
    t.cleanup();
  }
});

test("a pass that only trashes remote deletions still refreshes the board", async () => {
  const t = setup({ files: { "courses/a.md": "a", "courses/b.md": "b", "courses/c.md": "c", "courses/d.md": "d" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    const pulled = t.c.pulled;
    t.rem.store.delete("courses/a.md");
    const r = await t.svc.run();
    assert.equal(r.deleted, 1);
    assert.equal(r.pulled, 0);
    assert.equal(fs.existsSync(path.join(t.data, "courses", "a.md")), false);
    assert.equal(t.c.pulled, pulled + 1);
  } finally {
    t.cleanup();
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

test("a pass that moves nothing in, or only pushes, does not refresh the board", async () => {
  const t = setup({ files: { "courses/a.md": "a" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    const before = t.c.pulled;
    await t.svc.run(); // no-op
    fs.writeFileSync(path.join(t.data, "courses", "new.md"), "local only");
    const r = await t.svc.run();
    assert.equal(r.pushed, 1);
    assert.equal(r.pulled, 0);
    assert.equal(r.deleted, 0);
    assert.equal(t.c.pulled, before);
  } finally {
    t.cleanup();
  }
});

test("switching back to the original data folder reuses its manifest only after the other folder's pass rewrote it", async () => {
  // The manifest file is per provider; a pass on another folder rebinds it, so returning is a first-meeting pass again:
  // remote wins, nothing local is pushed over it and nothing remote is removed.
  const t = setup({ files: { "courses/c.md": "remote" } });
  try {
    await connectGithub(t.repo);
    await t.svc.run();
    const other = path.join(t.tmp, "other");
    fs.mkdirSync(other, { recursive: true });
    await otherDataRoot(t, other).run();
    // back on the original folder, with a local edit made meanwhile and an older mtime than the remote
    fs.writeFileSync(path.join(t.data, "courses", "c.md"), "edited-while-away");
    const past = new Date(1_600_000_000_000);
    fs.utimesSync(path.join(t.data, "courses", "c.md"), past, past);
    const before = { ...t.rem.calls };
    const r = await t.svc.run();
    assert.equal(r.state, "idle");
    assert.equal(t.rem.calls.remove, before.remove);
    assert.equal(t.rem.store.has("courses/c.md"), true);
    // whichever side wins, neither copy is lost
    const local = fs.readFileSync(path.join(t.data, "courses", "c.md"), "utf8");
    const remoteNow = t.rem.store.get("courses/c.md")!.data.toString();
    assert.equal(local, remoteNow);
    const kept = fs.readdirSync(path.join(t.repo, ".learner", "drive-conflicts"), { recursive: true }).map(String);
    const copies = kept.filter((f) => f.includes("c.md."));
    assert.ok(copies.length >= 1, `a conflict copy is kept, got ${JSON.stringify(kept)}`);
  } finally {
    t.cleanup();
  }
});
