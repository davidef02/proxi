import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  DRIVE_FOLDER,
  GoogleAuth,
  authUrl,
  driveEmail,
  ensureFolder,
  exchangeCode,
  gdriveProvider,
  pkcePair,
  revokeToken,
} from "../server/sync/gdrive.ts";
import { syncPass } from "../server/sync/engine.ts";
import { RemoteMovedError, type Manifest } from "../server/sync/types.ts";

type Logged = { method: string; url: string; auth?: string; contentType?: string; body: Buffer };

/** fetch bodies in gdrive.ts are strings, Buffers or URLSearchParams. */
function toBuffer(body: unknown): Buffer {
  if (body == null) return Buffer.alloc(0);
  if (body instanceof URLSearchParams) return Buffer.from(body.toString(), "utf8");
  if (typeof body === "string") return Buffer.from(body, "utf8");
  return Buffer.from(body as Uint8Array);
}

interface FakeFile {
  id: string;
  name: string;
  parents: string[];
  mimeType?: string;
  appProperties?: Record<string, string>;
  data: Buffer;
  modifiedTime: string;
  trashed: boolean;
}

/**
 * In-memory Google Drive + OAuth. Routes on method + path. `pageSize` caps every files.list page so pagination is
 * exercised. `invalidateAccess()` makes the current access token fail with 401 (forces a refresh).
 */
