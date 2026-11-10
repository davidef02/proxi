// Google Drive sync provider: PKCE OAuth (loopback), flat app folder, md5 change tokens.
import { createHash, randomBytes } from "node:crypto";
import { APP_ID, DATA_DIR_NAME, DRIVE_APP_PROPERTY } from "../../shared/identity.mjs";
import { RemoteMovedError, type RemoteStat, type SyncProvider } from "./types.ts";

export const GOOGLE_CLIENT_ID = "596427399450-dbi9kfci4g4hfmdic4l5o8shr6vetbna.apps.googleusercontent.com"; // human prerequisite: Desktop OAuth client
export const GOOGLE_CLIENT_SECRET = ""; // Desktop clients' secret is not confidential; "" = omit
export const DRIVE_SCOPE = "https://www.googleapis.com/auth/drive.file";
export const DRIVE_FOLDER: string = DATA_DIR_NAME;

const AUTH_URL = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL = "https://oauth2.googleapis.com/token";
const REVOKE_URL = "https://oauth2.googleapis.com/revoke";
const API = "https://www.googleapis.com/drive/v3";
const UPLOAD = "https://www.googleapis.com/upload/drive/v3";
const FOLDER_MIME = "application/vnd.google-apps.folder";
const FILE_FIELDS = "id,size,md5Checksum,modifiedTime";

export interface GoogleDeps {
  fetch?: typeof fetch;
  now?: () => number;
}

interface TokenResponse {
  access_token?: string;
  refresh_token?: string;
  expires_in?: number;
  error?: string;
  error_description?: string;
}

interface DriveFile {
  id: string;
  name?: string;
  size?: string;
  md5Checksum: string;
  modifiedTime: string;
}

const NO_CLIENT =
  "Client Google non configurato: imposta GOOGLE_CLIENT_ID in app/server/sync/gdrive.ts " +
  "oppure googleClientId in .learner/settings.json.";
const REVOKED = "Accesso a Google scaduto o revocato: riconnetti.";

const doFetch = (deps: GoogleDeps): typeof fetch => deps.fetch ?? globalThis.fetch;
const clock = (deps: GoogleDeps): (() => number) => deps.now ?? Date.now;

function requireClient(clientId: string): void {
  if (!clientId) throw new Error(NO_CLIENT);
}

/** PKCE pair (RFC 7636, S256). The 48 random bytes encode to exactly 64 url-safe chars. */
export function pkcePair(): { verifier: string; challenge: string } {
  const verifier = randomBytes(48).toString("base64url");
  return { verifier, challenge: createHash("sha256").update(verifier).digest("base64url") };
}

export function authUrl(o: { clientId: string; redirectUri: string; challenge: string; state: string }): string {
  requireClient(o.clientId);
  const params = new URLSearchParams({
    client_id: o.clientId,
    redirect_uri: o.redirectUri,
    response_type: "code",
    scope: DRIVE_SCOPE,
    code_challenge: o.challenge,
    code_challenge_method: "S256",
    state: o.state,
    access_type: "offline",
    prompt: "consent",
  });
  return `${AUTH_URL}?${params}`;
}

async function tokenRequest(deps: GoogleDeps, form: Record<string, string>): Promise<TokenResponse> {
  const res = await doFetch(deps)(TOKEN_URL, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams(form),
  });
  const json = (await res.json().catch(() => ({}))) as TokenResponse;
  if (!res.ok) {
    if (json.error === "invalid_grant") throw new Error(REVOKED);
    throw new Error(`Google ${res.status}: ${json.error_description ?? json.error ?? "errore sconosciuto"}`);
  }
  return json;
}

export async function exchangeCode(
  deps: GoogleDeps,
  o: { clientId: string; clientSecret?: string; code: string; verifier: string; redirectUri: string },
): Promise<{ refreshToken: string; accessToken: string; expiresAt: number }> {
  requireClient(o.clientId);
  const json = await tokenRequest(deps, {
    code: o.code,
    client_id: o.clientId,
    ...(o.clientSecret ? { client_secret: o.clientSecret } : {}),
    redirect_uri: o.redirectUri,
    grant_type: "authorization_code",
    code_verifier: o.verifier,
  });
  if (!json.refresh_token) throw new Error("Google non ha restituito un refresh token: riprova.");
  if (!json.access_token) throw new Error("Google non ha restituito un access token: riprova.");
  return {
    refreshToken: json.refresh_token,
    accessToken: json.access_token,
    expiresAt: clock(deps)() + ((json.expires_in ?? 0) - 60) * 1000,
  };
}

