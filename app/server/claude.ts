// Engine adapter: the user's own `claude` binary headless (stream-json). Writes system.md + mcp.json into the scratch dir.
// Resume token = the claude session uuid (fresh: --session-id <new uuid>, later --resume <token>).
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import type { ClaudeStatusResponse } from "../shared/protocol.ts";
import type { EngineCallbacks, EngineHandle, EngineLaunch, Grant } from "./types.ts";
import { createStreamParseState, parseStreamLine } from "./stream.ts";
import { recordLimits } from "./limits.ts";
import { readSettings } from "./settings.ts";

const TOOLS: Record<Grant, string[]> = { read: ["Read", "Glob", "Grep"], web: ["WebSearch", "WebFetch"] };
/** claude's own words when --resume names a session it does not have. */
const RESUME_REFUSED = /no conversation found|session.*not found/i;

export const CLAUDE_AUTH_ERROR_MESSAGE =
  "Claude Code non risulta autenticato. Esegui `claude login` nel terminale oppure configura una API key Anthropic nelle Impostazioni.";

export const CLAUDE_AUTH_ERROR_RE =
  /not logged in|authentication required|please (?:run )?(?:claude )?login|claude login|authentication_failed|oauth_org_not_allowed|invalid[ _]api[ _]key/i;

/** Pure: argv for `l`; `freshId` is the session uuid used when there is nothing to resume. */
export function buildClaudeArgs(l: EngineLaunch, freshId: string): string[] {
  const tools = l.grant.flatMap((g) => TOOLS[g]);
  return [
    ...(l.bin.args ?? []),
    "-p",
    "--model", l.model,
    ...(l.effort ? ["--effort", l.effort] : []),
    "--input-format", "stream-json",
    "--output-format", "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--setting-sources", "",
    "--system-prompt-file", path.join(l.scratchDir, "system.md"),
    "--tools", tools.join(","),
    "--disable-slash-commands",
    "--strict-mcp-config",
    "--mcp-config", path.join(l.scratchDir, "mcp.json"),
    "--allowedTools", [...tools, `mcp__${l.mcp.name}`].join(","),
    "--permission-prompts", "none",
    ...l.addDirs.flatMap((d) => ["--add-dir", d]),
    ...(l.dataRoot && path.resolve(l.dataRoot) !== path.resolve(l.repoRoot) && !l.addDirs.includes(l.dataRoot)
      ? ["--add-dir", l.dataRoot]
      : []),
    ...(l.maxTurns ? ["--max-turns", String(l.maxTurns)] : []),
    ...(l.resume ? ["--resume", l.resume] : ["--session-id", freshId]),
  ];
}

/** Splits a utf8 stream into lines (handles split chunks and \r\n); end() flushes the tail. */
export function lineSplitter(onLine: (line: string) => void) {
  let buf = "";
  return {
    push(chunk: string) {
      buf += chunk;
      let i: number;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i).replace(/\r$/, "");
        buf = buf.slice(i + 1);
        if (line.trim()) onLine(line);
      }
    },
    end() {
      const line = buf.replace(/\r$/, "");
      buf = "";
      if (line.trim()) onLine(line);
    },
  };
}

/** Kills the process tree (Windows: taskkill /T /F). */
export function killTree(child: ChildProcess) {
  if (child.pid === undefined) return;
  if (process.platform === "win32") {
    spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore", windowsHide: true }).on("error", () => child.kill());
  } else child.kill("SIGTERM");
}

export interface LocateClaudeOptions {
  env?: NodeJS.ProcessEnv;
  platform?: NodeJS.Platform;
  homeDir?: string;
}

/**
 * Resolves the Claude CLI executable.
 * Prioritizes LEARNER_CLAUDE_BIN override, walks PATH, and checks standard install dirs (~/.local/bin, ~/.claude/local, %APPDATA%/npm).
 * On Windows, strictly matches claude.exe and ignores .cmd / batch shims.
 */