function fakeDrive(opts: { pageSize?: number } = {}) {
  const files = new Map<string, FakeFile>();
  const log: Logged[] = [];
  let nextId = 1;
  let tokenSeq = 0;
  const validAccess = new Set<string>();
  let refreshGrants = 0;
  let omitRefresh = false;
  let aboutFails = false;
  const pageSize = opts.pageSize ?? 1000;

  const json = (status: number, body: unknown) =>
    new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

  const addFile = (f: Partial<FakeFile> & { name: string; parents?: string[]; data?: Buffer }): string => {
    const id = `f${nextId++}`;
    files.set(id, {
      id,
      parents: f.parents ?? [],
      data: f.data ?? Buffer.alloc(0),
      modifiedTime: f.modifiedTime ?? new Date(0).toISOString(),
      trashed: false,
      ...f,
    });
    return id;
  };

  const md5 = (d: Buffer) => createHash("md5").update(d).digest("hex");
  // Like Drive: folders carry no md5Checksum.
  const stat = (f: FakeFile) => ({
    id: f.id,
    name: f.name,
    size: String(f.data.length),
    md5Checksum: f.mimeType === "application/vnd.google-apps.folder" ? undefined : md5(f.data),
    modifiedTime: f.modifiedTime,
  });

  const fetchFn = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    const url = new URL(String(input));
    const method = (init?.method ?? "GET").toUpperCase();
    const headers = new Headers(init?.headers);
    const body = toBuffer(init?.body);
    const auth = headers.get("authorization") ?? undefined;
    const contentType = headers.get("content-type") ?? undefined;
    log.push({ method, url: url.toString(), auth, contentType, body });
    const host = url.host;
    const p = url.pathname;

    // OAuth endpoints
    if (host === "oauth2.googleapis.com" && p === "/token") {
      const form = new URLSearchParams(body.toString("utf8"));
      if (form.get("grant_type") === "refresh_token") {
        refreshGrants++;
        if (form.get("refresh_token") !== "rt-1") return json(400, { error: "invalid_grant" });
      }
      const access = `at-${++tokenSeq}`;
      validAccess.add(access);
      const out: Record<string, unknown> = { access_token: access, expires_in: 3600 };
      if (form.get("grant_type") === "authorization_code" && !omitRefresh) out.refresh_token = "rt-1";
      return json(200, out);
    }
    if (host === "oauth2.googleapis.com" && p === "/revoke") return new Response(null, { status: 200 });

    // Everything below is the Drive API: check the bearer token.
    if (host === "www.googleapis.com" || host === "accounts.google.com") {
      if (host === "www.googleapis.com" && auth) {
        const tok = auth.replace(/^Bearer /, "");
        if (!validAccess.has(tok)) return json(401, { error: { message: "Invalid Credentials" } });
      }
    }

    if (host === "www.googleapis.com" && p === "/drive/v3/about") {
      if (aboutFails) return json(500, { error: { message: "boom" } });
      return json(200, { user: { emailAddress: "davide@example.com" } });
    }

    if (host === "www.googleapis.com" && p === "/drive/v3/files" && method === "GET") {
      const q = url.searchParams.get("q") ?? "";
      if (q.includes(`mimeType='${"application/vnd.google-apps.folder"}'`)) {
        if (!q.includes("key='proxi'")) return json(200, { files: [] }); // a wrong appProperties key must find nothing
        const hits = [...files.values()].filter(
          (f) => f.mimeType === "application/vnd.google-apps.folder" && !f.trashed && f.appProperties?.proxi === "data",
        );
        return json(200, { files: hits.slice(0, 1).map((f) => ({ id: f.id })) });
      }
      const m = /^'(.+)' in parents/.exec(q);
      const parent = m?.[1] ?? "";
      const all = [...files.values()]
        .filter((f) => f.parents.includes(parent) && !f.trashed)
        .map(stat);
      const start = Number(url.searchParams.get("pageToken") ?? "0");
      const page = all.slice(start, start + pageSize);
      const next = start + pageSize < all.length ? { nextPageToken: String(start + pageSize) } : {};
      return json(200, { ...next, files: page });
    }

    if (host === "www.googleapis.com" && p === "/drive/v3/files" && method === "POST") {
      const meta = JSON.parse(body.toString("utf8")) as Record<string, unknown>;
      const id = addFile({
        name: String(meta.name),
        parents: [],
        mimeType: meta.mimeType as string,
        appProperties: meta.appProperties as Record<string, string>,
      });
      return json(200, { id });
    }

    const fileMatch = /^\/drive\/v3\/files\/([^/]+)$/.exec(p);
    if (host === "www.googleapis.com" && fileMatch) {
      const f = files.get(fileMatch[1]);
      if (!f) return json(404, { error: { message: "File not found" } });
      if (method === "PATCH") {
        const meta = JSON.parse(body.toString("utf8")) as { trashed?: boolean };
        if (meta.trashed) f.trashed = true;
        return json(200, stat(f));
      }
      if (url.searchParams.get("alt") === "media") return new Response(new Uint8Array(f.data), { status: 200 });
      return json(200, { md5Checksum: md5(f.data) });
    }

    const uploadMatch = /^\/upload\/drive\/v3\/files(?:\/([^/]+))?$/.exec(p);
    if (host === "www.googleapis.com" && uploadMatch) {
      const boundary = /boundary=(.+)$/.exec(contentType ?? "")?.[1] ?? "";
      const raw = body.toString("latin1");
      const parts = raw.split(`--${boundary}`);
      const metaPart = parts[1].slice(parts[1].indexOf("\r\n\r\n") + 4).replace(/\r\n$/, "");
      const dataPart = parts[2].slice(parts[2].indexOf("\r\n\r\n") + 4);
      const data = Buffer.from(dataPart.replace(/\r\n$/, ""), "latin1");
      const meta = JSON.parse(metaPart) as Record<string, unknown>;
      if (uploadMatch[1]) {
        const f = files.get(uploadMatch[1]);
        if (!f) return json(404, { error: { message: "File not found" } });
        f.data = data;
        f.modifiedTime = String(meta.modifiedTime ?? f.modifiedTime);
        return json(200, stat(f));
      }
      const id = addFile({
        name: String(meta.name),
        parents: meta.parents as string[],
        appProperties: meta.appProperties as Record<string, string>,
        data,
        modifiedTime: String(meta.modifiedTime),
      });
      return json(200, stat(files.get(id)!));
    }

    return json(404, { error: { message: `unrouted ${method} ${url}` } });
  };

  return {
    fetch: fetchFn as unknown as typeof fetch,
    log,
    files,
    addFile,
    /** Changes a file as if another PC wrote it. */
    setRemote(id: string, data: Buffer, modifiedTime = new Date().toISOString()) {
      const f = files.get(id)!;
      f.data = data;
      f.modifiedTime = modifiedTime;
    },
    invalidateAccess() {
      validAccess.clear();
    },
    get refreshGrants() {
      return refreshGrants;
    },
    setOmitRefresh(v: boolean) {
      omitRefresh = v;
    },
    setAboutFails(v: boolean) {
      aboutFails = v;
    },
    writesTo(prefix: string) {
      return log.filter((l) => l.method !== "GET" && l.url.includes(prefix)).length;
    },
  };
}

