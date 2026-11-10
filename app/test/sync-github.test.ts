import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  gitBlobSha,
  githubProvider,
  pollDeviceFlow,
  setupRepo,
  startDeviceFlow,
  type DeviceCode,
  type GithubDeps,
} from "../server/sync/github.ts";
import { syncPass } from "../server/sync/engine.ts";
import { RemoteMovedError, type Manifest } from "../server/sync/types.ts";

const FULL = "davide/proxi-data";
const SEED_DATE = "2026-03-01T10:00:00Z";
const NEW_DATE = "2026-03-02T10:00:00Z";
const README = Buffer.from("# proxi-data\n\nDati di studio sincronizzati da Proxi.\n");

type Entry = { sha: string; size: number; mode: string };
type Commit = { tree: string; date: string; parents: string[] };
type LogEntry = { method: string; path: string; body?: any; auth?: string };

/** In-memory GitHub: blobs, trees, commits and one branch ref, plus the device-flow endpoints. */
function fakeGithub(
  opts: {
    repo?: "private" | "public" | "none";
    seed?: Record<string, string>;
    symlinks?: string[];
    emptyRepo?: boolean;
    deviceReplies?: Record<string, unknown>[];
  } = {},
) {
  const log: LogEntry[] = [];
  const sleeps: number[] = [];
  const blobs = new Map<string, Buffer>();
  const trees = new Map<string, Map<string, Entry>>();
  const commits = new Map<string, Commit>();
  const st = {
    repo: opts.repo ?? ("private" as "private" | "public" | "none"),
    ref: undefined as string | undefined,
    refMoved: false,
    rateLimitOnce: false,
    truncated: false,
    deviceReplies: [...(opts.deviceReplies ?? [])],
  };
  let seq = 0;
  const newId = (kind: string) => `${kind}${++seq}`;

  function putBlob(data: Buffer): string {
    const s = gitBlobSha(data);
    blobs.set(s, data);
    return s;
  }
  function putCommit(tree: Map<string, Entry>, parents: string[], date: string): string {
    const treeSha = newId("tree");
    trees.set(treeSha, new Map(tree));
    const c = newId("commit");
    commits.set(c, { tree: treeSha, date, parents });
    return c;
  }

  if (st.repo !== "none") {
    const tree = new Map<string, Entry>();
    for (const [rel, text] of Object.entries(opts.seed ?? {})) {
      const data = Buffer.from(text);
      tree.set(rel, { sha: putBlob(data), size: data.length, mode: "100644" });
    }
    for (const rel of opts.symlinks ?? []) {
      const data = Buffer.from("target");
      tree.set(rel, { sha: putBlob(data), size: data.length, mode: "120000" });
    }
    if (!opts.emptyRepo) st.ref = putCommit(tree, [], SEED_DATE);
  }

  const json = (status: number, body: unknown, headers: Record<string, string> = {}) =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json", ...headers },
    });

  const fetchFn = async (input: string | URL | Request, init?: RequestInit): Promise<Response> => {
    const url = new URL(String(input));
    const method = (init?.method ?? "GET").toUpperCase();
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    const p = url.pathname + url.search;
    const headers = (init?.headers ?? {}) as Record<string, string>;
    log.push({ method, path: p, body, auth: headers.Authorization });

    if (url.host === "github.com") {
      if (p === "/login/device/code") {
        return json(200, {
          device_code: "dev-1",
          user_code: "WDJB-MJHT",
          verification_uri: "https://github.com/login/device",
          expires_in: 900,
          interval: 5,
        });
      }
      if (p === "/login/oauth/access_token") {
        return json(200, st.deviceReplies.shift() ?? { error: "authorization_pending" });
      }
      return json(404, { message: "unhandled" });
    }

    if (p === "/user") {
      if (st.rateLimitOnce) {
        st.rateLimitOnce = false;
        return new Response("{}", { status: 403, headers: { "retry-after": "1" } });
      }
      return json(200, { login: "davide" });
    }
    if (p === `/repos/${FULL}` && method === "GET") {
      if (st.repo === "none") return json(404, { message: "Not Found" });
      return json(200, { full_name: FULL, private: st.repo === "private", default_branch: "main" });
    }
    if (p === "/user/repos" && method === "POST") {
      st.repo = body.private ? "private" : "public";
      if (body.auto_init) {
        st.ref = putCommit(new Map([["README.md", { sha: putBlob(README), size: README.length, mode: "100644" }]]), [], SEED_DATE);
      }
      return json(201, { full_name: FULL, private: body.private, default_branch: "main" });
    }

    const repoPrefix = `/repos/${FULL}/`;
    if (!p.startsWith(repoPrefix)) return json(404, { message: "unhandled" });
    if (st.repo === "none") return json(404, { message: "Not Found" });
    const rest = p.slice(repoPrefix.length);

    if (rest === "git/ref/heads/main" && method === "GET") {
      if (!st.ref) return json(409, { message: "Git Repository is empty." });
      return json(200, { object: { sha: st.ref } });
    }
    if (rest === "contents/README.md" && method === "PUT") {
      const data = Buffer.from(body.content, "base64");
      const tree = new Map(st.ref ? trees.get(commits.get(st.ref)!.tree)! : []);
      tree.set("README.md", { sha: putBlob(data), size: data.length, mode: "100644" });
      st.ref = putCommit(tree, st.ref ? [st.ref] : [], NEW_DATE);
      return json(201, {});
    }
    if (rest.startsWith("git/commits/") && method === "GET") {
      const c = commits.get(rest.slice("git/commits/".length));
      if (!c) return json(404, { message: "Not Found" });
      return json(200, { tree: { sha: c.tree }, committer: { date: c.date } });
    }
    if (rest.startsWith("git/trees/") && method === "GET") {
      const t = trees.get(rest.slice("git/trees/".length).split("?")[0]);
      if (!t) return json(404, { message: "Not Found" });
      return json(200, {
        truncated: st.truncated,
        tree: [...t].map(([path, e]) => ({ path, mode: e.mode, type: "blob", sha: e.sha, size: e.size })),
      });
    }
    if (rest.startsWith("git/blobs/") && method === "GET") {
      const b = blobs.get(rest.slice("git/blobs/".length));
      if (!b) return json(404, { message: "Not Found" });
      return json(200, { content: b.toString("base64"), encoding: "base64" });
    }
    if (rest === "git/blobs" && method === "POST") {
      return json(201, { sha: putBlob(Buffer.from(body.content, "base64")) });
    }
    if (rest === "git/trees" && method === "POST") {
      const t = new Map(trees.get(body.base_tree) ?? []);
      for (const e of body.tree as { path: string; mode: string; sha: string | null }[]) {
        if (e.sha === null) t.delete(e.path);
        else t.set(e.path, { sha: e.sha, size: blobs.get(e.sha)!.length, mode: e.mode });
      }
      const s = newId("tree");
      trees.set(s, t);
      return json(201, { sha: s });
    }
    if (rest === "git/commits" && method === "POST") {
      const s = newId("commit");
      commits.set(s, { tree: body.tree, date: NEW_DATE, parents: body.parents });
      return json(201, { sha: s });
    }
    if (rest === "git/refs/heads/main" && method === "PATCH") {
      if (st.refMoved) return json(422, { message: "Update is not a fast forward" });
      if (body.force !== false) return json(400, { message: "force must be false" });
      st.ref = body.sha;
      return json(200, { object: { sha: body.sha } });
    }
    return json(404, { message: "unhandled" });
  };

  const deps: GithubDeps = {
    fetch: fetchFn as unknown as typeof fetch,
    sleep: async (ms: number) => {
      sleeps.push(ms);
    },
  };
  const count = (method: string, re: RegExp) => log.filter((e) => e.method === method && re.test(e.path)).length;
  return { deps, log, sleeps, st, count };
}

