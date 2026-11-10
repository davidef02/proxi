import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";
import type { ChatItem, SessionState } from "../shared/protocol.ts";
import { buildAgyArgs, createAgyParseState, mapToolName, parseAgyLine } from "../server/agy.ts";
import { createServer, type RunningServer } from "../server/main.ts";
import { makeTempRepo } from "./helpers.ts";

const FAKE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-agy.mjs");
const FAKE_CLAUDE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "fake-claude.mjs"); // a pinning bug must never reach the real claude
process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9";
const b64 = (o: unknown) => Buffer.from(JSON.stringify(o)).toString("base64");
const run = (lines: object[]) => {
  const st = createAgyParseState();
  return { st, ev: lines.flatMap((l) => parseAgyLine(JSON.stringify(l), st)) };
};
const su = (o: object) => ({ event: "step_update", step_update: { conversation_id: "c1", ...o } });

describe("parseAgyLine", () => {
  it("maps tool names back to the claude style", () => {
    assert.equal(mapToolName("mcp_app_quiz"), "mcp__app__quiz");
    assert.equal(mapToolName("mcp_job_submit_findings"), "mcp__job__submit_findings");
    assert.equal(mapToolName("view_file"), "view_file");
  });

  it("init, text deltas (also only in DONE), one assistant per step", () => {
    const { ev } = run([
      { event: "init", conversation_id: "c1", init: { tools: ["view_file"] } },
      su({ step_index: 1, state: "ACTIVE", step_type: "agent_response", text_delta: "Ci" }),
      su({ step_index: 1, state: "DONE", step_type: "agent_response", text_delta: "ao" }),
      su({ step_index: 2, state: "DONE", step_type: "agent_response", text_delta: "Solo in DONE" }),
    ]);
    assert.deepEqual(ev.map((e) => e.type), ["init", "text-start", "text-delta", "text-delta", "assistant", "text-start", "text-delta", "assistant"]);
    assert.equal((ev[0] as any).sessionId, "c1");
    assert.equal((ev[4] as any).texts[0].text, "Ciao");
    assert.equal((ev[7] as any).texts[0].text, "Solo in DONE");
  });

  it("tool ACTIVE -> toolUse once, DONE/ERROR -> tool-result with the same id", () => {
    const { ev } = run([
      su({ step_index: 3, state: "ACTIVE", step_type: "tool", tool_name: "mcp_app_echo", tool_info: { name: "mcp_app_echo", parameters: { a: 1 } } }),
      su({ step_index: 3, state: "DONE", step_type: "tool", tool_name: "mcp_app_echo", tool_info: { output: "ok" } }),
      su({ step_index: 4, state: "ACTIVE", step_type: "tool", tool_name: "mcp_job_x", tool_info: { name: "mcp_job_x", parameters: {} } }),
      su({ step_index: 4, state: "ERROR", step_type: "tool", tool_name: "mcp_job_x", tool_info: { error: { message: "boom" } } }),
    ]);
    const uses = ev.filter((e) => e.type === "assistant").flatMap((e: any) => e.toolUses);
    assert.deepEqual(uses.map((u) => [u.name, u.input]), [["mcp__app__echo", { a: 1 }], ["mcp__job__x", {}]]);
    const res = ev.filter((e) => e.type === "tool-result") as any[];
    assert.deepEqual(res.map((r) => [r.toolUseId, r.isError, r.text]), [[uses[0].id, false, "ok"], [uses[1].id, true, "boom"]]);
  });

  it("result usage is the per-turn diff of cumulative counters; no cost; errors and denied actions", () => {
    const r = (n: number, extra: object = {}) => ({ event: "result", result: { conversation_id: "c1", status: "SUCCESS", response: "x", usage: { input_tokens: 10 * n, output_tokens: 5 * n, thinking_tokens: n, cache_read_tokens: 2 * n }, ...extra } });
    const { ev } = run([r(1), r(3, { status: "ERROR", error: "no quota", denied_actions: [{ action: "read_url", display_name: "ReadUrlContent" }] })]);
    const [a, denied, b] = ev as any[];
    assert.deepEqual([a.inputTokens, a.outputTokens, a.cacheReadTokens, a.costUsd, a.isError, a.subtype], [10, 6, 2, 0, false, "success"]);
    assert.deepEqual(denied, { type: "action-denied", action: "ReadUrlContent" }, "a structured event before the result, not a stderr line");
    assert.deepEqual([b.inputTokens, b.outputTokens, b.cacheReadTokens, b.isError, b.subtype, b.text], [20, 12, 4, true, "error", "no quota"]);
  });

  it("ignores garbage and builds args without -p, with --conversation on resume", () => {
    assert.deepEqual(parseAgyLine("not json", createAgyParseState()), []);
    const args = buildAgyArgs({ bin: { cmd: "agy" }, model: "m", repoRoot: "/repo", addDirs: ["/x"] } as any, "c9");
    assert.ok(!args.includes("-p") && args.includes("--agent") && args.join(" ").includes("--conversation c9") && args.join(" ").includes("--add-dir /repo --add-dir /x"));
  });
});

