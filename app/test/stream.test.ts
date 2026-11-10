import { test } from "node:test";
import assert from "node:assert/strict";
import { createStreamParseState, parseStreamLine } from "../server/stream.ts";
import { buildClaudeArgs } from "../server/claude.ts";
import path from "node:path";
import type { EngineLaunch } from "../server/types.ts";

const parse = (o: unknown, st = createStreamParseState()) => parseStreamLine(typeof o === "string" ? o : JSON.stringify(o), st);

test("system/init", () => {
  const [e] = parse({
    type: "system", subtype: "init", session_id: "s1", tools: ["Read", "mcp__app__quiz"],
    mcp_servers: [{ name: "app", status: "connected", source: "dynamic" }],
  });
  assert.deepEqual(e, { type: "init", sessionId: "s1", tools: ["Read", "mcp__app__quiz"], mcpServers: [{ name: "app", status: "connected" }] });
});

test("system non-init, status, unknown types are ignored", () => {
  for (const o of [{ type: "system", subtype: "status" }, { type: "rate_limit_event" }, { type: "system", subtype: "ui_invalidate" }])
    assert.deepEqual(parse(o), [{ type: "ignored" }]);
});

test("stream_event: message_start remembered, text-start, text-delta; thinking ignored", () => {
  const st = createStreamParseState();
  assert.deepEqual(parse({ type: "stream_event", event: { type: "message_start", message: { id: "msg_1" } } }, st), [{ type: "ignored" }]);
  assert.deepEqual(parse({ type: "stream_event", event: { type: "content_block_start", index: 1, content_block: { type: "text", text: "" } } }, st), [
    { type: "text-start", messageId: "msg_1", index: 1 },
  ]);
  assert.deepEqual(parse({ type: "stream_event", event: { type: "content_block_start", index: 0, content_block: { type: "thinking" } } }, st), [{ type: "ignored" }]);
  assert.deepEqual(parse({ type: "stream_event", event: { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: "Ciao" } } }, st), [
    { type: "text-delta", messageId: "msg_1", index: 1, text: "Ciao" },
  ]);
  assert.deepEqual(parse({ type: "stream_event", event: { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: "x" } } }, st), [{ type: "ignored" }]);
  assert.deepEqual(parse({ type: "stream_event", event: { type: "content_block_delta", index: 2, delta: { type: "input_json_delta", partial_json: "{" } } }, st), [{ type: "ignored" }]);
  assert.deepEqual(parse({ type: "stream_event", event: { type: "message_stop" } }, st), [{ type: "ignored" }]);
});

test("assistant: text and tool_use blocks with content indexes", () => {
  const [e] = parse({
    type: "assistant",
    message: { id: "msg_2", content: [
      { type: "thinking", thinking: "" },
      { type: "text", text: "Vediamo." },
      { type: "tool_use", id: "toolu_1", name: "mcp__app__quiz", input: { question: "?" }, caller: { type: "direct" } },
    ] },
  });
  assert.deepEqual(e, {
    type: "assistant", messageId: "msg_2",
    texts: [{ index: 1, text: "Vediamo." }],
    toolUses: [{ index: 2, id: "toolu_1", name: "mcp__app__quiz", input: { question: "?" } }],
  });
});

test("user tool_result: array content, string content, is_error, several blocks; plain user ignored", () => {
  assert.deepEqual(
    parse({ type: "user", message: { content: [
      { type: "tool_result", tool_use_id: "t1", content: [{ type: "text", text: "Learner " }, { type: "text", text: "chose: 4" }] },
      { type: "tool_result", tool_use_id: "t2", content: "denied", is_error: true },
    ] } }),
    [
      { type: "tool-result", toolUseId: "t1", isError: false, text: "Learner chose: 4" },
      { type: "tool-result", toolUseId: "t2", isError: true, text: "denied" },
    ],
  );
  assert.deepEqual(parse({ type: "user", message: { content: "hi" } }), [{ type: "ignored" }]);
  assert.deepEqual(parse({ type: "user", message: { content: [{ type: "text", text: "[Request interrupted by user]" }] } }), [{ type: "ignored" }]);
});

test("result: success, error, missing usage", () => {
  assert.deepEqual(
    parse({ type: "result", subtype: "success", is_error: false, result: "Sette", session_id: "s", total_cost_usd: 0.5,
      usage: { input_tokens: 2, output_tokens: 5, cache_read_input_tokens: 625, cache_creation_input_tokens: 150 } }),
    [{ type: "result", isError: false, subtype: "success", text: "Sette", sessionId: "s", inputTokens: 2, outputTokens: 5, cacheReadTokens: 625, cacheCreationTokens: 150, costUsd: 0.5 }],
  );
  const [e] = parse({ type: "result", subtype: "error_during_execution", is_error: true, session_id: "s" });
  assert.equal(e.type === "result" && e.isError, true);
  assert.equal(e.type === "result" && e.inputTokens, 0);
  assert.equal(e.type === "result" && e.text, undefined);
});

test("control_response", () => {
  assert.deepEqual(parse({ type: "control_response", response: { subtype: "success", request_id: "int-1", response: {} } }), [{ type: "control-response", requestId: "int-1", ok: true, error: undefined }]);
  assert.deepEqual(parse({ type: "control_response", response: { subtype: "error", request_id: "int-2", error: "boom" } }), [{ type: "control-response", requestId: "int-2", ok: false, error: "boom" }]);
});

