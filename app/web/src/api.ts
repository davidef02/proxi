// Typed client for the HTTP API documented in app/shared/protocol.ts.
import type {
  AnkiImportResult,
  AnkiStatusResponse,
  AttachmentMeta,
  ClaudeStatusResponse,
  QuotaLimits,
  AnswerBody,
  CourseOverview,
  CourseLesson,
  ExerciseInfo,
  ExerciseRunResult,
  ExerciseWorkspace,
  GlobalEvent,
  JobRecord,
  KnowledgeMap,
  LearnerSettings,
  ServerEvent,
  SessionConfig,
  SessionState,
  SessionSummary,
  SyncConflict,
  SyncStatus,
  SyncTrashEntry,
  UpdateApplyResponse,
  UpdateStatusResponse,
} from "../../shared/protocol.ts";

async function call<T>(method: "GET" | "POST" | "DELETE", url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: method === "GET" ? undefined : { "content-type": "application/json", "x-learner-client": "1" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : undefined;
  if (!res.ok) throw Object.assign(new Error((data && data.error) || `${res.status} ${res.statusText}`), { code: data?.code as string | undefined });
  return data as T;
}

/** GET /api/settings: the settings plus the code repo path and the data folder path. */
export type SettingsResponse = LearnerSettings & { repoRoot: string; dataRoot?: string };

export const api = {
  courses: () => call<CourseOverview[]>("GET", "/api/courses"),
  standaloneLessons: () => call<CourseLesson[]>("GET", "/api/standalone-lessons"),
  anki: (course?: string) => call<AnkiStatusResponse>("GET", course ? "/api/anki?course=" + encodeURIComponent(course) : "/api/anki"),
  ankiReview: (course: string) => call<{ ok: true }>("POST", "/api/anki/review", { course }),
  ankiImport: (course: string) => call<AnkiImportResult>("POST", "/api/anki/import", { course }),
  limits: () => call<QuotaLimits | null>("GET", "/api/limits"),
  map: () => call<KnowledgeMap>("GET", "/api/map"),
  sessions: () => call<SessionSummary[]>("GET", "/api/sessions"),
  createSession: (config: SessionConfig) => call<{ id: string }>("POST", "/api/sessions", config),
  session: (id: string) => call<SessionState>("GET", `/api/sessions/${id}`),
  uploadAttachments: async (sessionId: string, files: File[]): Promise<AttachmentMeta[]> => {
    const fd = new FormData();
    for (const f of files) fd.append("files", f);
    const res = await fetch(`/api/sessions/${sessionId}/attachments`, {
      method: "POST",
      headers: { "x-learner-client": "1" },
      body: fd,
    });
    const text = await res.text();
    const data = text ? JSON.parse(text) : undefined;
    if (!res.ok) throw Object.assign(new Error((data && data.error) || `${res.status} ${res.statusText}`), { code: data?.code as string | undefined });
    return data as AttachmentMeta[];
  },
  message: (id: string, text: string, attachments?: AttachmentMeta[]) => call<{ ok: true }>("POST", `/api/sessions/${id}/message`, { text, attachments }),
  retry: (id: string) => call<{ ok: true }>("POST", `/api/sessions/${id}/retry`),
  answer: (id: string, body: AnswerBody) => call<{ ok: true }>("POST", `/api/sessions/${id}/answer`, body),
  interrupt: (id: string) => call<{ ok: true }>("POST", `/api/sessions/${id}/interrupt`),
  close: (id: string) => call<{ ok: true }>("POST", `/api/sessions/${id}/close`),
  deleteSession: (id: string) => call<{ ok: true }>("DELETE", `/api/sessions/${id}`),
  runExercise: (course: string, name: string, sessionId?: string) =>
    call<ExerciseRunResult>("POST", "/api/exercises/run", { course, name, sessionId }),
  exercises: (course: string) => call<ExerciseInfo[]>("GET", `/api/exercises/${course}`),
  workspace: (course: string, name: string) => call<ExerciseWorkspace>("GET", `/api/exercises/${course}/${name}`),
  saveExerciseFile: (course: string, name: string, path: string, content: string, baseVersion?: string) =>
    call<{ ok: true; version: string }>("POST", "/api/exercises/save", { course, name, path, content, baseVersion }),
  revealHint: (course: string, name: string, sessionId?: string) =>
    call<ExerciseInfo>("POST", "/api/exercises/hint", { course, name, sessionId }),
  openExercise: (course: string, name: string) => call<{ ok: true }>("POST", "/api/exercises/open", { course, name }),
  agyModels: () => call<string[]>("GET", "/api/agy/models"),
  opencodeModels: () => call<string[]>("GET", "/api/opencode/models"),
  claudeStatus: () => call<ClaudeStatusResponse>("GET", "/api/claude/status"),
  settings: () => call<SettingsResponse>("GET", "/api/settings"),
  saveSettings: (patch: Partial<LearnerSettings>) => call<LearnerSettings>("POST", "/api/settings", patch),
  sync: () => call<SyncStatus>("GET", "/api/sync"),
  runSync: (confirmDeletions?: boolean) => call<SyncStatus>("POST", "/api/sync/run", { confirmDeletions }),
  githubStart: () => call<SyncStatus>("POST", "/api/sync/github/start"),
  githubCancel: () => call<SyncStatus>("POST", "/api/sync/github/cancel"),
  googleStart: () => call<{ url: string }>("POST", "/api/sync/google/start"),
  disconnect: (provider: "github" | "gdrive") => call<SyncStatus>("POST", "/api/sync/disconnect", { provider }),
  setDataRoot: (dataRoot: string) => call<SyncStatus>("POST", "/api/sync/data-root", { dataRoot }),
  conflicts: () => call<SyncConflict[]>("GET", "/api/sync/conflicts"),
  resolveConflict: (id: string, choice: "local" | "remote" | "both") => call<{ ok: true }>("POST", "/api/sync/conflicts/resolve", { id, choice }),
  trash: () => call<SyncTrashEntry[]>("GET", "/api/sync/trash"),
  restoreTrash: (id: string) => call<{ ok: true }>("POST", "/api/sync/trash/restore", { id }),
  jobs: () => call<JobRecord[]>("GET", "/api/jobs"),
  cancelJob: (id: string) => call<{ ok: true }>("POST", `/api/jobs/${id}/cancel`),
  transcribe: async (audioBlob: Blob): Promise<{ text: string }> => {
    const fd = new FormData();
    fd.append("audio", audioBlob, "recording.webm");
    const res = await fetch("/api/transcribe", {
      method: "POST",
      headers: { "x-learner-client": "1" },
      body: fd,
    });
    const text = await res.text();
    const data = text ? JSON.parse(text) : undefined;
    if (!res.ok) throw new Error((data && data.error) || `${res.status} ${res.statusText}`);
    return data as { text: string };
  },
  update: () => call<UpdateStatusResponse>("GET", "/api/update"),
  checkUpdate: () => call<UpdateStatusResponse>("POST", "/api/update/check"),
  applyUpdate: (force?: boolean) => call<UpdateApplyResponse>("POST", "/api/update/apply", { force }),
};

/** Subscribe to app-wide events (jobs, graph, courses, sessions). EventSource reconnects by itself. */
export function subscribeGlobal(onEvent: (e: GlobalEvent) => void, onOpen?: () => void): () => void {
  const es = new EventSource("/api/events");
  if (onOpen) es.onopen = onOpen;
  es.onmessage = (m) => onEvent(JSON.parse(m.data) as GlobalEvent);
  return () => es.close();
}

/** Subscribe to a session's SSE stream. EventSource reconnects by itself; the server re-sends a snapshot on every connect. */
export function subscribe(id: string, onEvent: (e: ServerEvent) => void, onError?: () => void): () => void {
  const es = new EventSource(`/api/sessions/${id}/events`);
  es.onmessage = (m) => onEvent(JSON.parse(m.data) as ServerEvent);
  if (onError) es.onerror = onError;
  return () => es.close();
}

/** URL for a repo-relative file served under /files (lesson artifacts, reference sheets). */
export const fileUrl = (repoRelative: string) => `/files/${repoRelative.split("/").map(encodeURIComponent).join("/")}`;
