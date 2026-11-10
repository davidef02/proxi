import { X } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../api.ts";
import { useData } from "./data.tsx";
import { BrailleSpinner } from "./BrailleSpinner.tsx";

const RECENT_MS = 90_000;

/** Background jobs (subagents): running ones plus those finished in the last minute and a half. */
export function ActivityDock() {
  const { jobs } = useData();
  const [now, setNow] = useState(Date.now());
  const running = jobs.some((j) => j.status === "running");
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), running ? 5000 : 15000);
    return () => clearInterval(t);
  }, [running]);
  const shown = jobs.filter((j) => j.status === "running" || now - new Date(j.updatedAt).getTime() < RECENT_MS).slice(0, 4);
  if (!shown.length) return null;
  return (
    <section aria-label="Attività" className="border-t border-hairline px-3 py-2">
      <h2 className="mb-1 px-1 text-[11px] uppercase tracking-wider text-muted-foreground">Attività</h2>
      <ul className="space-y-0.5">
        {shown.map((j) => (
          <li key={j.id} className="group flex items-start gap-2 rounded px-1 py-1 hover:bg-accent/60">
            <span className="mt-px w-3 shrink-0 text-muted-foreground">
              {j.status === "running" ? <BrailleSpinner label={j.title} /> : j.status === "done" ? <span className="text-good">✓</span> : <span className="text-bad">!</span>}
            </span>
            <a href={j.sessionId ? `#/s/${j.sessionId}` : "#/"} className="min-w-0 flex-1">
              <span className="block truncate text-xs">{j.title}</span>
              <span className="block truncate text-[11px] text-muted-foreground">
                {j.status === "error" ? j.error ?? "errore" : j.status === "cancelled" ? "annullato" : j.activity}
              </span>
            </a>
            {j.status === "running" && (
              <button
                type="button"
                aria-label={`Annulla ${j.title}`}
                title="Annulla"
                onClick={() => api.cancelJob(j.id).catch(() => {})}
                className="rounded p-0.5 text-muted-foreground opacity-0 hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
              >
                <X size={12} />
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
