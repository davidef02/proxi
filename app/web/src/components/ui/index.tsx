import type { ButtonHTMLAttributes, ReactNode } from "react";

type Variant = "primary" | "secondary" | "ghost";
const VARIANTS: Record<Variant, string> = {
  primary: "bg-primary text-primary-foreground hover:opacity-90",
  secondary: "border border-border bg-card text-foreground hover:bg-muted",
  ghost: "text-foreground hover:bg-muted",
};

export function Button({ variant = "secondary", className = "", ...rest }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return (
    <button
      type="button"
      {...rest}
      className={`inline-flex items-center justify-center gap-1.5 rounded-lg px-3.5 py-1.5 font-sans text-sm font-medium transition disabled:cursor-not-allowed disabled:opacity-50 ${VARIANTS[variant]} ${className}`}
    />
  );
}

type Tone = "neutral" | "good" | "warn" | "bad" | "info";
const TONES: Record<Tone, string> = {
  neutral: "bg-muted text-muted-foreground",
  good: "bg-good/15 text-good",
  warn: "bg-warn/20 text-foreground",
  bad: "bg-bad/15 text-bad",
  info: "bg-info/15 text-info",
};

export function Badge({ tone = "neutral", children }: { tone?: Tone; children: ReactNode }) {
  return <span className={`inline-flex items-center rounded-full px-2 py-0.5 font-sans text-xs font-medium ${TONES[tone]}`}>{children}</span>;
}

export function Spinner({ label = "Caricamento…" }: { label?: string }) {
  return (
    <span role="status" className="inline-flex items-center gap-2 font-sans text-sm text-muted-foreground">
      <span className="size-4 animate-spin rounded-full border-2 border-border border-t-primary" />
      {label}
    </span>
  );
}

export function ErrorNote({ children }: { children: ReactNode }) {
  return <p role="alert" className="rounded-lg border border-bad/40 bg-bad/10 px-3 py-2 font-sans text-sm text-bad">{children}</p>;
}
