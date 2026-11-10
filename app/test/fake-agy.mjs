// Scriptable stand-in for `agy --agent ia-learner --input-format stream-json --output-format stream-json`.
// Turn scenario by marker in the user text (same markers as fake-claude): "FAKE:slow" (starts answering, never finishes: the
// app interrupts), "FAKE:tool:<name>:<base64 json input>" (calls mcp_<server>_<name> on the MCP server of the agent file),
// else a plain text answer naming the conversation. Usage is cumulative per process.
// Env FAKE_AGY_RESUME_FAIL=1: a --conversation is refused (exit 1 before init), like an unknown conversation id.
import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";

const argv = process.argv.slice(2);
console.error("fake-agy argv: " + JSON.stringify(argv));
const arg = (n) => (argv.includes(n) ? argv[argv.indexOf(n) + 1] : undefined);
if (process.env.FAKE_AGY_RESUME_FAIL === "1" && arg("--conversation")) {
  console.error("conversation not found: " + arg("--conversation"));
  process.exit(1);
}
const conv = arg("--conversation") ?? `conv-${process.pid}`;
const agentFile = fs.readFileSync(path.join(process.cwd(), ".agents", "agents", `${arg("--agent")}.md`), "utf8");
const url = /url: "([^"]+)"/.exec(agentFile)?.[1];
const token = /"x-learner-token":\s*"([^"]+)"/.exec(agentFile)?.[1];
const server = /- name: "([^"]+)"/.exec(agentFile)?.[1] ?? "app";

const out = (o) => process.stdout.write(JSON.stringify(o) + "\n");
const step = (o) => out({ event: "step_update", step_update: { conversation_id: conv, ...o } });
let n = 0;
let turns = 0;
let tokens = 0;

async function mcp(tool, input) {
  const post = async (body, sid) => {
    const res = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json, text/event-stream", "x-learner-token": token, ...(sid ? { "mcp-session-id": sid } : {}) },
      body: JSON.stringify(body),
    });
    const text = await res.text();
    return { sid: res.headers.get("mcp-session-id") ?? sid, json: text.trim() ? JSON.parse(text.replace(/^data:\s*/, "")) : null };
  };
  const a = await post({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "fake-agy", version: "1" } } });
  await post({ jsonrpc: "2.0", method: "notifications/initialized" }, a.sid);
  const list = await post({ jsonrpc: "2.0", id: 2, method: "tools/list" }, a.sid);
  const always = list.json.result.tools.every((t) => t._meta?.["antigravity/alwaysLoad"] === true);
  const r = await post({ jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: tool, arguments: input } }, a.sid);
  return { always, text: (r.json.result?.content ?? []).map((c) => c.text ?? "").join(""), isError: r.json.result?.isError === true };
}

out({ event: "init", conversation_id: conv, init: { model: arg("--model"), cwd: process.cwd(), agent: arg("--agent"), tools: [], permission_mode: "request-review" } });

const finish = (response) => {
  turns++;
  tokens += 30;
  out({
    event: "result",
    result: { conversation_id: conv, status: "SUCCESS", response, num_turns: turns, usage: { input_tokens: 10 * turns, output_tokens: 20 * turns, thinking_tokens: 0, cache_read_tokens: 0, total_tokens: tokens } },
  });
};
const say = (text) => {
  const i = ++n;
  step({ step_index: i, state: "ACTIVE", step_type: "agent_response", text_delta: "" });
  step({ step_index: i, state: "DONE", step_type: "agent_response", text_delta: text });
};

readline.createInterface({ input: process.stdin }).on("line", async (line) => {
  const text = JSON.parse(line).message?.content ?? "";
  if (text.includes("FAKE:slow")) return step({ step_index: ++n, state: "ACTIVE", step_type: "agent_response", text_delta: "Penso" }); // no result: the app kills us
  const m = /FAKE:tool:([^:\s]+):(\S+)/.exec(text);
  if (!m) {
    say(`Ciao, sono il docente (${conv}).`);
    return finish("ok");
  }
  const tool = m[1];
  const input = JSON.parse(Buffer.from(m[2], "base64").toString());
  const i = ++n;
  const name = `mcp_${server}_${tool}`;
  step({ step_index: i, state: "ACTIVE", step_type: "tool", tool_name: name, tool_info: { name, parameters: input } });
  const r = await mcp(tool, input).catch((e) => ({ text: String(e), isError: true, always: false }));
  step({ step_index: i, state: r.isError ? "ERROR" : "DONE", step_type: "tool", tool_name: name, tool_info: { name, ...(r.isError ? { error: { message: r.text } } : { output: r.text }) } });
  say(`Strumento: ${r.text} (alwaysLoad=${r.always})`);
  finish("ok");
});
