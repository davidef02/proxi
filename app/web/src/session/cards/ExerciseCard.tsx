import { api } from "../../api.ts";
import { exerciseHref } from "../../router.ts";
import type { ExerciseInfo } from "../../../../shared/protocol.ts";
import { Markdown } from "../Markdown.tsx";
import { Card, ErrorLine, btnPrimary, useSubmit } from "../ui.tsx";

export function ExerciseCard({ exercise, sessionId, compact = false }: { exercise: ExerciseInfo; sessionId: string; compact?: boolean }) {
  const { error, run } = useSubmit();
  const last = exercise.lastRun;

  return (
    <Card title={compact ? undefined : exercise.final ? "Esercizio finale" : "Esercizio"}>
      <h4 className="font-semibold">{exercise.name}</h4>
      {!compact && exercise.instructions && (
        <div className="mt-2 text-sm">
          <Markdown text={exercise.instructions} />
        </div>
      )}
      <details className="mt-2 text-sm">
        <summary className="cursor-pointer">{exercise.files.length} file in {exercise.dir}</summary>
        <ul className="mt-1 list-disc pl-6 font-mono text-xs">
          {exercise.files.map((f) => (
            <li key={f}>{f}</li>
          ))}
        </ul>
      </details>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <a href={exerciseHref(exercise.course, exercise.name, sessionId)} className={btnPrimary}>
          Apri esercizio
        </a>
        <button type="button" className="rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent/60 hover:text-foreground" onClick={() => void run(() => api.openExercise(exercise.course, exercise.name))}>
          Apri in VS Code
        </button>
        {exercise.hints && <span className="text-xs text-muted-foreground">suggerimenti {exercise.hintsRevealed ?? 0}/{exercise.hints.length}</span>}
      </div>
      <ErrorLine error={error} />
      {last && (
        <div role="status" className={`mt-3 rounded-md border p-2 text-sm ${last.ok ? "border-good bg-good/10" : "border-bad bg-bad/10"}`}>
          {last.error ? (
            <p>I test non sono partiti: {last.error}</p>
          ) : (
            <p className="font-medium">
              {last.ok ? "Tutti i test passano" : `${last.failed} test su ${last.total} falliscono`} ({last.passed}/{last.total} verdi, {(last.durationMs / 1000).toFixed(1)} s)
            </p>
          )}
          {last.failures.length > 0 && (
            <ul className="mt-1 space-y-1">
              {last.failures.map((f) => (
                <li key={f.name}>
                  <span className="font-medium">{f.name}</span>
                  <pre className="overflow-auto whitespace-pre-wrap text-xs">{f.message}</pre>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </Card>
  );
}
