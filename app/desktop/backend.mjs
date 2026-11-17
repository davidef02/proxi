// The desktop↔binary seam: locate the `proxi` binary, build its serve args, spawn it, kill its tree.
// Pure Node (no `electron` import) so node:test can load it; main.mjs uses it from the Electron main.
import { execFile, execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

/** The built `proxi` executable: `PROXI_BIN`, else the newest of target/release and target/debug. */
export function proxiBinaryPath(root, env = process.env) {
  const exe = process.platform === "win32" ? "proxi.exe" : "proxi";
  if (env.PROXI_BIN) {
    if (fs.existsSync(env.PROXI_BIN)) return env.PROXI_BIN;
    throw new Error(`PROXI_BIN not found: ${env.PROXI_BIN} (unset it to use target/, built by cargo build --release --locked)`);
  }
  const candidates = ["release", "debug"].map((profile) => path.join(root, "target", profile, exe)).filter((p) => fs.existsSync(p));
  if (candidates.length === 0) throw new Error("proxi binary not found under target/: run cargo build --release --locked");
  return candidates.reduce((newest, p) => (fs.statSync(p).mtimeMs > fs.statSync(newest).mtimeMs ? p : newest));
}

const ENGINE_ENV = {
  claude: { bin: "LEARNER_CLAUDE_BIN", args: "LEARNER_CLAUDE_BIN_ARGS", flag: "claude-bin" },
  agy: { bin: "LEARNER_AGY_BIN", args: "LEARNER_AGY_BIN_ARGS", flag: "agy-bin" },
  opencode: { bin: "LEARNER_OPENCODE_BIN", args: "LEARNER_OPENCODE_BIN_ARGS", flag: "opencode-bin" },
};

/** The `proxi serve` argv for the desktop shell; engine flags only when their env vars are set. */
export function serveArgs({ repoRoot, distDir, port, env = process.env }) {
  const args = ["serve", "--repo-root", repoRoot, "--dist-dir", distDir, "--port", String(port)];
  for (const [provider, names] of Object.entries(ENGINE_ENV)) {
    const bin = env[names.bin];
    if (bin) args.push("--" + names.flag, bin);
    const rawArgs = env[names.args];
    const binArgs = rawArgs ? JSON.parse(rawArgs) : [];
    for (const a of binArgs) args.push("--" + names.flag + "-arg", a);
  }
  return args;
}

/**
 * Resolves an engine CLI executable (claude/agy/opencode): env override, PATH, standard install dirs.
 * On Windows, strictly matches <name>.exe and ignores .cmd / batch shims.
 */
export function locateEngineBin(provider, env = process.env, platform = process.platform, homeDir) {
  const isWin = platform === "win32";
  const override = env[ENGINE_ENV[provider].bin]?.trim();
  if (override) return override;
  const binName = (isWin ? provider + ".exe" : provider);

  const searchDirs = [];
  const pathVal = env.PATH ?? (isWin ? (env.Path ?? env.path) : undefined) ?? "";
  for (const raw of pathVal.split(isWin ? ";" : ":")) {
    const trimmed = raw.trim().replace(/^"(.*)"$/, "$1").trim();
    if (trimmed) searchDirs.push(trimmed);
  }
  const home = homeDir ?? (isWin ? (env.USERPROFILE || os.homedir()) : (env.HOME || os.homedir()));
  if (home) {
    searchDirs.push(path.join(home, ".local", "bin"));
    if (provider === "claude") {
      searchDirs.push(path.join(home, ".claude", "local"));
      searchDirs.push(path.join(home, ".claude", "local", "bin"));
    }
  }
  if (isWin) {
    const appData = env.APPDATA || (home ? path.join(home, "AppData", "Roaming") : "");
    if (appData) searchDirs.push(path.join(appData, "npm"));
    const localAppData = env.LOCALAPPDATA || (home ? path.join(home, "AppData", "Local") : "");
    if (localAppData) searchDirs.push(path.join(localAppData, "Programs", provider));
  } else {
    searchDirs.push("/usr/local/bin", "/opt/homebrew/bin");
    if (home) searchDirs.push(path.join(home, ".npm-global", "bin"));
  }

  const seen = new Set();
  for (const dir of searchDirs) {
    const norm = path.normalize(dir);
    if (seen.has(norm)) continue;
    seen.add(norm);
    const candidate = path.join(dir, binName);
    try {
      if (fs.existsSync(candidate)) {
        const stat = fs.statSync(candidate, { throwIfNoEntry: false });
        if (stat?.isFile()) {
          if (isWin && !candidate.toLowerCase().endsWith(".exe")) continue;
          return path.resolve(candidate);
        }
      }
    } catch {
      // ignore unreadable dirs / permission errors
    }
  }
  return undefined;
}

/** Best-effort `--version` run: false when the engine CLI is missing or broken. */
export function probeEngine(bin, binArgs) {
  // .exe files run directly: cmd.exe does not quote the path, so a node.exe under "Program Files" never runs.
  // Only .cmd/.bat shims, and bare names cmd.exe must resolve, go through the shell (quoted).
  const shell = process.platform === "win32" && !/\.(exe|com)$/i.test(bin);
  const quote = (s) => `"${s}"`;
  const args = [...binArgs, "--version"];
  return new Promise((resolve) => {
    execFile(shell ? quote(bin) : bin, shell ? args.map(quote) : args, { timeout: 15000, windowsHide: true, shell }, (err) => resolve(!err));
  });
}

/** Spawn `bin` with `args` and wait for the `Proxi: <url>` line it prints on startup. */
export async function spawnBackend(bin, args, env = process.env) {
  // No shell: a quoted path breaks under cmd.exe. Detached on posix so the process group exists for killTreeSync.
  const child = spawn(bin, args, {
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
    env,
    detached: process.platform !== "win32",
  });
  let stdout = "";
  let stderr = "";
  let exited = false;
  const exitedOnce = new Promise((resolve) => child.once("close", () => { exited = true; resolve(); }));
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
    pid: child.pid,
    exited: exitedOnce,
    close: () =>
      (closing ??= (async () => {
        process.removeListener("exit", kill);
        if (exited) return;
        kill();
        await exitedOnce;
      })()),
  };
}

/** Synchronous best-effort kill of a whole process tree (exit hook: nothing async can run there). */
export function killTreeSync(pid) {
  try {
    if (process.platform === "win32") {
      execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
    } else {
      try {
        process.kill(-pid, "SIGTERM"); // the detached child leads its own group
      } catch {
        process.kill(pid, "SIGTERM");
      }
    }
  } catch {
    /* already gone */
  }
}
