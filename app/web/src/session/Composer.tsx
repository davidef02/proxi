import { useEffect, useRef, useState, type DragEvent, type KeyboardEvent } from "react";
import { ArrowUp, FileImage, FileText, Paperclip, RotateCcw, Square, X } from "lucide-react";
import { api } from "../api.ts";
import type { AttachmentMeta, SessionStatus } from "../../../shared/protocol.ts";
import { useDictation } from "./useDictation.ts";
import { DictationButton } from "./DictationButton.tsx";

const PLACEHOLDER: Record<SessionStatus, string> = {
  starting: "Il docente si sta avviando…",
  running: "Il docente sta scrivendo…",
  waiting: "Rispondi alla domanda",
  idle: "Scrivi al docente",
  error: "Invia un messaggio per riprendere",
  closed: "La sessione è chiusa",
};

const ALLOWED_EXTS = /\.(png|jpe?g|webp|gif|svg|pdf)$/i;

function isAcceptedFile(f: File): boolean {
  return f.type.startsWith("image/") || f.type === "application/pdf" || ALLOWED_EXTS.test(f.name);
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function Composer({
  sessionId,
  status,
  placeholder,
  retryable,
}: {
  sessionId: string;
  status: SessionStatus;
  placeholder?: string;
  retryable?: boolean;
}) {
  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<File[]>([]);
  const [isDragging, setIsDragging] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const dragDepthRef = useRef(0);

  const enabled = (status === "idle" || status === "error") && !busy;
  const working = status === "running" || status === "starting";
  const hasContent = !!text.trim() || attachments.length > 0;

  const retry = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.retry(sessionId);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const dictation = useDictation({
    onTranscribed: (speech) => {
      setText((cur) => (cur.trim() ? cur.trim() + " " + speech : speech));
      ref.current?.focus();
    },
    onError: (err) => setError(err),
  });

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 200) + "px";
  }, [text]);

  const addFiles = (files: FileList | File[]) => {
    const valid = Array.from(files).filter(isAcceptedFile);
    if (valid.length === 0) return;
    setAttachments((prev) => [...prev, ...valid]);
    ref.current?.focus();
  };

  const removeAttachment = (index: number) => {
    setAttachments((prev) => prev.filter((_, i) => i !== index));
  };

  const send = async () => {
    const t = text.trim();
    if (!hasContent || !enabled) return;
    setBusy(true);
    setError(null);
    try {
      let uploadedMetas: AttachmentMeta[] | undefined;
      if (attachments.length > 0) {
        uploadedMetas = await api.uploadAttachments(sessionId, attachments);
      }
      const msgText = t || (uploadedMetas && uploadedMetas.length === 1 ? `Ho allegato ${uploadedMetas[0].name}` : "Ho allegato dei file");
      await api.message(sessionId, msgText, uploadedMetas);
      setText("");
      setAttachments([]);
    } catch (e) {
      setError((e as Error).message);
    }
    setBusy(false);
  };

  useEffect(() => {
    const onGlobalKey = (e: globalThis.KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === "d" || e.key === "D")) {
        const target = e.target as HTMLElement | null;
        if (target?.closest?.("dialog,[role=dialog]")) return;
        if (!enabled && !dictation.isRecording) return;
        e.preventDefault();
        dictation.toggle();
        return;
      }
      if (e.key === "Escape" && dictation.isRecording) {
        e.preventDefault();
        dictation.cancel();
      }
    };
    window.addEventListener("keydown", onGlobalKey);
    return () => window.removeEventListener("keydown", onGlobalKey);
  }, [enabled, dictation]);

  const onKeyDown = (e: KeyboardEvent) => {
    if ((e.ctrlKey || e.metaKey) && (e.key === "d" || e.key === "D")) {
      e.preventDefault();
      dictation.toggle();
      return;
    }
    if (e.key === "Escape" && dictation.isRecording) {
      e.preventDefault();
      dictation.cancel();
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    }
  };

  const onPaste = (e: React.ClipboardEvent) => {
    if (!enabled) return;
    const files = e.clipboardData?.files;
    if (files && files.length > 0) {
      const valid = Array.from(files).filter(isAcceptedFile);
      if (valid.length > 0) {
        e.preventDefault();
        addFiles(valid);
      }
    }
  };

  const onDragEnter = (e: DragEvent) => {
    if (!enabled) return;
    if (Array.from(e.dataTransfer.types).includes("Files")) {
      e.preventDefault();
      dragDepthRef.current++;
      setIsDragging(true);
    }
  };

  const onDragOver = (e: DragEvent) => {
    if (!enabled) return;
    if (Array.from(e.dataTransfer.types).includes("Files")) {
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
    }
  };

  const onDragLeave = (e: DragEvent) => {
    if (!enabled) return;
    if (Array.from(e.dataTransfer.types).includes("Files")) {
      e.preventDefault();
      dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
      if (dragDepthRef.current === 0) setIsDragging(false);
    }
  };

  const onDrop = (e: DragEvent) => {
    if (!enabled) return;
    if (Array.from(e.dataTransfer.types).includes("Files")) {
      e.preventDefault();
      dragDepthRef.current = 0;
      setIsDragging(false);
      if (e.dataTransfer.files) addFiles(e.dataTransfer.files);
    }
  };

  return (
    <div
      className="relative mx-auto w-full max-w-[46rem]"
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      {error && (
        <p role="alert" className="mb-2 text-xs text-bad">
          Messaggio non inviato: {error}
        </p>
      )}

      <div
        className={`relative flex flex-col rounded-xl border bg-card px-3 py-2 transition-colors focus-within:border-muted-foreground ${
          isDragging ? "border-dashed border-primary bg-primary/5" : "border-border"
        }`}
      >
        {isDragging && (
          <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center rounded-xl bg-card/90 backdrop-blur-xs">
            <div className="flex items-center gap-2 text-sm font-medium text-primary">
              <Paperclip className="h-4 w-4" />
              <span>Rilascia qui immagini o PDF da allegare</span>
            </div>
          </div>
        )}

        {retryable && (
          <div className="mb-2 flex items-center justify-between gap-2 rounded-md border border-bad/30 bg-bad/10 px-3 py-1.5 text-xs text-bad">
            <span className="truncate">Il turno precedente non è andato a buon fine.</span>
            <button
              type="button"
              data-testid="composer-retry-button"
              disabled={busy}
              onClick={() => void retry()}
              className="inline-flex shrink-0 items-center gap-1.5 rounded border border-bad/40 bg-background px-2.5 py-1 text-xs font-medium text-foreground hover:bg-accent transition-colors"
            >
              <RotateCcw className="h-3 w-3" />
              Riprova
            </button>
          </div>
        )}

        {attachments.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-1.5 border-b border-border/60 pb-2">
            {attachments.map((file, idx) => {
              const isImg = file.type.startsWith("image/") || /\.(png|jpe?g|webp|gif|svg)$/i.test(file.name);
              const Icon = isImg ? FileImage : FileText;
              return (
                <span
                  key={`${file.name}-${idx}`}
                  className="inline-flex items-center gap-1.5 rounded-md border border-border bg-muted/50 px-2 py-0.5 text-xs text-foreground"
                >
                  <Icon className="h-3.5 w-3.5 text-muted-foreground" />
                  <span className="max-w-[12rem] truncate font-medium">{file.name}</span>
                  <span className="text-[10px] text-muted-foreground">{formatBytes(file.size)}</span>
                  <button
                    type="button"
                    title={`Rimuovi ${file.name}`}
                    aria-label={`Rimuovi ${file.name}`}
                    onClick={() => removeAttachment(idx)}
                    disabled={!enabled}
                    className="ml-0.5 rounded-xs text-muted-foreground hover:bg-accent hover:text-foreground"
                  >
                    <X className="h-3 w-3" />
                  </button>
                </span>
              );
            })}
          </div>
        )}

        <div className="flex items-end gap-2">
          <label className="relative min-w-0 flex-1">
            <span className="sr-only">Messaggio per il docente</span>
            <textarea
              ref={ref}
              rows={1}
              value={text}
              disabled={!enabled}
              placeholder={status === "idle" && placeholder ? placeholder : PLACEHOLDER[status]}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={onKeyDown}
              onPaste={onPaste}
              className="block w-full resize-none bg-transparent py-1 font-prose text-[15px] text-foreground outline-none placeholder:text-muted-foreground disabled:opacity-60"
            />
          </label>

          <input
            ref={fileInputRef}
            type="file"
            multiple
            accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml,application/pdf"
            className="hidden"
            onChange={(e) => {
              if (e.target.files) addFiles(e.target.files);
              e.target.value = "";
            }}
          />

          {enabled && (
            <button
              type="button"
              title="Allega immagini o PDF"
              aria-label="Allega file"
              disabled={!enabled}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-30"
              onClick={() => fileInputRef.current?.click()}
            >
              <Paperclip className="h-3.5 w-3.5" />
            </button>
          )}

          {enabled && <DictationButton dictation={dictation} disabled={!enabled} />}

          {working || status === "waiting" ? (
            <button
              type="button"
              title={status === "waiting" ? "Annulla la domanda" : "Interrompi"}
              aria-label={status === "waiting" ? "Annulla la domanda" : "Interrompi"}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-border hover:bg-accent"
              onClick={() => void api.interrupt(sessionId)}
            >
              <Square className="h-3 w-3 fill-current" />
            </button>
          ) : (
            <button
              type="button"
              title="Invia (Invio)"
              aria-label={status === "error" ? "Invia per riprendere" : "Invia"}
              disabled={!enabled || !hasContent}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-primary text-primary-foreground disabled:opacity-30"
              onClick={() => void send()}
            >
              <ArrowUp className="h-4 w-4" />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
