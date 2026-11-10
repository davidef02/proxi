import { useEffect, useRef, useState, type ReactNode } from "react";
import type { Decision } from "../../../shared/protocol.ts";

export const btn =
  "inline-flex items-center justify-center gap-1.5 rounded-md border border-border bg-transparent px-2.5 py-1 text-xs text-foreground hover:bg-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary disabled:cursor-not-allowed disabled:opacity-40";
export const btnPrimary =
  "inline-flex items-center justify-center gap-1.5 rounded-md bg-primary px-3 py-1 text-xs font-medium text-primary-foreground hover:opacity-90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary disabled:cursor-not-allowed disabled:opacity-40";
export const field =
  "w-full resize-none rounded-md border border-border bg-background px-3 py-2 font-prose text-[15px] text-foreground placeholder:text-muted-foreground focus-visible:outline-1 focus-visible:outline-offset-0 focus-visible:outline-primary";

export function Card({ title, tone = "default", className = "", children }: { title?: ReactNode; tone?: "default" | "muted"; className?: string; children: ReactNode }) {
  return (
    <section className={`relative min-w-0 break-words rounded-lg border border-hairline p-4 ${tone === "muted" ? "text-muted-foreground" : "bg-card"} ${className}`}>
      {title && <h3 className="mb-2 text-[11px] uppercase tracking-wider text-muted-foreground">{title}</h3>}
      {children}
    </section>
  );
}

export function DecisionBadge({ decision }: { decision: Decision }) {
  return (
    <p className={`mt-3 text-xs ${decision.approved ? "text-good" : "text-warn"}`}>
      {decision.approved ? "Approvato" : "Modifiche richieste"}
      {decision.feedback && <span className="font-prose text-sm text-muted-foreground">: {decision.feedback}</span>}
    </p>
  );
}

/** Runs an async submit, tracking busy/error. On success the server upserts the item with its result and the dock
 *  unmounts (a new requestId remounts it, clearing busy). If that confirmation never arrives, re-enable after 8 s with a quiet hint. */
export function useSubmit() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const run = async (fn: () => Promise<unknown>) => {
    clearTimeout(timer.current);
    setBusy(true);
    setError(null);
    try {
      await fn();
      timer.current = setTimeout(() => {
        setBusy(false);
        setError("nessuna conferma dal docente, riprova");
      }, 8000);
    } catch (e) {
      setError((e as Error).message);
      setBusy(false);
    }
  };
  return { busy, error, run };
}

export function ErrorLine({ error }: { error: string | null }) {
  return error ? (
    <p role="alert" className="mt-2 text-xs text-bad">
      Invio non riuscito: {error}
    </p>
  ) : null;
}

/** Long text collapsed to a few lines with a "Mostra tutto" toggle. */
export function Collapsible({ children }: { children: ReactNode }) {
  const [all, setAll] = useState(false);
  return (
    <div>
      <div className={all ? "" : "max-h-40 overflow-hidden [mask-image:linear-gradient(to_bottom,black_70%,transparent)]"}>{children}</div>
      <button type="button" aria-expanded={all} className="mt-1 text-xs text-muted-foreground underline hover:text-foreground" onClick={() => setAll((a) => !a)}>
        {all ? "Mostra meno" : "Mostra tutto"}
      </button>
    </div>
  );
}

/** True when a key event comes from somewhere the learner is typing. */
export const isTyping = (t: EventTarget | null) => {
  const el = t as HTMLElement | null;
  return !!el && (el.tagName === "TEXTAREA" || el.tagName === "INPUT" || el.isContentEditable);
};
