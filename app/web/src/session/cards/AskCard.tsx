import type { ChatItem } from "../../../../shared/protocol.ts";
import { Markdown } from "../Markdown.tsx";

export type AskItem = Extract<ChatItem, { kind: "ask" }>;

export const askOptions = (item: AskItem) => (item.input.options ?? []).map((o) => ({ ...o, value: o.value ?? o.label }));

/** The answered (or cancelled) question in the transcript. A pending one lives in the dock. */
export function AskCard({ item }: { item: AskItem }) {
  const { input, answer, cancelled } = item;
  if (!answer) return cancelled ? <p className="text-xs text-muted-foreground">Domanda interrotta (il docente o la sessione si sono interrotti).</p> : null;
  const options = askOptions(item);
  const labels = [...answer.selected.map((v) => options.find((o) => o.value === v)?.label ?? v), ...(answer.other ? [answer.other] : [])];
  return (
    <section className="rounded-lg border border-hairline bg-card px-4 py-3">
      <Markdown text={input.question} className="sm text-muted-foreground" />
      <p className="mt-1.5 font-prose text-[15px]">
        <span className="text-xs text-muted-foreground">Tu · </span>
        {labels.join("; ") || "nessuna risposta"}
      </p>
    </section>
  );
}
