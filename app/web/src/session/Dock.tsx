// The bottom dock. While an interactive item is pending it replaces the composer (quiz / ask / plan / curriculum);
// once answered, the item becomes a static card in the transcript. Interaction model from LearnHarness's question dock, re-authored.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronRight, Play } from "lucide-react";
import { api } from "../api.ts";
import type { AskAnswer, ChatItem, Decision, InteractiveItem, QuizAnswer, SessionStatus } from "../../../shared/protocol.ts";
import { Composer } from "./Composer.tsx";
import { ConfidencePicker } from "./ConfidenceRail.tsx";
import { Markdown } from "./Markdown.tsx";
import { askOptions, type AskItem } from "./cards/AskCard.tsx";
import { KIND_LABEL, LETTERS, quizOptions, type QuizItem } from "./cards/QuizCard.tsx";
import { ErrorLine, btn, btnPrimary, field, isTyping, useSubmit } from "./ui.tsx";
import { useDictation } from "./useDictation.ts";
import { DictationButton } from "./DictationButton.tsx";
import { useDockHeight } from "./useDockHeight.ts";

export { openProblem, pendingItem } from "./pending.ts";

/** Body scrolls; the footer (hint + submit) stays outside the scroll area so it is always reachable. */
function Shell({ label, footer, children }: { label: string; footer: ReactNode; children: ReactNode }) {
  const rootRef = useRef<HTMLElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const footerRef = useRef<HTMLDivElement>(null);
  const budget = useDockHeight(rootRef, bodyRef, footerRef);

  return (
    <section
      ref={rootRef}
      aria-label={label}
      data-dock
      style={{
        maxHeight: budget ? `${budget.shellMaxHeight}px` : undefined,
      }}
      className={`relative mx-auto flex max-h-[var(--dock-max-h,52vh)] w-full max-w-[46rem] flex-col rounded-xl border border-border bg-card shadow-[var(--shadow)] ${
        budget?.scrollWholeContent ? "overflow-y-auto" : ""
      }`}
    >
      <div
        ref={bodyRef}
        data-dock-scroll-area
        style={{
          maxHeight: budget && !budget.scrollWholeContent ? `${budget.bodyMaxHeight}px` : undefined,
        }}
        className={`relative min-h-0 flex-1 px-4 pt-4 pb-2 ${
          budget?.scrollWholeContent ? "" : "overflow-y-auto"
        }`}
      >
        {children}
      </div>
      <div ref={footerRef} className="shrink-0 border-t border-hairline px-4 py-2.5">
        {footer}
      </div>
    </section>
  );
}

/** Document-level keys while a dock is on screen. Re-bound every render, so handlers always see fresh state. */
function useDockKeys(onKey: (e: KeyboardEvent) => void) {
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.altKey || e.isComposing || e.repeat) return;
      if ((e.ctrlKey || e.metaKey) && e.key !== "Enter" && e.key !== "d" && e.key !== "D") return;
      const t = e.target as HTMLElement | null;
      // Only act from inside the dock or with nothing focused; never from the sidebar, dialogs or other widgets.
      if (!t || !(t === document.body || t.closest?.("[data-dock]")) || t.closest?.("dialog,[role=dialog]")) return;
      // Enter on a focused button / link / select keeps its native meaning.
      if (e.key === "Enter" && t.closest("button,a,select,summary")) return;
      onKey(e);
    };
    document.addEventListener("keydown", h);
    return () => document.removeEventListener("keydown", h);
  });
}

const Row = ({ n, on, onClick, children }: { n: string; on: boolean; onClick: () => void; children: ReactNode }) => (
  <button
    type="button"
    aria-pressed={on}
    onClick={onClick}
    className={`relative flex w-full items-start gap-3 rounded-md border px-3 py-2 text-left ${on ? "border-primary bg-accent" : "border-hairline hover:border-border hover:bg-accent/60"}`}
  >
    <span className={`mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded border text-[11px] ${on ? "border-primary bg-primary text-primary-foreground" : "border-border text-muted-foreground"}`} aria-hidden>
      {n}
    </span>
    <span className="min-w-0 flex-1">{children}</span>
  </button>
);

