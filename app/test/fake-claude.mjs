// Scriptable stand-in for `claude -p --input-format stream-json --output-format stream-json`.
// Scenario per turn: "FAKE:<name>" anywhere in the user text, else env FAKE_CLAUDE_SCENARIO, else "hello".
// Scenarios: hello | quiz | slow | tool:<name>:<base64 json> | job:<base64 json array of steps>
// A job step is {tool, input} (calls mcp__job__<tool> on the job server) or {sleep: ms}; the scenario marker travels
// inside the job's first message (e.g. in the brief), since the job process gets no env of its own.
// The MCP client side is real: it POSTs JSON-RPC to the url/headers in the --mcp-config file.
import fs from "node:fs";
import readline from "node:readline";

const argv = process.argv.slice(2);
console.error("fake-claude argv: " + JSON.stringify(argv));
// The desktop shell probes `<bin> --version` with stdin open: answer and exit, never wait for a turn.
if (argv.includes("--version")) {
  console.log("0.0.0 (fake-claude)");
  process.exit(0);
}
const arg = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
};
const sessionId = arg("--session-id") ?? arg("--resume") ?? "00000000-0000-4000-8000-000000000000";
const mcpFile = arg("--mcp-config");
// Test hook: simulate "--resume of an unknown session" (exit before init).
if (process.env.FAKE_CLAUDE_RESUME_FAIL === "1" && argv.includes("--resume")) {
  console.error("No conversation found with session ID " + arg("--resume"));
  process.exit(1);
}

const out = (o) => process.stdout.write(JSON.stringify(o) + "\n");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let n = 0;
// unique per process: after a respawn ids must not repeat (real claude ids are unique)
const nextId = (p) => `${p}_${process.pid}_${++n}`;

const USAGE = { input_tokens: 10, output_tokens: 20, cache_read_input_tokens: 30, cache_creation_input_tokens: 40 };

function emitInit() {
  out({
    type: "system",
    subtype: "init",
    session_id: sessionId,
    tools: ["Read", "mcp__app__quiz"],
    mcp_servers: [{ name: "app", status: mcpFile ? "connected" : "failed" }],
    model: "fake",
  });
}
const streamEv = (event) => out({ type: "stream_event", event, session_id: sessionId, parent_tool_use_id: null });
const startMessage = (id) => streamEv({ type: "message_start", message: { id, role: "assistant", content: [] } });
function startText(index) {
  streamEv({ type: "content_block_start", index, content_block: { type: "text", text: "" } });
}
// Like the real CLI: a thinking block at index 0, text at index 1; one `assistant` event per content block,
// each with an index-0 content array and the same message id.
function startThinking() {
  streamEv({ type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "" } });
  streamEv({ type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: "Rifletto." } });
}
const thinkingEvent = (id) => assistant(id, [{ type: "thinking", thinking: "Rifletto.", signature: "sig" }]);
const delta = (index, text) => streamEv({ type: "content_block_delta", index, delta: { type: "text_delta", text } });
const assistant = (id, content) =>
  out({ type: "assistant", message: { id, role: "assistant", content }, session_id: sessionId });
function result(text, extra = {}) {
  out({
    type: "result",
    subtype: "success",
    is_error: false,
    result: text,
    session_id: sessionId,
    total_cost_usd: 0.001,
    usage: USAGE,
    ...extra,
  });
}

// ---- MCP HTTP client (blocking tools/call) ----
async function mcpCall(toolName, args) {
  if (!mcpFile) throw new Error("no --mcp-config");
  const servers = JSON.parse(fs.readFileSync(mcpFile, "utf8")).mcpServers;
  const { url, headers = {} } = servers.app ?? servers.job;
  let sid;
  const post = async (body) => {
    const res = await fetch(url, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "application/json, text/event-stream",
        ...headers,
        ...(sid ? { "mcp-session-id": sid } : {}),
      },
      body: JSON.stringify(body),
    });
    sid = res.headers.get("mcp-session-id") ?? sid;
    const text = await res.text();
    if (!res.ok && res.status !== 202) throw new Error(`MCP HTTP ${res.status}: ${text}`);
    if (!text.trim()) return null;
    if ((res.headers.get("content-type") ?? "").includes("text/event-stream")) {
      const data = text.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim());
      return data.length ? JSON.parse(data[data.length - 1]) : null;
    }
    return JSON.parse(text);
  };
  await post({
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "fake-claude", version: "1" } },
  });
  await post({ jsonrpc: "2.0", method: "notifications/initialized" });
  const r = await post({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: toolName, arguments: args } });
  if (r?.error) return { text: `MCP error ${r.error.code}: ${r.error.message}`, isError: true };
  const text = (r?.result?.content ?? []).map((c) => c.text ?? "").join("");
  return { text, isError: r?.result?.isError === true };
}

