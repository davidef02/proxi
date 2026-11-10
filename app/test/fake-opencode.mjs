// Scriptable stand-in for `opencode run --format json`.
// Turn scenario by marker in user text (same markers as fake-claude and fake-agy):
// "FAKE:slow" (starts answering, never finishes: app interrupts),
// "FAKE:tool:<name>:<base64 json input>" (calls tool on MCP server configured in opencode.json),
// else a plain text answer naming the session.
// Env FAKE_OPENCODE_RESUME_FAIL=1: --session is refused (exit 1 before init), simulating an unknown session.
import fs from "node:fs";
import path from "node:path";

const argv = process.argv.slice(2);
console.error("fake-opencode argv: " + JSON.stringify(argv));

if (argv.includes("--version")) {
  console.log("opencode v2.2.1 (fake)");
  process.exit(0);
}

if (argv.includes("models")) {
  console.log("omniroute/auto/best-coding\nomniroute/auto/fast\ngoogle/gemini-2.5-pro\nopencode/big-pickle");
  process.exit(0);
}

const arg = (n) => (argv.includes(n) ? argv[argv.indexOf(n) + 1] : undefined);

if (process.env.FAKE_OPENCODE_RESUME_FAIL === "1" && arg("--session")) {
  console.error("session not found: " + arg("--session"));
  process.exit(1);
}

const conv = arg("--session") ?? `ses-${process.pid}`;

let url = "";
let token = "";
let server = "app";
try {
  const cfg = JSON.parse(fs.readFileSync(path.join(process.cwd(), "opencode.json"), "utf8"));
  const sName = Object.keys(cfg.mcp?.servers ?? {})[0] ?? "app";
  server = sName;
  url = cfg.mcp?.servers?.[sName]?.url ?? "";
  token = cfg.mcp?.servers?.[sName]?.headers?.["x-learner-token"] ?? "";
} catch {}

const out = (o) => process.stdout.write(JSON.stringify(o) + "\n");
let n = 0;

async function mcp(tool, input) {
  const post = async (body, sid) => {
    const res = await fetch(url, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "application/json, text/event-stream",
        "x-learner-token": token,
        ...(sid ? { "mcp-session-id": sid } : {}),
      },
      body: JSON.stringify(body),
    });
    const text = await res.text();
    return { sid: res.headers.get("mcp-session-id") ?? sid, json: text.trim() ? JSON.parse(text.replace(/^data:\s*/, "")) : null };
  };
  const a = await post({
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "fake-opencode", version: "1" } },
  });
  await post({ jsonrpc: "2.0", method: "notifications/initialized" }, a.sid);
  const r = await post({ jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: tool, arguments: input } }, a.sid);
  return {
    text: (r.json?.result?.content ?? []).map((c) => c.text ?? "").join(""),
    isError: r.json?.result?.isError === true,
  };
}

// Find user text argument from argv
let userText = "";
const runIdx = argv.indexOf("run");
if (runIdx >= 0) {
  for (let i = runIdx + 1; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--format" || a === "--agent" || a === "--model" || a === "--session") {
      i++;
      continue;
    }
    if (a.startsWith("-")) continue;
    userText = a;
    break;
  }
}

async function run() {
  out({
    type: "step_start",
    timestamp: Date.now(),
    sessionID: conv,
    part: { id: `prt_start_${++n}`, sessionID: conv, messageID: `msg_${n}`, type: "step-start" },
  });

  if (userText.includes("FAKE:slow")) {
    out({
      type: "text",
      timestamp: Date.now(),
      sessionID: conv,
      part: { id: `prt_txt_${++n}`, sessionID: conv, messageID: `msg_${n}`, type: "text", text: "Penso" },
    });
    // Never finishes: stays alive until caller interrupts/kills
    setInterval(() => {}, 1000);
    return;
  }

  const m = /FAKE:tool:([^:\s]+):(\S+)/.exec(userText);
  if (m) {
    const tool = m[1];
    const input = JSON.parse(Buffer.from(m[2], "base64").toString());
    const r = await mcp(tool, input).catch((e) => ({ text: String(e), isError: true }));
    out({
      type: "tool_use",
      timestamp: Date.now(),
      sessionID: conv,
      part: {
        id: `call_${++n}`,
        tool: "execute",
        state: {
          status: r.isError ? "error" : "completed",
          input,
          output: r.text,
          metadata: { metadata: { toolCalls: [{ tool: `${server}.${tool}`, status: r.isError ? "error" : "completed" }] } },
        },
      },
    });
    out({
      type: "step_finish",
      timestamp: Date.now(),
      sessionID: conv,
      part: {
        id: `prt_sf_${++n}`,
        type: "step-finish",
        reason: "tool-calls",
        cost: 0,
        tokens: { input: 10, output: 20, reasoning: 0, cache: { read: 0, write: 0 } },
      },
    });
    out({
      type: "step_start",
      timestamp: Date.now(),
      sessionID: conv,
      part: { id: `prt_start_${++n}`, sessionID: conv, messageID: `msg_${n}`, type: "step-start" },
    });
    out({
      type: "text",
      timestamp: Date.now(),
      sessionID: conv,
      part: { id: `prt_txt_${++n}`, sessionID: conv, messageID: `msg_${n}`, type: "text", text: `Strumento: ${r.text}` },
    });
    out({
      type: "step_finish",
      timestamp: Date.now(),
      sessionID: conv,
      part: {
        id: `prt_sf_${++n}`,
        type: "step-finish",
        reason: "stop",
        cost: 0,
        tokens: { input: 10, output: 20, reasoning: 0, cache: { read: 0, write: 0 } },
      },
    });
    process.exit(0);
  }

  out({
    type: "text",
    timestamp: Date.now(),
    sessionID: conv,
    part: { id: `prt_txt_${++n}`, sessionID: conv, messageID: `msg_${n}`, type: "text", text: `Ciao, sono il docente (${conv}).` },
  });
  out({
    type: "step_finish",
    timestamp: Date.now(),
    sessionID: conv,
    part: {
      id: `prt_sf_${++n}`,
      type: "step-finish",
      reason: "stop",
      cost: 0,
      tokens: { input: 10, output: 20, reasoning: 0, cache: { read: 0, write: 0 } },
    },
  });
  process.exit(0);
}

run().catch((e) => {
  console.error(e);
  process.exit(1);
});
