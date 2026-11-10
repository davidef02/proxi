import { MODELS, type ClaudeStatusResponse, type EffortSetting, type LearnerSettings, type ModelAlias } from "../../../../shared/protocol.ts";
import type { SettingsResponse } from "../../api.ts";
import { QuotaMeter } from "../QuotaMeter.tsx";
import { ROW, SELECT, Section, Label, Segmented, type Save } from "./ui.tsx";

const HINT: Record<ModelAlias, string> = { opus: "il più capace, pesa di più sulla quota", sonnet: "equilibrato", haiku: "veloce e leggero" };
const MODEL_KEYS = [
  ["teacherModel", "Docente", "lezioni, piani e ripasso", "teacher"],
  ["subagentModel", "Subagenti", "ricerche, figure, demo", "subagent"],
  ["mapModel", "Mappa", "estrazione delle idee", "map"],
] as const;
const EFFORT_OPTIONS: [EffortSetting, string][] = [
  ["default", "default"],
  ["low", "low"],
  ["medium", "medium"],
  ["high", "high"],
  ["xhigh", "xhigh"],
  ["max", "max"],
];
const PROVIDERS: [LearnerSettings["provider"], string][] = [["claude", "Claude Code"], ["agy", "Antigravity"], ["opencode", "OpenCode"]];
const CLAUDE_AUTH_OPTIONS: [LearnerSettings["claudeAuth"], string][] = [["subscription", "Abbonamento"], ["apiKey", "API key"]];

export function EngineSection({ s, save, claudeStatus, agyModels, opencodeModels }: { s: SettingsResponse; save: Save; claudeStatus?: ClaudeStatusResponse; agyModels: string[]; opencodeModels: string[] }) {
  const agy = s.provider === "agy";
  const opencode = s.provider === "opencode";
  return (
    <Section title="Motore">
      <div className={ROW}>
        <Label title="Chi insegna" hint={opencode ? "il tuo motore OpenCode locale" : agy ? "il tuo account Google AI Pro: quota, non costo" : "il tuo abbonamento Claude Code"} />
        <Segmented label="Motore" value={s.provider} options={PROVIDERS} onChange={(v) => save({ provider: v })} />
      </div>
      {s.provider === "claude" && (
        <>
          <div className={ROW}>
            <Label title="Autenticazione" hint={s.claudeAuth === "apiKey" ? "chiave API personale da Anthropic Console" : "login interattivo tramite claude /login"} />
            <Segmented label="Autenticazione" value={s.claudeAuth ?? "subscription"} options={CLAUDE_AUTH_OPTIONS} onChange={(v) => save({ claudeAuth: v })} />
          </div>
          {s.claudeAuth === "apiKey" && (
            <div className="py-2.5">
              <div className="flex items-center justify-between gap-3">
                <Label
                  title="API Key Anthropic"
                  hint="da console.anthropic.com (sk-ant-...)"
                />
              </div>
              <div className="mt-2 flex items-center gap-2">
                <input
                  type="password"
                  defaultValue={s.anthropicApiKey ?? ""}
                  placeholder="sk-ant-..."
                  onBlur={(e) => {
                    const val = e.target.value.trim();
                    if (val !== (s.anthropicApiKey ?? "")) {
                      save({ anthropicApiKey: val });
                    }
                  }}
                  aria-label="API Key Anthropic"
                  className="min-w-0 flex-1 rounded-md border border-border bg-background px-2 py-1 font-mono text-xs"
                />
              </div>
              <p className="mt-1.5 text-[11px] text-muted-foreground">
                Usata per il docente, i subagenti e l'estrazione della mappa.
              </p>
            </div>
          )}
          <div className="py-2.5">
            <div className="flex items-center justify-between gap-3">
              <Label
                title="Claude Code CLI"
                hint={
                  !claudeStatus
                    ? "verifica versione…"
                    : claudeStatus.status === "meets-minimum"
                    ? `compatibile (richiesta ≥ v${claudeStatus.minVersion})`
                    : claudeStatus.status === "update-required"
                    ? `aggiornamento richiesto (minima: v${claudeStatus.minVersion})`
                    : claudeStatus.status === "not-found"
                    ? "eseguibile non trovato"
                    : "versione non verificabile"
                }
              />
              <div className="flex items-center gap-2">
                <span
                  className={`size-2 rounded-full ${
                    !claudeStatus
                      ? "bg-muted-foreground/40 animate-pulse"
                      : claudeStatus.status === "meets-minimum"
                      ? "bg-good"
                      : claudeStatus.status === "update-required"
                      ? "bg-warn"
                      : claudeStatus.status === "not-found"
                      ? "bg-bad"
                      : "bg-muted-foreground/60"
                  }`}
                />
                <span className="font-mono text-xs text-muted-foreground">
                  {claudeStatus?.version
                    ? `v${claudeStatus.version}`
                    : claudeStatus
                    ? claudeStatus.status === "not-found"
                      ? "Non trovato"
                      : "Non verificabile"
                    : "…"}
                </span>
              </div>
            </div>
            {claudeStatus?.status === "update-required" && (
              <p className="mt-1.5 text-[11px] text-warn">
                Versione installata non supportata. Aggiorna Claude Code con <code className="font-mono font-medium">claude update</code> o <code className="font-mono font-medium">npm install -g @anthropic-ai/claude-code</code>.
              </p>
            )}
            {claudeStatus?.status === "not-found" && (
              <p className="mt-1.5 text-[11px] text-bad">
                Eseguibile <code className="font-mono">claude</code> non trovato nel PATH o nelle cartelle standard. Installa Claude Code prima di iniziare le sessioni.
              </p>
            )}
          </div>
          <QuotaMeter />
        </>
      )}
      {agy && (["teacher", "subagent", "map"] as const).map((role, i) => (
        <label key={role} className={ROW}>
          <Label title={MODEL_KEYS[i][1]} hint={MODEL_KEYS[i][2]} />
          <select value={s.agyModels[role]} onChange={(e) => save({ agyModels: { ...s.agyModels, [role]: e.target.value } })} className={SELECT}>
            {[...new Set([s.agyModels[role], ...agyModels])].map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </label>
      ))}
      {opencode && (["teacher", "subagent", "map"] as const).map((role, i) => (
        <label key={role} className={ROW}>
          <Label title={MODEL_KEYS[i][1]} hint={MODEL_KEYS[i][2]} />
          <select value={s.opencodeModels[role]} onChange={(e) => save({ opencodeModels: { ...s.opencodeModels, [role]: e.target.value } })} className={SELECT}>
            {[...new Set([s.opencodeModels[role], ...opencodeModels])].map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </label>
      ))}
      {s.provider === "claude" && MODEL_KEYS.map(([key, label, what, role]) => (
        <div key={key} className="py-2.5">
          <label className="flex items-center justify-between gap-4">
            <Label title={label} hint={what} />
            <select aria-label={label} value={s[key]} onChange={(e) => save({ [key]: e.target.value as ModelAlias })} className={SELECT}>
              {MODELS.map((m) => <option key={m} value={m}>{m} · {HINT[m]}</option>)}
            </select>
          </label>
          <div className="mt-2 flex flex-wrap items-center justify-between gap-2">
            <span className="text-[11px] text-muted-foreground">Effort di ragionamento</span>
            <Segmented
              label={`Effort ${label}`}
              value={s.effort?.[role] ?? "default"}
              options={EFFORT_OPTIONS}
              onChange={(v) => save({ effort: { ...s.effort, [role]: v } })}
            />
          </div>
        </div>
      ))}
    </Section>
  );
}
