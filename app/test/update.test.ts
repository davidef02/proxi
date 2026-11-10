import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { createServer } from "../server/main.ts";
import { HttpError } from "../server/sessions.ts";
import { applyUpdate, checkUpdate, countActiveWork, getStatus, _setStatusForTest } from "../server/update.ts";

function execGit(cwd: string, args: string[]): string {
  return execFileSync("git", args, { cwd, encoding: "utf8", windowsHide: true }).trim();
}

function initGitRepo(dir: string) {
  fs.mkdirSync(dir, { recursive: true });
  execGit(dir, ["init", "-b", "main"]);
  execGit(dir, ["config", "user.name", "Test User"]);
  execGit(dir, ["config", "user.email", "test@example.com"]);
  fs.writeFileSync(path.join(dir, "README.md"), "# Test\n");
  execGit(dir, ["add", "README.md"]);
  execGit(dir, ["commit", "-m", "initial commit"]);
}

function cloneGitRepo(src: string, dest: string) {
  execFileSync("git", ["clone", src, dest], { encoding: "utf8", windowsHide: true });
  execGit(dest, ["config", "user.name", "Test User"]);
  execGit(dest, ["config", "user.email", "test@example.com"]);
}

function rawRequest(
  port: number,
  urlPath: string,
  opts: { method?: string; body?: unknown; headers?: Record<string, string> } = {},
): Promise<{ status: number; body: any }> {
  return new Promise((resolve, reject) => {
    const data = opts.body !== undefined ? JSON.stringify(opts.body) : undefined;
    const req = http.request(
      {
        host: "127.0.0.1",
        port,
        path: urlPath,
        method: opts.method ?? "GET",
        headers: {
          "x-learner-client": "1",
          ...(data ? { "content-type": "application/json", "content-length": String(Buffer.byteLength(data)) } : {}),
          ...opts.headers,
        },
      },
      (res) => {
        let text = "";
        res.on("data", (chunk) => (text += chunk));
        res.on("end", () => {
          let parsed = text;
          try {
            parsed = JSON.parse(text);
          } catch {}
          resolve({ status: res.statusCode!, body: parsed });
        });
      },
    );
    req.on("error", reject);
    if (data) req.write(data);
    req.end();
  });
}

describe("countActiveWork", () => {
  it("counts sessions in active states (running, starting, waiting) and live pids", () => {
    const manager: any = {
      list: () => [
        { id: "1", status: "running" },
        { id: "2", status: "starting" },
        { id: "3", status: "waiting" },
        { id: "4", status: "idle" },
        { id: "5", status: "closed" },
      ],
      livePids: () => [101, 102],
    };
    const jobs: any = {
      livePids: () => [201, 202],
    };
    // 3 active sessions, 2 job pids -> total 5
    assert.equal(countActiveWork(manager, jobs), 5);
  });

  it("returns 0 when everything is idle or closed", () => {
    const manager: any = {
      list: () => [
        { id: "1", status: "idle" },
        { id: "2", status: "closed" },
      ],
      livePids: () => [],
    };
    const jobs: any = {
      livePids: () => [],
    };
    assert.equal(countActiveWork(manager, jobs), 0);
  });
});

describe("checkUpdate", () => {
  let tmpDir: string;
  let remoteDir: string;
  let localDir: string;

  before(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "learner-update-test-"));
    remoteDir = path.join(tmpDir, "remote");
    localDir = path.join(tmpDir, "local");
    initGitRepo(remoteDir);
    cloneGitRepo(remoteDir, localDir);
  });

  after(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("handles non-git directory gracefully without throwing", async () => {
    const nonGit = path.join(tmpDir, "non-git");
    fs.mkdirSync(nonGit, { recursive: true });
    const status = await checkUpdate(nonGit);
    assert.equal(status.state, "idle");
    assert.equal(status.behind, 0);
  });

  it("handles repo without upstream branch gracefully", async () => {
    const noUpstream = path.join(tmpDir, "no-upstream");
    initGitRepo(noUpstream);
    const status = await checkUpdate(noUpstream);
    assert.equal(status.state, "idle");
    assert.equal(status.behind, 0);
  });

  it("reports up_to_date when local HEAD matches upstream", async () => {
    const status = await checkUpdate(localDir);
    assert.equal(status.state, "up_to_date");
    assert.equal(status.behind, 0);
  });

  it("reports available with commit list when upstream has new commits", async () => {
    // Add commit to remote
    fs.writeFileSync(path.join(remoteDir, "file1.txt"), "hello\n");
    execGit(remoteDir, ["add", "file1.txt"]);
    execGit(remoteDir, ["commit", "-m", "feature: add file1"]);

    const status = await checkUpdate(localDir);
    assert.equal(status.state, "available");
    assert.equal(status.behind, 1);
    assert.ok(status.commits && status.commits.length > 0);
    assert.equal(status.commits[0].subject, "feature: add file1");
    assert.ok(status.commits[0].hash);
    assert.ok(status.commits[0].relativeTime);
  });

  it("tracks updates from origin/main when local branch has no upstream", async () => {
    // Create and switch to a feature branch with no upstream
    execGit(localDir, ["checkout", "-b", "feature-test-branch"]);

    try {
      // Remote has a new commit on main
      fs.writeFileSync(path.join(remoteDir, "feature2.txt"), "hello feature 2\n");
      execGit(remoteDir, ["add", "feature2.txt"]);
      execGit(remoteDir, ["commit", "-m", "feature: add feature2"]);

      const status = await checkUpdate(localDir);
      assert.equal(status.state, "available");
      assert.ok(status.behind >= 1);
      assert.ok(status.commits && status.commits.some((c) => c.subject === "feature: add feature2"));
    } finally {
      execGit(localDir, ["checkout", "main"]);
    }
  });

  it("handles offline/unreachable remote gracefully without throwing", async () => {
    // Break origin url
    execGit(localDir, ["remote", "set-url", "origin", "http://127.0.0.1:59999/does-not-exist.git"]);
    // Should catch fetch error silently and not crash
    const status = await checkUpdate(localDir);
    assert.ok(status.state === "available" || status.state === "up_to_date" || status.state === "idle");
    // Restore remote url
    execGit(localDir, ["remote", "set-url", "origin", remoteDir]);
  });
});

