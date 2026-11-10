import { useEffect, useReducer, useState } from "react";
import { api, subscribe } from "../api.ts";
import type { ChatItem, ServerEvent, SessionState } from "../../../shared/protocol.ts";

export interface SessionView {
  /** null until the first snapshot. */
  session: SessionState | null;
  connected: boolean;
}

export const initialView: SessionView = { session: null, connected: false };

export type ViewEvent = ServerEvent | { type: "reset" } | { type: "disconnected" };

export function sessionReducer(view: SessionView, ev: ViewEvent): SessionView {
  if (ev.type === "reset") return initialView;
  if (ev.type === "disconnected") return { ...view, connected: false };
  if (ev.type === "snapshot") return { ...view, session: ev.state, connected: true };
  const s = view.session;
  if (!s) return view; // events before the first snapshot are covered by it
  switch (ev.type) {
    case "item": {
      const i = s.items.findIndex((x) => x.id === ev.item.id);
      const items: ChatItem[] = i < 0 ? [...s.items, ev.item] : s.items.map((x, k) => (k === i ? ev.item : x));
      return { ...view, session: { ...s, items } };
    }
    case "delta": {
      const items = s.items.map((x) => (x.id === ev.itemId && x.kind === "teacher" ? { ...x, text: x.text + ev.text } : x));
      return { ...view, session: { ...s, items } };
    }
    case "status":
      return {
        ...view,
        session: {
          ...s,
          status: ev.status,
          retryable: ev.retryable ?? (ev.status === "idle" && !s.lastError ? false : s.retryable),
          lastError: ev.status === "error" ? (ev.error ?? s.lastError) : undefined,
        },
      };
    case "pending":
      return { ...view, session: { ...s, pendingRequestId: ev.requestId } };
    case "lesson":
      return { ...view, session: { ...s, lessonPath: ev.lessonPath } };
    case "plan":
      return { ...view, session: { ...s, plan: ev.plan } };
    case "exercises":
      return { ...view, session: { ...s, exercises: ev.exercises } };
    case "usage":
      return { ...view, session: { ...s, usage: ev.usage } };
  }
}

export function useSession(id: string) {
  const [view, dispatch] = useReducer(sessionReducer, initialView);
  const [loadError, setLoadError] = useState<string | null>(null);
  useEffect(() => {
    // A different session: drop everything from the previous one.
    dispatch({ type: "reset" });
    setLoadError(null);
    let stale = false;
    // The SSE snapshot is the source of truth; this call only surfaces "not found" early.
    api.session(id).catch((e: Error) => !stale && setLoadError(e.message));
    const off = subscribe(id, dispatch, () => dispatch({ type: "disconnected" }));
    return () => {
      stale = true;
      off();
    };
  }, [id]);
  return { view, loadError };
}
