// Engine adapter: `opencode` CLI, same EngineEvent stream and EngineHandle as claude.ts and agy.ts.
// opencode runs headless with --format json and takes system prompt from .opencode/agents/ia-learner.md
// and MCP servers from opencode.json in its scratch dir. Resume token = the opencode session ID.
import { spawn, type ChildProcess } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { killTree, lineSplitter } from "./claude.ts";
import type { EngineCallbacks, EngineEvent, EngineHandle, EngineBin, EngineLaunch } from "./types.ts";

export const OPENCODE_MODELS_FALLBACK = [
  "omniroute/auto/best-coding",
  "omniroute/auto/best-fast",
  "omniroute/auto/fast",
  "google/gemini-2.5-pro",
  "google/gemini-2.5-flash",
  "opencode/big-pickle",
];

const AGENT = "ia-learner";

const num = (v: unknown): number => (typeof v === "number" && Number.isFinite(v) ? v : 0);
const str = (v: unknown): string => (typeof v === "string" ? v : "");

export interface OpencodeParseState {
  run: string;
  sessionID: string;
  hasInit: boolean;
  seenTextMessages: Set<string>;
  messageText: Map<string, string>;
  tools: Map<string, string>;
  doneTools: Set<string>;
  tokens: { input: number; output: number; cache: number };
  costUsd: number;
  notes: string[];
}

let runN = 0;
export const createOpencodeParseState = (): OpencodeParseState => ({
  run: `${process.pid}_${++runN}`,
  sessionID: "",
  hasInit: false,
  seenTextMessages: new Set(),
  messageText: new Map(),
  tools: new Map(),
  doneTools: new Set(),
  tokens: { input: 0, output: 0, cache: 0 },
  costUsd: 0,
  notes: [],
});

/** Maps opencode tool names (e.g. app.write_node, app_write_node) to standard mcp__app__write_node. */
export function mapOpencodeToolName(name: string): string {
  if (name.startsWith("mcp__")) return name;
  return name.replace(/^(app|job)[._]/, "mcp__$1__");
}

function ensureInit(st: OpencodeParseState, out: EngineEvent[]): void {
  if (!st.hasInit) {
    st.hasInit = true;
    out.push({ type: "init", sessionId: st.sessionID, tools: [], mcpServers: [] });
  }
}

export function parseOpencodeLine(line: string, st: OpencodeParseState): EngineEvent[] {
  let m: any;
  try {
    m = JSON.parse(line);
  } catch {
    return [];
  }
  if (!m || typeof m !== "object") return [];
  const out: EngineEvent[] = [];

  const type = str(m.type);
  const sid = str(m.sessionID) || str(m.sessionId);
  if (sid) st.sessionID = sid;

  if (type === "step_start") {
    ensureInit(st, out);
    return out;
  }

  if (type === "text") {
    const part = m.part ?? {};
    const mid = str(part.messageID) || `msg_${st.run}`;
    const txt = str(part.text);
    ensureInit(st, out);
    if (!st.seenTextMessages.has(mid)) {
      st.seenTextMessages.add(mid);
      out.push({ type: "text-start", messageId: mid, index: 0 });
    }
    if (txt) {
      st.messageText.set(mid, (st.messageText.get(mid) ?? "") + txt);
      out.push({ type: "text-delta", messageId: mid, index: 0, text: txt });
      out.push({ type: "assistant", messageId: mid, texts: [{ index: 0, text: st.messageText.get(mid)! }], toolUses: [] });
    }
    return out;
  }

  if (type === "tool_use") {
    const part = m.part ?? {};
    const rawTool = str(part.state?.metadata?.metadata?.toolCalls?.[0]?.tool) || str(part.tool);
    const toolName = mapOpencodeToolName(rawTool);
    const id = str(part.id) || str(part.partID) || `opencode_${st.run}_call`;
    ensureInit(st, out);
    if (!st.tools.has(id)) {
      st.tools.set(id, toolName);
      out.push({
        type: "assistant",
        messageId: `m${id}`,
        texts: [],
        toolUses: [{ index: 0, id, name: toolName, input: part.state?.input ?? {} }],
      });
    }
    const status = str(part.state?.status);
    if (status === "completed" || status === "error") {
      const isError = status === "error";
      const outText = typeof part.state?.output === "string" ? part.state.output : JSON.stringify(part.state?.output ?? "");
      out.push({ type: "tool-result", toolUseId: id, isError, text: outText });
    }
    return out;
  }

  if (type === "step_finish") {
    const part = m.part ?? {};
    const tokens = part.tokens ?? {};
    st.tokens.input += num(tokens.input);
    st.tokens.output += num(tokens.output) + num(tokens.reasoning);
    st.tokens.cache += num(tokens.cache?.read);
    st.costUsd += num(part.cost);

    if (part.reason === "stop") {
      out.push({
        type: "result",
        isError: false,
        subtype: "success",
        sessionId: st.sessionID,
        inputTokens: st.tokens.input,
        outputTokens: st.tokens.output,
        cacheReadTokens: st.tokens.cache,
        cacheCreationTokens: 0,
        costUsd: st.costUsd,
      });
    }
    return out;
  }

  if (type === "error") {
    st.notes.push("opencode error: " + (str(m.error) || JSON.stringify(m)));
  }

  return out;
}

