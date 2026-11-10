// Settings: native modal with a vertical tab list (left) and the selected section (right). Owns fetching, optimistic saves (saveSeq) and the active section; sections live in ./settings/.
import { X } from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { api, type SettingsResponse } from "../api.ts";
import type { AnkiStatusResponse, ClaudeStatusResponse, LearnerSettings, UpdateStatusResponse } from "../../../shared/protocol.ts";
import { useData } from "./data.tsx";
import { DataSection } from "./settings/DataSection.tsx";
import { EngineSection } from "./settings/EngineSection.tsx";
import { GeneralSection } from "./settings/GeneralSection.tsx";
import { MapSection } from "./settings/MapSection.tsx";
import { SECTIONS, type SettingsSection } from "./settings/sections.ts";
import { SyncSection } from "./settings/SyncSection.tsx";
import { UpdateSection } from "./settings/UpdateSection.tsx";
import { VoiceSection } from "./settings/VoiceSection.tsx";

export function SettingsDialog({ section, onClose }: { section: SettingsSection | null; onClose(): void }) {
  const open = section !== null;
  const ref = useRef<HTMLDialogElement>(null);
  const { updateStatus, sync, setSync } = useData();
  const [s, setS] = useState<SettingsResponse>();
  const saveSeq = useRef(0);
  const downOnBackdrop = useRef(false);
  const [anki, setAnki] = useState<AnkiStatusResponse>();
  const [error, setError] = useState<string>();
  const [claudeStatus, setClaudeStatus] = useState<ClaudeStatusResponse>();
  const [agyModels, setAgyModels] = useState<string[]>([]);
  const [opencodeModels, setOpencodeModels] = useState<string[]>([]);

  const [update, setUpdate] = useState<UpdateStatusResponse | undefined>(updateStatus);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [applyingUpdate, setApplyingUpdate] = useState(false);
  const [confirmForce, setConfirmForce] = useState(false);
  const [updateError, setUpdateError] = useState<string>();

  // Active section, reset during render on each open so the DOM already marks the right tab selected when showModal() runs.
  const [active, setActive] = useState<SettingsSection>("generali");
  const [prevSection, setPrevSection] = useState(section);
  if (section !== prevSection) {
    setPrevSection(section);
    if (section) setActive(section);
  }
  const tabs = useRef<Partial<Record<SettingsSection, HTMLButtonElement | null>>>({});

  useEffect(() => {
    if (updateStatus) setUpdate(updateStatus);
  }, [updateStatus]);

  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) {
      d.showModal();
      d.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.focus();
    }
    if (!open && d.open) d.close();
    if (open) {
      setError(undefined);
      setUpdateError(undefined);
      setConfirmForce(false);
      api.settings().then(setS).catch((e) => setError(String(e.message ?? e)));
      api.claudeStatus().then(setClaudeStatus).catch(() => setClaudeStatus({ status: "version-unchecked", minVersion: "2.1.259" }));
      api.anki().then(setAnki).catch(() => setAnki({ online: false }));
      api.sync().then(setSync).catch(() => {});
      api.update().then(setUpdate).catch(() => {});
    }
  }, [open]);

  const agy = s?.provider === "agy";
  const opencode = s?.provider === "opencode";
  useEffect(() => {
    if (open && agy) api.agyModels().then(setAgyModels).catch(() => {});
    if (open && opencode) api.opencodeModels().then(setOpencodeModels).catch(() => {});
  }, [open, agy, opencode]);

  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    setUpdateError(undefined);
    try {
      const res = await api.checkUpdate();
      setUpdate(res);
    } catch (e: any) {
      setUpdateError(e?.message ?? "Impossibile verificare gli aggiornamenti");
    } finally {
      setCheckingUpdate(false);
    }
  };

  const handleApplyUpdate = async (force = false) => {
    if (update && update.activeSessions > 0 && !force) {
      setConfirmForce(true);
      return;
    }
    setConfirmForce(false);
    setApplyingUpdate(true);
    setUpdateError(undefined);
    try {
      await api.applyUpdate(force);
      setUpdate((u) => u && { ...u, state: "relaunching", behind: 0 });
    } catch (e: any) {
      if (e?.code === "sessions_running") {
        setConfirmForce(true);
      } else if (e?.code === "dirty_tree") {
        setUpdateError("Modifiche locali non salvate nel repository. Impossibile aggiornare senza rischiare di sovrascrivere il tuo lavoro.");
      } else if (e?.code === "diverged") {
        setUpdateError("I commit locali sono divergenti da quelli remoti. Impossibile eseguire un aggiornamento fast-forward.");
      } else {
        setUpdateError(e?.message ?? "Aggiornamento fallito");
      }
    } finally {
      setApplyingUpdate(false);
    }
  };

  // Optimistic local state; only the latest save may overwrite it (older replies arrive out of order and are dropped).
  const save = (patch: Partial<LearnerSettings>) => {
    const n = ++saveSeq.current;
    setS((cur) => cur && { ...cur, ...patch, ...(patch.effort ? { effort: { ...cur.effort, ...patch.effort } } : {}) });
    return api.saveSettings(patch)
      .then((r) => { if (n === saveSeq.current) setS((cur) => cur && { ...cur, ...r }); })
      .catch((e) => { if (n === saveSeq.current) { setError(String(e.message ?? e)); api.settings().then(setS).catch(() => {}); } });
  };

  const onTabKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const i = SECTIONS.findIndex((x) => x.id === active);
    const n = SECTIONS.length;
    const j = e.key === "ArrowDown" ? (i + 1) % n : e.key === "ArrowUp" ? (i - 1 + n) % n : e.key === "Home" ? 0 : e.key === "End" ? n - 1 : -1;
    if (j < 0) return;
    e.preventDefault();
    const next = SECTIONS[j].id;
    setActive(next);
    tabs.current[next]?.focus();
  };

  const loading = !s && !error ? <p className="px-1 text-muted-foreground">Carico…</p> : null;
  const content: Record<SettingsSection, ReactNode> = {
    generali: <GeneralSection />,
    motore: s ? <EngineSection s={s} save={save} claudeStatus={claudeStatus} agyModels={agyModels} opencodeModels={opencodeModels} /> : loading,
    dettatura: s ? <VoiceSection s={s} save={save} /> : loading,
    mappa: s ? <MapSection s={s} save={save} /> : loading,
    sync: s ? <SyncSection s={s} save={save} sync={sync} anki={anki} setSync={setSync} /> : loading,
    aggiornamenti: (
      <UpdateSection
        update={update}
        checkingUpdate={checkingUpdate}
        applyingUpdate={applyingUpdate}
        confirmForce={confirmForce}
        updateError={updateError}
        onCheck={() => void handleCheckUpdate()}
        onApply={(force) => void handleApplyUpdate(force)}
        onCancelForce={() => setConfirmForce(false)}
      />
    ),
    dati: s ? <DataSection sync={sync} setSync={setSync} /> : loading,
  };

  return (
    <dialog
      ref={ref}
      onClose={onClose}
      onMouseDown={(e) => { downOnBackdrop.current = e.target === ref.current; }}
      onClick={(e) => { if (e.target === ref.current && downOnBackdrop.current) onClose(); downOnBackdrop.current = false; }}
      aria-labelledby="settings-title"
      className="m-auto h-[min(34rem,calc(100vh-3rem))] max-h-none w-[min(50rem,calc(100vw-2rem))] max-w-none overflow-hidden rounded-xl border border-border bg-card p-0 text-card-foreground shadow-[var(--shadow)]"
    >
      <div className="flex h-full flex-col">
        <div className="flex shrink-0 items-center border-b border-hairline px-5 py-3.5">
          <h2 id="settings-title" className="text-sm font-medium text-heading">Impostazioni</h2>
          <button type="button" aria-label="Chiudi" onClick={onClose} className="ml-auto rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground">
            <X size={14} />
          </button>
        </div>
        <div className="flex min-h-0 flex-1">
          <div
            role="tablist"
            aria-orientation="vertical"
            aria-label="Sezioni delle impostazioni"
            onKeyDown={onTabKey}
            className="flex w-44 shrink-0 flex-col gap-0.5 overflow-y-auto border-r border-hairline p-2 text-xs"
          >
            {SECTIONS.map(({ id, label, icon: Icon }) => {
              const selected = id === active;
              return (
                <button
                  key={id}
                  ref={(el) => { tabs.current[id] = el; }}
                  type="button"
                  role="tab"
                  id={`settings-tab-${id}`}
                  aria-selected={selected}
                  aria-controls={`settings-panel-${id}`}
                  tabIndex={selected ? 0 : -1}
                  onClick={() => setActive(id)}
                  className={`flex items-center gap-2 rounded-md px-2.5 py-1.5 text-left transition-colors ${selected ? "bg-accent text-heading" : "text-muted-foreground hover:bg-accent/60 hover:text-foreground"}`}
                >
                  <Icon size={13} className="shrink-0" />
                  {label}
                </button>
              );
            })}
          </div>
          <div className="flex min-h-0 min-w-0 flex-1 flex-col text-xs">
            {error && <p role="alert" className="mx-5 mt-4 shrink-0 rounded-md border border-bad/40 bg-bad/10 px-3 py-2 text-bad">{error}</p>}
            {SECTIONS.map(({ id }) => (
              <div
                key={id}
                role="tabpanel"
                id={`settings-panel-${id}`}
                aria-labelledby={`settings-tab-${id}`}
                hidden={id !== active}
                className="min-h-0 flex-1 overflow-y-auto px-5 py-4"
              >
                {content[id]}
              </div>
            ))}
          </div>
        </div>
      </div>
    </dialog>
  );
}
