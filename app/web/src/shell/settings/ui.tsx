// Building blocks shared by the settings sections (see SettingsDialog.tsx).
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { LearnerSettings } from "../../../../shared/protocol.ts";
import { PALETTE_LABEL, PALETTES, type Mode, type Palette } from "../../palettes.ts";

export type Save = (patch: Partial<LearnerSettings>) => Promise<void>;

export const ROW = "flex items-center justify-between gap-4 py-2.5";
/** A full-width row whose control sits under its label (card grids need the room). */
export const STACK = "space-y-2 py-2.5";
export const SELECT = "max-w-[15rem] rounded-md border border-border bg-background px-2 py-1 text-xs";
/** A native checkbox drawn as a switch: label, keyboard and screen readers keep working. */
export const SWITCH =
  "relative h-[18px] w-8 shrink-0 appearance-none rounded-full bg-border transition-colors checked:bg-brand before:absolute before:left-0.5 before:top-0.5 before:size-3.5 before:rounded-full before:bg-card before:shadow before:transition-transform checked:before:translate-x-3.5";

export function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h3 className="mb-1.5 px-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">{title}</h3>
      <div className="divide-y divide-hairline rounded-lg border border-hairline bg-background/60 px-3">{children}</div>
    </section>
  );
}

export function Label({ title, hint }: { title: string; hint?: string }) {
  return (
    <span className="min-w-0">
      <span className="text-foreground">{title}</span>
      {hint && <span className="block text-[11px] text-muted-foreground">{hint}</span>}
    </span>
  );
}

/** Segmented control: a radio group of buttons. */
export function Segmented<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: [T, string, LucideIcon?][]; onChange(v: T): void }) {
  return (
    <div role="radiogroup" aria-label={label} className="flex shrink-0 rounded-md border border-border bg-background p-0.5">
      {options.map(([v, text, Icon]) => (
        <button
          key={v}
          type="button"
          role="radio"
          aria-checked={value === v}
          onClick={() => onChange(v)}
          className={`flex items-center gap-1.5 rounded px-2.5 py-1 text-xs transition-colors ${value === v ? "bg-accent text-heading shadow-sm" : "text-muted-foreground hover:text-foreground"}`}
        >
          {Icon && <Icon size={12} />}
          {text}
        </button>
      ))}
    </div>
  );
}

const CARD = "flex flex-col items-center gap-1.5 rounded-lg border px-3 py-2.5 text-xs transition-colors";
const cardClass = (selected: boolean) =>
  `${CARD} ${selected ? "border-primary bg-accent text-heading" : "border-border text-muted-foreground hover:text-foreground"}`;

/** Cards: a radio group of large buttons, each with an icon above its name. */
export function ChoiceCards<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: [T, string, LucideIcon][]; onChange(v: T): void }) {
  return (
    <div role="radiogroup" aria-label={label} className="grid grid-cols-3 gap-2">
      {options.map(([v, text, Icon]) => (
        <button key={v} type="button" role="radio" aria-checked={value === v} onClick={() => onChange(v)} className={cardClass(value === v)}>
          <Icon size={14} />
          {text}
        </button>
      ))}
    </div>
  );
}

/** One half of a palette orb: the data attributes make styles.css's token blocks colour it; nothing is duplicated in TS. */
function Orb({ p, m }: { p: Palette; m: Mode }) {
  return (
    <span data-palette={p} data-theme={m} className="relative inline-block size-5 rounded-full border border-border bg-background">
      <span className="absolute inset-[5px] rounded-full bg-primary" />
    </span>
  );
}

/** Palette cards: each shows a light and a dark orb in its own palette, plus its name. */
export function PaletteCards({ value, onChange }: { value: Palette; onChange(p: Palette): void }) {
  return (
    <div role="radiogroup" aria-label="Temi" className="grid grid-cols-3 gap-2">
      {PALETTES.map((p) => (
        <button key={p} type="button" role="radio" aria-checked={value === p} onClick={() => onChange(p)} className={cardClass(value === p)}>
          <span className="flex gap-1" aria-hidden>
            {(["light", "dark"] as const).map((m) => (
              <Orb key={m} p={p} m={m} />
            ))}
          </span>
          {PALETTE_LABEL[p]}
        </button>
      ))}
    </div>
  );
}
