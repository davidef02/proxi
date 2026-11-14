/**
 * Stateful fake GitHub on a FakeHttpServer: device flow, one `proxi-data` repo, blobs, trees, commits and a main ref.
 */
import * as crypto from "node:crypto";
import { FakeHttpServer, type FakeHandler, type RecordedRequest } from "./fake-servers.ts";

export interface FakeGithubSeed {
  /** Default "tester". */
  login?: string;
  /** Default "private". */
  repo?: "private" | "public" | "none";
  /** Initial commit; omitted → empty repo with no ref. */
  files?: Record<string, string>;
  deviceError?: { error: string; error_description?: string };
  deviceToken?: string;
}

export const FAKE_DEVICE = {
  device_code: "fake-device-code",
  user_code: "WXYZ-1234",
  verification_uri: "https://github.com/login/device",
  expires_in: 900,
  interval: 1,
};

export interface FakeGithub {
  /** The head commit tree, utf8. */
  files(): Record<string, string>;
  /** The head commit sha, or undefined for an empty repo. */
  head(): string | undefined;
  /** Non-GET requests to /repos/... and /user/repos, bodies JSON-parsed. */
  mutations(): { method: string; path: string; body: unknown }[];
}

type Entry = { sha: string; size: number; mode: string };
type Commit = { tree: string; date: string; parents: string[] };

const SEED_DATE = "2026-01-01T00:00:00Z";
const NEW_DATE = "2026-01-02T00:00:00Z";
const README = "# proxi-data\n\nDati di studio sincronizzati da Proxi.\n";

/** Same id git gives a blob: sha1("blob <len>\0" + data). */
function blobSha(data: Buffer): string {
  return crypto
    .createHash("sha1")
    .update(Buffer.concat([Buffer.from(`blob ${data.length}\0`), data]))
    .digest("hex");
}

/** GitHub wraps base64 every 60 chars and adds a trailing newline. */
function wrappedBase64(data: Buffer): string {
  const lines = data.toString("base64").match(/.{1,60}/g) ?? [];
  return lines.map((line) => `${line}\n`).join("");
}

function parseBody(req: RecordedRequest): any {
  try {
    return req.body ? JSON.parse(req.body) : undefined;
  } catch {
    return undefined;
  }
}

/** The flat tree GitHub returns: blobs plus one tree entry per directory. */
function treeEntries(tree: Map<string, Entry>): { path: string; mode: string; type: string; sha: string; size?: number }[] {
  const dirs = new Set<string>();
  for (const rel of tree.keys()) {
    const parts = rel.split("/");
    for (let i = 1; i < parts.length; i++) dirs.add(parts.slice(0, i).join("/"));
  }
  const all = [
    ...[...tree].map(([path, e]) => ({ path, mode: e.mode, type: "blob", sha: e.sha, size: e.size as number | undefined })),
    ...[...dirs].map((path) => ({ path, mode: "040000", type: "tree", sha: `dir-${path}`, size: undefined })),
  ];
  return all.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
}