const clockAt = (t: { ms: number }) => () => t.ms;

test("pkcePair: 64 url-safe verifier, challenge = base64url(sha256(verifier))", () => {
  const { verifier, challenge } = pkcePair();
  assert.match(verifier, /^[A-Za-z0-9_-]{64}$/);
  assert.equal(challenge, createHash("sha256").update(verifier).digest("base64url"));
  assert.notEqual(pkcePair().verifier, verifier);
});

test("authUrl carries the PKCE, offline and consent params", () => {
  const url = new URL(
    authUrl({ clientId: "cid", redirectUri: "http://127.0.0.1:5555/oauth/google/callback", challenge: "CH", state: "ST" }),
  );
  assert.equal(url.origin + url.pathname, "https://accounts.google.com/o/oauth2/v2/auth");
  assert.equal(url.searchParams.get("client_id"), "cid");
  assert.equal(url.searchParams.get("redirect_uri"), "http://127.0.0.1:5555/oauth/google/callback");
  assert.equal(url.searchParams.get("response_type"), "code");
  assert.equal(url.searchParams.get("scope"), "https://www.googleapis.com/auth/drive.file");
  assert.equal(url.searchParams.get("code_challenge"), "CH");
  assert.equal(url.searchParams.get("code_challenge_method"), "S256");
  assert.equal(url.searchParams.get("state"), "ST");
  assert.equal(url.searchParams.get("access_type"), "offline");
  assert.equal(url.searchParams.get("prompt"), "consent");
});

test("empty client id is refused with the Italian message", () => {
  assert.throws(
    () => authUrl({ clientId: "", redirectUri: "http://127.0.0.1/x", challenge: "c", state: "s" }),
    /Client Google non configurato/,
  );
  // the message names both places to set it: the source constant and the settings.json override
  assert.throws(
    () => authUrl({ clientId: "", redirectUri: "http://127.0.0.1/x", challenge: "c", state: "s" }),
    (e: Error) => {
      assert.match(e.message, /GOOGLE_CLIENT_ID in app\/server\/sync\/gdrive\.ts/);
      assert.match(e.message, /googleClientId in \.learner\/settings\.json/);
      assert.doesNotMatch(e.message, /nelle impostazioni/);
      return true;
    },
  );
});

test("exchangeCode sends code_verifier and omits an empty client secret", async () => {
  const d = fakeDrive();
  const r = await exchangeCode(
    { fetch: d.fetch, now: () => 1_000_000 },
    { clientId: "cid", clientSecret: "", code: "CODE", verifier: "VER", redirectUri: "http://127.0.0.1/cb" },
  );
  assert.equal(r.refreshToken, "rt-1");
  assert.equal(r.expiresAt, 1_000_000 + (3600 - 60) * 1000);
  const form = new URLSearchParams(d.log[0].body.toString("utf8"));
  assert.equal(form.get("code_verifier"), "VER");
  assert.equal(form.get("grant_type"), "authorization_code");
  assert.equal(form.get("client_id"), "cid");
  assert.equal(form.has("client_secret"), false);
});

