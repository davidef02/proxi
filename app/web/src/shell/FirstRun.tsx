// First-run screen: shown in place of the sidebar and the main pane while the data folder has no courses (sync.needsData).
import { useState } from "react";
import { api } from "../api.ts";
import { GithubConnect, GoogleConnect } from "./settings/Connect.tsx";
import { useData } from "./data.tsx";
import { APP_NAME } from "../../../shared/identity.mjs";

export function FirstRun() {
  const { sync, setSync } = useData();
  const [showFolder, setShowFolder] = useState(false);
  const [folder, setFolder] = useState("");
  const [busy, setBusy] = useState(false);
  const [restartRequired, setRestartRequired] = useState(false);
  const [err, setErr] = useState<string>();

  const useFolder = () => {
    setBusy(true);
    setErr(undefined);
    api.setDataRoot(folder.trim())
      .then((s) => { setSync(s); setRestartRequired(!!s.restartRequired); })
      .catch((e) => setErr(String(e.message ?? e)))
      .finally(() => setBusy(false));
  };

  return (
    <div className="flex min-h-0 flex-1 items-center justify-center overflow-y-auto p-6">
      <div className="flex w-full max-w-md flex-col gap-5 rounded-xl border border-border bg-card p-6 text-card-foreground shadow-[var(--shadow)]">
        <div className="flex flex-col gap-1.5">
          <h1 className="text-base font-medium text-heading">Connetti per scaricare i tuoi corsi</h1>
          <p className="text-xs text-muted-foreground">Corsi, mappa e mazzi stanno nel tuo spazio online: accedi per scaricarli su questo PC.</p>
        </div>

        <div className="flex flex-col gap-4 text-xs">
          <GoogleConnect sync={sync} onStatus={setSync} />
          <GithubConnect sync={sync} onStatus={setSync} />
        </div>

        <div className="border-t border-hairline pt-4 text-xs">
          {!showFolder ? (
            <button
              type="button"
              onClick={() => { setFolder(sync?.dataRoot ?? ""); setShowFolder(true); }}
              className="rounded-md border border-border px-2.5 py-1 hover:bg-accent"
            >
              Usa una cartella locale
            </button>
          ) : (
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2">
                <input
                  value={folder}
                  onChange={(e) => setFolder(e.target.value)}
                  aria-label="Cartella dei dati"
                  className="min-w-0 flex-1 rounded-md border border-border bg-background px-2 py-1 font-mono text-[11px]"
                />
                <button
                  type="button"
                  disabled={busy || folder.trim() === ""}
                  onClick={useFolder}
                  className="rounded-md border border-border px-2.5 py-1 hover:bg-accent disabled:opacity-50"
                >
                  Usa questa cartella
                </button>
              </div>
              {restartRequired && <p className="text-muted-foreground">{`Cartella impostata: riavvia ${APP_NAME} per usarla.`}</p>}
              {err && <p className="text-bad">{err}</p>}
            </div>
          )}
        </div>

        {sync?.state === "syncing" && <p className="text-xs text-muted-foreground">Scarico i tuoi corsi…</p>}
        {sync?.error && <p className="text-xs text-bad">{sync.error}</p>}
      </div>
    </div>
  );
}