export class GoogleAuth {
  private readonly deps: GoogleDeps;
  private readonly conf: { clientId: string; clientSecret?: string; refreshToken: string };
  private token?: { value: string; expiresAt: number };

  constructor(deps: GoogleDeps, conf: { clientId: string; clientSecret?: string; refreshToken: string }) {
    this.deps = deps;
    this.conf = conf;
  }

  async accessToken(force = false): Promise<string> {
    if (!force && this.token && clock(this.deps)() < this.token.expiresAt) return this.token.value;
    requireClient(this.conf.clientId);
    const json = await tokenRequest(this.deps, {
      client_id: this.conf.clientId,
      ...(this.conf.clientSecret ? { client_secret: this.conf.clientSecret } : {}),
      refresh_token: this.conf.refreshToken,
      grant_type: "refresh_token",
    });
    if (!json.access_token) throw new Error("Google non ha restituito un access token: riprova.");
    this.token = {
      value: json.access_token,
      expiresAt: clock(this.deps)() + ((json.expires_in ?? 0) - 60) * 1000,
    };
    return json.access_token;
  }
}

/** Best-effort: never throws. */
export async function revokeToken(deps: GoogleDeps, token: string): Promise<void> {
  try {
    await doFetch(deps)(REVOKE_URL, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({ token }),
    });
  } catch {
    // ignored: revoking is advisory, the local token is deleted regardless
  }
}

async function errorMessage(res: Response): Promise<string> {
  const text = await res.text().catch(() => "");
  try {
    const j = JSON.parse(text) as { error?: { message?: string } | string; error_description?: string };
    if (typeof j.error === "object" && j.error?.message) return j.error.message;
    return String(j.error_description ?? j.error ?? text);
  } catch {
    return text || res.statusText;
  }
}

/** Drive API call. On 401 the access token is refreshed once and the call retried. */
async function driveCall(
  deps: GoogleDeps,
  auth: GoogleAuth,
  url: string,
  init: { method?: string; headers?: Record<string, string>; body?: string | Uint8Array } = {},
): Promise<Response> {
  const send = (token: string) =>
    doFetch(deps)(url, {
      method: init.method ?? "GET",
      headers: { ...init.headers, Authorization: `Bearer ${token}` },
      body: init.body as BodyInit | undefined,
    });
  let res = await send(await auth.accessToken());
  if (res.status === 401) res = await send(await auth.accessToken(true));
  if (!res.ok) throw new Error(`Google Drive ${res.status}: ${await errorMessage(res)}`);
  return res;
}

export async function ensureFolder(deps: GoogleDeps, auth: GoogleAuth): Promise<string> {
  const q = `mimeType='${FOLDER_MIME}' and trashed=false and appProperties has { key='${DRIVE_APP_PROPERTY}' and value='data' }`;
  const found = await driveCall(deps, auth, `${API}/files?q=${encodeURIComponent(q)}&fields=files(id)&spaces=drive`);
  const { files } = (await found.json()) as { files?: { id: string }[] };
  if (files?.[0]) return files[0].id;
  const created = await driveCall(deps, auth, `${API}/files`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: DRIVE_FOLDER, mimeType: FOLDER_MIME, appProperties: { [DRIVE_APP_PROPERTY]: "data" } }),
  });
  return ((await created.json()) as { id: string }).id;
}

export async function driveEmail(deps: GoogleDeps, auth: GoogleAuth): Promise<string | undefined> {
  try {
    const res = await driveCall(deps, auth, `${API}/about?fields=user(emailAddress)`);
    const j = (await res.json()) as { user?: { emailAddress?: string } };
    return j.user?.emailAddress;
  } catch {
    return undefined;
  }
}

/** multipart/related upload: part 1 is the JSON metadata, part 2 the file bytes. */
async function upload(
  deps: GoogleDeps,
  auth: GoogleAuth,
  url: string,
  method: "POST" | "PATCH",
  meta: Record<string, unknown>,
  data: Buffer,
): Promise<DriveFile> {
  const boundary = `${APP_ID}-${randomBytes(12).toString("hex")}`;
  const body = Buffer.concat([
    Buffer.from(
      `--${boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n${JSON.stringify(meta)}\r\n` +
        `--${boundary}\r\nContent-Type: application/octet-stream\r\n\r\n`,
    ),
    data,
    Buffer.from(`\r\n--${boundary}--`),
  ]);
  const res = await driveCall(deps, auth, url, {
    method,
    headers: { "Content-Type": `multipart/related; boundary=${boundary}` },
    body,
  });
  return (await res.json()) as DriveFile;
}

const md5Hex = (data: Buffer): string => createHash("md5").update(data).digest("hex");