test("exchangeCode includes client_secret when set", async () => {
  const d = fakeDrive();
  await exchangeCode(
    { fetch: d.fetch },
    { clientId: "cid", clientSecret: "sec", code: "C", verifier: "V", redirectUri: "http://127.0.0.1/cb" },
  );
  assert.equal(new URLSearchParams(d.log[0].body.toString("utf8")).get("client_secret"), "sec");
});

test("exchangeCode without refresh_token throws", async () => {
  const d = fakeDrive();
  d.setOmitRefresh(true);
  await assert.rejects(
    exchangeCode({ fetch: d.fetch }, { clientId: "cid", code: "C", verifier: "V", redirectUri: "http://127.0.0.1/cb" }),
    { message: "Google non ha restituito un refresh token: riprova." },
  );
});

test("GoogleAuth caches until expiry, then refreshes", async () => {
  const d = fakeDrive();
  const t = { ms: 0 };
  const auth = new GoogleAuth({ fetch: d.fetch, now: clockAt(t) }, { clientId: "cid", refreshToken: "rt-1" });
  const a = await auth.accessToken();
  assert.equal(await auth.accessToken(), a);
  assert.equal(d.refreshGrants, 1);
  t.ms += 3600 * 1000; // past (expires_in - 60) s
  assert.notEqual(await auth.accessToken(), a);
  assert.equal(d.refreshGrants, 2);
});

test("GoogleAuth maps invalid_grant to the reconnect message", async () => {
  const d = fakeDrive();
  const auth = new GoogleAuth({ fetch: d.fetch }, { clientId: "cid", refreshToken: "revoked" });
  await assert.rejects(auth.accessToken(), { message: "Accesso a Google scaduto o revocato: riconnetti." });
});

test("driveEmail returns the address, undefined on error", async () => {
  const d = fakeDrive();
  const auth = new GoogleAuth({ fetch: d.fetch }, { clientId: "cid", refreshToken: "rt-1" });
  assert.equal(await driveEmail({ fetch: d.fetch }, auth), "davide@example.com");
  d.setAboutFails(true);
  assert.equal(await driveEmail({ fetch: d.fetch }, auth), undefined);
});

test("revokeToken is best-effort and never throws", async () => {
  const d = fakeDrive();
  await revokeToken({ fetch: d.fetch }, "rt-1");
  assert.equal(d.log.at(-1)?.url, "https://oauth2.googleapis.com/revoke");
  await revokeToken(
    {
      fetch: (async () => {
        throw new Error("offline");
      }) as unknown as typeof fetch,
    },
    "rt-1",
  );
});

test("ensureFolder finds an existing app folder, else creates it", async () => {
  const d = fakeDrive();
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  const created = await ensureFolder(deps, auth);
  const createCall = d.log.find((l) => l.method === "POST" && l.url.endsWith("/drive/v3/files"));
  assert.ok(createCall, "creates the folder");
  const meta = JSON.parse(createCall.body.toString("utf8"));
  assert.equal(meta.name, DRIVE_FOLDER);
  assert.equal(meta.mimeType, "application/vnd.google-apps.folder");
  assert.deepEqual(meta.appProperties, { proxi: "data" });
  assert.equal(d.files.get(created)?.appProperties?.proxi, "data");

  const again = await ensureFolder(deps, auth);
  assert.equal(again, created);
  assert.equal(d.log.filter((l) => l.method === "POST" && l.url.endsWith("/drive/v3/files")).length, 1);

  const d2 = fakeDrive();
  const existing = d2.addFile({
    name: DRIVE_FOLDER,
    mimeType: "application/vnd.google-apps.folder",
    appProperties: { proxi: "data" },
  });
  const auth2 = new GoogleAuth({ fetch: d2.fetch }, { clientId: "cid", refreshToken: "rt-1" });
  assert.equal(await ensureFolder({ fetch: d2.fetch }, auth2), existing);
  assert.equal(d2.log.filter((l) => l.method === "POST" && l.url.startsWith("https://www.googleapis.com/")).length, 0);
});

