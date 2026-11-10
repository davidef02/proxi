// Background subagent cards (research / visual / demo / map). Look from LearnHarness's artifact cards, re-authored.
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronRight, Maximize2, X } from "lucide-react";
import type { JobRecord } from "../../../../shared/protocol.ts";
import { withDemoCsp } from "../../../../shared/csp.ts";
import { Markdown } from "../Markdown.tsx";
import { Spinner } from "../Spinner.tsx";

const KIND_LABEL = { research: "Ricerca", visual: "Figura", demo: "Demo", map: "Mappa" } as const;

// ─── demo bridge (pure helpers, exported for tests) ───
/** Injected into every demo: ResizeObserver → postMessage height, window errors → postMessage. */
export const DEMO_BRIDGE = `<script>(function(){
var send=function(m){try{parent.postMessage(m,"*")}catch(e){}};
var post=function(){var b=document.body;var h=b?Math.max(b.scrollHeight,b.getBoundingClientRect().height):document.documentElement.scrollHeight;
if(b){var cs=getComputedStyle(b);h+=(parseFloat(cs.marginTop)||0)+(parseFloat(cs.marginBottom)||0)}
send({type:"demo-resize",height:Math.ceil(h)})};
if(typeof ResizeObserver!=="undefined")new ResizeObserver(post).observe(document.documentElement);
addEventListener("resize",post);addEventListener("load",post);
addEventListener("error",function(e){send({type:"demo-error",message:e.message||String(e)})});
setTimeout(post,50);setTimeout(post,400);
})();</script>`;

export const demoSrcDoc = (html: string) => (/<\/body>/i.test(html) ? html.replace(/<\/body>/i, () => `${DEMO_BRIDGE}</body>`) : html + DEMO_BRIDGE);

/** withDemoCsp, but after a leading doctype (a <meta> before it would put the demo in quirks mode). */
export function cspDoc(html: string): string {
  return withDemoCsp(html); // inserts the meta after a leading doctype
}

/** Clamp a height reported by the iframe; null when the message is not a usable resize. */
export function demoHeight(data: unknown): number | null {
  const m = data as { type?: unknown; height?: unknown } | null;
  if (!m || m.type !== "demo-resize" || typeof m.height !== "number" || !Number.isFinite(m.height) || m.height <= 0) return null;
  return Math.min(Math.max(Math.round(m.height), 80), 2400);
}

const svgUrl = (svg: string) => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;

const frame = "rounded-lg border border-hairline bg-card";

function Zoom({ src, alt, onClose }: { src: string; alt: string; onClose: () => void }) {
  useEffect(() => {
    const h = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", h);
    return () => document.removeEventListener("keydown", h);
  }, [onClose]);
  return createPortal(
    <div role="dialog" aria-modal="true" aria-label={alt} className="fixed inset-x-0 bottom-0 top-9 z-50 flex items-center justify-center bg-black/85 px-8 pb-8 pt-5" onClick={onClose}>
      <button type="button" aria-label="Chiudi" className="absolute right-4 top-3 rounded-md border border-border bg-background p-1.5 text-foreground hover:bg-accent" onClick={onClose}>
        <X className="h-4 w-4" />
      </button>
      <img src={src} alt={alt} className="max-h-full max-w-full rounded-lg bg-background object-contain" onClick={(e) => e.stopPropagation()} />
    </div>,
    document.body,
  );
}

function Visual({ job, svg, caption }: { job: JobRecord; svg: string; caption?: string }) {
  const [zoom, setZoom] = useState(false);
  const src = svgUrl(svg);
  return (
    <figure className={`${frame} p-3`}>
      <button type="button" aria-label="Ingrandisci la figura" className="group relative block w-full rounded-md bg-background" onClick={() => setZoom(true)}>
        <img src={src} alt={caption ?? job.title} className="mx-auto max-h-96 w-auto max-w-full p-2" />
        <Maximize2 className="absolute right-2 top-2 h-3.5 w-3.5 text-muted-foreground opacity-0 group-hover:opacity-100" aria-hidden />
      </button>
      {caption && <figcaption className="mt-2 font-prose text-sm text-muted-foreground">{caption}</figcaption>}
      {zoom && <Zoom src={src} alt={caption ?? job.title} onClose={() => setZoom(false)} />}
    </figure>
  );
}

