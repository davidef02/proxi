// Learner settings: <repo>/.learner/settings.json, merged over DEFAULT_SETTINGS.
import * as fs from "node:fs";
import * as path from "node:path";
import { z } from "zod";
import { DEFAULT_SETTINGS, EFFORT_SETTINGS, MODELS, PROVIDERS, SYNC_PROVIDERS, type EffortSetting, type LearnerSettings } from "../shared/protocol.ts";

const model = z.enum(MODELS as [string, ...string[]]);
const effortSetting = z.enum(EFFORT_SETTINGS as [EffortSetting, ...EffortSetting[]]);
export const settingsPatchSchema = z
  .object({
    provider: z.enum(PROVIDERS),
    claudeAuth: z.enum(["subscription", "apiKey"]),
    anthropicApiKey: z.string().max(200),
    agyModels: z.object({ teacher: z.string().min(1).max(100), subagent: z.string().min(1).max(100), map: z.string().min(1).max(100) }),
    opencodeModels: z.object({ teacher: z.string().min(1).max(100), subagent: z.string().min(1).max(100), map: z.string().min(1).max(100) }),
    teacherModel: model,
    subagentModel: model,
    mapModel: model,
    effort: z.object({ teacher: effortSetting, subagent: effortSetting, map: effortSetting }).partial(),
    autoMap: z.boolean(),
    sttProvider: z.enum(["groq", "deepgram"]),
    sttApiKey: z.string().max(200),
    dictationLanguage: z.string().min(2).max(20),
    syncProvider: z.enum(SYNC_PROVIDERS),
    githubClientId: z.string().max(200),
    googleClientId: z.string().max(300),
    googleClientSecret: z.string().max(200),
  })
  .partial()
  .strict();

export function maskApiKey(key?: string): string {
  if (!key || !key.trim()) return "";
  const t = key.trim();
  if (t.length <= 10) return "••••••••";
  return `${t.slice(0, 7)}…${t.slice(-4)}`;
}

const file = (repoRoot: string) => path.join(repoRoot, ".learner", "settings.json");

/** Lenient: a bad or unknown key is dropped on its own, the other settings survive. */
export function readSettings(repoRoot: string): LearnerSettings {
  const out: LearnerSettings = {
    ...DEFAULT_SETTINGS,
    agyModels: { ...DEFAULT_SETTINGS.agyModels },
    opencodeModels: { ...DEFAULT_SETTINGS.opencodeModels },
    effort: { ...DEFAULT_SETTINGS.effort },
  };
  try {
    const raw = JSON.parse(fs.readFileSync(file(repoRoot), "utf-8"));
    if (raw && typeof raw === "object")
      for (const [k, schema] of Object.entries(settingsPatchSchema.shape)) {
        const v = schema.safeParse(raw[k]);
        if (v.success && v.data !== undefined) {
          if (k === "effort" || k === "agyModels" || k === "opencodeModels") {
            (out as any)[k] = { ...(out as any)[k], ...(v.data as object) };
          } else {
            (out as any)[k] = v.data;
          }
        }
      }
  } catch {
    /* missing or unreadable: defaults */
  }
  return out;
}

export function writeSettings(repoRoot: string, patch: unknown): LearnerSettings {
  const parsed = settingsPatchSchema.parse(patch);
  if (parsed.anthropicApiKey !== undefined && (parsed.anthropicApiKey.includes("…") || parsed.anthropicApiKey.includes("•"))) {
    delete parsed.anthropicApiKey;
  }
  const current = readSettings(repoRoot);
  const next: LearnerSettings = {
    ...current,
    ...(parsed as Partial<LearnerSettings>),
    ...(parsed.effort ? { effort: { ...current.effort, ...parsed.effort } } : {}),
    ...(parsed.agyModels ? { agyModels: { ...current.agyModels, ...parsed.agyModels } } : {}),
    ...(parsed.opencodeModels ? { opencodeModels: { ...current.opencodeModels, ...parsed.opencodeModels } } : {}),
  };
  const f = file(repoRoot);
  fs.mkdirSync(path.dirname(f), { recursive: true });
  const body = JSON.stringify(next, null, 2) + "\n";
  const tmp = `${f}.${process.pid}.tmp`;
  try {
    fs.writeFileSync(tmp, body);
    fs.renameSync(tmp, f);
  } catch {
    // Windows: rename over a file another process holds open can fail with EPERM/EBUSY: write in place instead.
    fs.rmSync(tmp, { force: true });
    fs.writeFileSync(f, body);
  }
  return next;
}
