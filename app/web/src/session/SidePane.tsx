import { useMemo } from "react";
import { X } from "lucide-react";
import type { PlanNodeStatus, SessionState } from "../../../shared/protocol.ts";
import { Mermaid } from "./Mermaid.tsx";
import { planPalette } from "../theme-colors.ts";
import { token, useAppearance } from "../theme.ts";
import { ExerciseCard } from "./cards/ExerciseCard.tsx";

export type Tab = "plan" | "exercises";
export const TAB_LABEL: Record<Tab, string> = { plan: "Piano", exercises: "Esercizi" };

const LABEL: Record<PlanNodeStatus, string> = { done: "Fatto", current: "In corso", missed: "Da rivedere", todo: "Da fare" };
type Fill = { fill: string; stroke: string; color: string };

function planSource(dag: string, nodeStatus: Record<string, PlanNodeStatus>, palette: Record<PlanNodeStatus, Fill>) {
  const lines = [dag.trimEnd()];
  for (const [s, c] of Object.entries(palette)) lines.push(`classDef ${s} fill:${c.fill},stroke:${c.stroke},color:${c.color},stroke-width:1.5px;`);
  for (const s of Object.keys(palette)) {
    const ids = Object.entries(nodeStatus).filter(([, v]) => v === s).map(([k]) => k);
    if (ids.length) lines.push(`class ${ids.join(",")} ${s};`);
  }
  return lines.join("\n");
}

const Empty = ({ children }: { children: string }) => <p className="p-6 font-prose text-[15px] text-muted-foreground">{children}</p>;

/** The right-hand panel: plan DAG with node statuses, or exercises, chosen from the header. */
export function SidePane({ session, tab, onClose }: { session: SessionState; tab: Tab; onClose: () => void }) {
  const plan = session.plan;
  const appearance = useAppearance();
  const palette = useMemo(() => planPalette(token), [appearance]);
  const source = useMemo(() => (plan ? planSource(plan.dag, plan.nodeStatus, palette) : ""), [plan, palette]);
  return (
    <div className="flex h-full min-h-0 flex-col border-l border-hairline bg-background">
      <div className="flex h-9 shrink-0 items-center justify-between border-b border-hairline px-4 text-[11px] uppercase tracking-wider text-muted-foreground">
        <h2>{TAB_LABEL[tab]}</h2>
        <button type="button" aria-label="Chiudi il pannello" className="rounded p-1 hover:bg-accent hover:text-foreground" onClick={onClose}>
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {tab === "plan" &&
          (plan ? (
            <div className="space-y-3 p-4">
              <h3 className="font-prose text-lg font-semibold text-heading">{plan.topic}</h3>
              <p className="font-prose text-[15px]">{plan.goal}</p>
              <Mermaid source={source} />
              <ul className="flex flex-wrap gap-3 text-[11px] text-muted-foreground" aria-label="Legenda">
                {Object.entries(palette).map(([k, c]) => (
                  <li key={k} className="flex items-center gap-1.5">
                    <span className="inline-block h-2.5 w-2.5 rounded-sm border" style={{ background: c.fill, borderColor: c.stroke }} aria-hidden />
                    {LABEL[k as PlanNodeStatus]}
                  </li>
                ))}
              </ul>
              {!plan.approved && <p className="text-xs text-muted-foreground">Il piano non è ancora stato approvato.</p>}
            </div>
          ) : (
            <Empty>Il piano della lezione comparirà qui quando il docente lo proporrà.</Empty>
          ))}

        {tab === "exercises" &&
          (session.exercises.length ? (
            <div className="space-y-3 p-4">
              {session.exercises.map((e) => (
                <ExerciseCard key={`${e.course}/${e.name}`} exercise={e} sessionId={session.id} compact />
              ))}
            </div>
          ) : (
            <Empty>Non ci sono ancora esercizi per questa sessione.</Empty>
          ))}
      </div>
    </div>
  );
}
