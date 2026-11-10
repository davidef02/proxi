// Engine adapter: Google's `agy` (Antigravity) CLI, same EngineEvent stream and EngineHandle as claude.ts.
// agy takes its system prompt, tools and MCP servers only from an agent file in <cwd>/.agents/agents, so each
// session/job runs in its scratch dir and reaches the repo via --add-dir. Resume token = the agy conversation id.
import { spawn, type ChildProcess } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { killTree, lineSplitter } from "./claude.ts";
import type { EngineCallbacks, EngineEvent, EngineHandle, EngineBin, EngineLaunch, Grant } from "./types.ts";

export const AGY_MODELS_FALLBACK = [
  "gemini-3.8-flash-high",
  "gemini-3.8-flash-medium",
  "gemini-3.8-flash-low",
  "claude-sonnet-4-6",
  "claude-opus-4-6-thinking",
  "gpt-oss-120b-medium",
];

const AGENT = "ia-learner";
/** Grants -> agy tools. read_url_content (web fetch) is denied by agy without a global rule: omitted. */
const TOOLS: Record<Grant, string[]> = { read: ["view_file", "list_dir", "grep_search"], web: ["search_web"] };

const num = (v: unknown): number => (typeof v === "number" && Number.isFinite(v) ? v : 0);
const str = (v: unknown): string => (typeof v === "string" ? v : "");

export interface AgyParseState {
  /** Unique per process: message and tool ids must not repeat after a respawn. */
  run: string;
  conversationId: string;
  /** Tool steps already announced (index -> tool-use id). */
  tools: Map<number, string>;
  /** agent_response steps: text so far; absent from `done` = still open. */
  text: Map<number, string>;
  done: Set<number>;
  /** Cumulative usage of the process at the previous result. */
  last: { input: number; output: number; cache: number };
  /** Diagnostics for the stderr callback (error_message steps). */
  notes: string[];
}

let runN = 0;
export const createAgyParseState = (): AgyParseState => ({
  run: `${process.pid}_${++runN}`,
  conversationId: "",
  tools: new Map(),
  text: new Map(),
  done: new Set(),
  last: { input: 0, output: 0, cache: 0 },
  notes: [],
});

/** mcp_app_x -> mcp__app__x (the rest of the app matches claude-style names). */
export const mapToolName = (n: string) => n.replace(/^mcp_(app|job)_/, "mcp__$1__");

export function parseAgyLine(line: string, st: AgyParseState): EngineEvent[] {
  let m: any;
  try {
    m = JSON.parse(line);
  } catch {
    return [];
  }
  if (!m || typeof m !== "object") return [];
  try {
    return parse(m, st);
  } catch {
    return [];
  }
}

function finalise(st: AgyParseState, step: number, out: EngineEvent[]) {
  if (st.done.has(step)) return;
  st.done.add(step);
  const text = st.text.get(step) ?? "";
  if (text) out.push({ type: "assistant", messageId: `a${st.run}_${step}`, texts: [{ index: 0, text }], toolUses: [] });
}

