import type { SettingsResponse } from "../../api.ts";
import { DICTATION_LANGUAGES, type LearnerSettings } from "../../../../shared/protocol.ts";
import { ROW, SELECT, Section, Label, Segmented, type Save } from "./ui.tsx";

const STT_PROVIDERS: [LearnerSettings["sttProvider"], string][] = [["groq", "Groq (Whisper)"], ["deepgram", "Deepgram"]];

export function VoiceSection({ s, save }: { s: SettingsResponse; save: Save }) {
  return (
    <Section title="Dettatura vocale">
      <div className={ROW}>
        <Label title="Servizio STT" hint="Groq ha una quota gratuita di 2000 ric/giorno (nessuna carta)" />
        <Segmented label="Servizio STT" value={s.sttProvider ?? "groq"} options={STT_PROVIDERS} onChange={(v) => save({ sttProvider: v })} />
      </div>
      <div className={ROW}>
        <Label title="Lingua dettatura" hint="lingua attesa per la trascrizione vocale" />
        <select
          aria-label="Lingua dettatura"
          value={s.dictationLanguage ?? "it"}
          onChange={(e) => save({ dictationLanguage: e.target.value })}
          className={SELECT}
        >
          {DICTATION_LANGUAGES.map((l) => (
            <option key={l.code} value={l.code}>
              {l.label} ({l.code})
            </option>
          ))}
        </select>
      </div>
      <div className="py-2.5">
        <div className="flex items-center justify-between gap-3">
          <Label
            title={s.sttProvider === "deepgram" ? "API Key Deepgram" : "API Key Groq"}
            hint={
              s.sttProvider === "deepgram"
                ? "da console.deepgram.com (credito iniziale $200)"
                : "gratuita da console.groq.com (whisper-large-v3-turbo)"
            }
          />
        </div>
        <div className="mt-2 flex items-center gap-2">
          <input
            type="password"
            defaultValue={s.sttApiKey ?? ""}
            placeholder={s.sttProvider === "deepgram" ? "Token Deepgram..." : "gsk_..."}
            onBlur={(e) => {
              if (e.target.value.trim() !== (s.sttApiKey ?? "")) {
                save({ sttApiKey: e.target.value.trim() });
              }
            }}
            aria-label="API Key dettatura vocale"
            className="min-w-0 flex-1 rounded-md border border-border bg-background px-2 py-1 font-mono text-xs"
          />
        </div>
        <p className="mt-1.5 text-[11px] text-muted-foreground">
          Premi <kbd className="rounded border border-border px-1">Ctrl+D</kbd> nel composer o nelle domande per dettare a voce. Su Windows è disponibile anche <kbd className="rounded border border-border px-1">Win+H</kbd> di sistema.
        </p>
      </div>
    </Section>
  );
}