export function locateClaudeBinary(opts?: LocateClaudeOptions): string | undefined {
  const env = opts?.env ?? process.env;
  const override = env.LEARNER_CLAUDE_BIN?.trim();
  if (override) return override;

  const platform = opts?.platform ?? process.platform;
  const isWin = platform === "win32";
  const binName = isWin ? "claude.exe" : "claude";

  const searchDirs: string[] = [];

  // 1. Search PATH (checking PATH, Path, path)
  const pathVal = env.PATH ?? (isWin ? (env.Path ?? env.path) : undefined) ?? "";
  const pathParts = pathVal.split(isWin ? ";" : ":");
  for (const raw of pathParts) {
    const trimmed = raw.trim().replace(/^"(.*)"$/, "$1").trim();
    if (trimmed) searchDirs.push(trimmed);
  }

  // 2. Standard installation paths
  const home = opts?.homeDir ?? (isWin ? (env.USERPROFILE || os.homedir()) : (env.HOME || os.homedir()));
  if (home) {
    searchDirs.push(path.join(home, ".local", "bin"));
    searchDirs.push(path.join(home, ".claude", "local"));
    searchDirs.push(path.join(home, ".claude", "local", "bin"));
  }

  if (isWin) {
    const appData = env.APPDATA || (home ? path.join(home, "AppData", "Roaming") : "");
    if (appData) searchDirs.push(path.join(appData, "npm"));
    const localAppData = env.LOCALAPPDATA || (home ? path.join(home, "AppData", "Local") : "");
    if (localAppData) searchDirs.push(path.join(localAppData, "Programs", "claude"));
  } else {
    searchDirs.push("/usr/local/bin", "/opt/homebrew/bin");
    if (home) searchDirs.push(path.join(home, ".npm-global", "bin"));
  }

  const seen = new Set<string>();
  for (const dir of searchDirs) {
    const norm = path.normalize(dir);
    if (seen.has(norm)) continue;
    seen.add(norm);

    const candidate = path.join(dir, binName);
    try {
      if (fs.existsSync(candidate)) {
        const stat = fs.statSync(candidate, { throwIfNoEntry: false });
        if (stat?.isFile()) {
          if (isWin && !candidate.toLowerCase().endsWith(".exe")) {
            continue;
          }
          return path.resolve(candidate);
        }
      }
    } catch {
      // ignore unreadable dirs / permission errors
    }
  }

  return undefined;
}

const execFileAsync = promisify(execFile);

export const CLAUDE_MIN_VERSION = "2.1.259";

export interface ParsedSemver {
  major: number;
  minor: number;
  patch: number;
  raw: string;
}

export function parseClaudeVersion(output: string): ParsedSemver | null {
  const match = output.trim().match(/^(\d+)\.(\d+)\.(\d+)/);
  if (!match) return null;
  return {
    major: Number.parseInt(match[1], 10),
    minor: Number.parseInt(match[2], 10),
    patch: Number.parseInt(match[3], 10),
    raw: `${match[1]}.${match[2]}.${match[3]}`,
  };
}

export function compareSemver(
  a: { major: number; minor: number; patch: number },
  b: { major: number; minor: number; patch: number },
): number {
  if (a.major !== b.major) return a.major > b.major ? 1 : -1;
  if (a.minor !== b.minor) return a.minor > b.minor ? 1 : -1;
  if (a.patch !== b.patch) return a.patch > b.patch ? 1 : -1;
  return 0;
}

export interface ProbeClaudeOptions extends LocateClaudeOptions {
  binPath?: string;
  minVersion?: string;
  timeoutMs?: number;
}

export async function probeClaudeCli(opts?: ProbeClaudeOptions): Promise<ClaudeStatusResponse> {
  const minVersion = opts?.minVersion ?? CLAUDE_MIN_VERSION;
  const minParsed = parseClaudeVersion(minVersion)!;

  let binPath = opts?.binPath ?? locateClaudeBinary(opts);
  if (!binPath) {
    return { status: "not-found", minVersion };
  }

  if (binPath === "claude") {
    const resolved = locateClaudeBinary(opts);
    if (resolved) binPath = resolved;
  }

  try {
    const isJsScript = binPath.endsWith(".js") || binPath.endsWith(".mjs");
    const cmd = isJsScript ? process.execPath : binPath;
    const args = isJsScript ? [binPath, "--version"] : ["--version"];

    const { stdout } = await execFileAsync(cmd, args, {
      timeout: opts?.timeoutMs ?? 5000,
      windowsHide: true,
      shell: false,
      encoding: "utf8",
    });

    const parsed = parseClaudeVersion(stdout);
    if (!parsed) {
      return { status: "version-unchecked", minVersion, binPath, error: `Output non riconosciuto: ${stdout.trim()}` };
    }

    if (compareSemver(parsed, minParsed) >= 0) {
      return { status: "meets-minimum", version: parsed.raw, minVersion, binPath };
    }
    return { status: "update-required", version: parsed.raw, minVersion, binPath };
  } catch (err: any) {
    if (err?.code === "ENOENT") {
      return { status: "not-found", minVersion, binPath, error: err.message };
    }
    return {
      status: "version-unchecked",
      minVersion,
      binPath,
      error: err?.message || String(err),
    };
  }
}

