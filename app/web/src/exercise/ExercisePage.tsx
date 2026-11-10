// #/e/<course>/<exercise>[?s=<sessionId>]: in-app exercise workspace. Statement + hints on the left, editor + test console on the right.
import { ArrowLeft, Check, CircleDashed, Lightbulb, Lock, Play, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api.ts";
import { Markdown } from "../session/Markdown.tsx";
import { btn, btnPrimary } from "../session/ui.tsx";
import type { ExerciseInfo, ExerciseRunResult, ExerciseWorkspace } from "../../../shared/protocol.ts";
import { CodeEditor } from "./CodeEditor.tsx";

type Files = Record<string, string>;
const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));
const CHANGED = "Il file è cambiato fuori dall'editor";
const SAVE_LABEL = { saved: "salvato", dirty: "modifiche non salvate", saving: "salvataggio…", error: "errore di salvataggio" };

function Results({ r }: { r: ExerciseRunResult }) {
  if (r.error) return <p role="alert" className="text-xs text-bad">I test non sono partiti: {r.error}</p>;
  const tests = r.tests?.length ? r.tests : r.failures.map((f) => ({ name: f.name, status: "failed" as const }));
  return (
    <div role="status" data-testid="results">
      <p className={`mb-2 text-xs font-medium ${r.ok ? "text-good" : "text-muted-foreground"}`}>
        {r.ok ? "Tutti i traguardi raggiunti" : `${r.passed}/${r.total} traguardi`} · {(r.durationMs / 1000).toFixed(1)} s
      </p>
      <ol className="space-y-1.5">
        {tests.map((t) => {
          const fail = r.failures.find((f) => f.name === t.name);
          return (
            <li key={t.name} className="text-xs" data-status={t.status}>
              <span className="flex items-start gap-2">
                {t.status === "passed" ? <Check size={13} className="mt-0.5 shrink-0 text-good" /> : t.status === "failed" ? <X size={13} className="mt-0.5 shrink-0 text-bad" /> : <CircleDashed size={13} className="mt-0.5 shrink-0 text-muted-foreground" />}
                <span className={t.status === "passed" ? "text-muted-foreground" : "text-foreground"}>{t.name}</span>
              </span>
              {fail && <pre className="ml-5 mt-1 max-h-40 overflow-auto whitespace-pre-wrap rounded border border-hairline bg-card p-2 text-[11px] text-bad">{fail.message}</pre>}
            </li>
          );
        })}
      </ol>
    </div>
  );
}

