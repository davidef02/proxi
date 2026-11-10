import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnClaude } from "../server/claude.ts";
import type { EngineEvent, EngineHandle, EngineLaunch } from "../server/types.ts";

const FAKE = path.join(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "fake-claude-"));

function start(mcpUrl?: string) {
  const launch: EngineLaunch = {
    provider: "claude", bin: { cmd: process.execPath, args: [FAKE] }, repoRoot: tmp, scratchDir: fs.mkdtempSync(path.join(tmp, "s-")), model: "haiku",
    systemPrompt: "x", grant: [], mcp: { name: "app", url: mcpUrl ?? "http://127.0.0.1:1/mcp", token: "tok" }, addDirs: [],
  };
  const events: EngineEvent[] = [];
  const stderr: string[] = [];
  let exit: { code: number | null; signal: string | null } | undefined;
  const waiters: (() => void)[] = [];
  const poke = () => waiters.splice(0).forEach((w) => w());
  const handle: EngineHandle = spawnClaude(launch, {
    onEvent: (e) => (events.push(e), poke()),
    onStderr: (l) => (stderr.push(l), poke()),
    onExit: (code, signal) => ((exit = { code, signal }), poke()),
  });
  const until = (pred: () => boolean, ms = 10000) =>
    new Promise<void>((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("timeout; events=" + JSON.stringify(events.map((e) => e.type)))), ms);
      const check = () => (pred() ? (clearTimeout(t), resolve()) : waiters.push(check));
      check();
    });
  return { handle, events, stderr, until, exited: () => exit };
}

test("hello: init, text-start, deltas, assistant, result; argv logged to stderr", async () => {
  const s = start();
  s.handle.send("ciao");
  await s.until(() => s.events.some((e) => e.type === "result"));
  assert.deepEqual(s.events.map((e) => e.type), ["init", "text-start", "text-delta", "text-delta", "text-delta", "assistant", "assistant", "result"]); // thinking block event, then text block event
  const deltas = s.events.filter((e) => e.type === "text-delta").map((e) => (e as any).text).join("");
  assert.equal(deltas, "Ciao, sono il docente.");
  const a = s.events.find((e) => e.type === "assistant" && (e as any).texts.length) as any;
  assert.equal(a.texts[0].index, 0, "assistant events carry index-0 content arrays");
  assert.equal(a.texts[0].text, deltas);
  assert.equal((s.events.find((e) => e.type === "text-start") as any).index, 1, "stream index differs from the assistant one");
  const r = s.events.at(-1) as any;
  assert.equal(r.isError, false);
  assert.equal(r.inputTokens, 10);
  assert.equal(r.cacheCreationTokens, 40);
  assert.ok(s.stderr.some((l) => l.startsWith("fake-claude argv: ") && l.includes("--setting-sources") && l.includes("--session-id")));
  await s.handle.stop();
});

test("slow + interrupt yields result is_error; process survives", async () => {
  const s = start();
  s.handle.send("FAKE:slow");
  await s.until(() => s.events.some((e) => e.type === "text-delta"));
  s.handle.interrupt();
  await s.until(() => s.events.some((e) => e.type === "result"));
  assert.ok(s.events.some((e) => e.type === "control-response" && e.ok));
  const r = s.events.find((e) => e.type === "result") as any;
  assert.equal(r.isError, true);
  assert.equal(r.subtype, "error_during_execution");
  assert.equal(s.exited(), undefined);
  s.handle.send("ancora ciao");
  await s.until(() => s.events.filter((e) => e.type === "result").length === 2);
  await s.handle.stop();
});

test("stop() kills a running process and resolves; second stop resolves too", async () => {
  const s = start();
  s.handle.send("FAKE:slow");
  await s.until(() => s.events.some((e) => e.type === "text-delta"));
  const t0 = Date.now();
  await s.handle.stop();
  assert.ok(Date.now() - t0 < 5000);
  assert.ok(s.exited());
  await s.handle.stop();
});

test("quiz scenario calls our MCP server over HTTP and echoes the result", async () => {
  const calls: any[] = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const m = JSON.parse(body);
      calls.push({ method: m.method, token: req.headers["x-learner-token"], params: m.params });
      if (m.id === undefined) return res.writeHead(202).end();
      res.setHeader("content-type", "application/json");
      if (m.method === "initialize") return res.end(JSON.stringify({ jsonrpc: "2.0", id: m.id, result: { protocolVersion: m.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "app", version: "1" } } }));
      setTimeout(() => res.end(JSON.stringify({ jsonrpc: "2.0", id: m.id, result: { content: [{ type: "text", text: "Learner chose: 4" }] } })), 300);
    });
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const port = (server.address() as any).port;
  const s = start(`http://127.0.0.1:${port}/mcp/x`);
  try {
    s.handle.send("FAKE:quiz");
    await s.until(() => s.events.some((e) => e.type === "result"));
    const types = s.events.map((e) => e.type);
    assert.ok(types.indexOf("assistant") < types.indexOf("tool-result") && types.indexOf("tool-result") < types.indexOf("result"));
    const toolUse = s.events.flatMap((e) => (e.type === "assistant" ? e.toolUses : []))[0];
    assert.equal(toolUse.name, "mcp__app__quiz");
    const tr = s.events.find((e) => e.type === "tool-result") as any;
    assert.equal(tr.toolUseId, toolUse.id);
    assert.match(tr.text, /Learner chose: 4/);
    assert.match((s.events.at(-1) as any).text, /Learner chose: 4/);
    assert.deepEqual(calls.map((c) => c.method), ["initialize", "notifications/initialized", "tools/call"]);
    assert.equal(calls[2].params.name, "quiz");
    assert.equal(calls[2].token, "tok");
  } finally {
    await s.handle.stop();
    server.close();
  }
});

test("tool:<name>:<base64> decodes the JSON input", async () => {
  const seen: any[] = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      const m = JSON.parse(body);
      if (m.id === undefined) return res.writeHead(202).end();
      if (m.method === "tools/call") seen.push(m.params);
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ jsonrpc: "2.0", id: m.id, result: m.method === "tools/call" ? { content: [{ type: "text", text: "ok" }] } : { capabilities: {} } }));
    });
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const s = start(`http://127.0.0.1:${(server.address() as any).port}/mcp/x`);
  try {
    const b64 = Buffer.from(JSON.stringify({ q: "è già così" })).toString("base64");
    s.handle.send(`FAKE:tool:ask:${b64}`);
    await s.until(() => s.events.some((e) => e.type === "result"));
    assert.deepEqual(seen, [{ name: "ask", arguments: { q: "è già così" } }]);
  } finally {
    await s.handle.stop();
    server.close();
  }
});
