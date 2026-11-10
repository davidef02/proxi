import { useState } from "react";
import { api } from "../../api.ts";
import { copyText } from "../../clipboard.ts";
import type { SyncStatus } from "../../../../shared/protocol.ts";
import { Section, Label } from "./ui.tsx";

export function DataSection({ sync, setSync }: { sync?: SyncStatus; setSync(s: SyncStatus): void }) {
  const [copied, setCopied] = useState(false);
  const [folder, setFolder] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string>();
  const dataRoot = sync?.dataRoot ?? "";

  const change = () => {
    setBusy(true);
    setErr(undefined);
    api.setDataRoot(folder.trim())
      .then((s) => { setSync(s); setFolder(""); })
      .catch((e) => setErr(String(e.message ?? e)))
      .finally(() => setBusy(false));
  };

  return (
    <Section title="Dati">
      <div className="py-2.5">
        <div className="flex items-center justify-between gap-3">
          <Label title="Cartella dei dati" hint="corsi, lezioni, mappa e mazzi" />
          <button
            type="button"
            onClick={() =>
              void copyText(dataRoot).then((ok) => {
                if (!ok) return;
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              })
            }
            className="rounded border border-border px-2 py-0.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            {copied ? "Copiato!" : "Copia percorso"}
          </button>
        </div>
        <p className="mt-2 break-all font-mono text-[11px] text-foreground/80 selection:bg-accent">
          {dataRoot}
        </p>
        {sync?.dataRootFromEnv && <p className="mt-1.5 text-[11px] text-muted-foreground">Impostata da LEARNER_DATA_ROOT.</p>}
        {sync && !sync.dataRootFromEnv && (
          <div className="mt-2.5 flex flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <input
                value={folder}
                onChange={(e) => setFolder(e.target.value)}
                placeholder={dataRoot}
                aria-label="Nuova cartella dei dati"
                className="min-w-0 flex-1 rounded-md border border-border bg-background px-2 py-1 font-mono text-[11px]"
              />
              <button
                type="button"
                disabled={busy || folder.trim() === ""}
                onClick={change}
                className="rounded-md border border-border px-2.5 py-1 hover:bg-accent disabled:opacity-50"
              >
                Cambia cartella
              </button>
            </div>
            <p className="text-[11px] text-muted-foreground">
              I file non vengono spostati: copiali a mano nella nuova cartella.
            </p>
            {sync.restartRequired && <p className="text-[11px] text-muted-foreground">Riavvio in corso…, se l&apos;app non si riavvia da sola, chiudila e riaprila.</p>}
            {err && <p className="text-[11px] text-bad">{err}</p>}
          </div>
        )}
      </div>
    </Section>
  );
}