describe("integration: session through the agy path (fake-agy)", () => {
  let srv: RunningServer;
  let root: string;
  const api = (p: string, body?: unknown) =>
    fetch(srv.url + p, body === undefined ? undefined : { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const state = (id: string) => srv.manager.get(id) as SessionState;
  const texts = (id: string) => state(id).items.filter((i): i is Extract<ChatItem, { kind: "teacher" }> => i.kind === "teacher").map((t) => t.text);
  const waitFor = async <T,>(fn: () => T | undefined | false, ms = 15000): Promise<T> => {
    const end = Date.now() + ms;
    for (;;) {
      const v = fn();
      if (v) return v;
      if (Date.now() > end) throw new Error("waitFor timed out");
      await new Promise((r) => setTimeout(r, 25));
    }
  };
  const say = async (id: string, text: string) => assert.equal((await api(`/api/sessions/${id}/message`, { text })).status, 200);

  before(async () => {
    root = await makeTempRepo();
    fs.mkdirSync(path.join(root, ".learner"), { recursive: true });
    fs.writeFileSync(path.join(root, ".learner", "settings.json"), JSON.stringify({ provider: "agy" }));
    srv = await createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, addDirs: [], agyBin: process.execPath, agyBinArgs: [FAKE], claudeBin: process.execPath, claudeBinArgs: [FAKE_CLAUDE] });
  });
  after(async () => {
    await srv.close();
    await new Promise((r) => setTimeout(r, 500));
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
  });

  it("turn, MCP tool call (card + alwaysLoad), interrupt, resume on the same conversation", async () => {
    const r = await api("/api/sessions", { kind: "teach", course: "demo", target: { mode: "planned", title: "Il primo passo" }, ankiOptIn: false });
    const id = ((await r.json()) as { id: string }).id;
    await waitFor(() => state(id).status === "idle");
    const first = texts(id)[0];
    const conv = /\((conv-\d+)\)/.exec(first)?.[1];
    assert.ok(conv, "teacher answered with its conversation id: " + first);
    const scratch = path.join(root, ".learner", "sessions", id);
    assert.ok(fs.existsSync(path.join(scratch, ".agents", "agents", "ia-learner.md")));
    await waitFor(() => JSON.parse(fs.readFileSync(path.join(scratch, "state.json"), "utf-8")).resume === conv);
    assert.equal(JSON.parse(fs.readFileSync(path.join(scratch, "state.json"), "utf-8")).provider, "agy");
    assert.ok(state(id).usage.outputTokens > 0);

    await say(id, "FAKE:tool:ask:" + b64({ question: "Pronto?" }));
    const ask = await waitFor(() => state(id).items.find((i) => i.kind === "ask" && !i.answer) as any);
    assert.equal((await api(`/api/sessions/${id}/answer`, { requestId: ask.requestId, kind: "ask", answer: { selected: [], other: "si" } })).status, 200);
    await waitFor(() => state(id).status === "idle" && texts(id).some((t) => t.includes("alwaysLoad=true")));
    assert.ok(texts(id).some((t) => t.includes('"other":"si"')));

    await say(id, "FAKE:slow");
    await waitFor(() => state(id).status === "running");
    assert.equal((await api(`/api/sessions/${id}/interrupt`, {})).status, 200);
    await waitFor(() => state(id).status === "idle");
    assert.ok(state(id).items.some((i) => i.kind === "notice" && /interrotto/.test((i as any).text)));

    await say(id, "ancora qui");
    await waitFor(() => texts(id).filter((t) => t.includes(`(${conv})`)).length >= 2 && state(id).status === "idle");

    // Switching engine in settings does not move a running session: it stays on the engine that issued its token.
    assert.equal((await api("/api/settings", { provider: "claude" })).status, 200);
    await srv.close();
    srv = await createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false, addDirs: [], agyBin: process.execPath, agyBinArgs: [FAKE], claudeBin: process.execPath, claudeBinArgs: [FAKE_CLAUDE] });
    await say(id, "dopo il cambio");
    await waitFor(() => texts(id).filter((t) => t.includes(`(${conv})`)).length >= 3 && state(id).status === "idle");
  });
});