export function ExercisePage({ course, exercise, sessionId }: { course: string; exercise: string; sessionId?: string }) {
  const [ws, setWs] = useState<ExerciseWorkspace>();
  const [info, setInfo] = useState<ExerciseInfo>();
  const [error, setError] = useState<string>();
  const [files, setFiles] = useState<Files>({});
  const [active, setActive] = useState("");
  const [saveState, setSaveState] = useState<keyof typeof SAVE_LABEL>("saved");
  const [result, setResult] = useState<ExerciseRunResult>();
  const [running, setRunning] = useState(false);
  const [slow, setSlow] = useState(false); // a run past 5 s is most likely the first-time npm install
  const [hintBusy, setHintBusy] = useState(false);
  const [notice, setNotice] = useState<string>();
  const [leftPct, setLeftPct] = useState(40);
  const saved = useRef<Files>({});
  const live = useRef<Files>({});
  const versions = useRef<Record<string, string>>({}); // on-disk version each file had when loaded/saved: the save's baseVersion
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const row = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api
      .workspace(course, exercise)
      .then((w) => {
        setWs(w);
        setInfo(w.info);
        setResult(w.info.lastRun);
        const f = Object.fromEntries(w.files.map((x) => [x.path, x.content]));
        versions.current = Object.fromEntries(w.files.map((x) => [x.path, x.version]));
        saved.current = { ...f };
        live.current = f;
        setFiles(f);
        setActive((w.files.find((x) => !x.readOnly && /^esercizio\./i.test(x.path)) ?? w.files.find((x) => !x.readOnly) ?? w.files[0])?.path ?? "");
      })
      .catch((e) => setError(msg(e)));
  }, [course, exercise]);

  const isDirty = () => Object.keys(live.current).some((p) => live.current[p] !== saved.current[p]);

  /** Re-reads the workspace; overwrites a file when it is not dirty, or when `force` (a save conflict). Disk wins. */
  const reload = useCallback(
    async (force?: string) => {
      try {
        const w = await api.workspace(course, exercise);
        let changed = false;
        const next: Files = {};
        for (const x of w.files) {
          versions.current[x.path] = x.version;
          const dirty = live.current[x.path] !== saved.current[x.path];
          if ((dirty && x.path !== force) || live.current[x.path] === x.content) continue;
          saved.current[x.path] = live.current[x.path] = next[x.path] = x.content;
          changed = true;
        }
        if (changed) {
          setFiles((f) => ({ ...f, ...next }));
          setNotice(CHANGED + ": ricaricato");
          setSaveState(isDirty() ? "dirty" : "saved");
        }
      } catch { /* offline: keep the buffer */ }
    },
    [course, exercise],
  );

  /** Saves every dirty file; resolves true when nothing is left unsaved. */
  const flush = useCallback(async () => {
    clearTimeout(timer.current);
    const dirty = Object.keys(live.current).filter((p) => live.current[p] !== saved.current[p]);
    if (!dirty.length) return true;
    setSaveState("saving");
    try {
      for (const p of dirty) {
        const content = live.current[p];
        try {
          const r = await api.saveExerciseFile(course, exercise, p, content, versions.current[p] ?? "");
          versions.current[p] = r.version;
          saved.current[p] = content;
        } catch (e) {
          if ((e as { code?: string }).code !== "stale") throw e;
          await reload(p); // someone else edited the file: disk wins, the buffer is reloaded
          setSaveState("dirty");
          return false;
        }
      }
      setSaveState(Object.keys(live.current).some((p) => live.current[p] !== saved.current[p]) ? "dirty" : "saved");
      return true;
    } catch (e) {
      setSaveState("error");
      setError("Salvataggio non riuscito: " + msg(e));
      return false;
    }
  }, [course, exercise, reload]);

  const edit = (path: string, v: string) => {
    live.current[path] = v;
    setFiles((f) => ({ ...f, [path]: v }));
    setSaveState("dirty");
    setNotice(undefined);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => void flush(), 800);
  };

  // leaving the page flushes pending edits; closing the tab while unsaved asks first
  useEffect(() => {
    // closing must never be blocked or lose edits: fire-and-forget the save (Electron closes silently, a browser may ask first)
    const warn = (e: BeforeUnloadEvent) => {
      if (isDirty()) { void flush(); e.preventDefault(); }
    };
    const hide = () => { if (document.visibilityState === "hidden" && isDirty()) void flush(); };
    const focus = () => { if (!isDirty()) void reload(); };
    addEventListener("beforeunload", warn);
    addEventListener("pagehide", hide);
    addEventListener("focus", focus);
    document.addEventListener("visibilitychange", hide);
    return () => {
      removeEventListener("beforeunload", warn);
      removeEventListener("pagehide", hide);
      removeEventListener("focus", focus);
      document.removeEventListener("visibilitychange", hide);
      void flush();
    };
  }, [flush, reload]);

  useEffect(() => {
    setSlow(false);
    if (!running) return;
    const t = setTimeout(() => setSlow(true), 5000);
    return () => clearTimeout(t);
  }, [running]);

  const run = async () => {
    if (running) return;
    setRunning(true);
    setError(undefined);
    try {
      if (await flush()) setResult(await api.runExercise(course, exercise, sessionId));
    } catch (e) {
      setError(msg(e));
    }
    setRunning(false);
  };
  const runRef = useRef(run);
  runRef.current = run;
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
        e.preventDefault();
        void runRef.current();
      }
    };
    addEventListener("keydown", key);
    return () => removeEventListener("keydown", key);
  }, []);

  const reveal = async () => {
    setHintBusy(true);
    try {
      setInfo(await api.revealHint(course, exercise, sessionId));
    } catch (e) {
      setError(msg(e));
    }
    setHintBusy(false);
  };

  if (!ws || !info) return <p className="p-8 text-xs text-muted-foreground" role={error ? "alert" : "status"}>{error ?? "Caricamento…"}</p>;

  const hints = info.hints ?? [];
  const shown = info.hintsRevealed ?? 0;
  const readOnly = ws.files.find((f) => f.path === active)?.readOnly ?? true;
  const startDrag = (e: React.PointerEvent) => {
    const box = row.current!.getBoundingClientRect();
    const move = (m: PointerEvent) => setLeftPct(Math.min(70, Math.max(20, ((m.clientX - box.left) / box.width) * 100)));
    const up = () => {
      removeEventListener("pointermove", move);
      removeEventListener("pointerup", up);
    };
    addEventListener("pointermove", move);
    addEventListener("pointerup", up);
    e.preventDefault();
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="exercise-page">
      <header className="flex items-center gap-3 border-b border-hairline px-4 py-2 text-xs">
        {sessionId && (
          <a href={`#/s/${sessionId}`} className="inline-flex items-center gap-1 text-muted-foreground hover:text-foreground">
            <ArrowLeft size={12} /> Torna alla sessione
          </a>
        )}
        <h1 className="font-semibold">{info.name}</h1>
        {info.final && <span className="rounded border border-warn/60 px-1.5 py-0.5 text-[10px] uppercase tracking-wider text-warn">esercizio finale</span>}
        <span className="flex-1" />
        <span className="text-muted-foreground" data-testid="save-state">{SAVE_LABEL[saveState]}</span>
        <button type="button" className="rounded px-2 py-1 text-muted-foreground hover:bg-accent/60 hover:text-foreground" onClick={() => void api.openExercise(course, exercise).catch((e) => setError(msg(e)))}>
          Apri in VS Code
        </button>
        <button type="button" className={btnPrimary} disabled={running} onClick={() => void run()} title="Ctrl/Cmd + Invio">
          <Play size={12} /> {running ? (slow ? "Installo le dipendenze dell'esercizio (solo la prima volta)…" : "Test in corso…") : "Esegui test"}
        </button>
      </header>
      {notice && <p role="status" className="border-b border-hairline px-4 py-1.5 text-xs text-muted-foreground">{notice}</p>}
      {error && <p role="alert" className="border-b border-hairline px-4 py-1.5 text-xs text-bad">{error}</p>}

      <div ref={row} className="flex min-h-0 flex-1">
        <aside className="min-h-0 shrink-0 overflow-y-auto px-5 py-4" style={{ width: `${leftPct}%` }}>
          <div className="font-prose text-[15px]">
            <Markdown text={info.instructions ?? "La consegna è nei commenti `TODO` del file dell'esercizio; i test a destra dicono cosa deve succedere."} />
          </div>
          {hints.length > 0 && (
            <section className="mt-8 border-t border-hairline pt-4" data-testid="hints">
              <h2 className="mb-2 text-[11px] uppercase tracking-wider text-muted-foreground">Suggerimenti</h2>
              <ol className="space-y-3">
                {hints.slice(0, shown).map((h, i) => (
                  <li key={i} className="rounded-md border border-hairline bg-card p-3 font-prose text-sm">
                    <p className="mb-1 flex items-center gap-1.5 text-[11px] text-warn">
                      <Lightbulb size={11} /> Suggerimento {i + 1}/{hints.length}
                    </p>
                    <Markdown text={h} />
                  </li>
                ))}
              </ol>
              {shown < hints.length ? (
                <button type="button" className={`${btn} mt-3`} disabled={hintBusy} onClick={() => void reveal()}>
                  <Lightbulb size={12} /> Mostra suggerimento {shown + 1}/{hints.length}
                </button>
              ) : (
                <p className="mt-3 text-xs text-muted-foreground">Hai aperto tutti i suggerimenti.</p>
              )}
            </section>
          )}
        </aside>
        <div role="separator" aria-orientation="vertical" className="w-1 shrink-0 cursor-col-resize bg-hairline hover:bg-border" onPointerDown={startDrag} />
        <section className="flex min-h-0 min-w-0 flex-1 flex-col">
          <div role="tablist" className="flex shrink-0 gap-px overflow-x-auto border-b border-hairline text-xs">
            {ws.files.map((f) => (
              <button
                key={f.path}
                type="button"
                role="tab"
                aria-selected={f.path === active}
                onClick={() => setActive(f.path)}
                className={`inline-flex items-center gap-1.5 whitespace-nowrap px-3 py-1.5 ${f.path === active ? "bg-card text-foreground" : "text-muted-foreground hover:text-foreground"}`}
              >
                {f.readOnly && <Lock size={10} aria-label="sola lettura" />}
                {f.path}
                {files[f.path] !== saved.current[f.path] && <span className="size-1.5 rounded-full bg-warn" aria-label="modificato" />}
              </button>
            ))}
          </div>
          <div className="flex min-h-0 flex-[3] flex-col">
            <CodeEditor key={`${active}:${readOnly}`} path={active} value={files[active] ?? ""} readOnly={readOnly} onChange={(v) => edit(active, v)} />
          </div>
          <div className="min-h-0 flex-[2] overflow-y-auto border-t border-hairline bg-card/40 px-4 py-3" data-testid="console">
            <h2 className="mb-2 text-[11px] uppercase tracking-wider text-muted-foreground">Test</h2>
            {result ? <Results r={result} /> : <p className="text-xs text-muted-foreground">Premi «Esegui test» (Ctrl/Cmd + Invio): i test sono traguardi da raggiungere in ordine.</p>}
          </div>
        </section>
      </div>
    </div>
  );
}
