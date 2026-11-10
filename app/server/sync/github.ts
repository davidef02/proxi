// GitHub sync provider: device-flow login, a private `proxi-data` repo, and one commit per sync pass.
// The engine (engine.ts) decides what moves; this file only talks to the GitHub REST API.
import * as crypto from "node:crypto";
import { APP_ID, APP_NAME, DATA_DIR_NAME } from "../../shared/identity.mjs";
import { RemoteMovedError, type RemoteStat, type SyncProvider } from "./types.ts";

export const GITHUB_CLIENT_ID = "Ov23li8h0SouuYu9iGlp"; // human prerequisite: OAuth App with device flow enabled
export const DATA_REPO: string = DATA_DIR_NAME;

export interface GithubDeps {
  fetch?: typeof fetch;
  sleep?: (ms: number) => Promise<void>;
}

export interface DeviceCode {
  deviceCode: string;
  userCode: string;
  verificationUri: string;
  expiresIn: number;
  interval: number;
}

const API = "https://api.github.com";
const README_TEXT = `# ${DATA_REPO}\n\nDati di studio sincronizzati da ${APP_NAME}.\n`;

class GithubHttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
    this.name = "GithubHttpError";
  }
}

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

function deps(d: GithubDeps) {
  return {
    fetch: d.fetch ?? globalThis.fetch,
    sleep: d.sleep ?? defaultSleep,
  };
}

/** Git blob id of `data`: sha1("blob <len>\0" + data). Equals the `sha` GitHub reports for the blob. */
export function gitBlobSha(data: Buffer): string {
  return crypto
    .createHash("sha1")
    .update(Buffer.concat([Buffer.from(`blob ${data.length}\0`), data]))
    .digest("hex");
}

function assertClientId(clientId: string): void {
  if (!clientId) {
    throw new Error(
      "Client GitHub non configurato: imposta GITHUB_CLIENT_ID in app/server/sync/github.ts " +
        "oppure githubClientId in .learner/settings.json.",
    );
  }
}

export async function startDeviceFlow(d: GithubDeps, clientId: string): Promise<DeviceCode> {
  assertClientId(clientId);
  const { fetch: f } = deps(d);
  const res = await f("https://github.com/login/device/code", {
    method: "POST",
    headers: { Accept: "application/json", "Content-Type": "application/json" },
    body: JSON.stringify({ client_id: clientId, scope: "repo" }),
  });
  const j = (await res.json()) as Record<string, any>;
  if (!res.ok || j.error) throw new Error(j.error_description ?? j.error ?? `GitHub ${res.status}`);
  return {
    deviceCode: j.device_code,
    userCode: j.user_code,
    verificationUri: j.verification_uri,
    expiresIn: j.expires_in,
    interval: j.interval,
  };
}

export async function pollDeviceFlow(
  d: GithubDeps,
  clientId: string,
  code: DeviceCode,
  signal?: AbortSignal,
): Promise<string> {
  assertClientId(clientId);
  const { fetch: f, sleep } = deps(d);
  let interval = code.interval;
  let waited = 0;
  for (;;) {
    if (signal?.aborted) throw new Error("Annullato.");
    if (waited >= code.expiresIn * 1000) throw new Error("Codice scaduto: riprova.");
    await sleep(interval * 1000);
    waited += interval * 1000;
    if (signal?.aborted) throw new Error("Annullato.");
    const res = await f("https://github.com/login/oauth/access_token", {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json" },
      body: JSON.stringify({
        client_id: clientId,
        device_code: code.deviceCode,
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
      }),
    });
    const j = (await res.json()) as Record<string, any>;
    if (j.access_token) return j.access_token as string;
    switch (j.error) {
      case "authorization_pending":
        break;
      case "slow_down":
        interval = typeof j.interval === "number" ? j.interval : interval + 5;
        break;
      case "expired_token":
        throw new Error("Codice scaduto: riprova.");
      case "access_denied":
        throw new Error("Accesso a GitHub negato.");
      default:
        throw new Error(j.error_description ?? j.error ?? `GitHub ${res.status}`);
    }
  }
}