function parse(m: any, st: AgyParseState): EngineEvent[] {
  const out: EngineEvent[] = [];
  if (m.event === "init") {
    st.conversationId = str(m.conversation_id) || str(m.init?.conversation_id) || st.conversationId;
    return [{ type: "init", sessionId: st.conversationId, tools: Array.isArray(m.init?.tools) ? m.init.tools.map(String) : [], mcpServers: [] }];
  }
  if (m.event === "step_update") {
    const u = m.step_update ?? {};
    const step = num(u.step_index);
    st.conversationId = str(u.conversation_id) || st.conversationId;
    const type = str(u.step_type);
    if (type === "agent_response") {
      const delta = str(u.text_delta);
      const mid = `a${st.run}_${step}`;
      if (delta && !st.text.has(step)) {
        st.text.set(step, "");
        out.push({ type: "text-start", messageId: mid, index: 0 });
      }
      if (delta) {
        st.text.set(step, st.text.get(step) + delta);
        out.push({ type: "text-delta", messageId: mid, index: 0, text: delta });
      }
      if (u.state === "DONE" || u.state === "ERROR") finalise(st, step, out);
    } else if (type === "tool") {
      const info = u.tool_info ?? {};
      let id = st.tools.get(step);
      if (!id) {
        id = `agy${st.run}_${step}`;
        st.tools.set(step, id);
        out.push({
          type: "assistant",
          messageId: `m${id}`,
          texts: [],
          toolUses: [{ index: 0, id, name: mapToolName(str(u.tool_name) || str(info.name)), input: info.parameters ?? {} }],
        });
      }
      if (u.state === "DONE") out.push({ type: "tool-result", toolUseId: id, isError: false, text: str(info.output) });
      else if (u.state === "ERROR") out.push({ type: "tool-result", toolUseId: id, isError: true, text: str(info.error?.message) || "errore" });
    } else if (type === "error_message") {
      st.notes.push("agy error: " + (str(u.text_delta) || JSON.stringify(u)));
    }
    return out;
  }
  if (m.event === "result") {
    const r = m.result ?? {};
    for (const step of st.text.keys()) finalise(st, step, out);
    const us = r.usage ?? {};
    const cur = { input: num(us.input_tokens), output: num(us.output_tokens) + num(us.thinking_tokens), cache: num(us.cache_read_tokens) };
    const d = (a: number, b: number) => Math.max(0, a - b);
    const status = str(r.status) || "SUCCESS";
    for (const a of Array.isArray(r.denied_actions) ? r.denied_actions : []) out.push({ type: "action-denied", action: str(a?.display_name) || str(a?.action) });
    out.push({
      type: "result",
      isError: status !== "SUCCESS" || !!r.error,
      subtype: status === "SUCCESS" ? "success" : status.toLowerCase(),
      text: str(r.error) || str(r.response) || undefined,
      sessionId: str(r.conversation_id) || st.conversationId,
      inputTokens: d(cur.input, st.last.input),
      outputTokens: d(cur.output, st.last.output),
      cacheReadTokens: d(cur.cache, st.last.cache),
      cacheCreationTokens: 0,
      costUsd: 0, // quota-based
    });
    st.last = cur;
    // Next turn: its steps may reuse nothing from this one.
    st.tools.clear();
    st.text.clear();
    st.done.clear();
    st.conversationId = str(r.conversation_id) || st.conversationId;
    return out;
  }
  return out;
}

function agentFile(l: EngineLaunch): string {
  const tools = l.grant.flatMap((g) => TOOLS[g]);
  return [
    "---",
    `name: ${AGENT}`,
    `description: ${AGENT}`,
    "mainAgent: true",
    "subagent: false",
    "inheritMcp: false",
    "inheritCustomizations: false",
    "excludeDefaultComponents: false",
    `tools: ${JSON.stringify(tools)}`,
    "mcpServers:",
    `  - name: ${JSON.stringify(l.mcp.name)}`,
    `    url: ${JSON.stringify(l.mcp.url)}`,
    `    headers: ${JSON.stringify({ "x-learner-token": l.mcp.token })}`,
    "---",
    l.systemPrompt,
    "",
  ].join("\n");
}

export function buildAgyArgs(l: EngineLaunch, conversationId?: string): string[] {
  return [
    ...(l.bin.args ?? []),
    "--agent", AGENT,
    "--input-format", "stream-json",
    "--output-format", "stream-json",
    "--model", l.model,
    "--disable-slash-commands",
    ...[l.repoRoot, ...(l.dataRoot && path.resolve(l.dataRoot) !== path.resolve(l.repoRoot) ? [l.dataRoot] : []), ...l.addDirs].flatMap((d) => ["--add-dir", d]),
    ...(conversationId ? ["--conversation", conversationId] : []),
  ];
}

interface Proc {
  child: ChildProcess;
  /** Resolves on close (or spawn error). */
  exited: Promise<void>;
  /** Started with --conversation and no init seen yet: dying now means the conversation was refused. */
  resuming: boolean;
}

/** Virtual handle: agy has no interrupt, so interrupt = kill the process; the next send relaunches it with --conversation
 *  once the killed one has exited (it may still be writing that conversation). */
