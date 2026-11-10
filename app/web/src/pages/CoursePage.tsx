// #/c/<course>: destination, the Continua/Ripasso hero (one marked by recommend()), quiet secondary actions, lessons, then closed accordions: Fuori dal piano, Esercizi, Come stai andando.
import { ArrowRight, Check, ChevronRight, CircleDashed, CircleDot } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { api } from "../api.ts";
import { findOpenSession, nextAction, recommend } from "../next-action.ts";
import { BrailleSpinner } from "../shell/BrailleSpinner.tsx";
import { useData } from "../shell/data.tsx";
import { exerciseHref, lessonHref, navigate, referenceHref } from "../router.ts";
import { startSession } from "../shell/start.ts";
import type {
  AnkiStatusResponse,
  CourseLesson,
  CourseOverview,
  ExerciseInfo,
  MapNode,
  MapNodeStatus,
  SessionConfig,
} from "../../../shared/protocol.ts";

const quiet = "rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent/60 hover:text-foreground disabled:opacity-50";
const link = "inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground";
const sectionLabel = "text-[11px] uppercase tracking-wider text-muted-foreground";

// no state: the browser opens and closes it
function Fold({ title, children }: { title: string; children: ReactNode }) {
  return (
    <details className="group mt-8">
      <summary className="flex list-none items-center gap-1.5 py-1 hover:text-foreground [&::-webkit-details-marker]:hidden">
        <ChevronRight size={13} aria-hidden className="shrink-0 text-muted-foreground transition-transform group-open:rotate-90" />
        <span className={sectionLabel}>{title}</span>
      </summary>
      <div className="mt-2">{children}</div>
    </details>
  );
}

function LessonRow({ l, n }: { l: CourseLesson; n?: number }) {
  const taught = !!l.lessonPath;
  const Icon = !taught ? CircleDashed : l.inProgress ? CircleDot : Check;
  return (
    <li className={`flex items-start gap-3 py-2.5 ${taught ? "" : "text-muted-foreground"}`}>
      <span className="w-5 shrink-0 pt-0.5 text-right text-xs tabular-nums text-muted-foreground">{n ?? ""}</span>
      <Icon size={13} aria-hidden className={`mt-1 shrink-0 ${taught ? (l.inProgress ? "text-warn" : "text-good") : "text-muted-foreground"}`} />
      <span className="min-w-0 flex-1 font-prose text-[15px] leading-snug break-words">{l.title}</span>
      <span className="w-16 shrink-0 pt-0.5 text-right text-[11px] text-muted-foreground">
        {l.inProgress ? "in corso" : taught ? "fatta" : "da fare"}
      </span>
      <span className="flex w-40 shrink-0 justify-end gap-3 pt-0.5">
        {l.lessonPath && <a className={link} href={lessonHref(l.lessonPath)}>Lezione</a>}
        {l.referencePath && <a className={link} href={referenceHref(l.referencePath)}>Sintesi</a>}
      </span>
    </li>
  );
}

const PROGRESS: [MapNodeStatus, string, string][] = [
  ["solid", "solidi", "bg-good"],
  ["shaky", "da rinforzare", "bg-warn"],
  ["missed", "mancati", "bg-bad"],
  ["taught", "spiegati", "bg-muted-foreground/60"],
];

function Progress({ nodes }: { nodes: MapNode[] }) {
  const counts: Record<string, number> = {};
  for (const n of nodes) {
    counts[n.status] = (counts[n.status] ?? 0) + 1;
  }
  const total = Object.values(counts).reduce((a, b) => a + b, 0);
  if (!total) return null;
  return (
    <Fold title={`Come stai andando (${total} ${total === 1 ? "concetto" : "concetti"})`}>
      <p className="flex flex-wrap items-center gap-x-5 gap-y-1 text-xs text-muted-foreground">
        {PROGRESS.filter(([k]) => counts[k]).map(([k, label, dot]) => (
          <span key={k} className="inline-flex items-center gap-1.5">
            <span className={`size-1.5 rounded-full ${dot}`} />
            {counts[k]} {label}
          </span>
        ))}
      </p>
    </Fold>
  );
}

