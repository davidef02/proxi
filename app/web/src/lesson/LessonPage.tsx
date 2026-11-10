// #/l/<lesson path>[#node-<id>]: in-app lesson reader. The HTML artifact stays the persistence; this only reads its data island.
import { ArrowLeft } from "lucide-react";
import { useEffect, useState } from "react";
import { DocBody } from "./DocBody.tsx";
import type { FullLessonDocument } from "../../../shared/protocol.ts";
import { NodeView } from "./NodeView.tsx";

type State = { doc: FullLessonDocument } | { error: string } | null;

async function load(path: string): Promise<FullLessonDocument> {
  const r = await fetch(`/api/lesson?path=${encodeURIComponent(path)}`);
  if (r.ok) return (await r.json()) as FullLessonDocument;
  const body = (await r.json().catch(() => null)) as { error?: string } | null;
  throw new Error(body?.error ?? r.statusText);
}

export function LessonPage({ path, node }: { path: string; node?: string }) {
  const [state, setState] = useState<State>(null);
  useEffect(() => {
    let live = true;
    setState(null);
    load(path).then(
      (doc) => live && setState({ doc }),
      (e: Error) => live && setState({ error: e.message }),
    );
    return () => {
      live = false;
    };
  }, [path]);
  useEffect(() => {
    if (state && "doc" in state && node) document.getElementById(`node-${node}`)?.scrollIntoView();
  }, [state, node]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto w-full max-w-[46rem] px-4 py-6">
        <button type="button" onClick={() => history.back()} className="mb-4 inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
          <ArrowLeft size={12} aria-hidden /> Indietro
        </button>
        {!state && (
          <p role="status" className="text-xs text-muted-foreground">
            Apro la lezione…
          </p>
        )}
        {state && "error" in state && <DocBody path={path} />} {/* no data island: read it as a document */}
        {state && "doc" in state && (
          <>
            <h2 className="font-prose text-2xl font-semibold text-heading">{state.doc.metadata.title}</h2>
            {state.doc.metadata.targetGoal && <p className="mt-1 font-prose text-[15px] text-muted-foreground">{state.doc.metadata.targetGoal}</p>}
            {state.doc.nodes.length === 0 ? (
              <div className="mt-6">
                <DocBody path={path} />
              </div>
            ) : (
              <div className="mt-6 space-y-8">
                {state.doc.nodes.map((n, i) => (
                  <NodeView key={n.nodeId} node={n} number={String(i + 1)} quiz />
                ))}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