export function gdriveProvider(deps: GoogleDeps, auth: GoogleAuth, folderId: string): SyncProvider {
  // Filled by list(): rel -> every Drive file id with that name, newest first (duplicates happen when two PCs push the
  // same new file before either lists it), and rel -> md5 of the newest at list time (the write-time guard).
  const ids = new Map<string, string[]>();
  const tokens = new Map<string, string>();

  return {
    key: `gdrive:${folderId}`,

    async list(): Promise<Map<string, RemoteStat>> {
      ids.clear();
      tokens.clear();
      const out = new Map<string, RemoteStat>();
      const found = new Map<string, { id: string; md5: string; size: number; mtimeMs: number }[]>();
      let pageToken: string | undefined;
      do {
        const q = `'${folderId}' in parents and trashed=false`;
        let url =
          `${API}/files?q=${encodeURIComponent(q)}` +
          `&fields=${encodeURIComponent("nextPageToken,files(id,name,size,md5Checksum,modifiedTime)")}` +
          `&pageSize=1000&spaces=drive`;
        if (pageToken) url += `&pageToken=${encodeURIComponent(pageToken)}`;
        const page = (await (await driveCall(deps, auth, url)).json()) as {
          nextPageToken?: string;
          files?: { id: string; name: string; size?: string; md5Checksum?: string; modifiedTime: string }[];
        };
        for (const f of page.files ?? []) {
          if (!f.md5Checksum) continue; // folders and Google-native docs
          const copies = found.get(f.name) ?? [];
          copies.push({ id: f.id, md5: f.md5Checksum, size: Number(f.size), mtimeMs: Date.parse(f.modifiedTime) });
          found.set(f.name, copies);
        }
        pageToken = page.nextPageToken;
      } while (pageToken);
      for (const [name, copies] of found) {
        copies.sort((a, b) => b.mtimeMs - a.mtimeMs); // duplicate names: the newest is the file, the rest are kept for remove()
        const newest = copies[0];
        ids.set(name, copies.map((c) => c.id));
        tokens.set(name, newest.md5);
        out.set(name, { size: newest.size, mtimeMs: newest.mtimeMs, token: newest.md5 });
      }
      return out;
    },

    async read(rel: string): Promise<Buffer> {
      const id = ids.get(rel)?.[0];
      if (!id) throw new Error(`File remoto sconosciuto: ${rel}`);
      const res = await driveCall(deps, auth, `${API}/files/${id}?alt=media`);
      return Buffer.from(await res.arrayBuffer());
    },

    async write(rel: string, data: Buffer, mtimeMs: number): Promise<RemoteStat> {
      const modifiedTime = new Date(mtimeMs).toISOString();
      const id = ids.get(rel)?.[0];
      let file: DriveFile;
      if (id === undefined) {
        file = await upload(
          deps,
          auth,
          `${UPLOAD}/files?uploadType=multipart&fields=${FILE_FIELDS}`,
          "POST",
          { name: rel, parents: [folderId], appProperties: { [DRIVE_APP_PROPERTY]: "file" }, modifiedTime },
          data,
        );
      } else {
        // Guard: refuse to overwrite a file another PC changed since list(). The check and the PATCH are two
        // requests, so a change in between is not caught (a small race window; Drive has no compare-and-swap here).
        const cur = (await (await driveCall(deps, auth, `${API}/files/${id}?fields=md5Checksum`)).json()) as {
          md5Checksum?: string;
        };
        if (cur.md5Checksum !== tokens.get(rel)) throw new RemoteMovedError(`${rel} changed remotely`);
        file = await upload(
          deps,
          auth,
          `${UPLOAD}/files/${id}?uploadType=multipart&fields=${FILE_FIELDS}`,
          "PATCH",
          { modifiedTime },
          data,
        );
      }
      const others = (ids.get(rel) ?? []).filter((x) => x !== file.id);
      ids.set(rel, [file.id, ...others]);
      tokens.set(rel, file.md5Checksum);
      return { size: Number(file.size ?? data.length), mtimeMs: Date.parse(file.modifiedTime), token: file.md5Checksum };
    },

    async remove(rel: string): Promise<void> {
      const all = ids.get(rel);
      if (!all?.length) return;
      // Every copy goes, not only the newest: a surviving duplicate would reappear on the next list() with old bytes.
      for (const id of all) {
        await driveCall(deps, auth, `${API}/files/${id}`, {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ trashed: true }),
        });
      }
      ids.delete(rel);
      tokens.delete(rel);
    },

    async commit(): Promise<void> {
      // Drive has no batch commit: every write and removal is already applied.
    },

    contentEquals(data: Buffer, r: RemoteStat): boolean {
      return md5Hex(data) === r.token;
    },
  };
}