function QuizDock({ item, sessionId }: { item: QuizItem; sessionId: string }) {
  const { input } = item;
  const isCheck = input.kind === "check";
  const [revealed, setRevealed] = useState(!isCheck);
  const [readingThinking, setReadingThinking] = useState("");
  const options = quizOptions(item);
  const multi = !!input.multiSelect;
  const [selected, setSelected] = useState<string[]>([]);
  const [dontKnow, setDontKnow] = useState(false);
  const [conf, setConf] = useState(50);
  const [note, setNote] = useState("");
  const { busy, error, run } = useSubmit();
  const canSubmit = !busy && (dontKnow || selected.length > 0);

  const revealedRef = useRef(revealed);
  revealedRef.current = revealed;

  const dictation = useDictation({
    onTranscribed: (speech) => {
      if (!revealedRef.current) {
        setReadingThinking((prev) => (prev.trim() ? prev.trim() + " " + speech : speech));
      } else {
        setNote((prev) => (prev.trim() ? prev.trim() + " " + speech : speech));
      }
    },
  });

  const toggle = (v: string) => {
    setDontKnow(false);
    setSelected((s) => (multi ? (s.includes(v) ? s.filter((x) => x !== v) : [...s, v]) : [v]));
  };
  const noIdea = () => {
    setDontKnow((d) => !d);
    setSelected([]);
  };
  const submit = () => {
    if (!canSubmit) return;
    const answer: QuizAnswer = {
      selected: dontKnow ? [] : selected,
      dontKnow,
      confidence: conf,
      ...(note.trim() ? { note: note.trim() } : {}),
      ...(readingThinking.trim() ? { thinkingWhileReading: readingThinking.trim() } : {}),
    };
    void run(() => api.answer(sessionId, { requestId: item.requestId, kind: "quiz", answer }));
  };
  useDockKeys((e) => {
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
    if (!revealed) {
      if (e.key === "Escape") {
        if (isTyping(e.target)) (e.target as HTMLElement).blur();
        return;
      }
      if (e.key === "Enter") {
        if (e.shiftKey && !e.ctrlKey && !e.metaKey) return;
        e.preventDefault();
        setRevealed(true);
      }
      return;
    }
    if (e.key === "Enter") {
      if (e.shiftKey) return;
      e.preventDefault();
      submit();
      return;
    }
    if (isTyping(e.target)) {
      if (e.key === "Escape") (e.target as HTMLElement).blur();
      return;
    }
    const n = Number(e.key);
    if (e.key === "0") noIdea();
    else if (n >= 1 && n <= options.length) toggle(options[n - 1].value);
    else return;
    e.preventDefault();
  });

  if (isCheck && !revealed) {
    return (
      <section data-dock aria-label="Verifica in attesa" className="mx-auto w-full max-w-[46rem] space-y-2.5">
        <div data-testid="reading-thinking-field" className="rounded-xl border border-hairline bg-card/60 p-3 shadow-sm">
          <div className="mb-1.5 flex items-center justify-between">
            <label htmlFor={`reading-thinking-${item.requestId}`} className="block text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
              Cosa stavi pensando leggendo?
            </label>
            <DictationButton dictation={dictation} compact />
          </div>
          <textarea
            id={`reading-thinking-${item.requestId}`}
            className={`${field} min-h-[3.5rem] resize-y`}
            rows={2}
            value={readingThinking}
            placeholder="Qualsiasi riflessione o dubbio mentre leggi la spiegazione..."
            aria-label="Cosa stavi pensando leggendo?"
            onChange={(e) => setReadingThinking(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (!e.shiftKey || e.ctrlKey || e.metaKey)) {
                e.preventDefault();
                setRevealed(true);
              }
            }}
          />
        </div>
        <button
          type="button"
          data-testid="teach-check-reveal"
          onClick={() => setRevealed(true)}
          className="flex w-full items-center justify-between gap-3 rounded-xl border border-border bg-card p-3.5 text-left shadow-[var(--shadow)] transition hover:border-primary/50 hover:bg-accent/40 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
        >
          <div className="flex min-w-0 items-center gap-3">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
              <Play className="h-4 w-4 fill-primary" aria-hidden />
            </div>
            <div className="min-w-0 flex-1">
              <p className="text-sm font-semibold text-foreground">Verifica la tua comprensione</p>
              <p className="truncate text-xs text-muted-foreground">{input.question}</p>
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-1 text-xs font-medium text-primary">
            <span>Apri</span>
            <ChevronRight className="h-4 w-4" aria-hidden />
          </div>
        </button>
      </section>
    );
  }

  return (
    <Shell
      label="Domanda del docente"
      footer={
        <>
          <div className="flex items-center justify-between gap-3">
            <span className="text-[11px] text-muted-foreground">{multi ? "Più risposte possibili · " : ""}1–{options.length} sceglie · 0 non lo so · Invio invia</span>
            <button type="button" className={btnPrimary} disabled={!canSubmit} onClick={submit}>
              Invia ↵
            </button>
          </div>
          <ErrorLine error={error} />
        </>
      }
    >
      <p className="mb-2 text-[11px] uppercase tracking-wider text-muted-foreground">{KIND_LABEL[input.kind]}</p>
      <Markdown text={input.question} className="sm font-medium" />
      <fieldset disabled={busy} className="mt-3 space-y-1.5">
        <legend className="sr-only">{multi ? "Scegli tutte le risposte giuste" : "Scegli una risposta"}</legend>
        {options.map((o, i) => (
          <Row key={o.value} n={String(i + 1)} on={selected.includes(o.value)} onClick={() => toggle(o.value)}>
            <Markdown text={o.label} className="sm" />
            <span className="sr-only">Opzione {LETTERS[i]}</span>
          </Row>
        ))}
        <Row n="0" on={dontKnow} onClick={noIdea}>
          <span className="font-prose text-[15px] text-muted-foreground">Non lo so</span>
        </Row>
      </fieldset>
      <div className="mt-4">
        <div className="mb-1.5 flex items-center justify-between">
          <label htmlFor={`quiz-note-${item.requestId}`} className="block text-xs font-medium text-foreground">
            Il tuo ragionamento
          </label>
          <DictationButton dictation={dictation} compact />
        </div>
        <textarea
          id={`quiz-note-${item.requestId}`}
          className={`${field} min-h-[4.5rem] resize-y`}
          rows={3}
          value={note}
          placeholder="Perché questa risposta? Anche mezza idea va bene"
          aria-label="Il tuo ragionamento"
          onChange={(e) => setNote(e.target.value)}
        />
      </div>
      <div className="mt-4">
        <ConfidencePicker value={conf} onChange={setConf} />
      </div>
    </Shell>
  );
}