const CONN = { token: "tok", repo: FULL, branch: "main" };

test("gitBlobSha matches git's blob id", () => {
  assert.equal(gitBlobSha(Buffer.from("hello\n")), "ce013625030ba8dba906f756967f9e9ca394464a");
});

test("device flow: pending, then slow_down with a new interval, then token", async () => {
  const gh = fakeGithub({
    deviceReplies: [{ error: "authorization_pending" }, { error: "slow_down", interval: 10 }, { access_token: "tok" }],
  });
  const d = await startDeviceFlow(gh.deps, "cid");
  assert.deepEqual(d, {
    deviceCode: "dev-1",
    userCode: "WDJB-MJHT",
    verificationUri: "https://github.com/login/device",
    expiresIn: 900,
    interval: 5,
  });
  const token = await pollDeviceFlow(gh.deps, "cid", d);
  assert.equal(token, "tok");
  assert.deepEqual(gh.sleeps, [5000, 5000, 10000]);
});

test("device flow: slow_down without interval adds 5 seconds", async () => {
  const gh = fakeGithub({ deviceReplies: [{ error: "slow_down" }, { access_token: "t2" }] });
  const d = await startDeviceFlow(gh.deps, "cid");
  assert.equal(await pollDeviceFlow(gh.deps, "cid", d), "t2");
  assert.deepEqual(gh.sleeps, [5000, 10000]);
});

