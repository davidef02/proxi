// In-app reader for a lesson or reference-sheet HTML file (GET /api/doc): body markup, sanitized and typeset like a node.
import { ArrowLeft } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { SintesiDocument } from "../../../shared/protocol.ts";
import { mermaidConfig } from "../session/Mermaid.tsx";
import { useAppearance } from "../theme.ts";
import { Rich } from "./NodeView.tsx";
import { ReferenceView } from "./ReferenceView.tsx";

type State = { html: string } | { error: string } | null;

async function load(path: string): Promise<string> {
  const r = await fetch(`/api/doc?path=${encodeURIComponent(path)}`);
  const body = (await r.json().catch(() => null)) as { html?: string; error?: string } | null;
  if (r.ok && body?.html !== undefined) return body.html;
  throw new Error(body?.error ?? r.statusText);
}

export function DocBody({ path }: { path: string }) {
  const [state, setState] = useState<State>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const appearance = useAppearance();

  useEffect(() => {
    let live = true;
    setState(null);
    load(path).then(
      (html) => live && setState({ html }),
      (e: Error) => live && setState({ error: e.message }),
    );
    return () => {
      live = false;
    };
  }, [path]);

  useEffect(() => {
    if (!state || !("html" in state) || !containerRef.current) return;
    const el = containerRef.current;
    const pres = Array.from(el.querySelectorAll<HTMLPreElement>("pre.mermaid"));
    if (pres.length === 0) return;

    let live = true;
    (async () => {
      try {
        const m = (await import("mermaid")).default;
        m.initialize(mermaidConfig(appearance));
        for (let i = 0; i < pres.length; i++) {
          const pre = pres[i];
          if (!live) break;
          const src = pre.textContent?.trim();
          if (!src) continue;
          const id = `mmdoc_${i}_` + Math.random().toString(36).slice(2, 8);
          try {
            const { svg } = await m.render(id, src);
            if (live) {
              const div = document.createElement("div");
              div.className = "overflow-auto my-2 [&_svg]:mx-auto [&_svg]:h-auto [&_svg]:max-w-full";
              div.innerHTML = svg;
              pre.replaceWith(div);
            }
          } catch {
            document.getElementById("d" + id)?.remove();
          }
        }
      } catch {
        // Fallback: leave raw pre
      }
    })();
    return () => {
      live = false;
    };
  }, [state, appearance]);

  if (!state)
    return (
      <p role="status" className="text-xs text-muted-foreground">
        Apro il documento…
      </p>
    );
  if ("error" in state)
    return (
      <p role="alert" className="font-prose text-[15px] text-muted-foreground">
        Non riesco a leggere il documento ({state.error}).
      </p>
    );
  return (
    <div ref={containerRef}>
      {/* a new palette × mode remounts the body: the mermaid <pre> nodes are recreated before the effect re-renders them */}
      <Rich key={appearance.key} source={state.html} />
    </div>
  );
}

type RefState = { doc: SintesiDocument } | { fallback: boolean } | null;

async function loadReference(path: string): Promise<SintesiDocument> {
  const r = await fetch(`/api/reference?path=${encodeURIComponent(path)}`);
  if (!r.ok) throw new Error(r.statusText);
  return (await r.json()) as SintesiDocument;
}

/** #/r/<reference path>: a course's reference sheet ("Sintesi"). */
export function ReferencePage({ path }: { path: string }) {
  const [state, setState] = useState<RefState>(null);

  useEffect(() => {
    let live = true;
    setState(null);
    loadReference(path).then(
      (doc) => live && setState({ doc }),
      () => live && setState({ fallback: true }),
    );
    return () => {
      live = false;
    };
  }, [path]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto w-full max-w-[46rem] px-4 py-6">
        <button
          type="button"
          onClick={() => history.back()}
          className="mb-4 inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft size={12} aria-hidden /> Indietro
        </button>
        {!state ? (
          <p role="status" className="text-xs text-muted-foreground">
            Apro la sintesi…
          </p>
        ) : "doc" in state ? (
          <ReferenceView doc={state.doc} />
        ) : (
          <DocBody path={path} />
        )}
      </div>
    </div>
  );
}
