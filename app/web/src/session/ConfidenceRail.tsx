import { useRef, type KeyboardEvent } from "react";
import { CONFIDENCE_COLORS } from "../theme-colors.ts";
import { useMode, type Mode } from "../theme.ts";
// The six-stop confidence rail (0/30/50/70/90/100): red to green, ticks on the empty side. Look from LearnHarness, re-authored.
export const CONFIDENCE_STOPS = [0, 30, 50, 70, 90, 100] as const;

export const confidenceLabel = (c: number) =>
  c <= 0 ? "tiro a caso" : c <= 30 ? "poco sicuro" : c <= 50 ? "incerto" : c <= 70 ? "abbastanza sicuro" : c <= 90 ? "molto sicuro" : "certo";

function railStyle(value: number, mode: Mode) {
  const stops = CONFIDENCE_COLORS[mode];
  const color = stops[CONFIDENCE_STOPS.indexOf(value as (typeof CONFIDENCE_STOPS)[number])] ?? stops[2];
  const ticks = [30, 50, 70, 90];
  return {
    backgroundColor: "var(--background)",
    backgroundImage: [`linear-gradient(90deg, ${color} ${value}%, transparent ${value}%)`, ...ticks.map(() => "linear-gradient(color-mix(in srgb, var(--foreground) 30%, transparent), color-mix(in srgb, var(--foreground) 30%, transparent))")].join(","),
    backgroundSize: ["100% 100%", ...ticks.map(() => "1px 100%")].join(","),
    backgroundPosition: ["0 0", ...ticks.map((p) => `${p}% 0`)].join(","),
    backgroundRepeat: "no-repeat",
  };
}

/** Static rail (answered card). */
export function ConfidenceRail({ value }: { value: number }) {
  const mode = useMode();
  return <div aria-hidden className="h-1.5 w-full rounded-full border border-border" style={railStyle(value, mode)} />;
}

/** Interactive rail: a radiogroup with roving tabindex; arrow keys work only while a stop has focus. */
export function ConfidencePicker({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  const mode = useMode();
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const d = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    const i = Math.min(5, Math.max(0, CONFIDENCE_STOPS.indexOf(value as (typeof CONFIDENCE_STOPS)[number]) + d));
    onChange(CONFIDENCE_STOPS[i]);
    refs.current[i]?.focus({ preventScroll: true });
  };
  return (
    <div role="radiogroup" aria-label="Quanto sei sicuro?" className="min-w-0" onKeyDown={onKeyDown}>
      <div className="mb-1.5 flex items-baseline justify-between text-[11px] text-muted-foreground">
        <span>Sicurezza</span>
        <span>
          {value}% · {confidenceLabel(value)}
        </span>
      </div>
      <div className="h-1.5 w-full rounded-full border border-border" style={railStyle(value, mode)} aria-hidden />
      <div className="mt-1 grid grid-cols-6">
        {CONFIDENCE_STOPS.map((s, i) => (
          <button
            key={s}
            ref={(el) => {
              refs.current[i] = el;
            }}
            tabIndex={value === s ? 0 : -1}
            type="button"
            role="radio"
            aria-checked={value === s}
            onClick={() => onChange(s)}
            className={`py-0.5 text-[11px] hover:text-foreground ${value === s ? "text-foreground" : "text-muted-foreground"}`}
          >
            {s}
          </button>
        ))}
      </div>
    </div>
  );
}