test("device flow: expired_token, access_denied and the local expiry", async () => {
  const d: DeviceCode = { deviceCode: "dev-1", userCode: "X", verificationUri: "u", expiresIn: 900, interval: 5 };

  const expired = fakeGithub({ deviceReplies: [{ error: "expired_token" }] });
  await assert.rejects(pollDeviceFlow(expired.deps, "cid", d), { message: "Codice scaduto: riprova." });

  const denied = fakeGithub({ deviceReplies: [{ error: "access_denied" }] });
  await assert.rejects(pollDeviceFlow(denied.deps, "cid", d), { message: "Accesso a GitHub negato." });

  const pending = fakeGithub({ deviceReplies: Array.from({ length: 5 }, () => ({ error: "authorization_pending" })) });
  const short: DeviceCode = { ...d, expiresIn: 10 };
  await assert.rejects(pollDeviceFlow(pending.deps, "cid", short), { message: "Codice scaduto: riprova." });
  assert.equal(pending.sleeps.length, 2);
});

test("device flow: abort before the poll and during the wait", async () => {
  const d: DeviceCode = { deviceCode: "dev-1", userCode: "X", verificationUri: "u", expiresIn: 900, interval: 5 };
  const gh = fakeGithub();

  const before = new AbortController();
  before.abort();
  await assert.rejects(pollDeviceFlow(gh.deps, "cid", d, before.signal), { message: "Annullato." });

  const during = new AbortController();
  const deps: GithubDeps = { ...gh.deps, sleep: async () => during.abort() };
  await assert.rejects(pollDeviceFlow(deps, "cid", d, during.signal), { message: "Annullato." });
});

test("device flow refuses an empty client id", async () => {
  await assert.rejects(startDeviceFlow({}, ""), /Client GitHub non configurato/);
  // the message names both places to set it: the source constant and the settings.json override
  await assert.rejects(startDeviceFlow({}, ""), (e: Error) => {
    assert.match(e.message, /GITHUB_CLIENT_ID in app\/server\/sync\/github\.ts/);
    assert.match(e.message, /githubClientId in \.learner\/settings\.json/);
    assert.doesNotMatch(e.message, /nelle impostazioni/);
    return true;
  });
  await assert.rejects(
    pollDeviceFlow({}, "", { deviceCode: "d", userCode: "u", verificationUri: "v", expiresIn: 10, interval: 1 }),
    /Client GitHub non configurato/,
  );
});

test("setupRepo creates a private auto-initialised repo when none exists", async () => {
  const gh = fakeGithub({ repo: "none" });
  const r = await setupRepo(gh.deps, "tok");
  assert.deepEqual(r, { login: "davide", repo: FULL, branch: "main" });
  const lookup = gh.log.find((e) => e.method === "GET" && e.path.startsWith("/repos/"));
  assert.equal(lookup?.path, "/repos/davide/proxi-data");
  const post = gh.log.find((e) => e.method === "POST" && e.path === "/user/repos");
  assert.ok(post);
  assert.equal(post.body.name, "proxi-data");
  assert.equal(post.body.private, true);
  assert.equal(post.body.auto_init, true);
  assert.equal(gh.log[0].auth, "Bearer tok");
});

test("setupRepo refuses a public repository", async () => {
  const gh = fakeGithub({ repo: "public" });
  await assert.rejects(setupRepo(gh.deps, "tok"), {
    message: "Il repository proxi-data è pubblico: rendilo privato o rinominalo, poi riprova.",
  });
});

test("list maps blobs with their blob sha and skips symlinks; read returns the bytes", async () => {
  const gh = fakeGithub({ seed: { "courses/a.md": "hi" }, symlinks: ["courses/link.md"] });
  const p = githubProvider(gh.deps, CONN);
  const m = await p.list();
  assert.deepEqual([...m.keys()], ["courses/a.md"]);
  const a = m.get("courses/a.md")!;
  assert.equal(a.size, 2);
  assert.equal(a.token, gitBlobSha(Buffer.from("hi")));
  assert.equal(a.mtimeMs, Date.parse(SEED_DATE));
  assert.equal((await p.read("courses/a.md")).toString(), "hi");
  await assert.rejects(p.read("nope.md"), /File remoto sconosciuto/);
});

