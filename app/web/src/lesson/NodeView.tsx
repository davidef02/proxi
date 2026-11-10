// One cognitive node as a document block, in app style. Used inline in the chat transcript and in the lesson reader.
// Authored HTML (motivation/content/connection/svg) is sanitized here: the server only refuses obviously active content.
import DOMPurify from "dompurify";
import { Link2, Zap } from "lucide-react";
import katex from "katex";
import renderMathInElement from "katex/contrib/auto-render";
import { useEffect, useMemo, useRef, type ReactNode, type RefObject } from "react";
import type { CognitiveNodePayload, PedagogicalTier } from "../../../shared/protocol.ts";
import { Markdown } from "../session/Markdown.tsx";
import { Mermaid } from "../session/Mermaid.tsx";

const TAGS = [
  "p", "br", "div", "span", "h1", "h2", "h3", "h4", "h5", "ul", "ol", "li", "strong", "b", "em", "i", "code", "pre",
  "table", "thead", "tbody", "tr", "th", "td", "blockquote", "a", "sub", "sup", "hr", "header", "section", "small",
  "figure", "figcaption", "svg", "g", "path", "circle", "rect", "line", "polyline", "polygon", "text",
];
const ALLOWED_CLASSES = new Set([
  "callout", "grid", "card", "truth-item", "formula-card", "diagram-container", "ref-container", "mermaid",
  "node-card", "tier-unconditional-truth", "tier-axiom", "tier-derivation", "badge", "badge-truth", "badge-deriv",
  "node-card-claim", "misconceptions-card", "misconceptions-list", "evidence-prose", "code-card", "code-caption",
  "node-card-header", "node-card-title-group", "node-card-num", "node-card-title", "subtitle", "sintesi-nodes",
]);
const html = DOMPurify(window);
html.addHook("afterSanitizeAttributes", (el) => {
  // `class` survives only as the callout marker; links open outside the app.
  const cls = el.getAttribute("class");
  if (cls) {
    const kept = cls.split(/\s+/).filter((c) => ALLOWED_CLASSES.has(c)).join(" ");
    if (kept) el.setAttribute("class", kept);
    else el.removeAttribute("class");
  }
  if (el.tagName === "A") {
    el.setAttribute("target", "_blank");
    el.setAttribute("rel", "noreferrer");
  }
});
const svg = DOMPurify(window);

const DELIMITERS = [
  { left: "$$", right: "$$", display: true },
  { left: "$", right: "$", display: false },
];

/** Typesets $..$ and $$..$$ inside an element (pre/code are skipped by auto-render). */
function useMath(ref: RefObject<HTMLElement | null>, dep: string) {
  useEffect(() => {
    if (ref.current) renderMathInElement(ref.current, { delimiters: DELIMITERS, throwOnError: false });
  }, [ref, dep]);
}

const ATTRS = [
  "href", "class", "id", "title", "aria-hidden", "style", "viewBox", "width", "height",
  "fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin", "d", "xmlns",
  "cx", "cy", "r", "x", "y", "x1", "y1", "x2", "y2",
];

