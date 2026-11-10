import { Check, X } from "lucide-react";
import type { ChatItem } from "../../../../shared/protocol.ts";
import { Markdown } from "../Markdown.tsx";
import { ConfidenceRail, confidenceLabel } from "../ConfidenceRail.tsx";

export type QuizItem = Extract<ChatItem, { kind: "quiz" }>;
export const LETTERS = "ABCDEF";
export const KIND_LABEL = { probe: "Per capire da dove partire", predict: "Prima di spiegare", check: "Verifica" } as const;

/** Options in display order; displayOrder falls back to the authored order if it is malformed. */
export function quizOptions(item: QuizItem) {
  const { input, displayOrder } = item;
  const order = displayOrder.length === input.options.length && displayOrder.every((i) => input.options[i]) ? displayOrder : input.options.map((_, i) => i);
  return order.map((i) => ({ ...input.options[i], value: input.options[i].value ?? input.options[i].label }));
}

/** The answered (or cancelled) quiz in the transcript. A pending one lives in the dock. */
export function QuizCard({ item }: { item: QuizItem }) {
  const { input, result, cancelled } = item;
  if (!result) return cancelled ? <p className="text-xs text-muted-foreground">Domanda interrotta (il docente o la sessione si sono interrotti).</p> : null;
  const options = quizOptions(item);
  const correct = new Set(Array.isArray(input.correctAnswer) ? input.correctAnswer : [input.correctAnswer]);
  const chosen = new Set(result.selected);
  return (
    <section className="rounded-lg border border-hairline bg-card p-4">
      <div className="mb-2 flex items-center justify-between text-[11px] uppercase tracking-wider text-muted-foreground">
        <span>{KIND_LABEL[input.kind]}</span>
        <span className={result.dontKnow ? "" : result.correct ? "text-good" : "text-bad"}>{result.dontKnow ? "Non lo so" : result.correct ? "Corretta" : "Non corretta"}</span>
      </div>
      <Markdown text={input.question} className="sm font-medium" />
      <ul className="mt-3 space-y-1.5">
        {options.map((o, i) => {
          const isCorrect = correct.has(o.value);
          const isChosen = chosen.has(o.value);
          const miscon = isChosen && !isCorrect ? input.misconceptions?.find((m) => m.value === o.value)?.misconception : undefined;
          return (
            <li key={o.value} className={`rounded-md border px-3 py-1.5 ${isCorrect ? "border-good/60" : isChosen ? "border-bad/60" : "border-hairline opacity-70"}`}>
              <div className="flex items-start gap-2">
                <span className="mt-1 w-4 shrink-0 text-[11px] text-muted-foreground" aria-hidden>
                  {LETTERS[i]}
                </span>
                <div className="min-w-0 flex-1">
                  <Markdown text={o.label} className="sm" />
                </div>
                <span className="mt-1.5 shrink-0" title={isCorrect ? (isChosen ? "Giusta, scelta da te" : "Giusta") : isChosen ? "Scelta da te" : undefined}>
                  {isCorrect && <Check className="h-3.5 w-3.5 text-good" aria-label="giusta" />}
                  {!isCorrect && isChosen && <X className="h-3.5 w-3.5 text-bad" aria-label="sbagliata, scelta da te" />}
                </span>
              </div>
              {miscon && <Markdown text={miscon} className="sm mt-1 border-t border-bad/30 pt-1 text-muted-foreground" />}
            </li>
          );
        })}
      </ul>
      {result.confidence !== undefined && (
        <div className="mt-3 flex items-center gap-3 text-[11px] text-muted-foreground">
          <span className="shrink-0">
            Sicurezza {result.confidence}% · {confidenceLabel(result.confidence)}
          </span>
          <div className="w-32">
            <ConfidenceRail value={result.confidence} />
          </div>
        </div>
      )}
      {result.thinkingWhileReading && (
        <p className="mt-2 font-prose text-xs italic text-muted-foreground">Durante la lettura: {result.thinkingWhileReading}</p>
      )}
      {result.note && <p className="mt-2 font-prose text-sm italic text-muted-foreground">Perché: {result.note}</p>}
      <div className="mt-3 border-t border-hairline pt-2">
        <Markdown text={input.explanation} className="sm" />
      </div>
    </section>
  );
}