export function installFakeGithub(server: FakeHttpServer, seed: FakeGithubSeed = {}): FakeGithub {
  const login = seed.login ?? "tester";
  const full = `${login}/proxi-data`;
  const blobs = new Map<string, Buffer>();
  const trees = new Map<string, Map<string, Entry>>();
  const commits = new Map<string, Commit>();
  const st = { repo: seed.repo ?? "private", ref: undefined as string | undefined };
  let seq = 0;
  const newId = (kind: string) => `${kind}${++seq}`;

  function putBlob(data: Buffer): string {
    const sha = blobSha(data);
    blobs.set(sha, data);
    return sha;
  }

  function putCommit(tree: Map<string, Entry>, parents: string[], date: string): string {
    const treeSha = newId("tree");
    trees.set(treeSha, new Map(tree));
    const commitSha = newId("commit");
    commits.set(commitSha, { tree: treeSha, date, parents });
    return commitSha;
  }

  function headTree(): Map<string, Entry> {
    const commit = st.ref ? commits.get(st.ref) : undefined;
    return new Map(commit ? trees.get(commit.tree) : undefined);
  }

  if (st.repo !== "none" && seed.files) {
    const tree = new Map<string, Entry>();
    for (const [rel, text] of Object.entries(seed.files)) {
      const data = Buffer.from(text);
      tree.set(rel, { sha: putBlob(data), size: data.length, mode: "100644" });
    }
    st.ref = putCommit(tree, [], SEED_DATE);
  }

  server.on("POST", "/login/device/code", () => (seed.deviceError ? { status: 400, body: seed.deviceError } : { body: FAKE_DEVICE }));
  server.on("POST", "/login/oauth/access_token", () =>
    seed.deviceToken
      ? { body: { access_token: seed.deviceToken, token_type: "bearer", scope: "repo" } }
      : { body: { error: "authorization_pending" } },
  );
  server.on("GET", "/user", () => ({ body: { login } }));
  server.on("GET", `/repos/${full}`, () =>
    st.repo === "none"
      ? { status: 404, body: { message: "Not Found" } }
      : { body: { full_name: full, private: st.repo === "private", default_branch: "main" } },
  );
  server.on("POST", "/user/repos", (req) => {
    const body = parseBody(req) ?? {};
    st.repo = body.private ? "private" : "public";
    if (body.auto_init) {
      const data = Buffer.from(README);
      st.ref = putCommit(new Map([["README.md", { sha: putBlob(data), size: data.length, mode: "100644" }]]), [], SEED_DATE);
    }
    return { status: 201, body: { full_name: full, private: body.private, default_branch: "main" } };
  });

  const repoPrefix = `/repos/${full}/`;
  const fallback: FakeHandler = (req) => {
    const path = req.path.split("?")[0];
    if (!path.startsWith(repoPrefix) || st.repo === "none") return { status: 404, body: { message: "Not Found" } };
    const rest = path.slice(repoPrefix.length);
    const body = parseBody(req);

    if (rest === "git/ref/heads/main" && req.method === "GET") {
      if (!st.ref) return { status: 409, body: { message: "Git Repository is empty." } };
      return { body: { object: { sha: st.ref } } };
    }
    if (rest === "contents/README.md" && req.method === "PUT") {
      const data = Buffer.from(body.content, "base64");
      const tree = headTree();
      tree.set("README.md", { sha: putBlob(data), size: data.length, mode: "100644" });
      st.ref = putCommit(tree, st.ref ? [st.ref] : [], NEW_DATE);
      return { status: 201, body: {} };
    }
    if (rest.startsWith("git/commits/") && req.method === "GET") {
      const sha = rest.slice("git/commits/".length);
      const commit = commits.get(sha);
      if (!commit) return { status: 404, body: { message: "Not Found" } };
      return {
        body: { sha, tree: { sha: commit.tree }, committer: { date: commit.date }, parents: commit.parents.map((p) => ({ sha: p })) },
      };
    }
    if (rest.startsWith("git/trees/") && req.method === "GET") {
      const sha = rest.slice("git/trees/".length);
      const tree = trees.get(sha);
      if (!tree) return { status: 404, body: { message: "Not Found" } };
      return { body: { sha, truncated: false, tree: treeEntries(tree) } };
    }
    if (rest.startsWith("git/blobs/") && req.method === "GET") {
      const sha = rest.slice("git/blobs/".length);
      const data = blobs.get(sha);
      if (!data) return { status: 404, body: { message: "Not Found" } };
      return { body: { sha, size: data.length, encoding: "base64", content: wrappedBase64(data) } };
    }
    if (rest === "git/blobs" && req.method === "POST") {
      return { status: 201, body: { sha: putBlob(Buffer.from(body.content, "base64")) } };
    }
    if (rest === "git/trees" && req.method === "POST") {
      const tree = new Map<string, Entry>(trees.get(body.base_tree) ?? []);
      for (const e of body.tree as { path: string; mode: string; sha: string | null }[]) {
        if (e.sha === null) tree.delete(e.path);
        else tree.set(e.path, { sha: e.sha, size: blobs.get(e.sha)!.length, mode: e.mode });
      }
      const sha = newId("tree");
      trees.set(sha, tree);
      return { status: 201, body: { sha } };
    }
    if (rest === "git/commits" && req.method === "POST") {
      const sha = newId("commit");
      commits.set(sha, { tree: body.tree, date: NEW_DATE, parents: body.parents });
      return { status: 201, body: { sha } };
    }
    if (rest === "git/refs/heads/main" && req.method === "PATCH") {
      const commit = typeof body?.sha === "string" ? commits.get(body.sha) : undefined;
      // fast-forward only, and the provider must ask for it explicitly
      if (!commit || commit.parents[0] !== st.ref || body.force !== false) {
        return { status: 422, body: { message: "Update is not a fast forward" } };
      }
      st.ref = body.sha;
      return { body: { object: { sha: st.ref } } };
    }
    return { status: 404, body: { message: "Not Found" } };
  };
  server.onFallback(fallback);

  return {
    files() {
      const out: Record<string, string> = {};
      for (const [rel, e] of headTree()) out[rel] = (blobs.get(e.sha) ?? Buffer.alloc(0)).toString("utf8");
      return out;
    },
    head: () => st.ref,
    mutations() {
      return server.requests
        .filter((r) => r.method !== "GET" && (r.path === "/user/repos" || r.path.startsWith("/repos/")))
        .map((r) => ({ method: r.method, path: r.path, body: parseBody(r) ?? null }));
    },
  };
}
