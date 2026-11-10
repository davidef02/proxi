import type { ChatItem } from "../../../../shared/protocol.ts";
import { Markdown } from "../Markdown.tsx";
import { Card } from "../ui.tsx";

type ProblemItem = Extract<ChatItem, { kind: "problem" }>;

const MODE = { recall: "Richiamo", discrimination: "Discriminazione", composition: "Composizione" } as const;
const OUTCOME = {
  solved: ["Risolto", "text-good"],
  solvedWithHints: ["Risolto con aiuti", "text-good"],
  movedOn: ["Messo da parte", "text-warn"],
  surrendered: ["Non risolto", "text-bad"],
} as const;

export function ProblemCard({ item }: { item: ProblemItem }) {
  const { input, conclusion } = item;
  // Open problem: the statement lives in the dock above the input, not twice on screen.
  if (!conclusion) return <p className="text-center text-xs text-muted-foreground">Problema · {MODE[input.mode]}</p>;
  return (
    <Card title={`Problema · ${MODE[input.mode]}`}>
      <Markdown text={input.statement} className="sm" />
      <div className="mt-3 border-t border-hairline pt-2">
          <p className={`text-xs ${OUTCOME[conclusion.outcome][1]}`}>{OUTCOME[conclusion.outcome][0]}</p>
          <Markdown text={conclusion.verdict} className="sm" />
          <details className="mt-2">
            <summary className="text-xs text-muted-foreground hover:text-foreground">Soluzione di riferimento</summary>
            <div className="mt-1">
              <Markdown text={input.modelSolution} className="sm" />
            </div>
          </details>
      </div>
    </Card>
  );
}
