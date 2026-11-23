// Tests for the binary locator/spawner the eval and the binary-mode tests share.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { proxiBinary, spawnProxi } from "./binary.ts";

const dirs: string[] = [];
function tempDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  dirs.push(dir);
  return dir;
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

after(() => {
  for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
});

describe("proxiBinary", () => {
  it("names the build command when PROXI_BIN points at nothing", () => {
    const missing = path.join(tempDir("binary-missing-"), "no-such-proxi");
    assert.throws(
      () => proxiBinary(tempDir("binary-root-"), { PROXI_BIN: missing }),
      (err: unknown) => {
        const message = err instanceof Error ? err.message : String(err);
        return /PROXI_BIN not found/.test(message) && /cargo build --release --locked/.test(message);
      },
    );
  });
});

describe("spawnProxi", () => {
  it("spawns the executable and reports the URL it prints", async () => {
    const p = await spawnProxi(["-e", "console.log('Proxi: http://127.0.0.1:4321'); setInterval(() => {}, 1000)"], process.execPath);
    assert.equal(p.url, "http://127.0.0.1:4321");
    assert.equal(p.port, 4321);
    await p.close();
    await p.exited;
    await p.close();
  });

  it("rejects with the exit code and stderr when the process dies first", async () => {
    await assert.rejects(
      () => spawnProxi(["-e", "process.stderr.write('boom'); process.exit(3)"], process.execPath),
      /exited with code 3: boom/,
    );
  });

  // Regression (CI windows-latest hung test:rust until the job timeout): a descendant that inherited
  // the server's stdout/stderr and outlives it must not keep close() waiting, and on Windows the
  // whole tree goes down with the server. `detached` keeps the descendant out of the parent's
  // kill-on-close job, like an engine proxi was still spawning (created suspended, not yet in its job).
  it("close() returns even when a descendant holding the pipes outlives the server", async () => {
    const pidFile = path.join(tempDir("binary-tree-"), "grandchild.pid");
    const parent = [
      "const { spawn } = require('node:child_process');",
      "const fs = require('node:fs');",
      "const c = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'inherit', detached: true });",
      `fs.writeFileSync(${JSON.stringify(pidFile)}, String(c.pid));`,
      "console.log('Proxi: http://127.0.0.1:4322');",
      "setInterval(() => {}, 1000);",
    ].join("\n");
    const p = await spawnProxi(["-e", parent], process.execPath);
    const pid = Number(fs.readFileSync(pidFile, "utf8"));
    try {
      const closed = p.close().then(() => "closed");
      const timedOut = new Promise((resolve) => setTimeout(() => resolve("timeout"), 15_000).unref());
      assert.equal(await Promise.race([closed, timedOut]), "closed");
      if (process.platform === "win32") assert.equal(alive(pid), false, "the descendant dies with the server");
    } finally {
      if (alive(pid)) process.kill(pid);
    }
  });
});
