import { useState } from "react";
import { RotateCcw } from "lucide-react";
import { api } from "../api.ts";
import { Chat } from "./Chat.tsx";
import { Dock, openProblem, pendingItem } from "./Dock.tsx";
import { SessionHeader } from "./SessionHeader.tsx";
import { SidePane, type Tab } from "./SidePane.tsx";
import { useSession } from "./useSession.ts";
import "./session.css";

export function SessionPage({ id }: { id: string }) {
  const { view, loadError } = useSession(id);
  const [panel, setPanel] = useState<Tab | null>(null);
  const [panelFor, setPanelFor] = useState(id);
  if (panelFor !== id) {
    setPanelFor(id);
    setPanel(null);
  }
  // Until the reset effect has run, the view may still hold the previous session: never render it under this id.
  const s = view.session?.id === id ? view.session : null;

  if (!s)
    return (
      <p className="p-6 text-xs text-muted-foreground" role={loadError ? "alert" : "status"}>
        {loadError ? `Impossibile aprire la sessione: ${loadError}` : "Mi collego alla sessione…"}
      </p>
    );

  const pending = pendingItem(s.items, s.pendingRequestId);
  return (
    <div className="flex min-h-0 w-full min-w-0 flex-1 flex-col overflow-hidden bg-background">
      {/* The header portals itself into the app top bar (TopBarSlot). */}
      <SessionHeader session={s} connected={view.connected} panel={panel} onPanel={setPanel} />

      {s.status === "error" && s.lastError && (
        <div role="alert" className="flex items-center justify-between gap-3 border-b border-bad/40 bg-bad/5 px-4 py-1.5 text-xs text-bad">
          <span className="truncate">{s.lastError}</span>
          {s.retryable && (
            <button
              type="button"
              data-testid="retry-button"
              onClick={() => void api.retry(s.id)}
              className="inline-flex shrink-0 items-center gap-1.5 rounded border border-bad/40 bg-background px-2.5 py-0.5 text-xs font-medium text-foreground hover:bg-accent transition-colors"
            >
              <RotateCcw className="h-3 w-3" />
              Riprova
            </button>
          )}
        </div>
      )}

      <div className={`grid min-h-0 flex-1 grid-rows-[minmax(0,1fr)] ${panel ? "grid-cols-[minmax(0,1fr)_minmax(300px,min(46%,640px))]" : "grid-cols-[minmax(0,1fr)]"}`}>
        <section aria-label="Chat" className="flex min-h-0 min-w-0 flex-col">
          <Chat
            live={s.status === "running" || s.status === "starting"}
            items={s.items}
            sessionId={id}
            pendingId={pending?.requestId}
          />
          <div className="shrink-0 px-4 pb-4 pt-1">
            <Dock sessionId={id} status={s.status} pending={pending} problem={openProblem(s.items)} retryable={s.retryable} />
          </div>
        </section>
        {panel && (
          <aside aria-label="Pannello laterale" className="min-h-0 min-w-0">
            <SidePane session={s} tab={panel} onClose={() => setPanel(null)} />
          </aside>
        )}
      </div>
    </div>
  );
}