export function spawnClaude(l: EngineLaunch, cb: EngineCallbacks): EngineHandle {
  const settings = readSettings(l.repoRoot);
  const env: NodeJS.ProcessEnv = { ...process.env, MCP_TOOL_TIMEOUT: "86400000" };
  if (l.dataRoot) env.LEARNER_DATA_ROOT = l.dataRoot;
  if (settings.claudeAuth === "apiKey") {
    if (!settings.anthropicApiKey?.trim()) {
      queueMicrotask(() => {
        cb.onStderr("Nessuna API key Anthropic configurata nelle Impostazioni.");
        cb.onExit(1, null);
      });
      return {
        get pid() { return undefined; },
        send: () => {},
        interrupt: () => {},
        stop: async () => {},
      };
    }
    env.ANTHROPIC_API_KEY = settings.anthropicApiKey.trim();
  } else {
    delete env.ANTHROPIC_API_KEY;
  }

  fs.mkdirSync(l.scratchDir, { recursive: true });
  fs.writeFileSync(path.join(l.scratchDir, "system.md"), l.systemPrompt);
  const server = { type: "http", url: l.mcp.url, headers: { "x-learner-token": l.mcp.token } };
  fs.writeFileSync(path.join(l.scratchDir, "mcp.json"), JSON.stringify({ mcpServers: { [l.mcp.name]: server } }));
  const cmd = l.bin.cmd === "claude" ? (locateClaudeBinary() ?? "claude") : l.bin.cmd;
  const child = spawn(cmd, buildClaudeArgs(l, randomUUID()), {
    cwd: l.repoRoot,
    env,
    stdio: ["pipe", "pipe", "pipe"],
    windowsHide: true,
  });
  let exited = false;
  let gotInit = false;
  const tail: string[] = [];
  const exitPromise = new Promise<void>((res) => {
    child.once("close", () => res());
    child.once("error", () => res());
  });
  const exit = (code: number | null, signal: string | null) => {
    if (exited) return;
    exited = true;
    const combined = tail.join(" ");
    if (l.resume && !gotInit && RESUME_REFUSED.test(combined)) cb.onEvent({ type: "resume-failed" });
    if (CLAUDE_AUTH_ERROR_RE.test(combined)) cb.onEvent({ type: "auth-failed", message: CLAUDE_AUTH_ERROR_MESSAGE });
    cb.onExit(code, signal);
  };
  child.once("close", exit);
  child.on("error", (err) => {
    // spawn failure (ENOENT...): "close" may never fire, so report the exit ourselves.
    cb.onStderr(`spawn error: ${err.message}`);
    exit(null, null);
  });
  child.stdin.on("error", () => {}); // EPIPE after the process died
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  const state = createStreamParseState();
  const out = lineSplitter((line) => {
    for (const e of parseStreamLine(line, state)) {
      if (e.type === "init") gotInit = true;
      if (e.type === "result" && e.isError && e.text && CLAUDE_AUTH_ERROR_RE.test(e.text)) {
        cb.onEvent({ type: "auth-failed", message: CLAUDE_AUTH_ERROR_MESSAGE });
      }
      if (e.type === "limits") recordLimits(l.repoRoot, e.limits); // app-wide, not the turn's business
      else if (e.type !== "ignored") cb.onEvent(e);
    }
  });
  const err = lineSplitter((line) => {
    tail.push(line);
    if (tail.length > 20) tail.shift();
    cb.onStderr(line);
  });
  child.stdout.on("data", (c: string) => out.push(c));
  child.stderr.on("data", (c: string) => err.push(c));
  child.stdout.on("end", () => out.end());
  child.stderr.on("end", () => err.end());

  const write = (obj: unknown) => {
    if (!exited && child.stdin.writable) child.stdin.write(JSON.stringify(obj) + "\n");
  };
  let intN = 0;
  return {
    get pid() {
      return child.pid;
    },
    send: (text) => write({ type: "user", message: { role: "user", content: text } }),
    interrupt: () => write({ type: "control_request", request_id: `int-${++intN}`, request: { subtype: "interrupt" } }),
    async stop() {
      if (exited) return;
      child.stdin.end();
      killTree(child);
      // Never hang shutdown on a process that will not die.
      await Promise.race([exitPromise, new Promise<void>((r) => setTimeout(r, 5000).unref())]);
    },
  };
}
