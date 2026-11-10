import type { ChatItem } from "../../../../shared/protocol.ts";
import { Markdown } from "../Markdown.tsx";
import { Card, Collapsible, DecisionBadge } from "../ui.tsx";

type CurriculumItem = Extract<ChatItem, { kind: "curriculum" }>;

/** The proposal as read in the transcript; Approva / Chiedi modifiche live in the dock. */
export function CurriculumCard({ item }: { item: CurriculumItem }) {
  const { input, decision, cancelled } = item;
  if (cancelled && !decision) return <p className="text-xs text-muted-foreground">Proposta annullata.</p>;
  return (
    <Card title="Piano del corso">
      <h4 className="font-prose text-lg font-semibold text-heading">{input.chapter}</h4>
      <p className="mt-1 font-prose text-[15px]">{input.destination}</p>
      <ol className="mt-3 list-decimal space-y-1 pl-6 font-prose text-[15px]">
        {input.lessons.map((l) => (
          <li key={l.title}>
            {l.title}
            {l.plannedNodes !== undefined && <span className="text-muted-foreground"> · {l.plannedNodes} nodi</span>}
          </li>
        ))}
      </ol>
      {input.mission && (
        <details className="mt-3">
          <summary className="text-xs text-muted-foreground hover:text-foreground">Missione del corso</summary>
          <div className="mt-2">
            <Collapsible>
              <Markdown text={input.mission} className="sm" />
            </Collapsible>
          </div>
        </details>
      )}
      {decision && <DecisionBadge decision={decision} />}
    </Card>
  );
}
