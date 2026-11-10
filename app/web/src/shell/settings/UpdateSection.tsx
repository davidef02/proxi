import { APP_NAME } from "../../../../shared/identity.mjs";
import type { UpdateStatusResponse } from "../../../../shared/protocol.ts";
import { Section, Label } from "./ui.tsx";

export function UpdateSection({
  update,
  checkingUpdate,
  applyingUpdate,
  confirmForce,
  updateError,
  onCheck,
  onApply,
  onCancelForce,
}: {
  update?: UpdateStatusResponse;
  checkingUpdate: boolean;
  applyingUpdate: boolean;
  confirmForce: boolean;
  updateError?: string;
  onCheck(): void;
  onApply(force: boolean): void;
  onCancelForce(): void;
}) {
  return (
    <Section title="Aggiornamenti">
      <div className="py-2.5">
        <div className="flex items-center justify-between gap-3">
          <Label
            title={
              update?.state === "relaunching"
                ? "Riavvio in corso…"
                : update?.state === "updating" || applyingUpdate
                ? "Aggiornamento in corso…"
                : checkingUpdate || update?.state === "checking"
                ? "Verifica in corso…"
                : update && update.behind > 0
                ? `Aggiornamento disponibile (${update.behind} commit)`
                : `${APP_NAME} è aggiornato`
            }
            hint={
              update?.checkedAt
                ? `Ultima verifica: ${new Date(update.checkedAt).toLocaleTimeString("it-IT", { hour: "2-digit", minute: "2-digit" })}`
                : undefined
            }
          />
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={onCheck}
              disabled={checkingUpdate || applyingUpdate || update?.state === "updating" || update?.state === "relaunching"}
              className="rounded-md border border-border px-2.5 py-1 text-xs text-foreground hover:bg-accent disabled:opacity-50"
            >
              {checkingUpdate ? "Verifico…" : "Verifica disponibilità"}
            </button>
            {update && update.behind > 0 && (
              <button
                type="button"
                onClick={() => onApply(false)}
                disabled={checkingUpdate || applyingUpdate || update.state === "updating" || update.state === "relaunching"}
                className="rounded-md bg-info px-2.5 py-1 text-xs font-medium text-background hover:bg-info/90 disabled:opacity-50"
              >
                {applyingUpdate || update.state === "updating" ? "Aggiorno…" : "Aggiorna ora"}
              </button>
            )}
          </div>
        </div>
        {updateError && (
          <p role="alert" className="mt-2 text-[11px] text-bad">{updateError}</p>
        )}
        {confirmForce && (
          <div className="mt-2 rounded border border-warn/40 bg-warn/10 p-2.5 text-xs">
            <p className="font-medium text-warn">
              C'è una sessione in corso. Chiudila o conferma per interromperla prima di aggiornare.
            </p>
            <div className="mt-2 flex items-center gap-2">
              <button
                type="button"
                onClick={() => onApply(true)}
                disabled={applyingUpdate}
                className="rounded bg-warn/20 px-2.5 py-1 text-xs font-medium text-warn hover:bg-warn/30"
              >
                Interrompi e aggiorna
              </button>
              <button
                type="button"
                onClick={onCancelForce}
                className="rounded border border-border px-2.5 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                Annulla
              </button>
            </div>
          </div>
        )}
        {update && update.commits && update.commits.length > 0 && (
          <div className="mt-2.5 max-h-40 space-y-1.5 overflow-y-auto rounded-md border border-border/60 bg-background/50 p-2 text-xs">
            {update.commits.map((c) => (
              <div key={c.hash} className="flex items-baseline gap-2">
                <code className="shrink-0 font-mono text-[11px] text-info">{c.hash}</code>
                <span className="min-w-0 flex-1 truncate text-foreground/90">{c.subject}</span>
                <span className="shrink-0 text-[10px] text-muted-foreground">{c.relativeTime}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    </Section>
  );
}
