// App chrome: 36 px top bar (Electron drag region, 140 px kept free for the window controls), collapsible sidebar, main pane.
// The page-specific part of the top bar is filled with <TopBarSlot> (see TopBarSlot.tsx).
import { PanelLeftClose, PanelLeftOpen, X } from "lucide-react";
import { useCallback, useMemo, useState, type ReactNode } from "react";
import { useRoute } from "../router.ts";
import { DataProvider, useData } from "./data.tsx";
import { FirstRun } from "./FirstRun.tsx";
import { SettingsDialog } from "./SettingsDialog.tsx";
import { Sidebar } from "./Sidebar.tsx";
import { TopBarSlotContext } from "./TopBarSlot.tsx";
import { APP_NAME } from "../../../shared/identity.mjs";

const KEY = "ia-learner:sidebar";

function ShellContent({ children }: { children: ReactNode }) {
  const route = useRoute();
  const { updateStatus, sync, settingsSection, openSettings, closeSettings } = useData();
  const [open, setOpen] = useState(() => localStorage.getItem(KEY) !== "0");
  const [dismissedBehind, setDismissedBehind] = useState<number | null>(null);
  const [el, setEl] = useState<HTMLElement | null>(null);
  const [count, setCount] = useState(0);
  const bump = useCallback((d: 1 | -1) => setCount((n) => n + d), []);
  const slot = useMemo(() => ({ el, count, bump }), [el, count, bump]);
  const toggle = () =>
    setOpen((o) => {
      localStorage.setItem(KEY, o ? "0" : "1");
      return !o;
    });

  const showBanner = updateStatus && updateStatus.behind > 0 && dismissedBehind !== updateStatus.behind;

  return (
    <TopBarSlotContext.Provider value={slot}>
      <div className="flex h-dvh flex-col overflow-hidden">
        <header className="topbar flex h-9 shrink-0 items-center gap-2 border-b border-hairline pl-2 pr-[140px]">
          <button
            type="button"
            onClick={toggle}
            aria-label={open ? "Chiudi la barra laterale" : "Apri la barra laterale"}
            aria-expanded={open}
            className="rounded p-1.5 text-muted-foreground hover:text-foreground"
          >
            {open ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
          </button>
          <a href="#/" className="text-xs font-medium">{APP_NAME}</a>
          <div ref={setEl} className="flex min-w-0 flex-1 items-center gap-3" />
        </header>
        {showBanner && (
          <div className="flex items-center justify-between border-b border-info/30 bg-info/10 px-3 py-1.5 text-xs text-info">
            <span className="truncate">
              Aggiornamento disponibile ({updateStatus.behind} commit)
            </span>
            <div className="ml-2 flex shrink-0 items-center gap-2">
              <button
                type="button"
                onClick={() => openSettings("aggiornamenti")}
                className="rounded border border-info/40 bg-info/20 px-2 py-0.5 text-[11px] font-medium text-info hover:bg-info/30"
              >
                Vedi
              </button>
              <button
                type="button"
                onClick={() => setDismissedBehind(updateStatus.behind)}
                aria-label="Chiudi notifica aggiornamento"
                className="rounded p-0.5 text-info/70 hover:text-info"
              >
                <X size={13} />
              </button>
            </div>
          </div>
        )}
        {sync?.needsData ? (
          <FirstRun />
        ) : (
          <div className="flex min-h-0 flex-1">
            {open && <Sidebar onSettings={() => openSettings()} />}
            <main className={`flex min-h-0 min-w-0 flex-1 flex-col ${route.name === "session" || route.name === "exercise" ? "overflow-hidden" : "overflow-y-auto"}`}>
              {children}
            </main>
          </div>
        )}
        <SettingsDialog section={settingsSection} onClose={closeSettings} />
      </div>
    </TopBarSlotContext.Provider>
  );
}

export function Shell({ children }: { children: ReactNode }) {
  return (
    <DataProvider>
      <ShellContent>{children}</ShellContent>
    </DataProvider>
  );
}
