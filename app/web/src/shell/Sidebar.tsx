import { CircleX, Map as MapIcon, Plus, Settings, Trash2 } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { api } from "../api.ts";
import { navigate, lessonHref, useRoute } from "../router.ts";
import type { SessionStatus } from "../../../shared/protocol.ts";
import { ActivityDock } from "./ActivityDock.tsx";
import { BrailleSpinner } from "./BrailleSpinner.tsx";
import { useData } from "./data.tsx";
import { relTime, slugify, startSession } from "./start.ts";

const DOT: Partial<Record<SessionStatus, string>> = { waiting: "bg-warn", idle: "bg-warn", error: "bg-bad", closed: "bg-muted-foreground/40", starting: "bg-info" };
const heading = "px-3 pb-1 pt-4 text-[11px] uppercase tracking-wider text-muted-foreground";
const item = (active: boolean) =>
  `flex w-full items-center gap-2 rounded px-3 py-1.5 text-left text-xs hover:bg-accent/60 ${active ? "bg-accent text-foreground" : "text-foreground/85"}`;

function NewCourse() {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const [err, setErr] = useState<string>();
  const [busy, setBusy] = useState(false);
  const { courses } = useData();
  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const slug = slugify(title);
    if (!slug) return setErr("Usa almeno una lettera o un numero nel titolo.");
    if (slug === "lezioni-singole") return setErr("Nome riservato alle lezioni singole.");
    const close = () => { setOpen(false); setTitle(""); };
    if (courses?.some((c) => c.slug === slug)) { close(); return navigate("/c/" + slug); } // already exists: open it
    setBusy(true);
    try {
      await startSession({ kind: "plan", course: slug, chapterTitle: title.trim() });
      close();
    } catch (x) {
      setErr(x instanceof Error ? x.message : String(x));
    } finally {
      setBusy(false);
    }
  }
  if (!open)
    return (
      <button type="button" onClick={() => setOpen(true)} className={`${item(false)} text-muted-foreground`}>
        <Plus size={13} /> Nuovo corso
      </button>
    );
  return (
    <form onSubmit={submit} className="px-1 py-1">
      <input
        autoFocus
        value={title}
        onChange={(e) => { setTitle(e.target.value); setErr(undefined); }}
        onKeyDown={(e) => e.key === "Escape" && setOpen(false)}
        onBlur={() => !title && !busy && setOpen(false)}
        disabled={busy}
        placeholder="Titolo del corso, poi Invio"
        aria-label="Titolo del corso"
        className="w-full rounded border border-border bg-background px-2 py-1 text-xs placeholder:text-muted-foreground"
      />
      {err && <p role="alert" className="mt-1 text-[11px] text-bad">{err}</p>}
    </form>
  );
}

/** Quick-start: a topic in, a free teach session out. No dialog; the topic is also the session title. */
function NewStandaloneLesson() {
  const [topic, setTopic] = useState("");
  const [err, setErr] = useState<string>();
  const [busy, setBusy] = useState(false);
  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const trimmed = topic.trim();
    if (!trimmed) return setErr("Scrivi un argomento.");
    setBusy(true);
    setErr(undefined);
    try {
      await startSession({ kind: "teach", course: "lezioni-singole", target: { mode: "free" }, note: trimmed.slice(0, 5000) });
      setTopic("");
    } catch (x) {
      setErr(x instanceof Error ? x.message : String(x));
    } finally {
      setBusy(false);
    }
  }
  return (
    <form onSubmit={submit} className="px-1 py-1">
      <input
        value={topic}
        onChange={(e) => { setTopic(e.target.value); setErr(undefined); }}
        disabled={busy}
        maxLength={5000}
        placeholder="Argomento, poi Invio"
        aria-label="Argomento della lezione singola"
        className="w-full rounded border border-border bg-background px-2 py-1 text-xs placeholder:text-muted-foreground"
      />
      {err && <p role="alert" className="mt-1 text-[11px] text-bad">{err}</p>}
    </form>
  );
}