test("creates the data folder proxi-data marked with appProperties proxi", async () => {
  const d = fakeDrive();
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  await ensureFolder(deps, auth);
  assert.equal(DRIVE_FOLDER, "proxi-data");
  const createCall = d.log.find((l) => l.method === "POST" && l.url.endsWith("/drive/v3/files"))!;
  const meta = JSON.parse(createCall.body.toString("utf8")) as { name: string; appProperties: Record<string, string> };
  assert.equal(meta.name, DRIVE_FOLDER);
  assert.deepEqual(meta.appProperties, { proxi: "data" });
  const lookup = d.log.find((l) => l.method === "GET" && l.url.includes("/drive/v3/files?q="))!;
  assert.match(new URL(lookup.url).searchParams.get("q") ?? "", /key='proxi'/);
});

test("401 refreshes the access token once and retries", async () => {
  const d = fakeDrive();
  const folder = d.addFile({ name: "f", mimeType: "application/vnd.google-apps.folder" });
  d.addFile({ name: "a.txt", parents: [folder], data: Buffer.from("A") });
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  await auth.accessToken();
  d.invalidateAccess();
  const remote = await gdriveProvider(deps, auth, folder).list();
  assert.equal(remote.get("a.txt")?.size, 1);
  assert.equal(d.log.filter((l) => l.url.includes("/drive/v3/files?q=") && l.method === "GET").length, 2);
  assert.equal(d.refreshGrants, 2);
});

test("list paginates, skips folders and native docs, keeps the newest duplicate", async () => {
  const d = fakeDrive({ pageSize: 2 });
  const folder = d.addFile({ name: "app", mimeType: "application/vnd.google-apps.folder" });
  d.addFile({ name: "sub", parents: [folder], mimeType: "application/vnd.google-apps.folder" });
  d.addFile({ name: "a.json", parents: [folder], data: Buffer.from("aaa"), modifiedTime: "2026-01-01T00:00:00.000Z" });
  d.addFile({ name: "b.md", parents: [folder], data: Buffer.from("b"), modifiedTime: "2026-01-02T00:00:00.000Z" });
  d.addFile({ name: "dup", parents: [folder], data: Buffer.from("old"), modifiedTime: "2026-01-03T00:00:00.000Z" });
  d.addFile({ name: "dup", parents: [folder], data: Buffer.from("new!"), modifiedTime: "2026-01-04T00:00:00.000Z" });
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  const remote = await gdriveProvider(deps, auth, folder).list();

  assert.deepEqual([...remote.keys()].sort(), ["a.json", "b.md", "dup"]);
  assert.deepEqual(remote.get("a.json"), {
    size: 3,
    mtimeMs: Date.parse("2026-01-01T00:00:00.000Z"),
    token: createHash("md5").update("aaa").digest("hex"),
  });
  assert.equal(remote.get("dup")?.size, 4);
  assert.ok(d.log.filter((l) => l.url.includes("pageToken=")).length >= 1, "second page requested");
});

