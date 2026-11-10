import type { ChatItem } from "../../../../shared/protocol.ts";
import { Markdown } from "../Markdown.tsx";
import { Mermaid } from "../Mermaid.tsx";
import { Card, Collapsible, DecisionBadge } from "../ui.tsx";

type PlanItem = Extract<ChatItem, { kind: "plan" }>;

/** The proposal as read in the transcript; Approva / Chiedi modifiche live in the dock. */
export function PlanCard({ item }: { item: PlanItem }) {
  const { input, decision, cancelled } = item;
  if (cancelled && !decision) return <p className="text-xs text-muted-foreground">Proposta annullata.</p>;
  return (
    <Card title="Piano della lezione">
      <h4 className="font-prose text-lg font-semibold text-heading">{input.title ?? input.topic}</h4>
      <div className="mt-2">
        <Collapsible>
          <Markdown text={input.approach} className="sm" />
        </Collapsible>
      </div>
      <div className="my-3 rounded-md border border-hairline p-2">
        <Mermaid source={input.dag} />
      </div>
      <p className="font-prose text-[15px]">
        <span className="text-xs text-muted-foreground">Obiettivo · </span>
        {input.goal}
      </p>
      {decision && <DecisionBadge decision={decision} />}
    </Card>
  );
}
