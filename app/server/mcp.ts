// Minimal MCP (JSON-RPC 2.0 over HTTP) server side: initialize, ping, tools/list, tools/call.
import { z } from "zod";
import type { ToolOutcome } from "./types.ts";

export const MCP_PREFIX = "mcp__app__";

type Rpc = { jsonrpc?: string; id?: string | number | null; method?: string; params?: any };

const ok = (id: unknown, result: unknown) => ({ jsonrpc: "2.0", id, result });
const fail = (id: unknown, code: number, message: string) => ({ jsonrpc: "2.0", id: id ?? null, error: { code, message } });
const content = (text: string, isError = false, images: ToolOutcome["images"] = []) => ({
  content: [{ type: "text", text }, ...images.map((i) => ({ type: "image", data: i.data, mimeType: i.mimeType }))],
  isError,
});

/** Any tool served over MCP: session tools (AppTool + ToolContext) and job tools (JobTool + JobContext). */
export interface McpTool<C> {
  name: string;
  description: string;
  schema: z.ZodType<any>;
  run(input: any, ctx: C): Promise<ToolOutcome>;
}

export function toJsonSchema(schema: z.ZodType): Record<string, unknown> {
  const { $schema: _drop, ...rest } = z.toJSONSchema(schema, { io: "input" }) as Record<string, unknown>;
  return { type: "object", ...rest };
}

async function one<C>(req: Rpc, tools: McpTool<C>[], ctx: C, serverName: string): Promise<object | null> {
  if (!req || typeof req !== "object" || typeof req.method !== "string") return fail(req?.id, -32600, "Invalid Request");
  const { id, method, params } = req;
  const isNotification = id === undefined || id === null;
  if (method.startsWith("notifications/")) return null;
  switch (method) {
    case "initialize":
      return ok(id, { protocolVersion: params?.protocolVersion ?? "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: serverName, version: "1" } });
    case "ping":
      return ok(id, {});
    case "tools/list":
      return ok(id, { tools: tools.map((t) => ({ name: t.name, description: t.description, inputSchema: toJsonSchema(t.schema), _meta: { "antigravity/alwaysLoad": true } })) });
    case "tools/call": {
      const tool = tools.find((t) => t.name === params?.name);
      if (!tool) return ok(id, content(`Unknown tool "${params?.name}". Available: ${tools.map((t) => t.name).join(", ")}.`, true));
      const parsed = tool.schema.safeParse(params?.arguments ?? {});
      if (!parsed.success) {
        const issues = parsed.error.issues.map((i) => `${i.path.join(".") || "(input)"}: ${i.message}`).join("; ");
        return ok(id, content(`Invalid input: ${issues}. Fix and call again.`, true));
      }
      try {
        const out = await tool.run(parsed.data, ctx);
        return ok(id, content(out.text, out.isError === true, out.images));
      } catch (e) {
        return ok(id, content(e instanceof Error ? e.message : String(e), true));
      }
    }
    default:
      return isNotification ? null : fail(id, -32601, `Method not found: ${method}`);
  }
}

/** Returns the JSON-RPC response, or null for a notification (HTTP 202). */
export async function handleMcp<C>(body: unknown, tools: McpTool<C>[], ctx: C, serverName = "app"): Promise<object | null> {
  if (Array.isArray(body)) {
    const out = (await Promise.all(body.map((b) => one(b, tools, ctx, serverName)))).filter((r) => r !== null);
    return out.length ? (out as unknown as object) : null;
  }
  return one(body as Rpc, tools, ctx, serverName);
}