test("write: create uses multipart POST with app metadata; update PATCHes with modifiedTime only", async () => {
  const d = fakeDrive();
  const folder = d.addFile({ name: "app", mimeType: "application/vnd.google-apps.folder" });
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  const p = gdriveProvider(deps, auth, folder);
  await p.list();

  const mtime = Date.parse("2026-03-04T05:06:07.000Z");
  const created = await p.write("courses/demo/MISSION.md", Buffer.from("hello"), mtime);
  assert.equal(created.size, 5);
  assert.equal(created.token, createHash("md5").update("hello").digest("hex"));
  assert.equal(created.mtimeMs, mtime);

  const post = d.log.at(-1)!;
  assert.equal(post.method, "POST");
  assert.match(post.url, /\/upload\/drive\/v3\/files\?uploadType=multipart&fields=/);
  assert.match(post.contentType ?? "", /^multipart\/related; boundary=/);
  const raw = post.body.toString("latin1");
  const metaJson = JSON.parse(raw.split("\r\n\r\n")[1].split("\r\n--")[0]);
  assert.deepEqual(metaJson, {
    name: "courses/demo/MISSION.md",
    parents: [folder],
    appProperties: { proxi: "file" },
    modifiedTime: "2026-03-04T05:06:07.000Z",
  });
  assert.ok(raw.includes("hello"));

  await p.list();
  const updated = await p.write("courses/demo/MISSION.md", Buffer.from("bye"), mtime + 1000);
  assert.equal(updated.token, createHash("md5").update("bye").digest("hex"));
  const patch = d.log.at(-1)!;
  assert.equal(patch.method, "PATCH");
  assert.match(patch.url, /\/upload\/drive\/v3\/files\/f\d+\?uploadType=multipart/);
  const patchMeta = JSON.parse(patch.body.toString("latin1").split("\r\n\r\n")[1].split("\r\n--")[0]);
  assert.deepEqual(patchMeta, { modifiedTime: "2026-03-04T05:06:08.000Z" });
  assert.equal([...d.files.values()].find((f) => f.name === "courses/demo/MISSION.md")?.data.toString(), "bye");
});

test("write refuses to overwrite a file changed remotely since list()", async () => {
  const d = fakeDrive();
  const folder = d.addFile({ name: "app", mimeType: "application/vnd.google-apps.folder" });
  const id = d.addFile({ name: "curriculum.json", parents: [folder], data: Buffer.from("v1") });
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  const p = gdriveProvider(deps, auth, folder);
  await p.list();
  d.setRemote(id, Buffer.from("other PC"));
  await assert.rejects(p.write("curriculum.json", Buffer.from("mine"), 0), RemoteMovedError);
  assert.equal(d.files.get(id)?.data.toString(), "other PC");
});

test("remove sends trashed:true; unknown rel is a no-op", async () => {
  const d = fakeDrive();
  const folder = d.addFile({ name: "app", mimeType: "application/vnd.google-apps.folder" });
  const id = d.addFile({ name: "decks/x.apkg", parents: [folder], data: Buffer.from("x") });
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  const p = gdriveProvider(deps, auth, folder);
  await p.list();
  await p.remove("decks/x.apkg");
  const call = d.log.at(-1)!;
  assert.equal(call.method, "PATCH");
  assert.equal(call.url, `https://www.googleapis.com/drive/v3/files/${id}`);
  assert.deepEqual(JSON.parse(call.body.toString("utf8")), { trashed: true });
  assert.equal(d.files.get(id)?.trashed, true);

  const before = d.log.length;
  await p.remove("never/seen.md");
  assert.equal(d.log.length, before);
  await p.commit("ignored");
  assert.equal(d.log.length, before);
});

test("remove trashes every duplicate of a name, so the deleted file does not come back with old bytes", async () => {
  const d = fakeDrive();
  const folder = d.addFile({ name: "app", mimeType: "application/vnd.google-apps.folder" });
  // Two PCs pushed the same new file before either listed it.
  const older = d.addFile({ name: "courses/c/lessons/l.md", parents: [folder], data: Buffer.from("old"), modifiedTime: "2026-01-01T00:00:00.000Z" });
  const newer = d.addFile({ name: "courses/c/lessons/l.md", parents: [folder], data: Buffer.from("new!"), modifiedTime: "2026-01-02T00:00:00.000Z" });
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  const p = gdriveProvider(deps, auth, folder);

  assert.equal((await p.list()).get("courses/c/lessons/l.md")?.size, 4, "the newest copy is the file");
  await p.remove("courses/c/lessons/l.md");
  assert.equal(d.files.get(newer)?.trashed, true);
  assert.equal(d.files.get(older)?.trashed, true);
  assert.equal((await gdriveProvider(deps, auth, folder).list()).has("courses/c/lessons/l.md"), false);
});