/** One GitHub API call. Waits out rate limits (403/429) at most twice, capped at 60 s per wait. */
async function gh<T = any>(d: GithubDeps, token: string, method: string, path: string, body?: unknown): Promise<T> {
  const { fetch: f, sleep } = deps(d);
  for (let attempt = 0; ; attempt++) {
    const headers: Record<string, string> = {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": APP_ID,
    };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    const res = await f(`${API}${path}`, {
      method,
      headers,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (res.status === 403 || res.status === 429) {
      const retryAfter = res.headers.get("retry-after");
      const reset = res.headers.get("x-ratelimit-reset");
      const remaining = res.headers.get("x-ratelimit-remaining");
      if (attempt < 2 && (retryAfter || remaining === "0")) {
        const secs = retryAfter
          ? Number(retryAfter)
          : Math.max(0, Number(reset ?? 0) - Math.floor(Date.now() / 1000));
        await sleep(Math.min(secs, 60) * 1000);
        continue;
      }
    }
    if (!res.ok) {
      const text = await res.text();
      let message = text;
      try {
        message = JSON.parse(text).message ?? text;
      } catch {
        // not JSON: keep the raw body
      }
      throw new GithubHttpError(res.status, `GitHub ${res.status}: ${message}`);
    }
    if (res.status === 204) return undefined as T;
    return (await res.json()) as T;
  }
}

export async function setupRepo(d: GithubDeps, token: string): Promise<{ login: string; repo: string; branch: string }> {
  const user = await gh<{ login: string }>(d, token, "GET", "/user");
  const login = user.login;
  let repo: { full_name: string; default_branch: string; private: boolean };
  try {
    repo = await gh(d, token, "GET", `/repos/${login}/${DATA_REPO}`);
  } catch (err) {
    if (!(err instanceof GithubHttpError) || err.status !== 404) throw err;
    repo = await gh(d, token, "POST", "/user/repos", {
      name: DATA_REPO,
      private: true,
      auto_init: true,
      description: `${APP_NAME} learner data (synced by the app)`,
    });
  }
  if (repo.private === false) {
    throw new Error(`Il repository ${DATA_REPO} è pubblico: rendilo privato o rinominalo, poi riprova.`);
  }
  return { login, repo: repo.full_name, branch: repo.default_branch };
}

export function githubProvider(
  d: GithubDeps,
  conn: { token: string; repo: string; branch: string },
): SyncProvider {
  let head: { commit: string; tree: string } | undefined;
  const entries = new Map<string, string>(); // rel -> blob sha at head
  const staged = new Map<string, Buffer | null>(); // rel -> bytes (write) or null (remove)
  const base = `/repos/${conn.repo}`;
  const refPath = `${base}/git/ref/heads/${conn.branch}`;

  async function headSha(): Promise<string> {
    try {
      return (await gh<{ object: { sha: string } }>(d, conn.token, "GET", refPath)).object.sha;
    } catch (err) {
      if (!(err instanceof GithubHttpError) || (err.status !== 404 && err.status !== 409)) throw err;
      // Empty repo: seed a README so the branch exists, then read the ref again.
      await gh(d, conn.token, "PUT", `${base}/contents/README.md`, {
        message: "init",
        content: Buffer.from(README_TEXT).toString("base64"),
        branch: conn.branch,
      });
      return (await gh<{ object: { sha: string } }>(d, conn.token, "GET", refPath)).object.sha;
    }
  }

  return {
    key: "github:" + conn.repo,

    async list(): Promise<Map<string, RemoteStat>> {
      staged.clear();
      const commitSha = await headSha();
      const commit = await gh<{ tree: { sha: string }; committer: { date: string } }>(
        d,
        conn.token,
        "GET",
        `${base}/git/commits/${commitSha}`,
      );
      const treeSha = commit.tree.sha;
      const tree = await gh<{
        truncated?: boolean;
        tree: { path: string; type: string; mode: string; sha: string; size?: number }[];
      }>(d, conn.token, "GET", `${base}/git/trees/${treeSha}?recursive=1`);
      if (tree.truncated) throw new Error("Repository troppo grande per la sincronizzazione.");

      head = { commit: commitSha, tree: treeSha };
      entries.clear();
      const mtimeMs = Date.parse(commit.committer.date);
      const out = new Map<string, RemoteStat>();
      for (const e of tree.tree) {
        if (e.type !== "blob" || e.mode === "120000") continue;
        entries.set(e.path, e.sha);
        out.set(e.path, { size: e.size ?? 0, mtimeMs, token: e.sha });
      }
      return out;
    },

    async read(rel: string): Promise<Buffer> {
      const sha = entries.get(rel);
      if (!sha) throw new Error(`File remoto sconosciuto: ${rel}`);
      const blob = await gh<{ content: string }>(d, conn.token, "GET", `${base}/git/blobs/${sha}`);
      return Buffer.from(blob.content, "base64");
    },

    async write(rel: string, data: Buffer, mtimeMs: number): Promise<RemoteStat> {
      staged.set(rel, data);
      return { size: data.length, mtimeMs, token: gitBlobSha(data) };
    },

    async remove(rel: string): Promise<void> {
      staged.set(rel, null);
    },

    contentEquals(data: Buffer, r: RemoteStat): boolean {
      return gitBlobSha(data) === r.token;
    },

    async commit(message: string): Promise<void> {
      if (staged.size === 0) return;
      if (!head) throw new Error("commit prima di list()");
      const tree: { path: string; mode: "100644"; type: "blob"; sha: string | null }[] = [];
      for (const [rel, data] of staged) {
        if (data === null) {
          tree.push({ path: rel, mode: "100644", type: "blob", sha: null });
          continue;
        }
        const blob = await gh<{ sha: string }>(d, conn.token, "POST", `${base}/git/blobs`, {
          content: data.toString("base64"),
          encoding: "base64",
        });
        tree.push({ path: rel, mode: "100644", type: "blob", sha: blob.sha });
      }
      const newTree = await gh<{ sha: string }>(d, conn.token, "POST", `${base}/git/trees`, {
        base_tree: head.tree,
        tree,
      });
      const newCommit = await gh<{ sha: string }>(d, conn.token, "POST", `${base}/git/commits`, {
        message,
        tree: newTree.sha,
        parents: [head.commit],
      });
      try {
        await gh(d, conn.token, "PATCH", `${base}/git/refs/heads/${conn.branch}`, {
          sha: newCommit.sha,
          force: false,
        });
      } catch (err) {
        if (err instanceof GithubHttpError && err.status === 422) throw new RemoteMovedError("ref moved");
        throw err;
      }
      for (const [rel, data] of staged) {
        if (data === null) entries.delete(rel);
        else entries.set(rel, gitBlobSha(data));
      }
      head = { commit: newCommit.sha, tree: newTree.sha };
      staged.clear();
    },
  };
}
