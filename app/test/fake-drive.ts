/**
 * Stateful fake Google Drive on a FakeHttpServer: the OAuth token endpoint and one `proxi-data` app folder.
 * The paths are the ones `--google-url` maps to, so the Node fakeFetch and the binary reach the same routes.
 */
import * as crypto from "node:crypto";
import { DATA_DIR_NAME, DRIVE_APP_PROPERTY } from "../shared/identity.mjs";
import { FakeHttpServer, type FakeHandler, type FakeReply, type RecordedRequest } from "./fake-servers.ts";

export interface FakeDriveSeed {
  /** Default "rt-1". */
  refreshToken?: string;
  /** Default "tester@example.com". */
  email?: string;
  /** An existing app folder, created with `appProperties {proxi: "data"}`. */
  folderId?: string;
  /** utf8 files inside `folderId`, modifiedTime SEED_TIME. */
  files?: Record<string, string>;
}

export const SEED_TIME = "2026-01-01T00:00:00.000Z";

const FOLDER_MIME = "application/vnd.google-apps.folder";
const PAGE_SIZE = 1000;

interface FakeFile {
  id: string;
  name: string;
  parents: string[];
  mimeType?: string;
  appProperties?: Record<string, string>;
  data: string;
  modifiedTime: string;
  trashed: boolean;
}

export interface FakeDrive {
  /** Non-trashed files in the app folder, newest per name, utf8. */
  files(): Record<string, string>;
  /** The app folder's id, or undefined before it exists. */
  folderId(): string | undefined;
  /** Non-GET requests: token refreshes, uploads, trash patches and the revoke. */
  mutations(): { method: string; path: string; body: unknown }[];
}

function header(r: RecordedRequest, name: string): string {
  const value = r.headers[name];
  return Array.isArray(value) ? value.join(", ") : (value ?? "");
}

/** The two parts of a multipart upload: the JSON metadata and the utf8 data. */
function parseMultipart(body: string, boundary: string): { meta: Record<string, unknown>; data: string } {
  const chunks = body.split(`--${boundary}`);
  const part = (i: number): string => {
    const raw = chunks[i];
    if (!raw) return "";
    return raw.slice(raw.indexOf("\r\n\r\n") + 4).replace(/\r\n$/, "");
  };
  return { meta: JSON.parse(part(1)) as Record<string, unknown>, data: part(2) };
}

/** A recorded body as the app sent it: JSON, an OAuth form, or the two parts of a multipart upload. */
export function driveBody(r: RecordedRequest): unknown {
  if (!r.body) return null;
  const type = header(r, "content-type");
  if (type.includes("multipart/related")) return parseMultipart(r.body, /boundary=(.+)$/.exec(type)?.[1] ?? "");
  if (type.includes("application/x-www-form-urlencoded")) return Object.fromEntries(new URLSearchParams(r.body));
  if (type.includes("application/json")) return JSON.parse(r.body);
  return null;
}