function AskDock({ item, sessionId }: { item: AskItem; sessionId: string }) {
  const { input } = item;
  const options = askOptions(item);
  const multi = !!input.multiSelect;
  const freeText = options.length === 0;
  const [selected, setSelected] = useState<string[]>([]);
  const [otherOn, setOtherOn] = useState(false);
  const [other, setOther] = useState("");
  const otherRef = useRef<HTMLTextAreaElement>(null);
  const { busy, error, run } = useSubmit();
  const canSubmit = !busy && (freeText ? other.trim() !== "" : selected.length > 0 || (otherOn && other.trim() !== ""));

  const dictation = useDictation({
    onTranscribed: (speech) => {
      setOther((prev) => (prev.trim() ? prev.trim() + " " + speech : speech));
      if (!freeText) setOtherOn(true);
    },
  });

  const toggle = (v: string) => {
    if (!multi) setOtherOn(false);
    setSelected((s) => (multi ? (s.includes(v) ? s.filter((x) => x !== v) : [...s, v]) : [v]));
  };
  const toggleOther = () => {
    setOtherOn((o) => (multi ? !o : true));
    if (!multi) setSelected([]);
  };
  useEffect(() => {
    if (otherOn) otherRef.current?.focus({ preventScroll: true });
  }, [otherOn]);
  const submit = () => {
    if (!canSubmit) return;
    const a: AskAnswer = freeText ? { selected: [], other: other.trim() } : { selected, ...(otherOn && other.trim() ? { other: other.trim() } : {}) };
    void run(() => api.answer(sessionId, { requestId: item.requestId, kind: "ask", answer: a }));
  };
  useDockKeys((e) => {
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
    if (e.key === "Enter") {
      if (e.shiftKey) return;
      e.preventDefault();
      submit();
      return;
    }
    if (isTyping(e.target)) {
      if (e.key === "Escape") (e.target as HTMLElement).blur();
      return;
    }
    const n = Number(e.key);
    if (n >= 1 && n <= options.length) toggle(options[n - 1].value);
    else if (n === options.length + 1 && !freeText) toggleOther();
    else return;
    e.preventDefault();
  });

  return (
    <Shell
      label="Domanda del docente"
      footer={
        <>
          <div className="flex items-center justify-between gap-3">
            <span className="text-[11px] text-muted-foreground">{freeText ? "Invio invia · Maiusc+Invio a capo" : `${multi ? "Più risposte possibili · " : ""}1–${options.length + 1} sceglie · Invio invia`}</span>
            <button type="button" className={btnPrimary} disabled={!canSubmit} onClick={submit}>
              Invia ↵
            </button>
          </div>
          <ErrorLine error={error} />
        </>
      }
    >
      <Markdown text={input.question} className="sm font-medium" />
      {input.details && <Markdown text={input.details} className="sm text-muted-foreground" />}
      <fieldset disabled={busy} className="mt-3 space-y-1.5">
        <legend className="sr-only">Risposta</legend>
        {freeText ? (
          <div>
            <div className="mb-1 flex justify-end">
              <DictationButton dictation={dictation} compact />
            </div>
            <textarea
              ref={(el) => el?.focus({ preventScroll: true })}
              className={field}
              rows={3}
              value={other}
              placeholder="Scrivi la tua risposta"
              aria-label="La tua risposta"
              onChange={(e) => setOther(e.target.value)}
            />
          </div>
        ) : (
          <>
            {options.map((o, i) => (
              <Row key={o.value} n={String(i + 1)} on={selected.includes(o.value)} onClick={() => toggle(o.value)}>
                <span className="font-prose text-[15px]">{o.label}</span>
                {o.description && <span className="block font-prose text-sm text-muted-foreground">{o.description}</span>}
              </Row>
            ))}
            <Row n={String(options.length + 1)} on={otherOn} onClick={toggleOther}>
              <span className="font-prose text-[15px] text-muted-foreground">Altro</span>
            </Row>
            {otherOn && (
              <div className="mt-1.5">
                <div className="mb-1 flex justify-end">
                  <DictationButton dictation={dictation} compact />
                </div>
                <textarea ref={otherRef} className={field} rows={2} value={other} placeholder="Scrivi la tua risposta" aria-label="Altro: la tua risposta" onChange={(e) => setOther(e.target.value)} />
              </div>
            )}
          </>
        )}
      </fieldset>
    </Shell>
  );
}

