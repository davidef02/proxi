// Pure parser: one stream-json stdout line from `claude` -> normalised EngineEvent[].
import type { EngineEvent } from "./types.ts";

export interface StreamParseState {
  /** id of the message currently streaming (from stream_event message_start). */
  messageId: string;
}

export function createStreamParseState(): StreamParseState {
  return { messageId: "" };
}

const IGNORED: EngineEvent[] = [{ type: "ignored" }];
const num = (v: unknown): number => (typeof v === "number" && Number.isFinite(v) ? v : 0);
const str = (v: unknown): string => (typeof v === "string" ? v : "");
const isObj = (v: unknown): v is Record<string, any> => typeof v === "object" && v !== null;

function resultText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((p) => isObj(p) && p.type === "text" && typeof p.text === "string")
    .map((p) => p.text)
    .join("");
}

export function parseStreamLine(line: string, state: StreamParseState): EngineEvent[] {
  let m: unknown;
  try {
    m = JSON.parse(line);
  } catch {
    return IGNORED;
  }
  if (!isObj(m)) return IGNORED;
  try {
    return parseObject(m, state);
  } catch {
    return IGNORED;
  }
}

function parseObject(m: Record<string, any>, state: StreamParseState): EngineEvent[] {
  switch (m.type) {
    case "system":
      if (m.subtype !== "init") return IGNORED;
      return [
        {
          type: "init",
          sessionId: str(m.session_id),
          tools: Array.isArray(m.tools) ? m.tools.map(String) : [],
          mcpServers: Array.isArray(m.mcp_servers)
            ? m.mcp_servers.map((s: any) => ({ name: str(s?.name), status: str(s?.status) }))
            : [],
        },
      ];
    case "stream_event": {
      const e = m.event;
      if (!isObj(e)) return IGNORED;
      if (e.type === "message_start") {
        state.messageId = str(e.message?.id);
        return IGNORED;
      }
      if (e.type === "content_block_start" && e.content_block?.type === "text")
        return [{ type: "text-start", messageId: state.messageId, index: num(e.index) }];
      if (e.type === "content_block_delta" && e.delta?.type === "text_delta")
        return [{ type: "text-delta", messageId: state.messageId, index: num(e.index), text: str(e.delta.text) }];
      return IGNORED;
    }
    case "assistant": {
      const content: any[] = Array.isArray(m.message?.content) ? m.message.content : [];
      const texts: { index: number; text: string }[] = [];
      const toolUses: { index: number; id: string; name: string; input: unknown }[] = [];
      content.forEach((b, index) => {
        if (b?.type === "text") texts.push({ index, text: str(b.text) });
        else if (b?.type === "tool_use") toolUses.push({ index, id: str(b.id), name: str(b.name), input: b.input });
      });
      return [{ type: "assistant", messageId: str(m.message?.id), texts, toolUses }];
    }
    case "user": {
      const content = m.message?.content;
      if (!Array.isArray(content)) return IGNORED;
      const out: EngineEvent[] = content
        .filter((b) => b?.type === "tool_result")
        .map((b) => ({
          type: "tool-result" as const,
          toolUseId: str(b.tool_use_id),
          isError: b.is_error === true,
          text: resultText(b.content),
        }));
      return out.length ? out : IGNORED;
    }
    case "result": {
      const u = m.usage ?? {};
      return [
        {
          type: "result",
          isError: m.is_error === true,
          subtype: str(m.subtype),
          text: typeof m.result === "string" ? m.result : undefined,
          sessionId: str(m.session_id),
          inputTokens: num(u.input_tokens),
          outputTokens: num(u.output_tokens),
          cacheReadTokens: num(u.cache_read_input_tokens),
          cacheCreationTokens: num(u.cache_creation_input_tokens),
          costUsd: num(m.total_cost_usd),
        },
      ];
    }
    case "rate_limit_event": {
      // The subscription quota: unifiedWindows carries both windows; older builds report one window per event.
      const info = m.rate_limit_info ?? {};
      const win = (w: any) => (w && typeof w.utilization === "number" ? { utilization: w.utilization, resetsAt: typeof w.resetsAt === "number" ? w.resetsAt : undefined } : undefined);
      const u = info.unifiedWindows ?? {};
      const fiveHour = win(u.five_hour) ?? (info.rateLimitType === "five_hour" ? win(info) : undefined);
      const sevenDay = win(u.seven_day) ?? (info.rateLimitType === "seven_day" ? win(info) : undefined);
      if (!fiveHour && !sevenDay) return IGNORED;
      return [{ type: "limits", limits: { ...(fiveHour && { fiveHour }), ...(sevenDay && { sevenDay }), at: new Date().toISOString() } }];
    }
    case "control_response": {
      const r = m.response ?? {};
      return [
        {
          type: "control-response",
          requestId: str(r.request_id),
          ok: r.subtype === "success",
          error: typeof r.error === "string" ? r.error : undefined,
        },
      ];
    }
    default:
      return IGNORED;
  }
}
