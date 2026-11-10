// Pure helpers on the plan DAG (mermaid) and its per-node status.
import type { PlanNodeStatus, PlanState, ProgressRecord } from "../shared/protocol.ts";

const HEADER = /^(graph|flowchart)\s+(TD|LR)\b/;

/** `N1`, `N2`... in first-appearance order, for ids defined with `[`. */
export function parseDagIds(dag: string): string[] {
  const ids: string[] = [];
  for (const m of dag.matchAll(/\b(N\d+)\s*\[/g)) if (!ids.includes(m[1])) ids.push(m[1]);
  return ids;
}

/** Errors (line-numbered where possible); empty when the DAG is acceptable. */
export function validateDagLabels(dag: string): string[] {
  const errors: string[] = [];
  const lines = dag.split(/\r?\n/);
  const first = lines.findIndex((l) => l.trim() !== "");
  if (first < 0 || !HEADER.test(lines[first].trim())) {
    errors.push(`line ${first < 0 ? 1 : first + 1}: the DAG must start with "graph TD", "graph LR", "flowchart TD" or "flowchart LR".`);
  }
  let found = 0;
  lines.forEach((line, i) => {
    for (const m of line.matchAll(/\bN(\d+)\s*\[([^\]]*)\]/g)) {
      found++;
      if (!m[2].trim().startsWith(`"Nodo ${m[1]}: `)) {
        errors.push(`line ${i + 1}: label of N${m[1]} must be quoted and start with "Nodo ${m[1]}: ", e.g. N${m[1]}["Nodo ${m[1]}: ..."].`);
      }
    }
  });
  if (found === 0) errors.push('no plan node found: define nodes as N1["Nodo 1: ..."], N2["Nodo 2: ..."], ...');
  return errors;
}

/**
 * Status per DAG id. Lesson node i (capture order) realises N(i+1) => done. A latest incorrect
 * `check` (or dontKnow) overrides done with missed. `current` = node of the latest predict/check
 * record when still todo, else the first todo.
 */
export function derivePlanState(
  base: { topic: string; goal: string; dag: string; approved: boolean },
  lessonNodeCount: number,
  progress: ProgressRecord[],
): PlanState {
  const ids = parseDagIds(base.dag);
  const nodeStatus: Record<string, PlanNodeStatus> = {};
  ids.forEach((id, i) => (nodeStatus[id] = i < lessonNodeCount ? "done" : "todo"));
  for (const id of ids) {
    const latest = [...progress].reverse().find((r) => r.kind === "check" && r.nodeRefs.includes(id));
    if (latest && latest.correct === false) nodeStatus[id] = "missed";
  }
  const last = [...progress].reverse().find((r) => (r.kind === "predict" || r.kind === "check") && r.nodeRefs.some((n) => ids.includes(n)));
  const lastId = last?.nodeRefs.find((n) => ids.includes(n));
  const current = lastId && nodeStatus[lastId] === "todo" ? lastId : ids.find((id) => nodeStatus[id] === "todo");
  if (current) nodeStatus[current] = "current";
  return { topic: base.topic, goal: base.goal, dag: base.dag, approved: base.approved, nodeStatus };
}