type MenuTarget = { id: string; running: boolean; closed: boolean; x: number; y: number };

/** Right-click menu on a session row: close (stops the engine, keeps the session listed as closed; hidden when already closed) and delete (stops the engine, drops the transcript; lesson files stay). */
function SessionMenu({ menu, onClose }: { menu: MenuTarget; onClose(): void }) {
  const route = useRoute();
  const { reload } = useData();
  const [err, setErr] = useState<string>();
  useEffect(() => {
    const down = (e: PointerEvent) => { if (!(e.target instanceof Element && e.target.closest("[data-session-menu]"))) onClose(); };
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("pointerdown", down);
    document.addEventListener("keydown", key);
    return () => { document.removeEventListener("pointerdown", down); document.removeEventListener("keydown", key); };
  }, [onClose]);
  const del = async () => {
    try {
      await api.deleteSession(menu.id);
      if (route.name === "session" && route.id === menu.id) navigate("/");
      reload();
      onClose();
    } catch (x) {
      setErr(x instanceof Error ? x.message : String(x));
    }
  };
  const close = async () => {
    try {
      await api.close(menu.id);
      reload();
      onClose();
    } catch (x) {
      setErr(x instanceof Error ? x.message : String(x));
    }
  };
  return (
    <div data-session-menu role="menu" style={{ left: menu.x, top: menu.y }} className="fixed z-50 min-w-44 rounded-lg border border-border bg-card p-1 text-xs shadow-xl">
      {!menu.closed && (
        <button type="button" role="menuitem" autoFocus onClick={() => void close()} className="flex w-full items-center gap-2 rounded px-2.5 py-1.5 text-left hover:bg-accent">
          <CircleX size={13} /> Chiudi sessione
        </button>
      )}
      <button type="button" role="menuitem" autoFocus={menu.closed} onClick={() => void del()} className="flex w-full items-center gap-2 rounded px-2.5 py-1.5 text-left text-bad hover:bg-accent">
        <Trash2 size={13} /> {menu.running ? "Interrompi ed elimina" : "Elimina sessione"}
      </button>
      {err && <p role="alert" className="px-2.5 py-1 text-[11px] text-bad">{err}</p>}
    </div>
  );
}