test("garbage never throws", () => {
  for (const l of ["", "not json", "{", "null", "42", "[]", '"x"', '{"type":"stream_event"}', '{"type":"assistant","message":null}', '{"type":"result","usage":5}'])
    assert.doesNotThrow(() => parse(l));
  assert.deepEqual(parse("not json"), [{ type: "ignored" }]);
  assert.deepEqual(parse("null"), [{ type: "ignored" }]);
});

const launch: EngineLaunch = {
  provider: "claude", bin: { cmd: "claude" }, repoRoot: "/r", scratchDir: "/s", model: "sonnet", systemPrompt: "x",
  grant: ["read"], mcp: { name: "app", url: "http://x", token: "t" }, addDirs: ["/a", "/b"],
};
const sys = path.join("/s", "system.md");
const mcp = path.join("/s", "mcp.json");

test("buildClaudeArgs snapshot (first launch)", () => {
  assert.deepEqual(buildClaudeArgs(launch, "uuid-1"), [
    "-p", "--model", "sonnet", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages",
    "--setting-sources", "", "--system-prompt-file", sys, "--tools", "Read,Glob,Grep", "--disable-slash-commands",
    "--strict-mcp-config", "--mcp-config", mcp, "--allowedTools", "Read,Glob,Grep,mcp__app",
    "--permission-prompts", "none",
    "--add-dir", "/a", "--add-dir", "/b", "--session-id", "uuid-1",
  ]);
});

test("buildClaudeArgs: passes --permission-prompts none to prevent hanging on permission gates", () => {
  const args = buildClaudeArgs(launch, "uuid-1");
  const permIdx = args.indexOf("--permission-prompts");
  assert.ok(permIdx !== -1, "must include --permission-prompts flag");
  assert.equal(args[permIdx + 1], "none", "--permission-prompts value must be none");
  assert.ok(args.indexOf("--allowedTools") < permIdx, "--permission-prompts should appear after --allowedTools");
});

test("buildClaudeArgs: maxTurns adds --max-turns before the session flag", () => {
  const a = buildClaudeArgs({ ...launch, maxTurns: 14 }, "uuid-1");
  assert.equal(a[a.indexOf("--max-turns") + 1], "14");
  assert.ok(a.indexOf("--max-turns") < a.indexOf("--session-id"));
  assert.ok(!buildClaudeArgs(launch, "uuid-1").includes("--max-turns"));
});

test("buildClaudeArgs: effort flag appended when specified and omitted when undefined", () => {
  const withEffort = buildClaudeArgs({ ...launch, effort: "high" }, "uuid-1");
  assert.equal(withEffort[withEffort.indexOf("--effort") + 1], "high");
  assert.equal(withEffort.indexOf("--effort"), withEffort.indexOf("--model") + 2);
  assert.ok(!buildClaudeArgs(launch, "uuid-1").includes("--effort"));
});

test("buildClaudeArgs: bin args first, resume token, empty grant, job MCP server", () => {
  const a = buildClaudeArgs({ ...launch, bin: { cmd: "node", args: ["script.mjs"] }, resume: "tok-9", grant: [], addDirs: [], mcp: { name: "job", url: "u", token: "t" } }, "uuid-1");
  assert.equal(a[0], "script.mjs");
  assert.equal(a[a.indexOf("--tools") + 1], "");
  assert.equal(a[a.indexOf("--allowedTools") + 1], "mcp__job");
  assert.deepEqual(a.slice(-2), ["--resume", "tok-9"]);
  assert.ok(!a.includes("--session-id") && !a.includes("--add-dir"));
  assert.ok(!a.includes("--bare") && !a.includes("--dangerously-skip-permissions"));
  assert.deepEqual(buildClaudeArgs({ ...launch, grant: ["read", "web"] }, "u").filter((x) => x.includes("WebFetch")), ["Read,Glob,Grep,WebSearch,WebFetch", "Read,Glob,Grep,WebSearch,WebFetch,mcp__app"]);
});

test("rate_limit_event: the subscription's 5h and 7d windows (unified or one per event)", () => {
  const [e] = parse({
    type: "rate_limit_event",
    rate_limit_info: { status: "allowed_warning", rateLimitType: "seven_day", utilization: 0.94, unifiedWindows: { five_hour: { utilization: 0.26, resetsAt: 1793212200 }, seven_day: { utilization: 0.94, resetsAt: 1793401200 } } },
  });
  assert.equal(e.type, "limits");
  if (e.type !== "limits") return;
  assert.deepEqual([e.limits.fiveHour, e.limits.sevenDay], [{ utilization: 0.26, resetsAt: 1793212200 }, { utilization: 0.94, resetsAt: 1793401200 }]);
  const [one] = parse({ type: "rate_limit_event", rate_limit_info: { rateLimitType: "five_hour", utilization: 0.5 } });
  assert.ok(one.type === "limits" && one.limits.fiveHour?.utilization === 0.5 && !one.limits.sevenDay);
  assert.deepEqual(parse({ type: "rate_limit_event", rate_limit_info: { status: "allowed" } }), [{ type: "ignored" }]);
});
