import { useEffect, useState } from "react";
import { api, type SettingsResponse } from "../../api.ts";
import type { AnkiStatusResponse, SyncConflict, SyncProviderId, SyncStatus, SyncTrashEntry } from "../../../../shared/protocol.ts";
import { GithubConnect, GoogleConnect } from "./Connect.tsx";
import { ROW, Section, Label, Segmented, type Save } from "./ui.tsx";

const BUTTON = "rounded-md border border-border px-2.5 py-1 hover:bg-accent disabled:opacity-50";
const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));
const PROVIDERS: [SyncProviderId, string][] = [["local", "Solo questo PC"], ["github", "GitHub"], ["gdrive", "Google Drive"]];

export function SyncSection({ s, save, sync, anki, setSync }: { s: SettingsResponse; save: Save; sync?: SyncStatus; anki?: AnkiStatusResponse; setSync(st: SyncStatus): void }) {
  // The server changes the provider too (disconnect, a finished connect): the live status wins over the settings snapshot.
  const provider = sync?.provider ?? s.syncProvider;
  const connected = provider === "github" ? !!sync?.github.connected : provider === "gdrive" ? !!sync?.gdrive.connected : false;
  const syncing = sync?.state === "syncing";
  const [running, setRunning] = useState(false);
  const [runErr, setRunErr] = useState<string>();
  const [conflicts, setConflicts] = useState<SyncConflict[]>([]);
  const [conflictErr, setConflictErr] = useState<string>();
  const [trash, setTrash] = useState<SyncTrashEntry[]>([]);
  const [trashErr, setTrashErr] = useState<string>();

  const conflictCount = sync?.conflictCount ?? 0;
  const trashCount = sync?.trashCount ?? 0;
  const loadConflicts = () => { api.conflicts().then(setConflicts).catch(() => {}); };
  const loadTrash = () => { api.trash().then(setTrash).catch(() => {}); };
  useEffect(loadConflicts, [conflictCount]);
  useEffect(loadTrash, [trashCount]);

  const runSync = (confirmDeletions?: boolean) => {
    setRunning(true);
    setRunErr(undefined);
    api.runSync(confirmDeletions)
      .then(setSync)
      .catch((e) => setRunErr(errorText(e)))
      .finally(() => setRunning(false));
  };

  const resolve = (id: string, choice: "local" | "remote" | "both") => {
    setConflictErr(undefined);
    api.resolveConflict(id, choice)
      .then(() => { loadConflicts(); api.sync().then(setSync).catch(() => {}); })
      .catch((e) => setConflictErr(errorText(e)));
  };

  const restore = (id: string) => {
    setTrashErr(undefined);
    api.restoreTrash(id)
      .then(() => { loadTrash(); api.sync().then(setSync).catch(() => {}); })
      .catch((e) => setTrashErr(errorText(e)));
  };

  const hint =
    provider === "local" ? (
      <p className="text-[11px] text-muted-foreground">I dati restano nella cartella indicata in Dati. Per usare Drive per desktop, punta lì quella cartella.</p>
    ) : provider === "github" ? (
      <GithubConnect sync={sync} onStatus={setSync} />
    ) : (
      <GoogleConnect sync={sync} onStatus={setSync} />
    );

  return (
    <Section title="Sincronizzazione">
      <div className="py-2.5">
        <div className="flex items-center justify-between gap-4">
          <Label title="Dove sincronizzare" />
          <Segmented
            label="Dove sincronizzare"
            value={provider}
            options={PROVIDERS}
            onChange={(v) => { void save({ syncProvider: v }).then(() => api.sync()).then(setSync).catch(() => {}); }}
          />
        </div>
        <div className="mt-2">{hint}</div>

        {provider !== "local" && connected && (
          <div className="mt-2.5 flex flex-col gap-1.5">
            <div>
              <button type="button" disabled={syncing || running} onClick={() => runSync()} className={BUTTON}>
                {syncing || running ? "…" : "Sincronizza ora"}
              </button>
            </div>
            {sync?.state === "error" || runErr ? (
              <p className="text-[11px] text-bad">{runErr ?? sync?.error}</p>
            ) : sync?.at ? (
              <p className="text-[11px] text-muted-foreground">
                Ultima sincronizzazione {new Date(sync.at).toLocaleTimeString("it-IT", { hour: "2-digit", minute: "2-digit" })}: {sync.pushed ?? 0} inviati, {sync.pulled ?? 0} ricevuti
                {sync.deleted ? `, ${sync.deleted} eliminati` : ""}
                {sync.conflicts ? `, ${sync.conflicts} in conflitto` : ""}
              </p>
            ) : null}
            {sync?.state === "confirm" && sync.pendingDeletions && (
              <div className="flex flex-wrap items-center gap-2">
                <p className="text-[11px] text-warn">Questa sincronizzazione eliminerebbe {sync.pendingDeletions.count} file su {sync.pendingDeletions.total}.</p>
                <button type="button" disabled={running} onClick={() => runSync(true)} className={BUTTON}>Elimina e sincronizza</button>
              </div>
            )}
            {sync && !sync.encrypted && (
              <p className="text-[11px] text-warn">I token sono salvati in chiaro in .learner/secrets.json (app avviata senza Electron).</p>
            )}
          </div>
        )}
      </div>

      {conflictCount > 0 && (
        <div className="py-2.5">
          <h4 className="mb-1.5 text-foreground">Conflitti</h4>
          <ul className="flex flex-col gap-2">
            {conflicts.map((c) => (
              <li key={c.id} className="flex flex-col gap-1">
                <span className="break-all font-mono text-[11px]">{c.rel}</span>
                <span className="text-[11px] text-muted-foreground">copia {c.side} del {new Date(c.at).toLocaleString("it-IT")}</span>
                <div className="flex flex-wrap gap-1.5">
                  <button type="button" onClick={() => resolve(c.id, "local")} className={BUTTON}>Tieni locale</button>
                  <button type="button" onClick={() => resolve(c.id, "remote")} className={BUTTON}>Tieni remoto</button>
                  <button type="button" onClick={() => resolve(c.id, "both")} className={BUTTON}>Tieni entrambe</button>
                </div>
              </li>
            ))}
          </ul>
          {conflictErr && <p className="mt-1.5 text-[11px] text-bad">{conflictErr}</p>}
        </div>
      )}

      {trashCount > 0 && (
        <div className="py-2.5">
          <h4 className="mb-1.5 text-foreground">Cestino (30 giorni)</h4>
          <ul className="flex flex-col gap-2">
            {trash.map((t) => (
              <li key={t.id} className="flex items-center justify-between gap-3">
                <span className="min-w-0">
                  <span className="block break-all font-mono text-[11px]">{t.rel}</span>
                  <span className="text-[11px] text-muted-foreground">eliminato il {new Date(t.deletedAt).toLocaleString("it-IT")}</span>
                </span>
                <button type="button" onClick={() => restore(t.id)} className={BUTTON}>Ripristina</button>
              </li>
            ))}
          </ul>
          {trashErr && <p className="mt-1.5 text-[11px] text-bad">{trashErr}</p>}
        </div>
      )}

      <div className={ROW}>
        <Label title="Anki" hint={anki && !anki.online ? "aprilo per sincronizzare le flashcard" : undefined} />
        <span className="flex items-center gap-2 text-muted-foreground">
          <span className={`size-1.5 rounded-full ${anki?.online ? "bg-good" : "bg-muted-foreground/50"}`} />
          {!anki ? "…" : anki.online ? `${anki.due ?? 0} da ripassare · ${anki.newCards ?? 0} nuove` : "chiuso"}
        </span>
      </div>
    </Section>
  );
}
