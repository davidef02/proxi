import { AlertTriangle, Calculator, CheckCircle2, GitFork, Sparkles } from "lucide-react";
import type { PedagogicalTier, SintesiDocument, SintesiNode, SintesiMisconception } from "../../../shared/protocol.ts";
import { Mermaid } from "../session/Mermaid.tsx";
import { Rich } from "./NodeView.tsx";

const TIER_META: Record<PedagogicalTier, { label: string; badgeCls: string; borderCls: string }> = {
  "unconditional-truth": {
    label: "Verità incondizionata",
    badgeCls: "bg-[var(--truth)]/15 text-[var(--truth)] border border-[var(--truth)]/30",
    borderCls: "border-l-4 border-l-[var(--truth)]",
  },
  axiom: {
    label: "Assioma",
    badgeCls: "bg-[var(--truth)]/15 text-[var(--truth)] border border-[var(--truth)]/30",
    borderCls: "border-l-4 border-l-[var(--truth)]",
  },
  derivation: {
    label: "Derivazione",
    badgeCls: "bg-[var(--derive)]/15 text-[var(--derive)] border border-[var(--derive)]/30",
    borderCls: "border-l-4 border-l-[var(--derive)]",
  },
};

function SintesiNodeCard({ node }: { node: SintesiNode }) {
  const meta = TIER_META[node.tier] ?? TIER_META["unconditional-truth"];
  const numStr = String(node.number).padStart(2, "0");

  return (
    <article className={`rounded-lg border border-[var(--border)] bg-[var(--card)] p-4 shadow-xs ${meta.borderCls}`}>
      <header className="mb-2 flex items-start justify-between gap-3">
        <div className="flex flex-col gap-0.5">
          <span className="font-mono text-xs tracking-wider text-[var(--muted-foreground)] uppercase">
            Nodo {numStr}
          </span>
          <h3 className="font-ui text-base font-medium text-[var(--foreground)]">{node.title}</h3>
        </div>
        <span className={`shrink-0 rounded px-2 py-0.5 text-xs font-semibold ${meta.badgeCls}`}>
          {meta.label}
        </span>
      </header>

      <div className="text-[15px] leading-relaxed text-[var(--foreground)]/90">
        <Rich source={node.summary} />
      </div>

      {node.keyFormula && (
        <div className="mt-3 rounded border border-[var(--hairline)] bg-[var(--background)] px-3 py-2 text-center">
          <Rich source={node.keyFormula.startsWith("$$") ? node.keyFormula : `$$ ${node.keyFormula} $$`} />
        </div>
      )}

      {node.keyCode && (
        <div className="mt-3 overflow-hidden rounded border border-[var(--hairline)] bg-[var(--background)]">
          <div className="px-3 py-1 text-xs text-[var(--muted-foreground)] border-b border-[var(--hairline)]">
            {node.keyCode.language ?? "Codice"}
          </div>
          <pre className="overflow-x-auto p-3 font-mono text-xs text-[var(--foreground)]">
            <code>{node.keyCode.code}</code>
          </pre>
          {node.keyCode.caption && (
            <p className="px-3 py-1 text-xs text-[var(--muted-foreground)] bg-[var(--muted)]/30">
              {node.keyCode.caption}
            </p>
          )}
        </div>
      )}
    </article>
  );
}

