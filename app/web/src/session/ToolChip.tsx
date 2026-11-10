import type { ChatItem } from "../../../shared/protocol.ts";
import { Spinner } from "./Spinner.tsx";

type ToolItem = Extract<ChatItem, { kind: "tool" }>;

const LABELS: Record<string, string> = {
  Read: "Legge un file",
  Glob: "Cerca file",
  Grep: "Cerca nel testo",
  WebSearch: "Cerca sul web",
  WebFetch: "Legge una pagina web",
  quiz: "Prepara una domanda",
  ask: "Ti fa una domanda",
  propose_plan: "Propone il piano",
  write_node: "Scrive il nodo",
  sync_anki: "Sincronizza Anki",
  write_exercise: "Scrive l'esercizio",
  run_exercise: "Esegue i test",
  finish_lesson: "Chiude la lezione",
  propose_curriculum: "Propone il piano del corso",
  problem: "Propone un problema",
  conclude: "Conclude il problema",
  research: "Avvia una ricerca",
  visual: "Avvia una figura",
  demo: "Avvia una demo",
  read_lesson: "Rilegge la lezione",
};

export function toolLabel(tool: string) {
  const name = tool.replace(/^mcp__app__/, "");
  return LABELS[name] ?? name.replace(/_/g, " ");
}

/** One short line describing what the tool was asked to do. */
export function summarize(input: unknown): string {
  if (!input || typeof input !== "object") return "";
  const o = input as Record<string, unknown>;
  for (const k of ["file_path", "path", "pattern", "query", "url", "question", "title", "planNode", "name", "topic", "chapter"]) {
    if (typeof o[k] === "string" && o[k]) return o[k] as string;
  }
  const node = o.node as { title?: string } | undefined;
  return node?.title ?? "";
}

const MARK = { running: null, done: <span className="text-good">·</span>, error: <span className="text-bad">!</span> } as const;

/** A tool call is one quiet line; the result, when there is one, expands underneath. */
export function ToolChip({ item }: { item: ToolItem }) {
  const sum = summarize(item.input);
  const body = (
    <>
      <span className="flex w-3 shrink-0 justify-center" aria-hidden>{item.status === "running" ? <Spinner className="h-2.5" /> : MARK[item.status]}</span>
      <span>{toolLabel(item.tool)}</span>
      {sum && <span className="truncate opacity-60">{sum}</span>}
      <span className="sr-only">({item.status === "running" ? "in corso" : item.status === "done" ? "fatto" : "errore"})</span>
    </>
  );
  const cls = "flex max-w-full items-center gap-2 text-xs text-muted-foreground";
  if (!item.resultText) return <div className={cls}>{body}</div>;
  return (
    <details className="max-w-full text-xs text-muted-foreground">
      <summary className={`${cls} cursor-pointer list-none hover:text-foreground`}>{body}</summary>
      <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded border border-hairline bg-card p-2 [overflow-wrap:anywhere]">{item.resultText}</pre>
    </details>
  );
}