function DecisionDock({ item, sessionId }: { item: Extract<ChatItem, { kind: "plan" | "curriculum" }>; sessionId: string }) {
  const [changing, setChanging] = useState(false);
  const [feedback, setFeedback] = useState("");
  const { busy, error, run } = useSubmit();

  const dictation = useDictation({
    onTranscribed: (speech) => {
      setFeedback((prev) => (prev.trim() ? prev.trim() + " " + speech : speech));
      setChanging(true);
    },
  });

  const decide = (answer: Decision) => run(() => api.answer(sessionId, item.kind === "plan" ? { requestId: item.requestId, kind: "plan", answer } : { requestId: item.requestId, kind: "curriculum", answer }));
  const sendChanges = () => feedback.trim() && !busy && void decide({ approved: false, feedback: feedback.trim() });
  useDockKeys((e) => {
    if ((e.ctrlKey || e.metaKey) && (e.key === "d" || e.key === "D")) {
      e.preventDefault();
      if (!changing) setChanging(true);
      dictation.toggle();
      return;
    }
    if (e.key === "Escape" && dictation.isRecording) {
      e.preventDefault();
      dictation.cancel();
      return;
    }
    if (e.key !== "Enter" || e.shiftKey) return;
    if (changing) {
      e.preventDefault();
      sendChanges();
    } else if (e.ctrlKey || e.metaKey) {
      // Approving is an explicit act: click, or Ctrl+Enter. Never a bare Enter.
      e.preventDefault();
      if (!busy) void decide({ approved: true });
    }
  });
  const what = item.kind === "plan" ? "il piano della lezione" : "il piano del corso";
  return (
    <Shell
      label="Decisione"
      footer={
        <>
          <div className="flex flex-wrap items-center gap-2">
        {changing ? (
          <>
            <button type="button" className={btnPrimary} disabled={busy || !feedback.trim()} onClick={sendChanges}>
              Invia le modifiche ↵
            </button>
            <button type="button" className={btn} disabled={busy} onClick={() => { setChanging(false); setFeedback(""); }}>
              Annulla
            </button>
          </>
        ) : (
          <>
            <button type="button" className={btnPrimary} disabled={busy} onClick={() => void decide({ approved: true })}>
              Approva
            </button>
            <button type="button" className={btn} disabled={busy} onClick={() => setChanging(true)}>
              Chiedi modifiche
            </button>
          </>
        )}
          </div>
          <ErrorLine error={error} />
        </>
      }
    >
      <p className="font-prose text-[15px]">
        Leggi {what} qui sopra. Va bene così?
      </p>
      {changing && (
        <div className="mt-3">
          <div className="mb-1 flex justify-end">
            <DictationButton dictation={dictation} compact />
          </div>
          <textarea
            ref={(el) => el?.focus({ preventScroll: true })}
            className={field}
            rows={3}
            value={feedback}
            placeholder="Che cosa vuoi cambiare?"
            aria-label="Che cosa vuoi cambiare?"
            onChange={(e) => setFeedback(e.target.value)}
          />
        </div>
      )}
    </Shell>
  );
}