export function ReferenceView({ doc }: { doc: SintesiDocument }) {
  const truths = doc.nodes.filter((n: SintesiNode) => n.tier === "unconditional-truth" || n.tier === "axiom");
  const derivations = doc.nodes.filter((n: SintesiNode) => n.tier === "derivation");

  return (
    <div className="space-y-6">
      {/* Header */}
      <header className="border-b border-[var(--border)] pb-4">
        <div className="flex flex-wrap items-center gap-2">
          <span className="rounded bg-[var(--accent)] px-2 py-0.5 text-xs font-medium text-[var(--foreground)]">
            Sintesi
          </span>
          {doc.topic && (
            <span className="text-xs text-[var(--muted-foreground)]">
              &bull; {doc.topic}
            </span>
          )}
        </div>
        <h1 className="mt-2 text-2xl font-bold tracking-tight text-[var(--foreground)]">
          {doc.title}
        </h1>
        <p className="mt-1 text-sm text-[var(--muted-foreground)]">
          Sintesi rapida per il ripasso: concetti chiave, derivazioni, grafo ed errori emersi.
        </p>

        <div className="mt-4 flex flex-wrap gap-4 text-xs text-[var(--muted-foreground)]">
          <span className="inline-flex items-center gap-1">
            <CheckCircle2 size={13} className="text-[var(--truth)]" />
            <strong className="text-[var(--foreground)]">{truths.length}</strong> verità / assiomi
          </span>
          <span className="inline-flex items-center gap-1">
            <Sparkles size={13} className="text-[var(--derive)]" />
            <strong className="text-[var(--foreground)]">{derivations.length}</strong> derivazioni
          </span>
          {doc.formulas.length > 0 && (
            <span className="inline-flex items-center gap-1">
              <Calculator size={13} className="text-[var(--brand)]" />
              <strong className="text-[var(--foreground)]">{doc.formulas.length}</strong> formule
            </span>
          )}
        </div>
      </header>

      {/* Conceptual Nodes */}
      <section aria-labelledby="section-nodes" className="space-y-3">
        <h2 id="section-nodes" className="text-sm font-semibold tracking-wide text-[var(--foreground)] uppercase">
          Nodi concettuali ({doc.nodes.length})
        </h2>
        <div className="space-y-3">
          {doc.nodes.map((node: SintesiNode) => (
            <SintesiNodeCard key={node.nodeId || String(node.number)} node={node} />
          ))}
        </div>
      </section>

      {/* Dependency DAG */}
      {doc.initialDag && (
        <section aria-labelledby="section-dag" className="space-y-2">
          <h2 id="section-dag" className="flex items-center gap-2 text-sm font-semibold tracking-wide text-[var(--foreground)] uppercase">
            <GitFork size={15} /> Grafo delle dipendenze
          </h2>
          <div className="overflow-hidden rounded-lg border border-[var(--border)] bg-[var(--card)] p-4">
            <Mermaid source={doc.initialDag} />
          </div>
        </section>
      )}

      {/* Misconceptions from Learning Record */}
      {doc.misconceptions.length > 0 && (
        <section aria-labelledby="section-misconceptions" className="space-y-2">
          <h2 id="section-misconceptions" className="flex items-center gap-2 text-sm font-semibold tracking-wide text-[var(--warn)] uppercase">
            <AlertTriangle size={15} /> Errori e dubbi emersi nella sessione ({doc.misconceptions.length})
          </h2>
          <div className="rounded-lg border-l-4 border-l-[var(--warn)] border border-[var(--border)] bg-[var(--card)] p-4 space-y-3">
            <ul className="list-disc pl-5 space-y-1.5 text-xs text-[var(--foreground)]/90">
              {doc.misconceptions.map((m: SintesiMisconception, idx: number) => (
                <li key={idx}>
                  {m.sourceNode && <strong className="text-[var(--foreground)]">{m.sourceNode}: </strong>}
                  <span>{m.description}</span>
                </li>
              ))}
            </ul>
          </div>
        </section>
      )}

      {/* Formula Inventory */}
      {doc.formulas.length > 0 && (
        <section aria-labelledby="section-formulas" className="space-y-2">
          <h2 id="section-formulas" className="flex items-center gap-2 text-sm font-semibold tracking-wide text-[var(--foreground)] uppercase">
            <Calculator size={15} /> Inventario formule ({doc.formulas.length})
          </h2>
          <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
            {doc.formulas.map((f: { math: string; sourceNode: string }, idx: number) => (
              <div key={idx} className="rounded-lg border border-[var(--border)] bg-[var(--card)] p-3 text-center">
                <Rich source={f.math.startsWith("$$") ? f.math : `$$ ${f.math} $$`} />
                <span className="mt-1 block text-xs text-[var(--muted-foreground)]">
                  Fonte: {f.sourceNode}
                </span>
              </div>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
