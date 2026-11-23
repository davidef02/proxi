// Locate and spawn the built `proxi` binary; no server import, so `app/eval` can use it.
import { execFile, spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "../..");

/**
 * The built `proxi` executable: `PROXI_BIN`, else the newest of target/release and target/debug.
 * @param {string} [root]
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {string}
 */
export function proxiBinary(root = ROOT, env = process.env) {
  const exe = process.platform === "win32" ? "proxi.exe" : "proxi";
  if (env.PROXI_BIN) {
    if (fs.existsSync(env.PROXI_BIN)) return env.PROXI_BIN;
    throw new Error(`PROXI_BIN not found: ${env.PROXI_BIN} (unset it to use target/, built by cargo build --release --locked)`);
  }
  const candidates = ["release", "debug"].map((profile) => path.join(root, "target", profile, exe)).filter((p) => fs.existsSync(p));
  if (candidates.length === 0) throw new Error("proxi binary not found under target/: run cargo build --release --locked");
  return candidates.reduce((newest, p) => (fs.statSync(p).mtimeMs > fs.statSync(newest).mtimeMs ? p : newest));
}

/**
 * Spawn `bin` with `args` and wait for the `Proxi: <url>` line it prints on startup.
 * @param {string[]} args
 * @param {string} [bin]
 * @returns {Promise<import("./proxi-bin.mjs").RunningProxi>}
 */
export async function spawnProxi(args, bin = proxiBinary()) {
  // No shell (L15): a quoted path breaks under cmd.exe.
  const child = spawn(bin, args, { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
  let stdout = "";
  let stderr = "";
  let exited = false;
  // `exit`, not `close`: on Windows every child proxi spawns inherits proxi's stdout/stderr (the
  // pipes to us), so a descendant that outlives proxi keeps them open and `close` would never fire.
  const exitedOnce = new Promise((resolve) => child.once("exit", () => { exited = true; resolve(); }));
  const kill = () => {
    try {
      child.kill();
    } catch {
      /* already gone */
    }
  };
  process.once("exit", kill);

  const started = new Promise((resolve, reject) => {
    let settled = false;
    /** @param {() => void} fn */
    const finish = (fn) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      fn();
    };
    const timer = setTimeout(() => {
      finish(() => {
        kill();
        reject(new Error(`proxi serve did not print its URL within 30 s: ${stderr}`));
      });
    }, 30_000);
    const reported = () => {
      const m = stdout.match(/^Proxi: (http:\/\/127\.0\.0\.1:(\d+))\s*$/m);
      return m ? { url: m[1], port: Number(m[2]) } : undefined;
    };
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      const found = reported();
      if (found) finish(() => resolve(found));
    });
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", (error) => finish(() => reject(error)));
    // `close`, not `exit`: it fires after stdio closes, so the stderr of a process that dies at
    // startup reaches us before we build the message.
    child.once("close", (code) => {
      finish(() => {
        const found = reported();
        if (found) resolve(found);
        else reject(new Error(`proxi serve exited with code ${code}: ${stderr}`));
      });
    });
  });

  let endpoint;
  try {
    endpoint = await started;
  } catch (error) {
    process.removeListener("exit", kill);
    throw error;
  }

  let closing;
  return {
    url: endpoint.url,
    port: endpoint.port,
    exited: exitedOnce,
    close: () =>
      (closing ??= (async () => {
        process.removeListener("exit", kill);
        if (!exited) {
          await killTree(child);
          kill();
          await exitedOnce;
        }
        // A descendant that survived proxi may still hold the pipes: stop reading them so it cannot
        // keep this process (a test file, the eval run) alive.
        child.stdout.destroy();
        child.stderr.destroy();
      })()),
  };
}

/**
 * Windows: kill proxi together with its descendants. An engine proxi was spawning when it died is
 * left created-suspended and outside its kill-on-close job object (process-wrap assigns the job
 * after CreateProcess), so it would outlive a plain TerminateProcess forever; `taskkill /T` walks
 * the live tree and takes it down too. Unix: the caller's `child.kill()` is enough.
 * @param {import("node:child_process").ChildProcess} child
 * @returns {Promise<void>}
 */
function killTree(child) {
  if (process.platform !== "win32" || child.pid === undefined) return Promise.resolve();
  return new Promise((resolve) => {
    execFile("taskkill", ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true, timeout: 10_000 }, () => resolve());
  });
}
