// The Claude subscription's 5-hour and 7-day quota, shown in Impostazioni → Motore under the Claude block. claude
// reports it with every turn (rate_limit_event); between turns it shows the last report and when it was made.
import type { QuotaWindow } from "../../../shared/protocol.ts";
import { useData } from "./data.tsx";

const hhmm = (d: Date) => d.toLocaleTimeString("it-IT", { hour: "2-digit", minute: "2-digit" });

/** "si azzera oggi alle 18:00" / "si azzera gio alle 09:00"; null when claude gave no reset time. */
function resets(epochS?: number) {
  if (!epochS) return null;
  const d = new Date(epochS * 1000);
  const day = d.toDateString() === new Date().toDateString() ? "oggi" : d.toLocaleDateString("it-IT", { weekday: "short" });
  return `si azzera ${day} alle ${hhmm(d)}`;
}

function Window({ label, w }: { label: string; w?: QuotaWindow }) {
  if (!w) return null;
  const pct = Math.round(w.utilization * 100);
  const fill = pct >= 90 ? "bg-bad" : pct >= 75 ? "bg-warn" : "bg-muted-foreground/60";
  const text = pct >= 90 ? "text-bad" : pct >= 75 ? "text-warn" : "text-heading";
  const foot = resets(w.resetsAt);
  return (
    <div className="min-w-0">
      <div className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</div>
      <div className={`mb-1.5 mt-0.5 text-[22px] leading-tight tabular-nums ${text}`}>{pct}%</div>
      <div className="relative h-1.5 w-full overflow-hidden rounded-full bg-border">
        <span data-quota-fill className={`absolute inset-y-0 left-0 ${fill}`} style={{ width: `${Math.min(100, pct)}%` }} />
      </div>
      {foot && <div className="mt-1 text-[11px] text-muted-foreground">{foot}</div>}
    </div>
  );
}

export function QuotaMeter() {
  const { limits } = useData();
  if (!limits || (!limits.fiveHour && !limits.sevenDay)) return null;
  return (
    <div role="group" aria-label="Quota dell'abbonamento Claude" className="pb-3 pt-2.5">
      <div className="mb-2.5 flex items-baseline justify-between gap-3">
        <span className="text-[13px] text-heading">Quota dell'abbonamento Claude</span>
        <span className="text-[11px] text-muted-foreground" title="si aggiorna a ogni risposta">rilevata alle {hhmm(new Date(limits.at))}</span>
      </div>
      <div className="grid grid-cols-2 gap-[18px]">
        <Window label="5 ore" w={limits.fiveHour} />
        <Window label="7 giorni" w={limits.sevenDay} />
      </div>
    </div>
  );
}