function Demo({ title, html, notes }: { title: string; html: string; notes?: string }) {
  const ref = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(240);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const h = (e: MessageEvent) => {
      if (e.source !== ref.current?.contentWindow) return;
      const next = demoHeight(e.data);
      if (next) setHeight(next);
      const d = e.data as { type?: string; message?: unknown } | null;
      if (d?.type === "demo-error") setError(typeof d.message === "string" ? d.message : "errore sconosciuto");
    };
    addEventListener("message", h);
    return () => removeEventListener("message", h);
  }, []);
  return (
    <figure className={`${frame} overflow-hidden`}>
      <figcaption className="border-b border-hairline px-3 py-1.5 text-[11px] uppercase tracking-wider text-muted-foreground">Demo · {title}</figcaption>
      <iframe ref={ref} title={`Demo: ${title}`} sandbox="allow-scripts" srcDoc={cspDoc(demoSrcDoc(html))} style={{ height }} className="block w-full border-0 bg-white" />
      {(notes || error) && (
        <div className="space-y-1 border-t border-hairline px-3 py-2 text-xs">
          {error && <p className="text-bad">Errore nella demo: {error}</p>}
          {notes && <p className="font-prose text-sm text-muted-foreground">{notes}</p>}
        </div>
      )}
    </figure>
  );
}

function Research({ job, brief, sources }: { job: JobRecord; brief: string; sources: { title: string; url: string }[] }) {
  return (
    <details className={`${frame} group`}>
      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 text-xs text-muted-foreground hover:text-foreground">
        <ChevronRight className="h-3.5 w-3.5 shrink-0 transition-transform group-open:rotate-90" aria-hidden />
        <span className="uppercase tracking-wider">Ricerca</span>
        <span className="min-w-0 flex-1 truncate font-prose text-sm text-foreground">{job.title.replace(/^Ricerca:\s*/, "")}</span>
        <span className="shrink-0">{sources.length} font{sources.length === 1 ? "e" : "i"}</span>
      </summary>
      <div className="border-t border-hairline px-4 py-3">
        <Markdown text={brief} className="sm" />
        {sources.length > 0 && (
          <ul className="mt-3 space-y-0.5 border-t border-hairline pt-2 text-xs">
            {sources.map((s) => (
              <li key={s.url} className="truncate">
                <a href={s.url} target="_blank" rel="noreferrer" className="text-info underline underline-offset-2 hover:text-foreground">
                  {s.title}
                </a>
              </li>
            ))}
          </ul>
        )}
      </div>
    </details>
  );
}

export function JobCard({ job }: { job: JobRecord }) {
  const label = KIND_LABEL[job.kind];
  if (job.status === "running")
    return (
      <div className={`${frame} flex items-center gap-2 px-3 py-2 text-xs`}>
        <Spinner label={`${label} in corso`} />
        <span className="uppercase tracking-wider text-muted-foreground">{label}</span>
        <span className="min-w-0 truncate font-prose text-sm">{job.title.replace(/^[^:]+:\s*/, "")}</span>
        {job.activity && <span className="ml-auto min-w-0 truncate text-muted-foreground">{job.activity}</span>}
      </div>
    );
  if (job.status === "error" || job.status === "cancelled" || !job.result)
    return (
      <p className="text-xs text-muted-foreground">
        <span className="uppercase tracking-wider">{label}</span> · {job.title}
        <span className={job.status === "cancelled" ? "" : "text-bad"}> · {job.status === "cancelled" ? "annullata" : (job.error ?? "non riuscita")}</span>
      </p>
    );
  const r = job.result;
  switch (r.kind) {
    case "research":
      return <Research job={job} brief={r.brief} sources={r.sources} />;
    case "visual":
      return <Visual job={job} svg={r.svg} caption={r.caption} />;
    case "demo":
      return <Demo title={r.title} html={r.html} notes={r.notes} />;
    case "map":
      return (
        <p className="text-xs text-muted-foreground">
          Mappa aggiornata: <span className="text-foreground">+{r.added} idee, {r.linked} collegate</span>
        </p>
      );
  }
}
