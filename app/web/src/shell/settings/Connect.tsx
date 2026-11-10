// Provider connection blocks: GitHub device flow and Google consent. Reused by the first-run screen (FirstRun.tsx) and the sync settings (SyncSection.tsx).
import { useState } from "react";
import { api } from "../../api.ts";
import { copyText } from "../../clipboard.ts";
import { APP_NAME, DATA_DIR_NAME } from "../../../../shared/identity.mjs";
import type { SyncStatus } from "../../../../shared/protocol.ts";

const BUTTON = "rounded-md border border-border px-2.5 py-1 hover:bg-accent disabled:opacity-50";
const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function GithubConnect({ sync, onStatus }: { sync?: SyncStatus; onStatus(s: SyncStatus): void }) {
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [revokeNote, setRevokeNote] = useState(false);
  const [err, setErr] = useState<string>();
  const gh = sync?.github;

  const run = (action: () => Promise<SyncStatus>) => {
    setBusy(true);
    setErr(undefined);
    action()
      .then(onStatus)
      .catch((e) => setErr(errorText(e)))
      .finally(() => setBusy(false));
  };

  if (gh?.connected) {
    return (
      <div className="flex flex-col gap-1.5">
        <p className="text-foreground">Connesso come {gh.login ?? "GitHub"}{gh.repo ? ` · ${gh.repo}` : ""}</p>
        <div>
          <button type="button" disabled={busy} onClick={() => { setRevokeNote(true); run(() => api.disconnect("github")); }} className={BUTTON}>
            Disconnetti
          </button>
        </div>
        {revokeNote && <p className="text-[11px] text-muted-foreground">Il token è rimosso da questo PC; per revocarlo: github.com/settings/applications</p>}
        {(err || gh.error) && <p className="text-[11px] text-bad">{err ?? gh.error}</p>}
      </div>
    );
  }

  if (gh?.pending) {
    const { userCode, verificationUri } = gh.pending;
    return (
      <div className="flex flex-col gap-1.5">
        <p className="text-foreground">
          Apri <a href={verificationUri} target="_blank" rel="noreferrer" className="underline">github.com/login/device</a> e inserisci il codice
        </p>
        <div className="flex items-center gap-2">
          <code className="rounded-md border border-border bg-background px-2 py-1 font-mono text-sm tracking-widest">{userCode}</code>
          <button
            type="button"
            onClick={() => void copyText(userCode).then((ok) => { if (!ok) return; setCopied(true); setTimeout(() => setCopied(false), 1500); })}
            className={BUTTON}
          >
            {copied ? "Copiato!" : "Copia"}
          </button>
          <button type="button" disabled={busy} onClick={() => run(() => api.githubCancel())} className={BUTTON}>
            Annulla
          </button>
        </div>
        {(err || gh.error) && <p className="text-[11px] text-bad">{err ?? gh.error}</p>}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-1.5">
      <div>
        <button type="button" disabled={busy} onClick={() => run(() => api.githubStart())} className={BUTTON}>
          {busy ? "…" : "Accedi con GitHub"}
        </button>
      </div>
      <p className="text-[11px] text-muted-foreground">{`Permesso richiesto: repo (crea e aggiorna il repository privato ${DATA_DIR_NAME})`}</p>
      {revokeNote && <p className="text-[11px] text-muted-foreground">Il token è rimosso da questo PC; per revocarlo: github.com/settings/applications</p>}
      {(err || gh?.error) && <p className="text-[11px] text-bad">{err ?? gh?.error}</p>}
    </div>
  );
}

export function GoogleConnect({ sync, onStatus }: { sync?: SyncStatus; onStatus(s: SyncStatus): void }) {
  const [busy, setBusy] = useState(false);
  const [fallbackUrl, setFallbackUrl] = useState<string>();
  const [err, setErr] = useState<string>();
  const gd = sync?.gdrive;

  if (gd?.connected) {
    return (
      <div className="flex flex-col gap-1.5">
        <p className="text-foreground">Connesso a Google Drive{gd.email ? ` (${gd.email})` : ""}</p>
        <div>
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              setErr(undefined);
              api.disconnect("gdrive").then(onStatus).catch((e) => setErr(errorText(e))).finally(() => setBusy(false));
            }}
            className={BUTTON}
          >
            Disconnetti
          </button>
        </div>
        {(err || gd.error) && <p className="text-[11px] text-bad">{err ?? gd.error}</p>}
      </div>
    );
  }

  const waiting = fallbackUrl !== undefined || gd?.pending;
  return (
    <div className="flex flex-col gap-1.5">
      <div>
        <button
          type="button"
          disabled={busy}
          onClick={() => {
            setBusy(true);
            setErr(undefined);
            api.googleStart()
              .then(({ url }) => {
                window.open(url, "_blank");
                setFallbackUrl(url);
              })
              .catch((e) => setErr(errorText(e)))
              .finally(() => setBusy(false));
          }}
          className={BUTTON}
        >
          {busy ? "…" : "Accedi con Google"}
        </button>
      </div>
      <p className="text-[11px] text-muted-foreground">{`Permesso richiesto: solo i file creati da ${APP_NAME}`}</p>
      {waiting && (
        <p className="text-[11px] text-muted-foreground">
          Completa l&apos;accesso nel browser…{fallbackUrl && (
            <>{" "}<a href={fallbackUrl} target="_blank" rel="noreferrer" className="underline">Apri di nuovo la pagina di accesso</a></>
          )}
        </p>
      )}
      {(err || gd?.error) && <p className="text-[11px] text-bad">{err ?? gd?.error}</p>}
    </div>
  );
}