describe("applyUpdate", () => {
  let tmpDir: string;
  let remoteDir: string;
  let localDir: string;

  before(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "learner-apply-test-"));
    remoteDir = path.join(tmpDir, "remote");
    localDir = path.join(tmpDir, "local");
    initGitRepo(remoteDir);
    cloneGitRepo(remoteDir, localDir);
  });

  after(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("throws 409 sessions_running if active work exists and force is false", async () => {
    const manager: any = {
      list: () => [{ id: "1", status: "running" }],
      livePids: () => [],
    };
    const jobs: any = { livePids: () => [] };

    await assert.rejects(
      applyUpdate(localDir, { force: false, manager, jobs }),
      (err: any) => err instanceof HttpError && err.status === 409 && err.code === "sessions_running",
    );
  });

  it("shuts down active sessions and jobs when force is true", async () => {
    let managerShutdown = false;
    let jobsShutdown = false;
    const manager: any = {
      list: () => [{ id: "1", status: "running" }],
      livePids: () => [12345],
      shutdown: async () => { managerShutdown = true; },
    };
    const jobs: any = {
      livePids: () => [67890],
      shutdown: async () => { jobsShutdown = true; },
    };

    const res = await applyUpdate(localDir, { force: true, manager, jobs });
    assert.equal(res.ok, true);
    assert.equal(managerShutdown, true);
    assert.equal(jobsShutdown, true);
  });

  it("throws 409 dirty_tree if working directory has uncommitted changes", async () => {
    const manager: any = { list: () => [], livePids: () => [] };
    const jobs: any = { livePids: () => [] };

    fs.writeFileSync(path.join(localDir, "uncommitted.txt"), "dirty work");

    try {
      await assert.rejects(
        applyUpdate(localDir, { force: true, manager, jobs }),
        (err: any) => err instanceof HttpError && err.status === 409 && err.code === "dirty_tree",
      );
    } finally {
      fs.unlinkSync(path.join(localDir, "uncommitted.txt"));
    }
  });

  it("throws 409 diverged if local commits diverged from upstream", async () => {
    const manager: any = { list: () => [], livePids: () => [] };
    const jobs: any = { livePids: () => [] };

    // Remote commit
    fs.writeFileSync(path.join(remoteDir, "remote-file.txt"), "remote");
    execGit(remoteDir, ["add", "remote-file.txt"]);
    execGit(remoteDir, ["commit", "-m", "remote commit"]);

    // Local commit without pulling
    fs.writeFileSync(path.join(localDir, "local-file.txt"), "local");
    execGit(localDir, ["add", "local-file.txt"]);
    execGit(localDir, ["commit", "-m", "local conflicting commit"]);

    // Fetch upstream into local repo so @{u} is updated
    execGit(localDir, ["fetch", "origin"]);

    await assert.rejects(
      applyUpdate(localDir, { force: true, manager, jobs }),
      (err: any) => err instanceof HttpError && err.status === 409 && err.code === "diverged",
    );

    // Reset local repo to match remote
    execGit(localDir, ["reset", "--hard", "origin/main"]);
  });

  it("performs fast-forward update and triggers relaunch callback", async () => {
    const manager: any = { list: () => [], livePids: () => [] };
    const jobs: any = { livePids: () => [] };

    // Commit to remote
    fs.writeFileSync(path.join(remoteDir, "new-feature.txt"), "feature v2");
    execGit(remoteDir, ["add", "new-feature.txt"]);
    execGit(remoteDir, ["commit", "-m", "feature v2"]);

    // Fetch so upstream is aware
    execGit(localDir, ["fetch", "origin"]);

    let relaunched = false;
    const res = await applyUpdate(localDir, {
      force: true,
      manager,
      jobs,
      relaunch: () => {
        relaunched = true;
      },
    });

    assert.equal(res.ok, true);
    assert.equal(res.relaunching, true);

    // Verify local HEAD matches remote HEAD
    const localHead = execGit(localDir, ["rev-parse", "HEAD"]);
    const remoteHead = execGit(remoteDir, ["rev-parse", "HEAD"]);
    assert.equal(localHead, remoteHead);

    // Wait for the asynchronous relaunch callback (500ms)
    await new Promise((r) => setTimeout(r, 600));
    assert.equal(relaunched, true);
  });

  it("switches to main automatically before updating when on a clean feature branch", async () => {
    const manager: any = { list: () => [], livePids: () => [] };
    const jobs: any = { livePids: () => [] };

    // Create a feature branch
    execGit(localDir, ["checkout", "-b", "another-feature-branch"]);

    // Remote commit on main
    fs.writeFileSync(path.join(remoteDir, "feature3.txt"), "feature v3");
    execGit(remoteDir, ["add", "feature3.txt"]);
    execGit(remoteDir, ["commit", "-m", "feature v3"]);

    try {
      const res = await applyUpdate(localDir, {
        force: true,
        manager,
        jobs,
      });

      assert.equal(res.ok, true);
      // Local branch should now be main
      const currentBranch = execGit(localDir, ["rev-parse", "--abbrev-ref", "HEAD"]);
      assert.equal(currentBranch, "main");
      // And HEAD should match remote
      const localHead = execGit(localDir, ["rev-parse", "HEAD"]);
      const remoteHead = execGit(remoteDir, ["rev-parse", "HEAD"]);
      assert.equal(localHead, remoteHead);
    } finally {
      execGit(localDir, ["checkout", "main"]);
    }
  });

  it("detects package.json change and runs npmBin", async () => {
    const manager: any = { list: () => [], livePids: () => [] };
    const jobs: any = { livePids: () => [] };

    // Mock npm executable that writes a marker file and the skip-model env it received
    const markerFile = path.join(tmpDir, "npm-installed.marker");
    const envFile = path.join(tmpDir, "npm-env.txt");
    const mockNpmScript = path.join(tmpDir, process.platform === "win32" ? "mock-npm.cmd" : "mock-npm.sh");

    if (process.platform === "win32") {
      fs.writeFileSync(
        mockNpmScript,
        `@echo off\necho installed > "${markerFile}"\necho %PROXI_SKIP_MODEL% > "${envFile}"\nexit /b 0\n`,
      );
    } else {
      fs.writeFileSync(
        mockNpmScript,
        `#!/bin/sh\necho installed > "${markerFile}"\necho "$PROXI_SKIP_MODEL" > "${envFile}"\nexit 0\n`,
      );
      fs.chmodSync(mockNpmScript, 0o755);
    }

    // Commit package.json change in remote
    fs.writeFileSync(path.join(remoteDir, "package.json"), JSON.stringify({ name: "test-pkg", version: "1.0.1" }));
    execGit(remoteDir, ["add", "package.json"]);
    execGit(remoteDir, ["commit", "-m", "bump package.json"]);

    // Fetch in local repo
    execGit(localDir, ["fetch", "origin"]);

    const res = await applyUpdate(localDir, {
      force: true,
      manager,
      jobs,
      npmBin: mockNpmScript,
      relaunch: () => {},
    });

    assert.equal(res.ok, true);
    assert.ok(fs.existsSync(markerFile), "npm install was executed due to package.json diff");
    assert.equal(fs.readFileSync(envFile, "utf8").trim(), "1", "npm install runs with the model download skipped");
  });
});

