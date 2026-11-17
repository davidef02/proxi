// Tests for the desktop↔binary seam (backend.mjs): locator, serve args, spawner, tree kill (AC3 mechanism).
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { killTreeSync, locateEngineBin, proxiBinaryPath, serveArgs, spawnBackend } from "../desktop/backend.mjs";

const dirs: string[] = [];
function tempDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  dirs.push(dir);
  return dir;
}

after(() => {
  for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
});

function isDead(pid: number): Promise<boolean> {
  if (process.platform === "win32") {
    return new Promise((resolve) => {
      execFile("tasklist", ["/FI", `PID eq ${pid}`, "/FO", "CSV", "/NH"], { windowsHide: true }, (_err, stdout) => {
        resolve(!stdout.includes(`"${pid}"`));
      });
    });
  }
  try {
    process.kill(pid, 0);
    return Promise.resolve(false);
  } catch (e: any) {
    return Promise.resolve(e.code === "ESRCH");
  }
}

async function waitDead(pid: number): Promise<boolean> {
  for (let i = 0; i < 50; i++) {
    if (await isDead(pid)) return true;
    await new Promise((r) => setTimeout(r, 100));
  }
  return false;
}

describe("proxiBinaryPath", () => {
  it("names the build command when the binary is missing", () => {
    assert.throws(
      () => proxiBinaryPath(tempDir("backend-root-"), {}),
      (err: unknown) => {
        const message = err instanceof Error ? err.message : String(err);
        return /proxi binary not found/.test(message) && /cargo build --release --locked/.test(message);
      },
    );
  });

  it("honors the PROXI_BIN override", () => {
    const bin = path.join(tempDir("backend-bin-"), "proxi.exe");
    fs.writeFileSync(bin, "");
    assert.equal(proxiBinaryPath(tempDir("backend-root2-"), { PROXI_BIN: bin }), bin);
  });
});

describe("serveArgs", () => {
  it("maps every LEARNER_ engine var, including JSON args, and omits unset ones", () => {
    const args = serveArgs({
      repoRoot: "C:/repo",
      distDir: "C:/repo/app/web/dist",
      port: 4317,
      env: {
        LEARNER_CLAUDE_BIN: "claude.exe",
        LEARNER_CLAUDE_BIN_ARGS: JSON.stringify(["--model", "sonnet"]),
        LEARNER_AGY_BIN: "agy",
        LEARNER_OPENCODE_BIN_ARGS: JSON.stringify(["--verbose"]),
      },
    });
    assert.deepEqual(args, [
      "serve",
      "--repo-root", "C:/repo",
      "--dist-dir", "C:/repo/app/web/dist",
      "--port", "4317",
      "--claude-bin", "claude.exe",
      "--claude-bin-arg", "--model",
      "--claude-bin-arg", "sonnet",
      "--agy-bin", "agy",
      "--opencode-bin-arg", "--verbose",
    ]);
  });

  it("emits no engine flags when no env var is set", () => {
    assert.deepEqual(serveArgs({ repoRoot: "/r", distDir: "/d", port: 0, env: {} }), [
      "serve", "--repo-root", "/r", "--dist-dir", "/d", "--port", "0",
    ]);
  });
});

describe("locateEngineBin", () => {
  it("returns the env override first", () => {
    assert.equal(locateEngineBin("claude", { LEARNER_CLAUDE_BIN: "C:/bin/claude.exe" }), "C:/bin/claude.exe");
  });

  it("finds the binary on PATH (Path on win32) and requires .exe there", () => {
    const dir = tempDir("engine-path-");
    const exe = path.join(dir, process.platform === "win32" ? "claude.exe" : "claude");
    fs.writeFileSync(exe, "");
    const pathKey = process.platform === "win32" ? "Path" : "PATH";
    assert.equal(locateEngineBin("claude", { [pathKey]: dir }), path.resolve(exe));
    if (process.platform === "win32") {
      // a .cmd shim is never good enough on Windows
      const shimDir = tempDir("engine-shim-");
      const shim = path.join(shimDir, "claude.cmd");
      fs.writeFileSync(shim, "");
      // empty homeDir: the standard install dirs of the real home must not leak in
      assert.equal(locateEngineBin("claude", { Path: shimDir }, "win32", tempDir("engine-home-")), undefined);
    }
  });
});

describe("spawnBackend", () => {
  it("reports the URL the backend prints", async () => {
    const p = await spawnBackend(process.execPath, ["-e", "console.log('Proxi: http://127.0.0.1:4321'); setInterval(() => {}, 1000)"]);
    assert.equal(p.url, "http://127.0.0.1:4321");
    assert.equal(p.port, 4321);
    await p.close();
    await p.exited;
  });

  it("rejects with the exit code and stderr when the process dies first", async () => {
    await assert.rejects(
      () => spawnBackend(process.execPath, ["-e", "process.stderr.write('boom'); process.exit(3)"]),
      /exited with code 3: boom/,
    );
  });
});

describe("killTreeSync", () => {
  it("kills the backend and the engine child it spawned", async () => {
    const pidFile = path.join(tempDir("backend-kill-"), "engine.pid");
    const script =
      "const { spawn } = require('node:child_process');" +
      "const fs = require('node:fs');" +
      "const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)']);" +
      "fs.writeFileSync(process.env.PROXI_ENGINE_PID_FILE, String(child.pid));" +
      "console.log('Proxi: http://127.0.0.1:4333');" +
      "setInterval(() => {}, 1000);";
    const p = await spawnBackend(process.execPath, ["-e", script], { ...process.env, PROXI_ENGINE_PID_FILE: pidFile });
    try {
      const enginePid = Number(fs.readFileSync(pidFile, "utf8"));
      assert.ok(Number.isInteger(enginePid) && enginePid > 0);
      killTreeSync(p.pid);
      assert.equal(await waitDead(enginePid), true, "engine child survived");
      assert.equal(await waitDead(p.pid), true, "backend survived");
    } finally {
      await p.close().catch(() => {});
    }
  });
});
