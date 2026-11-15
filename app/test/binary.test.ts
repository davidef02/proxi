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
});
