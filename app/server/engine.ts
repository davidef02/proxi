// The engine seam: one EngineLaunch (intent) in, one EngineHandle + EngineEvent stream out. Three adapters: claude.ts, agy.ts, opencode.ts.
import type { EffortLevel, LearnerSettings } from "../shared/protocol.ts";
import { spawnAgy } from "./agy.ts";
import { spawnClaude } from "./claude.ts";
import { spawnOpencode } from "./opencode.ts";
import type { EngineCallbacks, EngineHandle, EngineBin, EngineLaunch, Grant, Provider } from "./types.ts";

export type EngineBins = Record<Provider, EngineBin>;

/** Prompts name the claude tools; under agy the built-in read tools are called otherwise. Appended to the system prompt at launch. */
export function withEngineNote(systemPrompt: string, provider: Provider, grant: Grant[]): string {
  if (provider !== "agy" || !grant.length) return systemPrompt;
  return `${systemPrompt}

On this engine the read-only tools are named view_file (Read), list_dir (Glob), grep_search (Grep), search_web (WebSearch); there is no page fetch: cite search results with their URLs and say when a claim rests only on a snippet.`;
}

/** The one model rule. agy/opencode: its model id per role from settings. claude: the session's alias, else the role's setting. */
export function modelFor(settings: LearnerSettings, provider: Provider, role: "teacher" | "subagent" | "map", alias?: string): string {
  if (provider === "agy") return settings.agyModels[role];
  if (provider === "opencode") return settings.opencodeModels[role];
  return alias ?? { teacher: settings.teacherModel, subagent: settings.subagentModel, map: settings.mapModel }[role];
}

/** The one effort rule. Claude: setting for the role when not "default". agy/opencode: undefined. */
export function effortFor(settings: LearnerSettings, provider: Provider, role: "teacher" | "subagent" | "map"): EffortLevel | undefined {
  if (provider === "agy" || provider === "opencode") return undefined;
  const eff = settings.effort?.[role];
  return eff && eff !== "default" ? eff : undefined;
}

export function spawnEngine(l: EngineLaunch, cb: EngineCallbacks): EngineHandle {
  if (l.provider === "agy") return spawnAgy(l, cb);
  if (l.provider === "opencode") return spawnOpencode(l, cb);
  return spawnClaude(l, cb);
}