/** Practice: the statement stays in view (collapsible) above the learner's reasoning field. */
function PracticeDock({ problem, sessionId, status, retryable }: { problem: Extract<ChatItem, { kind: "problem" }>; sessionId: string; status: SessionStatus; retryable?: boolean }) {
  return (
    <div className="mx-auto w-full max-w-[46rem] space-y-2">
      <details open className="max-h-[var(--practice-details-max-h,36vh)] overflow-y-auto rounded-xl border border-border bg-card px-4 py-2.5">
        <summary className="cursor-pointer text-[11px] uppercase tracking-wider text-muted-foreground">Problema</summary>
        <div className="mt-2">
          <Markdown text={problem.input.statement} className="sm" />
        </div>
      </details>
      <Composer sessionId={sessionId} status={status} placeholder="Il tuo ragionamento…" retryable={retryable} />
    </div>
  );
}

export function Dock({ sessionId, status, pending, problem, retryable }: { sessionId: string; status: SessionStatus; pending: InteractiveItem | null; problem?: Extract<ChatItem, { kind: "problem" }> | null; retryable?: boolean }) {
  if (!pending) return problem ? <PracticeDock problem={problem} sessionId={sessionId} status={status} retryable={retryable} /> : <Composer sessionId={sessionId} status={status} retryable={retryable} />;
  // key: a new question remounts the dock, so each one starts with a clean form.
  switch (pending.kind) {
    case "quiz":
      return <QuizDock key={pending.id} item={pending} sessionId={sessionId} />;
    case "ask":
      return <AskDock key={pending.id} item={pending} sessionId={sessionId} />;
    default:
      return <DecisionDock key={pending.id} item={pending} sessionId={sessionId} />;
  }
}
