// Scenario loader + anchor checker for `npm run eval` (issue #65, AC5).
// The runner (run.ts) does the I/O; the judgement here is pure so it can be unit-tested.
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import type { CognitiveNodePayload } from "../../extensions/html-lesson.ts";
import type { LessonTarget, SessionKind } from "../shared/protocol.ts";

export interface EvalScenario {
  course: string;
  kind: SessionKind;
  target?: LessonTarget;
  ankiOptIn?: boolean;
  /** First learner message, sent when the session starts. */
  message: string;
  /** Graph ids the scenario seeds: an anchor to anything else does not count as a known concept. */
  knownIds: string[];
  maxTurns: number;
  /** Reply to a teacher turn that asks nothing interactive. */
  autoReply: string;
}

const SCENARIOS_DIR = fileURLToPath(new URL("./scenarios/", import.meta.url));

export function loadScenario(name: string): { scenario: EvalScenario; dir: string } {
  const dir = path.join(SCENARIOS_DIR, name);
  const file = path.join(dir, "scenario.json");
  if (!fs.existsSync(file)) throw new Error(`unknown eval scenario: ${name}`);
  return { scenario: JSON.parse(fs.readFileSync(file, "utf-8")) as EvalScenario, dir };
}

/** Everything the learner reads: if a concept is named anywhere here, the anchor is visible in the prose.
 *  Markup is stripped first: `<strong><code>ref</code></strong> di Vue` names the concept as plainly as
 *  `ref di Vue`, and the teach prompt asks for exactly that formatting on code identifiers. */
function proseOf(n: CognitiveNodePayload): string {
  const parts: string[] = [n.title, n.content];
  if (n.motivation) parts.push(n.motivation);
  if (n.connection) parts.push(n.connection);
  for (const s of n.steps ?? []) for (const v of Object.values(s)) if (typeof v === "string") parts.push(v);
  return parts
    .join("\n")
    .replace(/<[^>]*>/g, " ")
    .replace(/[`*_]/g, "")
    .replace(/\s+/g, " ")
    .toLowerCase();
}

export function checkAnchors(nodes: CognitiveNodePayload[], known: Map<string, string>): { ok: boolean; failures: string[] } {
  const failures: string[] = [];
  if (nodes.length === 0) failures.push("no nodes written");

  for (const n of nodes) {
    // Anchors to ids outside the seeded map are ignored: they prove nothing about known concepts.
    const anchors = (n.anchors ?? []).filter((a) => known.has(a.id));
    if (anchors.length === 0) {
      failures.push(`${n.nodeId}: no anchor`);
      continue;
    }
    const prose = proseOf(n);
    for (const a of anchors) {
      const live = known.get(a.id)!.toLowerCase();
      if (!prose.includes(live) && !prose.includes(a.name.toLowerCase())) {
        failures.push(`${n.nodeId}: prose does not name ${a.name}`);
      }
    }
  }

  return { ok: failures.length === 0, failures };
}