export function spawnAgy(l: EngineLaunch, cb: EngineCallbacks): EngineHandle {
  fs.mkdirSync(path.join(l.scratchDir, ".agents", "agents"), { recursive: true });
  fs.writeFileSync(path.join(l.scratchDir, ".agents", "agents", `${AGENT}.md`), agentFile(l));

  let conv = l.resume;
  let current: Proc | undefined;
  let dying: Promise<unknown> = Promise.resolve(); // processes killed by interrupt(), not yet exited
  let stopped = false;
  let exitReported = false;
  let inFlight = false; // a turn was sent and its result has not arrived
  let gen = 0; // bumped by interrupt(): a send still waiting for the relaunch is dropped
  let toolSteps = 0;

  const reportExit = (code: number | null, signal: string | null) => {
    if (exitReported) return;
    exitReported = true;
    cb.onExit(code, signal);
  };

  const launch = (): Proc => {
    const child = spawn(l.bin.cmd, buildAgyArgs(l, conv), {
      cwd: l.scratchDir,
      env: { ...process.env, ...(l.dataRoot ? { LEARNER_DATA_ROOT: l.dataRoot } : {}), MCP_TOOL_TIMEOUT: "86400000" },
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    const me: Proc = {
      child,
      resuming: !!conv,
      exited: new Promise<void>((r) => {
        child.once("close", () => r());
        child.once("error", () => r());
      }),
    };
    current = me;
    const live = () => current === me;
    child.stdin.on("error", () => {});
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    const st = createAgyParseState();
    const out = lineSplitter((line) => {
      if (!live()) return;
      for (const e of parseAgyLine(line, st)) {
        if (e.type === "init") me.resuming = false;
        if (e.type === "result") inFlight = false;
        cb.onEvent(e);
        if (e.type === "assistant" && e.toolUses.length && l.maxTurns && ++toolSteps > l.maxTurns) {
          interrupt("error_max_turns");
          return;
        }
      }
      for (const n of st.notes.splice(0)) cb.onStderr(n);
      conv = st.conversationId || conv;
    });
    const err = lineSplitter((line) => live() && cb.onStderr(line));
    child.stdout.on("data", (c: string) => out.push(c));
    child.stderr.on("data", (c: string) => err.push(c));
    child.stdout.on("end", () => out.end());
    child.stderr.on("end", () => err.end());
    const exit = (code: number | null, signal: string | null) => {
      if (!live()) return; // killed by interrupt(): the handle lives on
      current = undefined;
      // ponytail: agy's refusal text is unknown, so "died before init with --conversation" counts as refused.
      if (me.resuming && !stopped) cb.onEvent({ type: "resume-failed" });
      reportExit(code, signal);
    };
    child.on("error", (e) => {
      cb.onStderr(`spawn error: ${e.message}`);
      exit(null, null);
    });
    child.once("close", exit);
    return me;
  };

  /** Kills the process; the in-flight turn ends with a synthetic result. */
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
    // Async: the caller cancels its pending cards right after interrupt() returns, before the result may be handled.
    setImmediate(() =>
      cb.onEvent({ type: "result", isError: true, subtype, sessionId: "", inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheCreationTokens: 0, costUsd: 0 }),
    );
  }

  launch(); // eager: callers read pid right after spawn
  return {
    get pid() {
      return current?.child.pid;
    },
    send(text) {
      if (stopped || exitReported) return;
      inFlight = true;
      toolSteps = 0;
      const line = JSON.stringify({ event: "user", message: { content: text } }) + "\n";
      if (current) return void current.child.stdin?.write(line);
      const g = gen;
      void dying.then(() => {
        if (stopped || g !== gen) return;
        (current ?? launch()).child.stdin?.write(line);
      });
    },
    interrupt: () => interrupt(),
    async stop() {
      stopped = true;
      const p = current;
      if (p) {
        p.child.stdin?.end();
        killTree(p.child);
      }
      // onExit only once every process is gone (the close handler reports first when p was live).
      const gone = Promise.all([dying, p?.exited]).then(() => reportExit(null, null));
      await Promise.race([gone, new Promise<void>((r) => setTimeout(r, 5000).unref())]);
    },
  };
}

/** Live `agy models` (one id per line), static fallback; cached for the server lifetime. */
let modelsCache: Promise<string[]> | undefined;
export function listAgyModels(bin: EngineBin): Promise<string[]> {
  modelsCache ??= new Promise<string[]>((resolve) => {
    let buf = "";
    const c = spawn(bin.cmd, [...(bin.args ?? []), "models"], { stdio: ["ignore", "pipe", "ignore"], windowsHide: true });
    const t = setTimeout(() => c.kill(), 10_000);
    c.stdout.on("data", (d) => (buf += d));
    const done = () => {
      clearTimeout(t);
      const ids = buf.split(/\r?\n/).filter((l) => l.includes("\t")).map((l) => l.split("\t")[0].trim());
      resolve(ids.length ? ids : AGY_MODELS_FALLBACK);
    };
    c.on("error", done);
    c.on("close", done);
  });
  return modelsCache;
}