function Reinforce({ nodes }: { nodes: MapNode[] }) {
  const shaky = nodes.filter((n) => n.status === "shaky");
  if (shaky.length === 0) return null;
  return (
    <section data-testid="reinforce" className="mt-10">
      <h2 className={`mb-1 ${sectionLabel}`}>Da rinforzare</h2>
      <ul className="divide-y divide-hairline border-y border-hairline">
        {shaky.map((n) => (
          <li key={n.key} className="flex items-baseline justify-between gap-3 py-2">
            <a
              className="min-w-0 flex-1 text-[13px] text-foreground hover:underline break-words"
              href={lessonHref(n.lessonPath, n.nodeId)}
            >
              {n.title}
            </a>
            <span className="shrink-0 text-xs text-muted-foreground">Lezione {n.lessonNumber}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Exercises({ course, hasExercises }: { course: string; hasExercises: boolean }) {
  const [list, setList] = useState<ExerciseInfo[]>([]);
  useEffect(() => {
    // empty until the fetch lands, so no stale list shows
    setList([]);
    if (!hasExercises) return;
    let live = true;
    api.exercises(course)
      .then((res) => {
        if (live) setList(res);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [course, hasExercises]);

  // gate on the fetched list, not hasExercises: the flag can be true with nothing on disk
  if (!list.length) return null;
  return (
    <Fold title={`Esercizi (${list.length})`}>
      <p className="mb-3 text-xs text-muted-foreground">
        Chiedi esercizi o lasciali scegliere al teacher: usa lezioni e card Anki del corso.
      </p>
      <ul className="divide-y divide-hairline border-y border-hairline">
        {list.map((e) => (
          <li key={e.name} className="flex items-start gap-3 py-2">
            <a className="min-w-0 flex-1 text-[13px] hover:underline break-words" href={exerciseHref(course, e.name)}>
              {e.name}
            </a>
            {e.final && <span className="shrink-0 text-[10px] uppercase tracking-wider text-warn">finale</span>}
          </li>
        ))}
      </ul>
    </Fold>
  );
}

export function CoursePage({ slug }: { slug: string }) {
  const { courses, error } = useData();
  const [busy, setBusy] = useState(false);
  const [fail, setFail] = useState<string>();
  const [reviewMsg, setReviewMsg] = useState<string>();
  const [anki, setAnki] = useState<AnkiStatusResponse | undefined>();
  const [courseNodes, setCourseNodes] = useState<MapNode[]>([]);
  const course: CourseOverview | undefined = courses?.find((c) => c.slug === slug);

  useEffect(() => {
    if (!course) return;
    let active = true;
    const load = () => {
      api.anki(course.slug)
        .then((res) => {
          if (active) setAnki(res);
        })
        .catch(() => {
          if (active) setAnki({ online: false });
        });
      api.map()
        .then((m) => {
          if (active) setCourseNodes(m.nodes.filter((n) => n.course === course.slug));
        })
        .catch(() => {});
      api.ankiImport(course.slug)
        .then((res) => {
          if (active && res.imported > 0) {
            api.map()
              .then((m) => {
                if (active) setCourseNodes(m.nodes.filter((n) => n.course === course.slug));
              })
              .catch(() => {});
          }
        })
        .catch(() => undefined);
    };
    load();
    window.addEventListener("focus", load);
    return () => {
      active = false;
      window.removeEventListener("focus", load);
    };
  }, [course?.slug]);

  if (!course)
    return (
      <p className="p-8 text-xs text-muted-foreground" role={error ? "alert" : "status"}>
        {error ? `Non riesco a caricare i corsi: ${error}` : courses ? "Corso non trovato." : "Caricamento…"}
      </p>
    );

  const go = async (config: SessionConfig, reuse = false) => {
    if (busy) return;
    setBusy(true);
    setFail(undefined);
    setReviewMsg(undefined);
    try {
      const open = reuse ? findOpenSession(await api.sessions(), config) : undefined;
      if (open) return navigate("/s/" + open.id);
      await startSession(config);
    } catch (e) {
      setFail(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  const onReview = async () => {
    if (busy) return;
    setBusy(true);
    setFail(undefined);
    setReviewMsg(undefined);
    try {
      await api.ankiReview(course.slug);
      setReviewMsg("Ripasso aperto in Anki.");
    } catch (e) {
      setFail(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const next = nextAction(course);
  const done = course.lessons.filter((l) => l.lessonPath && !l.inProgress).length;
  // Anki's "due" today = review + learning cards (new cards are not a backlog)
  const due = (anki?.due ?? 0) + (anki?.learning ?? 0);
  const rec = recommend(next, anki && { online: anki.online, due });
  const recLesson = rec === "lesson";
  const recReview = rec === "review";

  const dates = courseNodes.map((n) => n.lastAt).filter((d): d is string => !!d);
  const lastStudied = dates.length ? dates.reduce((a, b) => (a > b ? a : b)) : undefined;
  const lastStudiedDate = lastStudied
    ? new Date(lastStudied).toLocaleDateString("it-IT", { day: "numeric", month: "long" })
    : undefined;

  let lessonSubtext: string;
  if (next.kind === "plan") {
    lessonSubtext = `${done}/${course.lessons.length} lezioni fatte`;
  } else if (next.kind === "start") {
    const i = course.lessons.findIndex((l) => !l.lessonPath);
    const n = i >= 0 ? i + 1 : 1;
    lessonSubtext = `Lezione ${n} di ${course.lessons.length} · ${done} fatte`;
  } else {
    const progress = (l: CourseLesson) => !!l.lessonPath && !!l.inProgress;
    const going = course.lessons.find(progress);
    if (going) {
      const n = course.lessons.indexOf(going) + 1;
      lessonSubtext = `Lezione ${n} di ${course.lessons.length} · ${done} fatte`;
    } else {
      lessonSubtext = `${done}/${course.lessons.length} fatte`;
    }
  }
  if (lastStudiedDate) lessonSubtext += ` · ultimo studio ${lastStudiedDate}`;

  const cardBase = "flex min-h-[112px] flex-col items-start gap-1 rounded-lg p-4 text-left disabled:opacity-60";
  const isReviewDisabled = busy || !anki || !anki.online || anki.deckExists === false;

  return (
    <div className="mx-auto w-full max-w-3xl px-8 py-10">
      <h1 className="font-prose text-[26px] font-semibold leading-tight">{course.title}</h1>
      {course.destination && <p className="mt-2 max-w-[60ch] font-prose text-[15px] leading-relaxed text-muted-foreground">{course.destination}</p>}

      <div className="mt-6 grid gap-3 sm:grid-cols-2">
        <button
          type="button"
          data-testid="next-action"
          aria-describedby={recLesson ? "recommended-badge" : undefined}
          disabled={busy}
          onClick={() => go(next.config, next.kind !== "plan")}
          className={`${cardBase} ${recLesson ? "bg-primary text-primary-foreground hover:opacity-90" : "border border-hairline hover:bg-accent/60"}`}
        >
          <div className="flex w-full items-center justify-between gap-2">
            <span className="text-xs font-medium">{next.heading}</span>
            <div className="flex items-center gap-2">
              {recLesson && (
                <span id="recommended-badge" className="rounded bg-primary-foreground/15 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wider">
                  Consigliato ora
                </span>
              )}
              {busy ? <BrailleSpinner /> : <ArrowRight size={13} className="shrink-0 opacity-70" />}
            </div>
          </div>
          {next.lessonTitle && (
            <span className="font-prose text-[17px] leading-snug break-words">
              {next.lessonTitle}
            </span>
          )}
          <span className={`mt-auto text-xs ${recLesson ? "text-primary-foreground/75" : "text-muted-foreground"}`}>
            {lessonSubtext}
          </span>
        </button>

        <button
          type="button"
          data-testid="review-action"
          aria-describedby={recReview ? "recommended-badge" : undefined}
          disabled={isReviewDisabled}
          onClick={onReview}
          className={`${cardBase} ${recReview ? "bg-primary text-primary-foreground hover:opacity-90" : "border border-hairline hover:bg-accent/60"}`}
        >
          <div className="flex w-full items-center justify-between gap-2">
            <span className="text-xs font-medium">Ripasso</span>
            {recReview && (
              <span id="recommended-badge" className="rounded bg-primary-foreground/15 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wider">
                Consigliato ora
              </span>
            )}
          </div>
          {anki === undefined ? (
            <span className="font-prose text-[17px] leading-snug">…</span>
          ) : !anki.online ? (
            <span className="font-prose text-[17px] leading-snug text-muted-foreground">Apri Anki per ripassare</span>
          ) : anki.deckExists === false ? (
            <span className="font-prose text-[17px] leading-snug text-muted-foreground">Nessuna card ancora</span>
          ) : due > 0 ? (
            <>
              <span className="font-prose text-[17px] leading-snug break-words">
                {due} card da ripassare
              </span>
              {(anki.newCards ?? 0) > 0 && (
                <span className={`mt-auto text-xs ${recReview ? "text-primary-foreground/75" : "text-muted-foreground"}`}>
                  + {anki.newCards} nuove
                </span>
              )}
            </>
          ) : (
            <span className="font-prose text-[17px] leading-snug break-words">
              Niente da ripassare ora{(anki.newCards ?? 0) > 0 ? ` · ${anki.newCards} nuove` : ""}
            </span>
          )}
        </button>
      </div>

      {reviewMsg && <p role="status" className="mt-3 text-xs text-muted-foreground">{reviewMsg}</p>}

      <div data-testid="quiet-actions" className="mt-2 flex flex-wrap gap-x-1">
        <button type="button" disabled={busy} className={quiet} onClick={() => go({ kind: "teach", course: course.slug, target: { mode: "free" } })}>
          Lezione libera
        </button>
        {next.kind !== "plan" && (
          <button type="button" disabled={busy} className={quiet} onClick={() => go({ kind: "plan", course: course.slug, chapterTitle: course.title })}>
            Riorganizza corso
          </button>
        )}
        <button type="button" data-testid="exercises-action" disabled={busy} className={quiet} onClick={() => go({ kind: "practice", course: course.slug, practiceNodes: [] })}>
          Esercizi
        </button>
      </div>
      {fail && <p role="alert" className="mt-3 text-xs text-bad">{fail}</p>}

      <Reinforce nodes={courseNodes} />

      <section className="mt-10">
        <h2 className={`mb-1 ${sectionLabel}`}>Lezioni</h2>
        {course.lessons.length === 0 ? (
          <p className="py-3 text-xs text-muted-foreground">Nessuna lezione pianificata: parti da «Pianifica le prossime lezioni».</p>
        ) : (
          <ol className="divide-y divide-hairline border-y border-hairline">
            {course.lessons.map((l, i) => <LessonRow key={l.slug} l={l} n={i + 1} />)}
          </ol>
        )}
      </section>

      {course.orphans.length > 0 && (
        <Fold title={`Fuori dal piano (${course.orphans.length})`}>
          <ul className="divide-y divide-hairline border-y border-hairline">
            {course.orphans.map((l) => <LessonRow key={l.slug} l={l} />)}
          </ul>
        </Fold>
      )}

      <Exercises course={course.slug} hasExercises={course.hasExercises} />

      <Progress nodes={courseNodes} />
    </div>
  );
}
