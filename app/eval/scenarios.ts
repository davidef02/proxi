// The fixed eval runs: one lesson and one chapter plan, each with the fixture graph and with nothing to
// reuse. Checks come from checks.ts; harness.ts drives the engine.
import type { SessionConfig } from "../shared/protocol.ts";
import { checkCurriculum, checkMinProbes, checkSolidSkipped, checkStrands, checkTaughtGroups } from "./checks.ts";
import type { RunResult } from "./harness.ts";

export interface Scenario {
  name: string;
  /** Fixture Vue graph: false = nothing relevant to reuse, as on a new machine. */
  graph: boolean;
  config: SessionConfig;
  stopAt: "plan" | "curriculum";
  check: (r: RunResult) => string[];
}

const NUXT_NOTE = "Voglio imparare Nuxt: una lezione su come Nuxt organizza pagine e dati sopra Vue.";

export const SCENARIOS: Scenario[] = [
  {
    name: "lezione-singola-nuxt",
    graph: true,
    config: { kind: "teach", course: "lezioni-singole", target: { mode: "free" }, ankiOptIn: false, note: NUXT_NOTE },
    stopAt: "plan",
    check: (r) => [
      ...checkSolidSkipped(r.probes),
      ...checkTaughtGroups(r.probes),
      ...checkMinProbes(r.probes, 1),
      ...checkStrands(r.probes, 2),
    ],
  },
  {
    name: "lezione-singola-nuxt-vuoto",
    graph: false,
    config: { kind: "teach", course: "lezioni-singole", target: { mode: "free" }, ankiOptIn: false, note: NUXT_NOTE },
    stopAt: "plan",
    check: (r) => [...checkMinProbes(r.probes, 2), ...checkStrands(r.probes, 2)],
  },
  {
    name: "plan-nuxt",
    graph: true,
    config: { kind: "plan", course: "nuxt", chapterTitle: "Nuxt" },
    stopAt: "curriculum",
    check: (r) => [
      ...checkSolidSkipped(r.probes),
      ...checkCurriculum(r.curriculumTitles ?? []),
      ...(r.curriculumTitles === undefined ? ["no curriculum proposed"] : []),
    ],
  },
  {
    name: "plan-nuxt-vuoto",
    graph: false,
    config: { kind: "plan", course: "nuxt", chapterTitle: "Nuxt" },
    stopAt: "curriculum",
    check: (r) => (r.curriculumTitles === undefined ? ["no curriculum proposed"] : []),
  },
];