test("list on an empty repo seeds README.md first", async () => {
  const gh = fakeGithub({ seed: {}, emptyRepo: true });
  const m = await githubProvider(gh.deps, CONN).list();
  assert.ok(m.has("README.md"));
  assert.equal(gh.count("PUT", /\/contents\/README\.md$/), 1);
});

test("commit with 2 writes and 1 removal makes exactly one commit", async () => {
  const gh = fakeGithub({ seed: { "courses/a.md": "a", "courses/old.md": "old" } });
  const p = githubProvider(gh.deps, CONN);
  await p.list();
  await p.write("courses/x.md", Buffer.from("x"), 0);
  await p.write("courses/y.md", Buffer.from("y"), 0);
  await p.remove("courses/old.md");
  const headBefore = gh.st.ref;
  gh.log.splice(0);

  await p.commit("sync: test");

  assert.equal(gh.count("POST", /\/git\/blobs$/), 2);
  assert.equal(gh.count("POST", /\/git\/trees$/), 1);
  assert.equal(gh.count("POST", /\/git\/commits$/), 1);
  assert.equal(gh.count("PATCH", /\/git\/refs\/heads\/main$/), 1);
  const tree = gh.log.find((e) => e.method === "POST" && /\/git\/trees$/.test(e.path))!.body;
  assert.ok(tree.tree.some((e: any) => e.path === "courses/old.md" && e.sha === null));
  const commit = gh.log.find((e) => e.method === "POST" && /\/git\/commits$/.test(e.path))!.body;
  assert.equal(commit.message, "sync: test");
  assert.deepEqual(commit.parents, [headBefore]);
  assert.equal(gh.log.find((e) => e.method === "PATCH")!.body.force, false);

  const after = await p.list();
  assert.deepEqual([...after.keys()].sort(), ["courses/a.md", "courses/x.md", "courses/y.md"]);
});

test("commit with nothing staged makes no request", async () => {
  const gh = fakeGithub({ seed: { "courses/a.md": "a" } });
  const p = githubProvider(gh.deps, CONN);
  await p.list();
  gh.log.splice(0);
  await p.commit("noop");
  assert.equal(gh.log.length, 0);
});

test("a 422 on the ref update throws RemoteMovedError", async () => {
  const gh = fakeGithub({ seed: { "courses/a.md": "a" } });
  const p = githubProvider(gh.deps, CONN);
  await p.list();
  await p.write("courses/b.md", Buffer.from("b"), 0);
  gh.st.refMoved = true;
  await assert.rejects(p.commit("m"), RemoteMovedError);
});

test("403 with retry-after waits the advertised seconds, then succeeds", async () => {
  const gh = fakeGithub();
  gh.st.rateLimitOnce = true;
  const r = await setupRepo(gh.deps, "tok");
  assert.equal(r.login, "davide");
  assert.deepEqual(gh.sleeps, [1000]);
});

test("two GitHub sync passes: the second is a no-op with no commit", async () => {
  const dataRoot = fs.mkdtempSync(path.join(os.tmpdir(), "gh-data-"));
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "gh-state-"));
  try {
    fs.mkdirSync(path.join(dataRoot, "courses", "demo"), { recursive: true });
    fs.writeFileSync(path.join(dataRoot, "courses", "demo", "MISSION.md"), "# demo\n");
    const gh = fakeGithub({ seed: {} });
    const provider = githubProvider(gh.deps, CONN);
    const opts = { dataRoot, provider, conflictsDir: path.join(tmp, "conflicts"), trashDir: path.join(tmp, "trash"), now: () => 1 };

    const m0: Manifest = { v: 2, key: provider.key, files: {} };
    const first = await syncPass({ ...opts, manifest: m0 });
    assert.equal(first.result.pushed, 1);
    assert.deepEqual(first.result.errors, []);
    assert.equal(gh.count("POST", /\/git\/commits$/), 1);

    gh.log.splice(0);
    const second = await syncPass({ ...opts, manifest: first.manifest });
    assert.deepEqual([second.result.pushed, second.result.pulled, second.result.deleted], [0, 0, 0]);
    assert.equal(gh.count("POST", /\/git\/commits$/), 0);
    assert.equal(gh.count("GET", /\/git\/blobs\//), 0);
  } finally {
    fs.rmSync(dataRoot, { recursive: true, force: true });
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
