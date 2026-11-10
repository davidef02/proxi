// Session title, status (token breakdown in its tooltip) and actions, portalled into the app top bar (A5 TopBarSlot).

import { Dumbbell, Workflow } from "lucide-react";
import { api } from "../api.ts";
import type { SessionState } from "../../../shared/protocol.ts";
import { STATUS_LABEL } from "../status.ts";
import { TopBarSlot } from "../shell/TopBarSlot.tsx";
import { Spinner } from "./Spinner.tsx";
import { TAB_LABEL, type Tab } from "./SidePane.tsx";

const ICON = { plan: Workflow, exercises: Dumbbell } as const;
const DOT: Record<SessionState["status"], string> = { starting: "bg-muted-foreground", running: "bg-info", waiting: "bg-warn", idle: "bg-good", error: "bg-bad", closed: "bg-border" };

const fmt = (n: number) => n.toLocaleString("it-IT");

export function SessionHeader({ session: s, connected, panel, onPanel }: { session: SessionState; connected: boolean; panel: Tab | null; onPanel: (t: Tab | null) => void }) {
  const working = s.status === "running" || s.status === "starting";
  const tabs: Tab[] = ["plan", "exercises"];
  const u = s.usage;
  const tokens = u.turns
    ? `Token di questa sessione · ${u.turns} turni · nuovi ${fmt(u.inputTokens + u.cacheCreationTokens)} · in uscita ${fmt(u.outputTokens)} · dalla cache ${fmt(u.cacheReadTokens)}`
    : undefined;
  return (
    <TopBarSlot>
      <h1 className="min-w-0 flex-1 truncate text-xs font-medium text-foreground" title={s.title}>
        {s.title}
      </h1>
      <span role="status" aria-live="polite" title={tokens} className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
        {working ? <Spinner label={STATUS_LABEL[s.status].short} /> : <span className={`h-1.5 w-1.5 rounded-full ${DOT[s.status]}`} aria-hidden />}
        {STATUS_LABEL[s.status].short}
      </span>
      {!connected && <span className="shrink-0 text-xs text-warn">connessione persa…</span>}
      <nav aria-label="Pannello laterale" className="flex shrink-0 items-center gap-1 text-xs">
        {tabs.map((t) => {
          const Icon = ICON[t];
          const on = panel === t;
          return (
            <button
              key={t}
              type="button"
              aria-pressed={on}
              onClick={() => onPanel(on ? null : t)}
              className={`flex items-center gap-1.5 rounded-md px-2 py-1 hover:bg-accent hover:text-foreground ${on ? "bg-accent text-foreground" : "text-muted-foreground"}`}
            >
              <Icon className="h-3.5 w-3.5" aria-hidden />
              {TAB_LABEL[t]}
              {t === "exercises" && s.exercises.length > 0 && <span className="opacity-60">{s.exercises.length}</span>}
            </button>
          );
        })}
      </nav>
      {(working || s.status === "waiting") && (
        <button type="button" className="shrink-0 rounded-md px-2 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => void api.interrupt(s.id)}>
          Interrompi
        </button>
      )}
    </TopBarSlot>
  );
}
