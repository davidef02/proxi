// Own tiny client for the map (api.ts belongs to the shell agent). POSTs need x-learner-client.
import { lessonHref } from "../router.ts";
import type { GlobalEvent, GraphSource, JobRecord, SessionConfig } from "../../../shared/protocol.ts";
import type { GraphSnapshot, Position } from "../../../shared/graph/index.ts";

async function call<T>(method: "GET" | "POST", url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: method === "POST" ? { "content-type": "application/json", "x-learner-client": "1" } : undefined,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : undefined;
  if (!res.ok) throw new Error((data && data.error) || `${res.status} ${res.statusText}`);
  return data as T;
}

export const mapApi = {
  graph: () => call<GraphSnapshot>("GET", "/api/graph"),
  sources: () => call<GraphSource[]>("GET", "/api/graph/sources"),
  jobs: () => call<JobRecord[]>("GET", "/api/jobs"),
  extract: (sources?: string[]) => call<{ jobs: JobRecord[] }>("POST", "/api/graph/extract", sources ? { sources } : {}),
  positions: (positions: Record<string, Position>) => call<{ ok: true }>("POST", "/api/graph/positions", { positions }),
  deleteNode: (id: string) => call<{ ok: true }>("POST", `/api/graph/nodes/${encodeURIComponent(id)}/delete`),
  practice: (config: SessionConfig) => call<{ id: string }>("POST", "/api/sessions", config),
};

/** App-wide events (EventSource reconnects by itself). */
export function subscribeEvents(onEvent: (e: GlobalEvent) => void): () => void {
  const es = new EventSource("/api/events");
  es.onmessage = (m) => {
    try {
      onEvent(JSON.parse(m.data) as GlobalEvent);
    } catch {
      /* ignore malformed frame */
    }
  };
  return () => es.close();
}

/** Lesson file path from a provenance source id `<course>/<stem>`. */
export const lessonPathOf = (sourceId: string): string => {
  const i = sourceId.indexOf("/");
  return i < 0 ? sourceId : `courses/${sourceId.slice(0, i)}/lessons/${sourceId.slice(i + 1)}.html`;
};
export const courseOf = (sourceId: string): string => sourceId.split("/")[0] ?? "";

/** In-app reader href, optionally anchored on a lesson node id. */
export const lessonUrl = (sourceId: string, nodeId?: string): string => lessonHref(lessonPathOf(sourceId), nodeId);
