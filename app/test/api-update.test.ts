// GET/POST /api/update over plain HTTP: checks, refusals, fast-forward, rebuild and relaunch in both server modes.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import type { GlobalEvent } from "../shared/protocol.ts";
import { startServer, type TestServer } from "./harness.ts";

type UpdateEvent = Extract<GlobalEvent, { type: "update" }>;

const DIRTY_TREE =
  "Modifiche locali non salvate nel repository. Impossibile aggiornare senza rischiare di sovrascrivere il tuo lavoro.";
const DIVERGED = "I commit locali sono divergenti da quelli remoti. Impossibile eseguire un aggiornamento fast-forward.";

function execGit(cwd: string, args: string[]): string {
  return execFileSync("git", args, { cwd, encoding: "utf8", windowsHide: true }).trim();
}

function initGitRepo(dir: string): void {
  fs.mkdirSync(dir, { recursive: true });
  execGit(dir, ["init", "-b", "main"]);
  execGit(dir, ["config", "user.name", "Test User"]);
  execGit(dir, ["config", "user.email", "test@example.com"]);
  fs.writeFileSync(path.join(dir, "README.md"), "# Test\n");
  // The build writes target/release: keep it out of the dirty-tree check.
  fs.writeFileSync(path.join(dir, ".gitignore"), "target/\n");
  execGit(dir, ["add", "README.md", ".gitignore"]);
  execGit(dir, ["commit", "-m", "initial commit"]);
}

function cloneGitRepo(src: string, dest: string): void {
  execFileSync("git", ["clone", src, dest], { encoding: "utf8", windowsHide: true });
  execGit(dest, ["config", "user.name", "Test User"]);
  execGit(dest, ["config", "user.email", "test@example.com"]);
}

function commitFile(dir: string, name: string, content: string, message: string): void {
  fs.writeFileSync(path.join(dir, name), content);
  execGit(dir, ["add", name]);
  execGit(dir, ["commit", "-m", message]);
}

/** A remote and a fresh clone of it under `tmp`. */
function cloneFixture(tmp: string): { root: string; remote: string } {
  const remote = path.join(tmp, "remote");
  const root = path.join(tmp, "local");
  initGitRepo(remote);
  cloneGitRepo(remote, root);
  return { root, remote };
}

/**
 * One fresh tmp root and server per test: in binary mode a successful apply ends the process,
 * and Node's update status is process-global, so nothing can be shared.
 */
async function withRepo(
  setup: (tmp: string) => { root: string; remote?: string },
  fn: (server: TestServer, f: { tmp: string; root: string; remote: string }) => Promise<void>,
): Promise<void> {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "api-update-"));
  const data = path.join(tmp, "data");
  fs.mkdirSync(data);
  const { root, remote = "" } = setup(tmp);
  let server: TestServer | undefined;
  try {
    // Every successful apply rebuilds: the fake keeps cargo out of the fixture repo.
    server = await startServer({ repoRoot: root, dataRoot: data, bins: { cargo: fakeBuild(tmp) } });
    await fn(server, { tmp, root, remote });
  } finally {
    await server?.close();
    fs.rmSync(tmp, { recursive: true, force: true, maxRetries: 3 });
  }
}

function post(server: TestServer, p: string, body?: unknown): Promise<Response> {
  const data = body === undefined ? undefined : JSON.stringify(body);
  return fetch(`${server.url}${p}`, {
    method: "POST",
    headers: {
      "x-learner-client": "1",
      ...(data === undefined ? {} : { "content-type": "application/json" }),
    },
    body: data,
  });
}

