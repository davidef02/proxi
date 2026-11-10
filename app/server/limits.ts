// Subscription quota (the 5-hour and 7-day windows): `claude` reports it in every stream as a rate_limit_event. The
// latest report is kept in memory and in .learner/limits.json, so the meter has a value before the first turn.
import fs from "node:fs";
import path from "node:path";
import type { QuotaLimits } from "../shared/protocol.ts";
import { emitGlobal } from "./events.ts";

const last = new Map<string, QuotaLimits>(); // by repo root
const file = (repoRoot: string) => path.join(repoRoot, ".learner", "limits.json");

export function recordLimits(repoRoot: string, limits: QuotaLimits): void {
  // a report without unifiedWindows covers one window only: keep the other window's last value
  const next = { ...readLimits(repoRoot), ...limits };
  last.set(repoRoot, next);
  try {
    fs.mkdirSync(path.dirname(file(repoRoot)), { recursive: true });
    fs.writeFileSync(file(repoRoot), JSON.stringify(next));
  } catch {
    /* best effort: the meter still has the in-memory value */
  }
  emitGlobal({ type: "limits", limits: next });
}

export function readLimits(repoRoot: string): QuotaLimits | null {
  const hit = last.get(repoRoot);
  if (hit) return hit;
  try {
    const l = JSON.parse(fs.readFileSync(file(repoRoot), "utf-8")) as QuotaLimits;
    last.set(repoRoot, l);
    return l;
  } catch {
    return null;
  }
}