export function Sidebar({ onSettings }: { onSettings(): void }) {
  const route = useRoute();
  const { courses, standaloneLessons, sessions, updateStatus, ankiByCourse } = useData();
  const [menu, setMenu] = useState<MenuTarget>();
  return (
    <aside aria-label="Navigazione" className="flex h-full w-[248px] shrink-0 flex-col border-r border-hairline bg-background">
      <nav className="min-h-0 flex-1 overflow-y-auto px-2 pb-2 pt-2">
        <a href="#/" aria-current={route.name === "home" ? "page" : undefined} className={item(route.name === "home")}>
          <MapIcon size={13} /> Mappa
        </a>

        <h2 className={heading}>Corsi</h2>
        {courses?.map((c) => {
          const total = c.lessons.length;
          const done = c.lessons.filter((l) => l.lessonPath && !l.inProgress).length;
          const inProg = c.lessons.filter((l) => l.lessonPath && l.inProgress);
          const progressLabel = `${done}/${total} lezioni fatte${inProg.length ? ` · in corso: ${inProg.map((l) => l.title).join(", ")}` : ""}`;
          const ankiStats = ankiByCourse?.[c.slug];
          const due = (ankiStats?.due ?? 0) + (ankiStats?.learning ?? 0);
          const active = route.name === "course" && route.slug === c.slug;
          return (
            <a key={c.slug} href={`#/c/${c.slug}`} aria-current={active ? "page" : undefined} className={item(active)}>
              <span className="min-w-0 flex-1 truncate">{c.title}</span>
              {total >= 1 && total <= 20 ? (
                <span
                  data-testid="lesson-grid"
                  role="img"
                  aria-label={progressLabel}
                  title={progressLabel}
                  className="grid shrink-0 grid-cols-[repeat(10,max-content)] gap-0.5"
                >
                  {c.lessons.map((l) => {
                    const state = !l.lessonPath ? "todo" : l.inProgress ? "inProgress" : "done";
                    const cls = state === "done" ? "bg-good" : state === "inProgress" ? "bg-warn animate-pulse" : "bg-border";
                    return <span key={l.slug} data-lesson-state={state} className={`size-2 rounded-[1px] ${cls}`} />;
                  })}
                </span>
              ) : (
                <span
                  data-testid="lesson-progress-fallback"
                  aria-label={progressLabel}
                  title={progressLabel}
                  className="shrink-0 text-[11px] tabular-nums text-muted-foreground"
                >
                  {done}/{total}
                </span>
              )}
              {due > 0 && (
                <span
                  data-testid="anki-due-badge"
                  title={`${due} card da ripassare oggi`}
                  aria-label={`${due} card da ripassare oggi`}
                  className="shrink-0 rounded bg-warn/15 px-1.5 py-0.5 text-[10px] font-medium tabular-nums leading-none text-warn"
                >
                  {due}
                </span>
              )}
            </a>
          );
        })}
        <NewCourse />

        <h2 className={heading}>Lezioni singole</h2>
        <div data-testid="standalone-lessons-section">
          {standaloneLessons?.map((l) => {
            const active = route.name === "lesson" && route.path === l.lessonPath;
            const state = l.inProgress ? "inProgress" : "done";
            const dotCls = l.inProgress ? "bg-warn animate-pulse" : "bg-good";
            return (
              <a
                key={l.slug}
                href={lessonHref(l.lessonPath!)}
                aria-current={active ? "page" : undefined}
                className={item(active)}
              >
                <span
                  data-standalone-state={state}
                  title={l.inProgress ? "in corso" : "fatta"}
                  className={`size-2 shrink-0 rounded-[1px] ${dotCls}`}
                />
                <span className="min-w-0 flex-1 truncate">{l.title}</span>
              </a>
            );
          })}
          <NewStandaloneLesson />
        </div>

        {sessions.length > 0 && <h2 className={heading}>Sessioni</h2>}
        {sessions.slice(0, 10).map((s) => {
          const active = route.name === "session" && route.id === s.id;
          return (
            <a
              key={s.id}
              href={`#/s/${s.id}`}
              aria-current={active ? "page" : undefined}
              onContextMenu={(e) => { e.preventDefault(); setMenu({ id: s.id, running: s.status === "running" || s.status === "starting" || s.status === "waiting", closed: s.status === "closed", x: e.clientX, y: e.clientY }); }}
              className={item(active)}
            >
              <span className="w-2 shrink-0 text-center">
                {s.status === "running" ? (
                  <BrailleSpinner label="in corso" className="text-info" />
                ) : (
                  <span className={`inline-block size-1.5 rounded-full ${DOT[s.status] ?? "bg-muted-foreground/40"}`} />
                )}
              </span>
              <span className="min-w-0 flex-1 truncate">{s.title}</span>
              <span className="shrink-0 text-[11px] text-muted-foreground">{relTime(s.updatedAt)}</span>
            </a>
          );
        })}
      </nav>
      {menu && <SessionMenu menu={menu} onClose={() => setMenu(undefined)} />}
      <ActivityDock />
      <div className="border-t border-hairline p-2">
        <button type="button" onClick={onSettings} aria-label="Impostazioni" className={`${item(false)} text-muted-foreground`}>
          <Settings size={13} />
          <span className="min-w-0 flex-1 truncate">Impostazioni</span>
          {updateStatus && updateStatus.behind > 0 && (
            <span className="size-2 rounded-full bg-info" title="Aggiornamento disponibile" />
          )}
        </button>
      </div>
    </aside>
  );
}
