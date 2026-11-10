import type { ChatItem, InteractiveItem } from "../../../shared/protocol.ts";

/**
 * The unresolved interactive item matching the authoritative server pendingRequestId.
 * When pendingRequestId is null or not found, returns null (never resurrects older/superseded questions).
 */
export function pendingItem(items: ChatItem[], pendingRequestId: string | null | undefined): InteractiveItem | null {
  if (!pendingRequestId) return null;
  const item = items.find((it): it is InteractiveItem => "requestId" in it && it.requestId === pendingRequestId);
  if (!item) return null;
  if (item.cancelled || ("result" in item && !!item.result) || ("answer" in item && !!item.answer) || ("decision" in item && !!item.decision)) {
    return null;
  }
  return item;
}

/** The open practice problem, if any: the last `problem` item that has no conclusion yet. */
export function openProblem(items: ChatItem[]): Extract<ChatItem, { kind: "problem" }> | null {
  const last = items.findLast((it) => it.kind === "problem");
  return last && last.kind === "problem" && !last.conclusion ? last : null;
}