export function buildOpencodeArgs(l: EngineLaunch, text: string, sessionID?: string): string[] {
  return [
    ...(l.bin.args ?? []),
    "run",
    "--format", "json",
    "--auto",
    "--agent", AGENT,
    "--model", l.model,
    ...(sessionID ? ["--session", sessionID] : []),
    text,
  ];
}

export function resolveOpencodeCmd(cmd: string): { cmd: string; shell?: boolean } {
  if (process.platform !== "win32") return { cmd };
  if (cmd.endsWith(".exe")) return { cmd };
  if (cmd.endsWith(".cmd") || cmd.endsWith(".bat")) return { cmd, shell: true };
  if (cmd === "opencode") {
    const appData = process.env.APPDATA || "";
    const npmExe = path.join(appData, "npm", "node_modules", "@opencode", "cli", "bin", "opencode.exe");
    if (fs.existsSync(npmExe)) return { cmd: npmExe };
    return { cmd, shell: true };
  }
  return { cmd };
}

interface Proc {
  child: ChildProcess;
  exited: Promise<void>;
  resuming: boolean;
}

export function spawnOpencode(l: EngineLaunch, cb: EngineCallbacks): EngineHandle {
  fs.mkdirSync(path.join(l.scratchDir, ".opencode", "agents"), { recursive: true });
  fs.writeFileSync(
    path.join(l.scratchDir, ".opencode", "agents", `${AGENT}.md`),
    ["---", `description: ${AGENT}`, "mode: primary", "---", "", l.systemPrompt, ""].join("\n"),
  );

  const mcpConfig = {
    $schema: "https://opencode.ai/config.json",
    mcp: {
      servers: {
        [l.mcp.name]: {
          type: "remote",
          url: l.mcp.url,
          headers: {
            "x-learner-token": l.mcp.token,
          },
        },
      },
    },
  };
  fs.writeFileSync(path.join(l.scratchDir, "opencode.json"), JSON.stringify(mcpConfig, null, 2));

  let sessionID = l.resume;
  let current: Proc | undefined;
  let dying: Promise<unknown> = Promise.resolve();
  let stopped = false;
  let exitReported = false;
  let inFlight = false;
  let gen = 0;
  let toolSteps = 0;

  const reportExit = (code: number | null, signal: string | null) => {
    if (exitReported) return;
    exitReported = true;
    cb.onExit(code, signal);
  };

  const launchTurn = (text: string): Proc => {
    const resolved = resolveOpencodeCmd(l.bin.cmd);
    const child = spawn(resolved.cmd, buildOpencodeArgs(l, text, sessionID), {
      cwd: l.scratchDir,
      env: { ...process.env, ...(l.dataRoot ? { LEARNER_DATA_ROOT: l.dataRoot } : {}), MCP_TOOL_TIMEOUT: "86400000" },
      stdio: ["ignore", "pipe", "pipe"],
      shell: resolved.shell,
      windowsHide: true,
    });

    let gotInit = false;
    let turnFinished = false;
    const tail: string[] = [];
    const me: Proc = {
      child,
      resuming: !!sessionID,
      exited: new Promise<void>((r) => {
        child.once("close", () => r());
        child.once("error", () => r());
      }),
    };
    current = me;
    const live = () => current === me;

    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    const st = createOpencodeParseState();
    if (sessionID) st.sessionID = sessionID;

    const out = lineSplitter((line) => {
      if (!live()) return;
      for (const e of parseOpencodeLine(line, st)) {
        if (e.type === "init") {
          gotInit = true;
          me.resuming = false;
          sessionID = e.sessionId || sessionID;
        }
        if (e.type === "result") {
          inFlight = false;
          turnFinished = true;
          sessionID = e.sessionId || sessionID;
        }
        cb.onEvent(e);
        if (e.type === "assistant" && e.toolUses.length && l.maxTurns && ++toolSteps > l.maxTurns) {
          interrupt("error_max_turns");
          return;
        }
      }
      for (const n of st.notes.splice(0)) cb.onStderr(n);
    });

    const err = lineSplitter((line) => {
      tail.push(line);
      if (tail.length > 5) tail.shift();
      if (live()) cb.onStderr(line);
    });

    child.stdout.on("data", (c: string) => out.push(c));
    child.stderr.on("data", (c: string) => err.push(c));
    child.stdout.on("end", () => out.end());
    child.stderr.on("end", () => err.end());

    const exit = (code: number | null, signal: string | null) => {
      if (!live()) return;
      current = undefined;
      if (me.resuming && (code !== 0 || signal !== null)) {
        cb.onEvent({ type: "resume-failed" });
        return reportExit(code, signal);
      }
      if (inFlight && !turnFinished) {
        inFlight = false;
        cb.onEvent({
          type: "result",
          isError: code !== 0,
          subtype: code === 0 ? "success" : "error",
          sessionId: sessionID || "",
          inputTokens: 0,
          outputTokens: 0,
          cacheReadTokens: 0,
          cacheCreationTokens: 0,
          costUsd: 0,
        });
      }
      if (stopped) reportExit(code, signal);
    };

    child.once("close", exit);
    child.on("error", (e) => {
      cb.onStderr(`spawn error: ${e.message}`);
      exit(null, null);
    });

    return me;
  };

  function interrupt(subtype = "interrupted") {
    gen++;
    const p = current;
    current = undefined;
    if (p) {
      killTree(p.child);
      dying = Promise.all([dying, p.exited]);
    }
    if (!inFlight) return;
    inFlight = false;
    setImmediate(() =>
      cb.onEvent({
        type: "result",
        isError: true,
        subtype,
        sessionId: sessionID || "",
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        costUsd: 0,
      }),
    );
  }

  return {
    get pid() {
      return current?.child.pid;
    },
    send(text: string) {
      if (stopped || exitReported) return;
      inFlight = true;
      toolSteps = 0;
      const g = gen;
      void dying.then(() => {
        if (stopped || g !== gen) return;
        launchTurn(text);
      });
    },
    interrupt: () => interrupt(),
    async stop() {
      stopped = true;
      const p = current;
      if (p) {
        killTree(p.child);
      }
      const gone = Promise.all([dying, p?.exited]).then(() => reportExit(null, null));
      await Promise.race([gone, new Promise<void>((r) => setTimeout(r, 5000).unref())]);
    },
  };
}

let modelsCache: Promise<string[]> | undefined;
export function listOpencodeModels(bin: EngineBin): Promise<string[]> {
  modelsCache ??= new Promise<string[]>((resolve) => {
    let buf = "";
    const resolved = resolveOpencodeCmd(bin.cmd);
    const c = spawn(resolved.cmd, [...(bin.args ?? []), "models"], {
      stdio: ["ignore", "pipe", "ignore"],
      shell: resolved.shell,
      windowsHide: true,
    });
    const t = setTimeout(() => c.kill(), 10_000);
    c.stdout.on("data", (d) => (buf += d));
    const done = () => {
      clearTimeout(t);
      const ids = buf
        .split(/\r?\n/)
        .map((l) => l.trim())
        .filter((l) => l.length > 0 && !l.startsWith("error") && !l.startsWith("warn") && !l.startsWith("info"));
      resolve(ids.length ? ids : OPENCODE_MODELS_FALLBACK);
    };
    c.on("error", done);
    c.on("close", done);
  });
  return modelsCache;
}
