// App-wide data for the shell (sidebar + course page): courses, sessions, jobs; kept fresh by /api/events.
import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { api, subscribeGlobal } from "../api.ts";
import type { AnkiCourseStats, CourseLesson, CourseOverview, JobRecord, QuotaLimits, SessionSummary, SyncStatus, UpdateStatusResponse } from "../../../shared/protocol.ts";
import type { SettingsSection } from "./settings/sections.ts";

interface Data {
  courses?: CourseOverview[];
  /** Lessons in courses/lezioni-singole/, outside the structured course list. */
  standaloneLessons?: CourseLesson[];
  sessions: SessionSummary[];
  jobs: JobRecord[];
  /** Last reported Claude subscription quota (5h / 7d); null until a claude process has run once. */
  limits?: QuotaLimits | null;
  updateStatus?: UpdateStatusResponse;
  /** Data folder, provider and sync state; `needsData` shows the first-run screen. */
  sync?: SyncStatus;
  /** Per-course Anki due counts; undefined while Anki is offline (badges hide). */
  ankiByCourse?: Record<string, AnkiCourseStats>;
  setSync(s: SyncStatus): void;
  error?: string;
  reload(): void;
  /** Section the settings dialog is open on; null when closed. */
  settingsSection: SettingsSection | null;
  /** Opens the settings dialog on `section` (default "generali"); every open starts there, nothing is remembered. */
  openSettings(section?: SettingsSection): void;
  closeSettings(): void;
}
const Ctx = createContext<Data>({ sessions: [], jobs: [], reload: () => {}, setSync: () => {}, settingsSection: null, openSettings: () => {}, closeSettings: () => {} });
export const useData = () => useContext(Ctx);

export function DataProvider({ children }: { children: ReactNode }) {
  const [courses, setCourses] = useState<CourseOverview[]>();
  const [standaloneLessons, setStandaloneLessons] = useState<CourseLesson[]>();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [jobs, setJobs] = useState<JobRecord[]>([]);
  const [error, setError] = useState<string>();
  const [limits, setLimits] = useState<QuotaLimits | null>();
  const [updateStatus, setUpdateStatus] = useState<UpdateStatusResponse>();
  const [sync, setSync] = useState<SyncStatus>();
  const [ankiByCourse, setAnkiByCourse] = useState<Record<string, AnkiCourseStats>>();
  const [settingsSection, setSettingsSection] = useState<SettingsSection | null>(null);
  const openSettings = useCallback((section: SettingsSection = "generali") => setSettingsSection(section), []);
  const closeSettings = useCallback(() => setSettingsSection(null), []);
  // Request counters: a response that is no longer the latest one is dropped (out-of-order replies).
  const seq = useRef({ courses: 0, standalone: 0, sessions: 0, jobs: 0, sync: 0, anki: 0 });

  const loadUpdate = useCallback(() => {
    api.update().then(setUpdateStatus).catch(() => {});
  }, []);
  const loadSync = useCallback(() => {
    const n = ++seq.current.sync;
    api.sync().then((s) => { if (n === seq.current.sync) setSync(s); }).catch(() => {});
  }, []);
  const loadCourses = useCallback(() => {
    const n = ++seq.current.courses;
    api.courses().then((c) => { if (n === seq.current.courses) { setCourses(c); setError(undefined); } })
      .catch((e) => { if (n === seq.current.courses) setError(String(e.message ?? e)); });
  }, []);
  const loadStandalone = useCallback(() => {
    const n = ++seq.current.standalone;
    api.standaloneLessons()
      .then((l) => { if (n === seq.current.standalone) setStandaloneLessons(l); })
      .catch(() => {});
  }, []);
  const loadSessions = useCallback(() => {
    const n = ++seq.current.sessions;
    api.sessions().then((s) => { if (n === seq.current.sessions) setSessions(s); }).catch(() => {});
  }, []);
  const loadJobs = useCallback(() => {
    const n = ++seq.current.jobs;
    api.jobs().then((j) => { if (n === seq.current.jobs) setJobs(j); }).catch(() => {});
  }, []);
  const loadAnki = useCallback(() => {
    const n = ++seq.current.anki;
    api.anki()
      .then((r) => { if (n === seq.current.anki) setAnkiByCourse(r.online ? r.byCourse : undefined); })
      .catch(() => { if (n === seq.current.anki) setAnkiByCourse(undefined); });
  }, []);
  const reload = useCallback(() => { loadCourses(); loadStandalone(); loadSessions(); loadAnki(); }, [loadCourses, loadStandalone, loadSessions, loadAnki]);

  const timers = useRef<Record<string, ReturnType<typeof setTimeout>>>({});
  const soon = useCallback((key: string, fn: () => void) => {
    clearTimeout(timers.current[key]);
    timers.current[key] = setTimeout(fn, 250);
  }, []);

  useEffect(() => {
    const t = timers.current;
    const onFocus = () => soon("anki", loadAnki);
    // due counts change as he reviews in Anki itself, so refresh when the window comes back
    window.addEventListener("focus", onFocus);
    const off = subscribeGlobal(
      (e) => {
        if (e.type === "job") {
          // job events may omit `result`: merge the light record, then refetch the list on any change
          setJobs((js) => [e.job, ...js.filter((j) => j.id !== e.job.id)]);
          soon("jobs", loadJobs);
          if (e.job.status !== "running") soon("courses", reload);
        } else if (e.type === "courses") {
          soon("courses", loadCourses);
          soon("standalone", loadStandalone);
          soon("anki", loadAnki);
          // needsData may flip when the courses change (first pull, data folder change)
          soon("sync", loadSync);
        } else if (e.type === "sync") setSync(e.status);
        else if (e.type === "sessions") soon("sessions", loadSessions);
        else if (e.type === "limits") setLimits(e.limits);
        else if (e.type === "update") {
          setUpdateStatus((cur) => ({ ...(cur ?? { activeSessions: 0 }), ...e.status }));
          soon("update", loadUpdate);
        }
      },
      // first connect and every reconnect (server restart, dropped stream): resync everything
      () => { reload(); loadJobs(); loadUpdate(); loadSync(); api.limits().then(setLimits).catch(() => {}); },
    );
    return () => { off(); window.removeEventListener("focus", onFocus); Object.values(t).forEach(clearTimeout); };
  }, [reload, loadCourses, loadStandalone, loadSessions, loadJobs, loadUpdate, loadSync, loadAnki, soon]);

  return <Ctx.Provider value={{ courses, standaloneLessons, sessions, jobs, limits, updateStatus, sync, setSync, error, reload, settingsSection, openSettings, closeSettings, ankiByCourse }}>{children}</Ctx.Provider>;
}
