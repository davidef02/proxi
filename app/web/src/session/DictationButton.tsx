import { Loader2, Mic, Square } from "lucide-react";
import type { useDictation } from "./useDictation.ts";

export function DictationButton({
  dictation,
  disabled,
  compact = false,
  className = "",
}: {
  dictation: ReturnType<typeof useDictation>;
  disabled?: boolean;
  compact?: boolean;
  className?: string;
}) {
  const { isRecording, isTranscribing, toggle, cancel } = dictation;

  if (isTranscribing) {
    return (
      <div
        className={`flex items-center gap-1.5 rounded-md border border-hairline bg-accent/40 px-2 py-1 text-xs text-muted-foreground ${className}`}
        role="status"
        aria-live="polite"
      >
        <Loader2 size={13} className="animate-spin text-primary" />
        {!compact && <span className="text-[11px]">Trascrizione…</span>}
      </div>
    );
  }

  if (isRecording) {
    return (
      <div className={`flex items-center gap-1.5 ${className}`}>
        <button
          type="button"
          onClick={toggle}
          title="Ferma dettatura (Ctrl+D)"
          aria-label="Ferma registrazione"
          className="flex items-center gap-1.5 rounded-md bg-bad/15 px-2 py-1 text-xs font-medium text-bad hover:bg-bad/25 transition-colors"
        >
          <span className="relative flex h-2 w-2">
            <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-bad opacity-75" />
            <span className="relative inline-flex h-2 w-2 rounded-full bg-bad" />
          </span>
          <Square size={10} className="fill-current" />
          {!compact && <span>Parla…</span>}
        </button>
        <button
          type="button"
          onClick={cancel}
          title="Annulla dettatura (Esc)"
          aria-label="Annulla registrazione"
          className="text-[11px] text-muted-foreground hover:text-foreground hover:underline px-1"
        >
          Esc
        </button>
      </div>
    );
  }

  return (
    <button
      type="button"
      onClick={toggle}
      disabled={disabled}
      title="Dettatura vocale (Ctrl+D)"
      aria-label="Dettatura vocale (Ctrl+D)"
      className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-transparent text-muted-foreground hover:border-border hover:bg-accent hover:text-foreground disabled:opacity-30 transition-colors ${className}`}
    >
      <Mic size={14} />
    </button>
  );
}