export function installFakeDrive(server: FakeHttpServer, seed: FakeDriveSeed = {}): FakeDrive {
  const refreshToken = seed.refreshToken ?? "rt-1";
  const email = seed.email ?? "tester@example.com";
  const files = new Map<string, FakeFile>();
  let nextId = 1;
  let tokenSeq = 0;
  const validAccess = new Set<string>();

  const add = (f: {
    id?: string;
    name: string;
    parents?: string[];
    mimeType?: string;
    appProperties?: Record<string, string>;
    data?: string;
    modifiedTime?: string;
  }): string => {
    const id = f.id ?? `f${nextId++}`;
    files.set(id, {
      id,
      name: f.name,
      parents: f.parents ?? [],
      mimeType: f.mimeType,
      appProperties: f.appProperties,
      data: f.data ?? "",
      modifiedTime: f.modifiedTime ?? SEED_TIME,
      trashed: false,
    });
    return id;
  };

  const appFolder = (): string | undefined =>
    [...files.values()].find((f) => f.mimeType === FOLDER_MIME && !f.trashed && f.appProperties?.[DRIVE_APP_PROPERTY] === "data")?.id;

  const md5 = (text: string): string => crypto.createHash("md5").update(text, "utf8").digest("hex");
  // Like Drive: folders carry no md5Checksum.
  const stat = (f: FakeFile) => ({
    id: f.id,
    name: f.name,
    size: String(Buffer.byteLength(f.data, "utf8")),
    ...(f.mimeType === FOLDER_MIME ? {} : { md5Checksum: md5(f.data) }),
    modifiedTime: f.modifiedTime,
  });

  const denied = (req: RecordedRequest): FakeReply | undefined => {
    const token = header(req, "authorization").replace(/^Bearer /, "");
    return validAccess.has(token) ? undefined : { status: 401, body: { error: { message: "Invalid Credentials" } } };
  };

  if (seed.folderId !== undefined) {
    add({ id: seed.folderId, name: DATA_DIR_NAME, mimeType: FOLDER_MIME, appProperties: { [DRIVE_APP_PROPERTY]: "data" } });
  }
  for (const [name, data] of Object.entries(seed.files ?? {})) {
    add({ name, parents: seed.folderId === undefined ? [] : [seed.folderId], data });
  }

  server.on("POST", "/token", (req) => {
    const form = new URLSearchParams(req.body);
    if (form.get("grant_type") === "refresh_token" && form.get("refresh_token") !== refreshToken) {
      return { status: 400, body: { error: "invalid_grant" } };
    }
    const access = `at-${++tokenSeq}`;
    validAccess.add(access);
    const out: Record<string, unknown> = { access_token: access, expires_in: 3600 };
    if (form.get("grant_type") === "authorization_code") out.refresh_token = refreshToken;
    return { body: out };
  });
  server.on("POST", "/revoke", () => ({ status: 200 }));

  server.on("GET", "/drive/v3/about", (req) => denied(req) ?? { body: { user: { emailAddress: email } } });

  server.on("GET", "/drive/v3/files", (req) => {
    const no = denied(req);
    if (no) return no;
    const url = new URL(req.path, "http://fake");
    const q = url.searchParams.get("q") ?? "";
    if (q.includes(`mimeType='${FOLDER_MIME}'`)) {
      if (!q.includes(`key='${DRIVE_APP_PROPERTY}'`)) return { body: { files: [] } }; // a wrong appProperties key finds nothing
      const id = appFolder();
      return { body: { files: id ? [{ id }] : [] } };
    }
    const parent = /^'(.+)' in parents/.exec(q)?.[1] ?? "";
    const all = [...files.values()]
      .filter((f) => !f.trashed && f.parents.includes(parent))
      .map(stat);
    const start = Number(url.searchParams.get("pageToken") ?? "0");
    const page = all.slice(start, start + PAGE_SIZE);
    const next = start + PAGE_SIZE < all.length ? { nextPageToken: String(start + PAGE_SIZE) } : {};
    return { body: { ...next, files: page } };
  });

  server.on("POST", "/drive/v3/files", (req) => {
    const no = denied(req);
    if (no) return no;
    const meta = JSON.parse(req.body) as { name?: string; mimeType?: string; appProperties?: Record<string, string> };
    return { body: { id: add({ name: String(meta.name), parents: [], mimeType: meta.mimeType, appProperties: meta.appProperties }) } };
  });

  const fallback: FakeHandler = (req) => {
    const path = req.path.split("?")[0];
    if (!path.startsWith("/drive/v3/") && !path.startsWith("/upload/drive/v3/")) {
      return { status: 404, body: { error: { message: `unrouted ${req.method} ${req.path}` } } };
    }
    const no = denied(req);
    if (no) return no;

    const fileMatch = /^\/drive\/v3\/files\/([^/]+)$/.exec(path);
    if (fileMatch) {
      const f = files.get(fileMatch[1]);
      if (!f) return { status: 404, body: { error: { message: "File not found" } } };
      if (req.method === "PATCH") {
        const meta = JSON.parse(req.body) as { trashed?: boolean };
        if (meta.trashed) f.trashed = true;
        return { body: stat(f) };
      }
      if (req.method === "GET" && new URL(req.path, "http://fake").searchParams.get("alt") === "media") {
        return { body: f.data };
      }
      return { body: { md5Checksum: md5(f.data) } };
    }

    const uploadMatch = /^\/upload\/drive\/v3\/files(?:\/([^/]+))?$/.exec(path);
    if (uploadMatch) {
      const boundary = /boundary=(.+)$/.exec(header(req, "content-type"))?.[1] ?? "";
      const { meta, data } = parseMultipart(req.body, boundary);
      const id = uploadMatch[1];
      if (id === undefined) {
        const newId = add({
          name: String(meta.name),
          parents: (meta.parents as string[] | undefined) ?? [],
          appProperties: meta.appProperties as Record<string, string> | undefined,
          data,
          modifiedTime: meta.modifiedTime === undefined ? SEED_TIME : String(meta.modifiedTime),
        });
        return { body: stat(files.get(newId)!) };
      }
      const f = files.get(id);
      if (!f) return { status: 404, body: { error: { message: "File not found" } } };
      f.data = data;
      if (meta.modifiedTime !== undefined) f.modifiedTime = String(meta.modifiedTime);
      return { body: stat(f) };
    }

    return { status: 404, body: { error: { message: `unrouted ${req.method} ${req.path}` } } };
  };
  server.onFallback(fallback);

  return {
    files() {
      const folder = appFolder();
      const out: Record<string, string> = {};
      if (!folder) return out;
      const newest = new Map<string, FakeFile>();
      for (const f of files.values()) {
        if (f.trashed || f.mimeType === FOLDER_MIME || !f.parents.includes(folder)) continue;
        const seen = newest.get(f.name);
        if (!seen || Date.parse(f.modifiedTime) > Date.parse(seen.modifiedTime)) newest.set(f.name, f);
      }
      for (const [name, f] of newest) out[name] = f.data;
      return out;
    },
    folderId: appFolder,
    mutations() {
      return server.requests
        .filter((r) => r.method !== "GET")
        .map((r) => ({ method: r.method, path: r.path, body: driveBody(r) }));
    },
  };
}