/** Same poll as integration.test.ts. */
async function waitFor<T>(fn: () => T | undefined | false, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Rejects when `p` has not settled within `ms`; clears its timer either way. */
async function within(p: Promise<void>, ms: number, what: string): Promise<void> {
  let timer: NodeJS.Timeout | undefined;
  try {
    await Promise.race([
      p,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${what} within ${ms} ms`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** Minimal SSE client: `ready` resolves when the headers arrive, `close` aborts the stream. */
function sse(url: string) {
  const events: GlobalEvent[] = [];
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

/** Fake cargo: logs its command line next to the script, then writes a new exe in the repo root. */
function fakeBuild(dir: string): string {
  if (process.platform === "win32") {
    const script = path.join(dir, "fake-cargo.cmd");
    fs.writeFileSync(
      script,
      [
        "@echo off",
        '>>"%~dp0build-log.txt" echo %* GIT_TERMINAL_PROMPT=%GIT_TERMINAL_PROMPT%',
        "if not exist target\\release mkdir target\\release",
        "echo new build> target\\release\\proxi.exe",
        "exit /b 0",
        "",
      ].join("\r\n"),
    );
    return script;
  }
  const script = path.join(dir, "fake-cargo.sh");
  fs.writeFileSync(
    script,
    [
      "#!/bin/sh",
      'echo "$* GIT_TERMINAL_PROMPT=$GIT_TERMINAL_PROMPT" >> "$(dirname "$0")/build-log.txt"',
      "mkdir -p target/release && echo new build > target/release/proxi",
      "",
    ].join("\n"),
  );
  fs.chmodSync(script, 0o755);
  return script;
}

describe("/api/update", () => {
  // First in the file: Node's update status is process-global, so no earlier test may have checked.
  it("GET /api/update starts idle", async () => {
    await withRepo(
      (tmp) => ({ root: tmp }),
      async (server) => {
        const res = await fetch(`${server.url}/api/update`);
        assert.equal(res.status, 200);
        assert.deepEqual(await res.json(), { state: "idle", behind: 0, activeSessions: 0 });
      },
    );
  });

  it("check: up to date", async () => {
    await withRepo(cloneFixture, async (server) => {
      const res = await post(server, "/api/update/check");
      assert.equal(res.status, 200);
      const body = await res.json();
      assert.equal(body.state, "up_to_date");
      assert.equal(body.behind, 0);
      assert.deepEqual(Object.keys(body), ["state", "behind", "checkedAt", "activeSessions"]);
      assert.ok(!Number.isNaN(Date.parse(body.checkedAt)));
    });
  });

  it("check: available commits", async () => {
    await withRepo(cloneFixture, async (server, { remote }) => {
      commitFile(remote, "file1.txt", "hello\n", "feature: add file1");

      const res = await post(server, "/api/update/check");
      assert.equal(res.status, 200);
      const body = await res.json();
      assert.equal(body.state, "available");
      assert.equal(body.behind, 1);
      assert.deepEqual(Object.keys(body), ["state", "behind", "commits", "checkedAt", "activeSessions"]);
      assert.equal(body.commits[0].subject, "feature: add file1");
      assert.deepEqual(Object.keys(body.commits[0]), ["hash", "subject", "relativeTime"]);
      assert.ok(body.commits[0].hash);
      assert.ok(body.commits[0].relativeTime);
    });
  });

  it("check: a non-git directory is idle", async () => {
    await withRepo(
      (tmp) => ({ root: tmp }),
      async (server) => {
        const res = await post(server, "/api/update/check");
        assert.equal(res.status, 200);
        const body = await res.json();
        assert.equal(body.state, "idle");
        assert.equal(body.behind, 0);
      },
    );
  });

  it("check: a feature branch without upstream tracks origin/main", async () => {
    await withRepo(cloneFixture, async (server, { root, remote }) => {
      execGit(root, ["checkout", "-b", "feature-test-branch"]);
      commitFile(remote, "feature2.txt", "hello feature 2\n", "feature: add feature2");

      const res = await post(server, "/api/update/check");
      assert.equal(res.status, 200);
      const body = await res.json();
      assert.equal(body.state, "available");
      assert.ok(body.behind >= 1);
      assert.ok(body.commits.some((c: { subject: string }) => c.subject === "feature: add feature2"));
    });
  });

  it("apply: refuses a dirty tree", async () => {
    await withRepo(cloneFixture, async (server, { root }) => {
      fs.writeFileSync(path.join(root, "uncommitted.txt"), "dirty work");

      const res = await post(server, "/api/update/apply", {});
      assert.equal(res.status, 409);
      assert.deepEqual(await res.json(), { error: DIRTY_TREE, code: "dirty_tree" });
    });
  });

  it("apply: refuses diverged history", async () => {
    await withRepo(cloneFixture, async (server, { root, remote }) => {
      commitFile(remote, "remote-file.txt", "remote\n", "remote commit");
      commitFile(root, "local-file.txt", "local\n", "local conflicting commit");
      execGit(root, ["fetch", "origin"]);

      const res = await post(server, "/api/update/apply", {});
      assert.equal(res.status, 409);
      assert.deepEqual(await res.json(), { error: DIVERGED, code: "diverged" });
    });
  });

  it("apply: reports a missing upstream", async () => {
    await withRepo(
      (tmp) => {
        const root = path.join(tmp, "repo");
        initGitRepo(root);
        return { root };
      },
      async (server) => {
        const res = await post(server, "/api/update/apply", {});
        assert.equal(res.status, 400);
        assert.deepEqual(await res.json(), { error: "Nessun branch upstream configurato." });
      },
    );
  });

  it("apply: fast-forwards, rebuilds and relaunches", async () => {
    await withRepo(
      (tmp) => cloneFixture(tmp),
      async (server, { tmp, root, remote }) => {
        commitFile(remote, "new-feature.txt", "feature v2\n", "feature v2");

        const stream = sse(`${server.url}/api/events`);
        try {
          assert.equal((await stream.ready).status, 200);

          const res = await post(server, "/api/update/apply", { force: true });
          assert.equal(res.status, 200);
          assert.deepEqual(await res.json(), { ok: true, relaunching: true });

          assert.equal(execGit(root, ["rev-parse", "HEAD"]), execGit(remote, ["rev-parse", "HEAD"]));

          const states = await waitFor(() => {
            const seen = stream.events.filter((e): e is UpdateEvent => e.type === "update").map((e) => e.status.state);
            return seen.includes("relaunching") ? seen : undefined;
          });
          assert.ok(states.includes("updating"), `states: ${states.join(",")}`);
          assert.equal(states.at(-1), "relaunching");
        } finally {
          stream.close();
        }

        if (server.mode !== "binary") return;
        const exe = path.join(root, "target", "release", process.platform === "win32" ? "proxi.exe" : "proxi");
        assert.equal(fs.readFileSync(exe, "utf8").trim(), "new build");

        const log = fs.readFileSync(path.join(tmp, "build-log.txt"), "utf8");
        const lines = log.split(/\r?\n/).filter((line) => line.trim());
        assert.equal(lines.length, 1, `build log: ${log}`);
        assert.ok(lines[0].includes("build --release"), `build log: ${log}`);
        assert.ok(lines[0].includes("GIT_TERMINAL_PROMPT=0"), `build log: ${log}`);

        await within(server.exited!, 5000, "the binary exits");
      },
    );
  });

  it("apply: switches a clean feature branch to main", async () => {
    await withRepo(cloneFixture, async (server, { root, remote }) => {
      execGit(root, ["checkout", "-b", "feature-work"]);
      commitFile(remote, "feature3.txt", "feature v3\n", "feature v3");

      const res = await post(server, "/api/update/apply", {});
      assert.equal(res.status, 200);
      assert.equal(execGit(root, ["rev-parse", "--abbrev-ref", "HEAD"]), "main");
      assert.equal(execGit(root, ["rev-parse", "HEAD"]), execGit(remote, ["rev-parse", "HEAD"]));
    });
  });
});