async function callTool(tool, input, intro) {
  const msgId = nextId("msg");
  const toolUseId = nextId("toolu_fake");
  startMessage(msgId);
  startThinking();
  startText(1);
  delta(1, intro);
  thinkingEvent(msgId);
  assistant(msgId, [{ type: "text", text: intro }]);
  assistant(msgId, [{ type: "tool_use", id: toolUseId, name: `mcp__app__${tool}`, input }]);
  let r;
  try {
    r = await mcpCall(tool, input);
  } catch (e) {
    r = { text: `fake-claude MCP failure: ${e.message}`, isError: true };
  }
  out({
    type: "user",
    message: {
      role: "user",
      content: [{ type: "tool_result", tool_use_id: toolUseId, content: [{ type: "text", text: r.text }], is_error: r.isError }],
    },
  });
  const finalId = nextId("msg");
  const final = `Il risultato dello strumento: ${r.text}`;
  startMessage(finalId);
  startText(0);
  delta(0, final);
  assistant(finalId, [{ type: "text", text: final }]);
  result(final);
}

async function runJob(steps) {
  const outs = [];
  for (const st of steps) {
    if (st.sleep) {
      await sleep(st.sleep);
      continue;
    }
    const msgId = nextId("msg");
    const toolUseId = nextId("toolu_fake");
    startMessage(msgId);
    assistant(msgId, [{ type: "tool_use", id: toolUseId, name: `mcp__job__${st.tool}`, input: st.input ?? {} }]);
    let r;
    try {
      r = await mcpCall(st.tool, st.input ?? {});
    } catch (e) {
      r = { text: `fake-claude MCP failure: ${e.message}`, isError: true };
    }
    outs.push(r);
    out({
      type: "user",
      message: { role: "user", content: [{ type: "tool_result", tool_use_id: toolUseId, content: [{ type: "text", text: r.text }], is_error: r.isError }] },
    });
  }
  const finalId = nextId("msg");
  const final = outs.map((r) => (r.isError ? "ERR " : "OK ") + r.text.slice(0, 160)).join(" | ") || "fatto";
  startMessage(finalId);
  startText(0);
  delta(0, final);
  assistant(finalId, [{ type: "text", text: final }]);
  result(final);
}

// ---- scenarios ----
let interrupt = null; // set while a slow turn runs: () => void

const QUIZ = {
  kind: "probe",
  question: "Quanto fa 2+2?",
  options: [{ label: "3" }, { label: "4" }, { label: "5" }],
  explanation: "Due più due fa quattro.",
  correctAnswer: "4",
};

let failOnceTriggered = false;

async function play(scenario) {
  emitInit();
  if (scenario.startsWith("job:")) return runJob(JSON.parse(Buffer.from(scenario.slice(4), "base64").toString("utf8")));
  if (scenario === "quiz") return callTool("quiz", QUIZ, "Una domanda per capire da dove partiamo.");
  if (scenario.startsWith("tool:")) {
    const rest = scenario.slice(5);
    const i = rest.indexOf(":");
    const name = rest.slice(0, i);
    const input = JSON.parse(Buffer.from(rest.slice(i + 1), "base64").toString("utf8"));
    return callTool(name, input, `Chiamo ${name}.`);
  }
  if (scenario === "error_once") {
    if (!failOnceTriggered) {
      failOnceTriggered = true;
      const id = nextId("msg");
      startMessage(id);
      startThinking();
      thinkingEvent(id);
      return result("Rate limit or model error", { subtype: "error_during_execution", is_error: true });
    }
    // second attempt succeeds
    scenario = "hello";
  }
  if (scenario === "error") {
    const id = nextId("msg");
    startMessage(id);
    startThinking();
    thinkingEvent(id);
    return result("Rate limit or model error", { subtype: "error_during_execution", is_error: true });
  }
  if (scenario === "crash") {
    process.exit(1);
  }
  if (scenario === "slow") {
    const id = nextId("msg");
    startMessage(id);
    startThinking();
    startText(1);
    let text = "";
    let stopped = false;
    const wake = new Promise((r) => (interrupt = () => ((stopped = true), r())));
    for (let k = 0; k < 50 && !stopped; k++) {
      const t = `parola${k} `;
      text += t;
      delta(1, t);
      await Promise.race([sleep(200), wake]);
    }
    interrupt = null;
    thinkingEvent(id);
    assistant(id, [{ type: "text", text }]);
    if (stopped) return result("[Request interrupted by user]", { subtype: "error_during_execution", is_error: true });
    return result(text);
  }
  // hello
  const id = nextId("msg");
  const parts = ["Ciao", ", sono il", " docente."];
  startMessage(id);
  startThinking();
  startText(1);
  for (const p of parts) delta(1, p);
  thinkingEvent(id);
  assistant(id, [{ type: "text", text: parts.join("") }]);
  result(parts.join(""));
}

// ---- stdin loop ----
let chain = Promise.resolve();
const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  let m;
  try {
    m = JSON.parse(line);
  } catch {
    return;
  }
  if (m.type === "control_request" && m.request?.subtype === "interrupt") {
    out({ type: "control_response", response: { subtype: "success", request_id: m.request_id, response: { still_queued: [] } } });
    interrupt?.();
    return;
  }
  if (m.type !== "user") return;
  const c = m.message?.content;
  const text = typeof c === "string" ? c : (c ?? []).map((b) => b.text ?? "").join("");
  const scenario = /FAKE:(\S+)/.exec(text)?.[1] ?? process.env.FAKE_CLAUDE_SCENARIO ?? "hello";
  chain = chain.then(() => play(scenario)).catch((e) => console.error("fake-claude error: " + e.stack));
});
rl.on("close", () => chain.then(() => process.exit(0)));