test("write patches the newest copy and keeps the others for a later remove", async () => {
  const d = fakeDrive();
  const folder = d.addFile({ name: "app", mimeType: "application/vnd.google-apps.folder" });
  const older = d.addFile({ name: "decks/x.apkg", parents: [folder], data: Buffer.from("a"), modifiedTime: "2026-01-01T00:00:00.000Z" });
  const newer = d.addFile({ name: "decks/x.apkg", parents: [folder], data: Buffer.from("bb"), modifiedTime: "2026-01-02T00:00:00.000Z" });
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  const p = gdriveProvider(deps, auth, folder);
  await p.list();
  await p.write("decks/x.apkg", Buffer.from("ccc"), Date.parse("2026-01-03T00:00:00.000Z"));
  assert.equal(d.files.get(newer)?.data.toString(), "ccc");
  assert.equal(d.files.get(older)?.data.toString(), "a", "the older copy is not patched");
  await p.remove("decks/x.apkg");
  assert.equal(d.files.get(older)?.trashed, true);
  assert.equal(d.files.get(newer)?.trashed, true);
});

test("contentEquals compares md5 of the bytes", async () => {
  const d = fakeDrive();
  const auth = new GoogleAuth({ fetch: d.fetch }, { clientId: "cid", refreshToken: "rt-1" });
  const p = gdriveProvider({ fetch: d.fetch }, auth, "folder");
  const token = createHash("md5").update("same").digest("hex");
  assert.equal(p.contentEquals?.(Buffer.from("same"), { size: 4, mtimeMs: 0, token }), true);
  assert.equal(p.contentEquals?.(Buffer.from("diff"), { size: 4, mtimeMs: 0, token }), false);
});

test("syncPass end to end: first pass pushes, second pass is a no-op", async () => {
  const d = fakeDrive();
  const folder = d.addFile({ name: DRIVE_FOLDER, mimeType: "application/vnd.google-apps.folder", appProperties: { proxi: "data" } });
  const deps = { fetch: d.fetch };
  const auth = new GoogleAuth(deps, { clientId: "cid", refreshToken: "rt-1" });
  const provider = gdriveProvider(deps, auth, folder);

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "gdrive-sync-"));
  try {
    const dataRoot = path.join(tmp, "data");
    fs.mkdirSync(path.join(dataRoot, "courses", "demo"), { recursive: true });
    fs.writeFileSync(path.join(dataRoot, "courses", "demo", "MISSION.md"), "# demo\n");
    fs.writeFileSync(path.join(dataRoot, "curriculum.json"), "{}\n");
    const conflictsDir = path.join(tmp, "conflicts");
    const trashDir = path.join(tmp, "trash");
    const manifest: Manifest = { v: 2, key: provider.key, files: {} };

    const first = await syncPass({ dataRoot, provider, manifest, conflictsDir, trashDir });
    assert.equal(first.result.pushed, 2);
    assert.deepEqual(first.result.errors, []);

    const writesAfterFirst = d.writesTo("/upload/drive/v3/files");
    const reads = d.log.filter((l) => l.method === "GET" && l.url.includes("alt=media")).length;
    const second = await syncPass({ dataRoot, provider, manifest: first.manifest, conflictsDir, trashDir });
    assert.equal(second.result.pushed + second.result.pulled + second.result.deleted, 0);
    assert.equal(d.writesTo("/upload/drive/v3/files"), writesAfterFirst);
    assert.equal(d.log.filter((l) => l.method === "GET" && l.url.includes("alt=media")).length, reads);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
