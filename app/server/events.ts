// App-wide event bus behind GET /api/events. Modules emit; the SSE route fans out.
import type { GlobalEvent } from "../shared/protocol.ts";

const subs = new Set<(e: GlobalEvent) => void>();

export function emitGlobal(e: GlobalEvent): void {
  for (const fn of subs) {
    try {
      fn(e);
    } catch {
      /* a dead subscriber must not break the others */
    }
  }
}

export function subscribeGlobal(fn: (e: GlobalEvent) => void): () => void {
  subs.add(fn);
  return () => subs.delete(fn);
}