export function Rich({ source, className = "" }: { source: string; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const clean = useMemo(() => html.sanitize(source, { ALLOWED_TAGS: TAGS, ALLOWED_ATTR: ATTRS }), [source]);
  useMath(ref, clean);
  return <div ref={ref} className={`prose-lh node-rich ${className}`} dangerouslySetInnerHTML={{ __html: clean }} />;
}

/** One step of the loop in its own tinted panel (the old artifact's step-motivate / step-connect), so the eye can tell
 *  why-it-matters and how-it-links apart from the explanation itself. */
function Step({ kind, icon, label, source }: { kind: "motivate" | "connect"; icon: ReactNode; label: string; source: string }) {
  return (
    <section className={`node-step node-step-${kind}`} aria-label={label}>
      <p className="node-step-label">{icon}{label}</p>
      <Rich source={source} className="sm" />
    </section>
  );
}

const TIER: Record<PedagogicalTier, { label: string; cls: string }> = {
  "unconditional-truth": { label: "Verità incondizionata", cls: "node-tier-truth" },
  axiom: { label: "Assioma", cls: "node-tier-truth" },
  derivation: { label: "Derivazione", cls: "node-tier-derive" },
};

const Title = ({ text }: { text: string }) => {
  const ref = useRef<HTMLSpanElement>(null);
  useMath(ref, text);
  return <span ref={ref}>{text}</span>;
};

const display = (m: string) => katex.renderToString(m.startsWith("$$") && m.endsWith("$$") ? m.slice(2, -2) : m, { displayMode: true, throwOnError: false });

function fence(code: string, language: string = "") {
  const ticks = "`".repeat(Math.max(3, ...[...code.matchAll(/`+/g)].map((m) => m[0].length + 1)));
  return `${ticks}${language}\n${code.replace(/\n$/, "")}\n${ticks}`;
}

const Caption = ({ children }: { children?: string }) => (children ? <figcaption className="mt-1 text-center text-xs text-muted-foreground">{children}</figcaption> : null);

function Visual({ v }: { v: NonNullable<CognitiveNodePayload["visuals"]>[number] }) {
  const o = typeof v === "string" ? { type: v.trim().startsWith("<svg") ? "svg-inline" : "image", content: v } : v;
  const caption = typeof v === "string" ? undefined : v.caption;
  const body =
    o.type === "mermaid" ? (
      <Mermaid source={o.content} />
    ) : o.type === "svg-inline" ? (
      <div className="overflow-auto [&_svg]:mx-auto [&_svg]:h-auto [&_svg]:max-w-full" dangerouslySetInnerHTML={{ __html: svg.sanitize(o.content, { USE_PROFILES: { svg: true, svgFilters: true }, FORBID_TAGS: ["style"] }) }} />
    ) : /^(https?:\/\/|data:image\/)/.test(o.content) ? (
      <img src={o.content} alt={typeof v === "string" ? "" : (v.alt ?? "")} className="mx-auto max-w-full" />
    ) : null;
  return body ? (
    <figure className="my-4 rounded-md border border-hairline p-3">
      {body}
      <Caption>{caption}</Caption>
    </figure>
  ) : null;
}

/** The check quiz mirrored in the artifact; only the reader shows it (in chat the quiz is asked live in the dock). */
function QuizMirror({ q }: { q: NonNullable<CognitiveNodePayload["quiz"]> }) {
  return (
    <details className="node-quiz mt-5 rounded-md border border-hairline px-3 py-2">
      <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">Domanda di verifica</summary>
      <div className="mt-2">
        <Markdown text={q.question} className="sm font-medium" />
        {q.options && (
          <ol className="mt-1 list-[upper-alpha] pl-6 font-prose text-[15px]">
            {q.options.map((o, i) => (
              <li key={i}>
                <Markdown text={o} className="sm" />
              </li>
            ))}
          </ol>
        )}
        <details className="mt-2">
          <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">Mostra la risposta</summary>
          <div className="mt-1">
            {q.correctAnswer && <Markdown text={`**${q.correctAnswer}**`} className="sm" />}
            <Markdown text={q.explanation} className="sm text-muted-foreground" />
          </div>
        </details>
      </div>
    </details>
  );
}

export function NodeView({ node, number, footer, quiz = false }: { node: CognitiveNodePayload; number?: string; footer?: ReactNode; quiz?: boolean }) {
  const tier = TIER[node.tier] ?? TIER.derivation;
  const hasSteps = !!(node.steps && node.steps.length > 0);
  return (
    <article id={`node-${node.nodeId}`} className={`node-card ${tier.cls} min-w-0 scroll-mt-4 [overflow-wrap:anywhere]`}>
      <header className="mb-3">
        <p className="flex flex-wrap items-center gap-2 text-[11px] uppercase tracking-wider text-muted-foreground">
          {number && <span>Nodo {number}</span>}
          <span className="node-tier">{tier.label}</span>
        </p>
        <h3 className="mt-1.5 font-prose text-[1.4rem] font-semibold leading-snug text-heading">
          <Title text={node.title} />
        </h3>
      </header>
      {node.motivation && <Step kind="motivate" icon={<Zap size={12} aria-hidden />} label="Perché ti serve" source={node.motivation} />}
      {hasSteps ? (
        <div className="node-steps mt-3 space-y-3">
          {node.steps!.map((s) => (
            <div key={s.stepId} className="node-step rounded-md border border-hairline bg-card/40 p-3">
              <div className="mb-1.5 flex items-center gap-2 text-[11px] uppercase tracking-wider text-muted-foreground">
                <span className="font-semibold text-primary">{s.stepId}</span>
                {s.title && <span>{s.title}</span>}
              </div>
              <Rich source={s.content} className="node-body" />
              {s.math?.map((m, i) => (
                <div key={i} className="node-math prose-lh my-3 overflow-x-auto" dangerouslySetInnerHTML={{ __html: display(m) }} />
              ))}
              {s.codeBlocks?.map((cb, i) => (
                <figure key={i} className="my-3">
                  <Markdown text={fence(cb.code, cb.language)} />
                  <Caption>{cb.caption}</Caption>
                </figure>
              ))}
              {s.visuals?.map((v, i) => (
                <Visual key={i} v={v} />
              ))}
            </div>
          ))}
        </div>
      ) : (
        <>
          <Rich source={node.content} className="node-body" />
          {node.math?.map((m, i) => (
            <div key={i} className="node-math prose-lh my-3 overflow-x-auto" dangerouslySetInnerHTML={{ __html: display(m) }} />
          ))}
          {node.codeBlocks?.map((cb, i) => (
            <figure key={i} className="my-3">
              <Markdown text={fence(cb.code, cb.language)} />
              <Caption>{cb.caption}</Caption>
            </figure>
          ))}
          {node.visuals?.map((v, i) => (
            <Visual key={i} v={v} />
          ))}
        </>
      )}
      {node.connection && <Step kind="connect" icon={<Link2 size={12} aria-hidden />} label="Collegamento" source={node.connection} />}
      {quiz && node.quiz && <QuizMirror q={node.quiz} />}
      {footer && <footer className="mt-4 border-t border-hairline pt-2 text-xs text-muted-foreground">{footer}</footer>}
    </article>
  );
}
