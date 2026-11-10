import { Check } from "lucide-react";
import type { ChatItem } from "../../../../shared/protocol.ts";
import { NodeView } from "../../lesson/NodeView.tsx";
import { lessonHref } from "../../router.ts";

type NodeItem = Extract<ChatItem, { kind: "node" }>;

/** The node as a document block in the transcript. Transcripts saved before v3 carry no payload: a compact link to the reader. */
export function NodeCard({ item }: { item: NodeItem }) {
  const n = item.planNode.replace(/^N/, "");
  if (!item.node)
    return (
      <a href={lessonHref(item.lessonPath, item.nodeId)} className="group flex w-full items-baseline gap-2 border-y border-hairline py-2 text-xs hover:bg-card">
        <Check className="h-3.5 w-3.5 shrink-0 translate-y-0.5 text-good" aria-hidden />
        <span className="min-w-0 flex-1 truncate">
          <span className="text-muted-foreground">Nodo {n} · </span>
          <span className="font-prose text-sm">{item.title}</span>
          {item.anki && <span className="text-muted-foreground"> · {item.anki}</span>}
        </span>
        <span className="shrink-0 text-muted-foreground group-hover:text-foreground">leggi →</span>
      </a>
    );
  return <NodeView node={item.node} number={n} footer={item.anki ? `Anki · ${item.anki}` : undefined} />;
}
