import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  buildOpencodeArgs,
  createOpencodeParseState,
  mapOpencodeToolName,
  parseOpencodeLine,
  resolveOpencodeCmd,
  OPENCODE_MODELS_FALLBACK,
} from "../server/opencode.ts";
import type { EngineLaunch } from "../server/types.ts";

describe("opencode adapter unit tests", () => {
  it("mapOpencodeToolName handles prefixes", () => {
    assert.equal(mapOpencodeToolName("app.write_node"), "mcp__app__write_node");
    assert.equal(mapOpencodeToolName("app_write_node"), "mcp__app__write_node");
    assert.equal(mapOpencodeToolName("job.submit_findings"), "mcp__job__submit_findings");
    assert.equal(mapOpencodeToolName("job_submit_findings"), "mcp__job__submit_findings");
    assert.equal(mapOpencodeToolName("mcp__app__quiz"), "mcp__app__quiz");
    assert.equal(mapOpencodeToolName("read"), "read");
  });

  it("buildOpencodeArgs constructs arguments correctly", () => {
    const launch: EngineLaunch = {
      provider: "opencode",
      bin: { cmd: "opencode", args: ["--extra-flag"] },
      repoRoot: "/repo",
      scratchDir: "/scratch",
      model: "omniroute/auto/best-coding",
      systemPrompt: "sys",
      grant: ["read"],
      mcp: { name: "app", url: "http://127.0.0.1:4000/mcp", token: "tok" },
      addDirs: [],
    };

    const freshArgs = buildOpencodeArgs(launch, "hello");
    assert.deepEqual(freshArgs, [
      "--extra-flag",
      "run",
      "--format", "json",
      "--auto",
      "--agent", "ia-learner",
      "--model", "omniroute/auto/best-coding",
      "hello",
    ]);

    const resumeArgs = buildOpencodeArgs(launch, "hello again", "ses_12345");
    assert.deepEqual(resumeArgs, [
      "--extra-flag",
      "run",
      "--format", "json",
      "--auto",
      "--agent", "ia-learner",
      "--model", "omniroute/auto/best-coding",
      "--session", "ses_12345",
      "hello again",
    ]);
  });

  it("resolveOpencodeCmd handles command types", () => {
    const r1 = resolveOpencodeCmd("C:\\path\\to\\opencode.exe");
    assert.equal(r1.cmd, "C:\\path\\to\\opencode.exe");
    assert.equal(r1.shell, undefined);

    const r2 = resolveOpencodeCmd("custom.cmd");
    if (process.platform === "win32") {
      assert.equal(r2.shell, true);
    }
  });

  it("parseOpencodeLine handles step_start, text, tool_use, and step_finish", () => {
    const st = createOpencodeParseState();

    // 1. step_start: emits init
    const e1 = parseOpencodeLine(
      JSON.stringify({
        type: "step_start",
        sessionID: "ses_abc123",
        part: { id: "p1", messageID: "m1", type: "step-start" },
      }),
      st,
    );
    assert.equal(e1.length, 1);
    assert.equal(e1[0].type, "init");
    assert.equal((e1[0] as any).sessionId, "ses_abc123");

    // 2. text: emits text-start, text-delta, assistant
    const e2 = parseOpencodeLine(
      JSON.stringify({
        type: "text",
        sessionID: "ses_abc123",
        part: { id: "p2", messageID: "m1", type: "text", text: "Ciao studente" },
      }),
      st,
    );
    assert.equal(e2.length, 3);
    assert.equal(e2[0].type, "text-start");
    assert.equal(e2[1].type, "text-delta");
    assert.equal((e2[1] as any).text, "Ciao studente");
    assert.equal(e2[2].type, "assistant");

    // 3. tool_use: emits assistant and tool-result
    const e3 = parseOpencodeLine(
      JSON.stringify({
        type: "tool_use",
        sessionID: "ses_abc123",
        part: {
          id: "call_99",
          tool: "execute",
          state: {
            status: "completed",
            input: { query: "test" },
            output: "risultato",
            metadata: { metadata: { toolCalls: [{ tool: "app.ask", status: "completed" }] } },
          },
        },
      }),
      st,
    );
    assert.equal(e3.length, 2);
    assert.equal(e3[0].type, "assistant");
    assert.equal((e3[0] as any).toolUses[0].name, "mcp__app__ask");
    assert.equal(e3[1].type, "tool-result");
    assert.equal((e3[1] as any).text, "risultato");

    // 4. step_finish (stop): emits result
    const e4 = parseOpencodeLine(
      JSON.stringify({
        type: "step_finish",
        sessionID: "ses_abc123",
        part: {
          id: "p4",
          type: "step-finish",
          reason: "stop",
          cost: 0.005,
          tokens: { input: 150, output: 50, reasoning: 10, cache: { read: 20, write: 0 } },
        },
      }),
      st,
    );
    assert.equal(e4.length, 1);
    assert.equal(e4[0].type, "result");
    const res = e4[0] as any;
    assert.equal(res.isError, false);
    assert.equal(res.subtype, "success");
    assert.equal(res.inputTokens, 150);
    assert.equal(res.outputTokens, 60);
    assert.equal(res.cacheReadTokens, 20);
    assert.equal(res.costUsd, 0.005);
  });
});
