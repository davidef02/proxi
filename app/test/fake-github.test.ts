import { test } from "node:test";
import assert from "node:assert/strict";
import { FakeHttpServer } from "./fake-servers.ts";
import { FAKE_DEVICE, installFakeGithub, type FakeGithub, type FakeGithubSeed } from "./fake-github.ts";
import { githubProvider, pollDeviceFlow, setupRepo, startDeviceFlow, type GithubDeps } from "../server/sync/github.ts";
import { fakeFetch } from "./harness.ts";
import { RemoteMovedError } from "../server/sync/types.ts";

const FULL = "tester/proxi-data";
const CONN = { token: "tok", repo: FULL, branch: "main" };
const README_TEXT = "# proxi-data\n\nDati di studio sincronizzati da Proxi.\n";

async function startFake(seed: FakeGithubSeed = {}): Promise<{ server: FakeHttpServer; gh: FakeGithub; deps: GithubDeps }> {
  const server = new FakeHttpServer("github");
  const url = await server.start();
  const gh = installFakeGithub(server, seed);
  return { server, gh, deps: { fetch: fakeFetch({ github: url }), sleep: async () => {} } };
}

test("setupRepo creates the private repo when missing", async () => {
  const { server, gh, deps } = await startFake({ repo: "none" });
  try {
    assert.deepEqual(await setupRepo(deps, "tok"), { login: "tester", repo: FULL, branch: "main" });
    const created = gh.mutations().find((m) => m.path === "/user/repos");
    assert.deepEqual(created?.body, {
      name: "proxi-data",
      private: true,
      auto_init: true,
      description: "Proxi learner data (synced by the app)",
    });
    assert.equal(server.requests[0].headers.authorization, "Bearer tok");
    assert.deepEqual(gh.files(), { "README.md": README_TEXT });
  } finally {
    await server.close();
  }
});

test("an empty repo is seeded with README.md on list", async () => {
  const { server, gh, deps } = await startFake();
  try {
    assert.equal(gh.head(), undefined);
    const m = await githubProvider(deps, CONN).list();
    assert.ok(m.has("README.md"));
    assert.equal(gh.mutations().filter((r) => r.method === "PUT" && r.path === `/repos/${FULL}/contents/README.md`).length, 1);
    assert.equal(gh.files()["README.md"], README_TEXT);
  } finally {
    await server.close();
  }
});

test("a commit with two writes and a removal updates files() and the next list", async () => {
  const { server, gh, deps } = await startFake({ files: { "courses/a.md": "a", "courses/old.md": "old" } });
  try {
    const p = githubProvider(deps, CONN);
    await p.list();
    await p.write("courses/x.md", Buffer.from("x"), 0);
    await p.write("courses/y.md", Buffer.from("y"), 0);
    await p.remove("courses/old.md");
    await p.commit("sync: test");

    assert.deepEqual(gh.files(), { "courses/a.md": "a", "courses/x.md": "x", "courses/y.md": "y" });
    const next = await p.list();
    assert.deepEqual([...next.keys()].sort(), ["courses/a.md", "courses/x.md", "courses/y.md"]);
  } finally {
    await server.close();
  }
});

test("blob reads decode the wrapped base64", async () => {
  const text = "x".repeat(200);
  const { server, deps } = await startFake({ files: { "courses/long.md": text } });
  try {
    const p = githubProvider(deps, CONN);
    const m = await p.list();
    const sha = m.get("courses/long.md")!.token;
    assert.equal((await p.read("courses/long.md")).toString(), text);

    const res = await fetch(`${server.url}/repos/${FULL}/git/blobs/${sha}`);
    const blob = (await res.json()) as { content: string; encoding: string };
    assert.equal(blob.encoding, "base64");
    assert.ok(blob.content.endsWith("\n"));
    const lines = blob.content.trimEnd().split("\n");
    assert.ok(lines.length > 1);
    for (const line of lines.slice(0, -1)) assert.equal(line.length, 60);
  } finally {
    await server.close();
  }
});

test("a ref update on a moved head answers 422 → RemoteMovedError", async () => {
  const { server, deps } = await startFake({ files: { "courses/a.md": "a" } });
  try {
    const a = githubProvider(deps, CONN);
    const b = githubProvider(deps, CONN);
    await a.list();
    await b.list();

    await a.write("courses/b.md", Buffer.from("b"), 0);
    await a.commit("a");
    await b.write("courses/c.md", Buffer.from("c"), 0);
    await assert.rejects(b.commit("b"), RemoteMovedError);
  } finally {
    await server.close();
  }
});

test("device flow returns FAKE_DEVICE and the token when deviceToken is set", async () => {
  const { server, deps } = await startFake({ deviceToken: "tok" });
  try {
    const d = await startDeviceFlow(deps, "cid");
    assert.deepEqual(d, {
      deviceCode: FAKE_DEVICE.device_code,
      userCode: FAKE_DEVICE.user_code,
      verificationUri: FAKE_DEVICE.verification_uri,
      expiresIn: FAKE_DEVICE.expires_in,
      interval: FAKE_DEVICE.interval,
    });
    assert.equal(await pollDeviceFlow(deps, "cid", d), "tok");

    const start = server.requests.find((r) => r.path === "/login/device/code")!;
    assert.deepEqual(JSON.parse(start.body), { client_id: "cid", scope: "repo" });
    const token = server.requests.find((r) => r.path === "/login/oauth/access_token")!;
    assert.equal((JSON.parse(token.body) as { device_code: string }).device_code, FAKE_DEVICE.device_code);
  } finally {
    await server.close();
  }
});

test("PATCH ref follows the fast-forward rule", async () => {
  const { server, gh } = await startFake({ files: { "courses/a.md": "a" } });
  const base = `${server.url}/repos/${FULL}`;
  const commit = async (parents: string[]): Promise<string> => {
    const res = await fetch(`${base}/git/commits`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ message: "c", tree: "tree1", parents }),
    });
    assert.equal(res.status, 201);
    return ((await res.json()) as { sha: string }).sha;
  };
  const patch = (body: unknown) =>
    fetch(`${base}/git/refs/heads/main`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  try {
    const head0 = gh.head()!;
    const first = await commit([head0]);
    const ok = await patch({ sha: first, force: false });
    assert.equal(ok.status, 200);
    assert.deepEqual(await ok.json(), { object: { sha: first } });
    assert.equal(gh.head(), first);

    const behind = await commit([head0]);
    assert.equal((await patch({ sha: behind, force: false })).status, 422);
    assert.equal(gh.head(), first);

    assert.equal((await patch({ sha: "unknown", force: false })).status, 422);
    assert.equal(gh.head(), first);

    const ahead = await commit([first]);
    assert.equal((await patch({ sha: ahead, force: true })).status, 422);
    assert.equal(gh.head(), first);
  } finally {
    await server.close();
  }
});
