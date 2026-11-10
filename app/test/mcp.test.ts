import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { z } from "zod";
import { handleMcp, MCP_PREFIX } from "../server/mcp.ts";
import type { AppTool, ToolContext } from "../server/types.ts";

const ctx = {} as ToolContext;
const echo: AppTool<{ msg: string; n?: number }> = {
  name: "quiz",
  description: "echo",
  schema: z.object({ kind: z.literal("x"), msg: z.string(), n: z.number().optional() }) as never,
  async run(i) {
    if (i.msg === "boom") throw new Error("kaboom");
    if (i.msg === "bad") return { text: "nope", isError: true };
    return { text: `got ${i.msg}` };
  },
};
const tools = [echo as unknown as AppTool];
const rpc = (method: string, params?: unknown, id: number | undefined = 1) => handleMcp({ jsonrpc: "2.0", id, method, params }, tools, ctx) as Promise<any>;

describe("handleMcp", () => {
  it("exports the prefix", () => assert.equal(MCP_PREFIX, "mcp__app__"));

  it("initialize echoes the protocol version", async () => {
    const r = await rpc("initialize", { protocolVersion: "2025-03-26" });
    assert.equal(r.id, 1);
    assert.equal(r.result.protocolVersion, "2025-03-26");
    assert.deepEqual(r.result.capabilities, { tools: {} });
    assert.equal(r.result.serverInfo.name, "app");
  });

  it("notifications return null, ping returns {}", async () => {
    assert.equal(await rpc("notifications/initialized", undefined, undefined), null);
    assert.deepEqual((await rpc("ping")).result, {});
  });

  it("tools/list gives an object JSON schema without $schema, discriminator first", async () => {
    const t = (await rpc("tools/list")).result.tools[0];
    assert.equal(t.name, "quiz");
    assert.equal(t.inputSchema.type, "object");
    assert.equal(t.inputSchema.$schema, undefined);
    assert.deepEqual(Object.keys(t.inputSchema.properties), ["kind", "msg", "n"]);
    assert.deepEqual(t.inputSchema.required, ["kind", "msg"]);
  });

  it("tools/call runs the tool", async () => {
    const r = (await rpc("tools/call", { name: "quiz", arguments: { kind: "x", msg: "hi" } })).result;
    assert.deepEqual(r, { content: [{ type: "text", text: "got hi" }], isError: false });
  });

  it("invalid input yields an isError result naming the path", async () => {
    const r = (await rpc("tools/call", { name: "quiz", arguments: { kind: "y", n: "a" } })).result;
    assert.equal(r.isError, true);
    assert.match(r.content[0].text, /^Invalid input: .*kind.*msg.*n.*Fix and call again\.$/s);
  });

  it("a throwing tool and an isError outcome stay inside the result", async () => {
    const boom = (await rpc("tools/call", { name: "quiz", arguments: { kind: "x", msg: "boom" } })).result;
    assert.equal(boom.isError, true);
    assert.equal(boom.content[0].text, "kaboom");
    const bad = (await rpc("tools/call", { name: "quiz", arguments: { kind: "x", msg: "bad" } })).result;
    assert.deepEqual(bad, { content: [{ type: "text", text: "nope" }], isError: true });
  });

  it("unknown tool is an isError result", async () => {
    const r = (await rpc("tools/call", { name: "nope", arguments: {} })).result;
    assert.equal(r.isError, true);
    assert.match(r.content[0].text, /Unknown tool/);
  });

  it("unknown methods with an id (incl. server/discover) are -32601", async () => {
    for (const m of ["server/discover", "resources/list"]) {
      const r = await rpc(m);
      assert.equal(r.error.code, -32601);
      assert.equal(r.id, 1);
    }
  });

  it("garbage body is -32600", async () => {
    const r = (await handleMcp("nonsense", tools, ctx)) as any;
    assert.equal(r.error.code, -32600);
  });
});