describe("Server update routes", () => {
  let tmpDir: string;
  let remoteDir: string;
  let localDir: string;
  let srv: Awaited<ReturnType<typeof createServer>>;

  before(async () => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "learner-srv-update-"));
    remoteDir = path.join(tmpDir, "remote");
    localDir = path.join(tmpDir, "local");
    initGitRepo(remoteDir);
    cloneGitRepo(remoteDir, localDir);

    srv = await createServer({
      repoRoot: localDir,
      dataRoot: localDir,
      port: 0,
      openBrowser: false,
      checkUpdateOnStart: false,
    });
  });

  after(async () => {
    await srv.close();
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it("GET /api/update returns current status and activeSessions", async () => {
    const res = await rawRequest(srv.port, "/api/update");
    assert.equal(res.status, 200);
    assert.ok("state" in res.body);
    assert.ok("behind" in res.body);
    assert.ok("activeSessions" in res.body);
  });

  it("POST /api/update/check checks for updates and returns status", async () => {
    const res = await rawRequest(srv.port, "/api/update/check", { method: "POST" });
    assert.equal(res.status, 200);
    assert.equal(res.body.state, "up_to_date");
    assert.equal(res.body.behind, 0);
  });

  it("POST /api/update/apply applies update and returns relaunching", async () => {
    // Add remote commit
    fs.writeFileSync(path.join(remoteDir, "route-test.txt"), "route test");
    execGit(remoteDir, ["add", "route-test.txt"]);
    execGit(remoteDir, ["commit", "-m", "route test commit"]);

    const res = await rawRequest(srv.port, "/api/update/apply", {
      method: "POST",
      body: { force: true },
    });
    assert.equal(res.status, 200);
    assert.equal(res.body.ok, true);
    assert.equal(res.body.relaunching, true);
  });
});
