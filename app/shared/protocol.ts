// The one contract between the server (app/server) and the web UI (app/web).
// Types only, plus a few constants. Both sides import this file; change it deliberately.
import type { CognitiveNodePayload, CognitiveStepPayload, FullLessonDocument, PedagogicalTier, SintesiDocument, SintesiNode, SintesiMisconception } from "../../extensions/html-lesson.ts";

export type { CognitiveNodePayload, CognitiveStepPayload, FullLessonDocument, PedagogicalTier, SintesiDocument, SintesiNode, SintesiMisconception };

// ───────────────────────────── sessions ─────────────────────────────

export type SessionKind = "teach" | "plan" | "practice";
export type ModelAlias = "opus" | "sonnet" | "haiku";
export const MODELS: ModelAlias[] = ["opus", "sonnet", "haiku"];

export type EffortLevel = "low" | "medium" | "high" | "xhigh" | "max";
export type EffortSetting = "default" | EffortLevel;
export const EFFORT_SETTINGS: EffortSetting[] = ["default", "low", "medium", "high", "xhigh", "max"];

export interface RoleEffort {
  teacher: EffortSetting;
  subagent: EffortSetting;
  map: EffortSetting;
}

/** What a teach session works on. Paths are repo-relative with forward slashes. */
export type LessonTarget =
  /** A lesson planned in curriculum.json and not yet taught: the artifact is created when the learner approves the plan, with `title` verbatim. */
  | { mode: "planned"; title: string }
  /** Continue an existing lesson artifact (append nodes to it). */
  | { mode: "existing"; lessonPath: string }
  /** A lesson outside the plan ("Fuori dal piano"); title proposed by the teacher in propose_plan. */
  | { mode: "free" };

export interface SessionConfig {
  kind: SessionKind;
  /** Course directory under courses/. For kind "plan" it may not exist yet. */
  course: string;
  /** Teacher model. Absent = settings.teacherModel. */
  model?: ModelAlias;
  /** teach only. */
  target?: LessonTarget;
  /** teach only; gates sync_anki. Absent at creation = the server asks ANKI_OPT_IN_QUESTION as the session's first
   *  ask card (options "Sì"/"No") and launches claude only after the answer, which it stores here. */
  ankiOptIn?: boolean;
  /** plan only: chapter title (new or existing chapter in curriculum.json). */
  chapterTitle?: string;
  /** practice only: node keys to practise (see MapNode.key). Empty = teacher picks from the map. */
  practiceNodes?: string[];
  /** practice only: graph proposition ids (GraphNode.id, "n_…") picked on the map. */
  practicePropositions?: string[];
  /** Optional free text from the learner, sent with the opening turn. */
  note?: string;
}

export type SessionStatus =
  | "starting" // claude process spawning
  | "running" // a turn is in progress
  | "waiting" // a tool is blocked on the learner (quiz/ask/plan/curriculum)
  | "idle" // turn finished, waiting for a learner message
  | "error"
  | "closed";

export interface Usage {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  turns: number;
}

export interface SessionSummary {
  id: string;
  config: SessionConfig;
  title: string;
  createdAt: string;
  updatedAt: string;
  status: SessionStatus;
  /** Repo-relative path of the lesson artifact once it exists. */
  lessonPath?: string;
  usage: Usage;
  retryable?: boolean;
}

export interface SessionState extends SessionSummary {
  items: ChatItem[];
  /** The interactive item currently blocking the teacher, if any (also present in items). */
  pendingRequestId: string | null;
  plan: PlanState | null;
  exercises: ExerciseInfo[];
  lastError?: string;
}

// ───────────────────────────── attachments ─────────────────────────────

export interface AttachmentMeta {
  id: string;
  name: string;
  /** Repo-relative path with forward slashes (e.g. .learner/sessions/<id>/attachments/<name>). */
  path: string;
  mimeType: string;
  size: number;
}

export interface MessageBody {
  text: string;
  attachments?: AttachmentMeta[];
}

// ───────────────────────────── chat items ─────────────────────────────
// The transcript as the UI renders it. Items are upserted by `id`.

interface ItemBase {
  id: string;
  at: string; // ISO
}

export type ChatItem =
  | (ItemBase & { kind: "learner"; text: string; attachments?: AttachmentMeta[] })
  | (ItemBase & { kind: "teacher"; text: string; streaming: boolean }) // markdown + $LaTeX$ + ```mermaid
  | (ItemBase & { kind: "tool"; tool: string; input: unknown; status: "running" | "done" | "error"; resultText?: string }) // generic chip (Read, WebSearch, write_node...)
  | (ItemBase & { kind: "quiz"; requestId: string; input: QuizInput; displayOrder: number[]; result?: QuizResult; cancelled?: boolean })
  | (ItemBase & { kind: "ask"; requestId: string; input: AskInput; answer?: AskAnswer; cancelled?: boolean })
  | (ItemBase & { kind: "plan"; requestId: string; input: ProposePlanInput; decision?: Decision; cancelled?: boolean })
  | (ItemBase & { kind: "curriculum"; requestId: string; input: ProposeCurriculumInput; decision?: Decision; cancelled?: boolean })
  /** `node` = the payload as written (rendered inline in the chat, app style); absent in transcripts saved before v3. */
  | (ItemBase & { kind: "node"; nodeId: string; planNode: string; title: string; lessonPath: string; anki?: string; node?: CognitiveNodePayload })
  | (ItemBase & { kind: "exercise"; exercise: ExerciseInfo })
  | (ItemBase & { kind: "problem"; input: ProblemInput; conclusion?: ConcludeInput })
  | (ItemBase & { kind: "finish"; result: FinishResult })
  | (ItemBase & { kind: "notice"; level: "info" | "error"; text: string })
  /** A background subagent started by this session (id = "job:" + job.id), upserted as it progresses. */
  | (ItemBase & { kind: "job"; job: JobRecord });

export type InteractiveItem = Extract<ChatItem, { requestId: string }>;

// ───────────────────────────── tool inputs (what Claude sends) ─────────────────────────────
// Tool names as Claude sees them: mcp__app__<name>.

export type AppToolName =
  | "quiz"
  | "ask"
  | "propose_plan"
  | "write_node"
  | "sync_anki"
  | "write_exercise"
  | "run_exercise"
  | "finish_lesson"
  | "propose_curriculum"
  | "problem"
  | "conclude"
  | "research"
  | "visual"
  | "demo"
  | "read_lesson"
  | "search_concepts"
  | "open_concept"
  | "browse_concepts";

export interface QuizOption {
  label: string; // bare claim, markdown + LaTeX
  value?: string; // defaults to label
}

export interface QuizInput {
  /** probe = level mapping (Phase 1a); predict = before teaching a node (open); check = after teaching (Quiz-check). */
  kind: "probe" | "predict" | "check";
  /** Plan node id ("N3") for predict/check. */
  nodeId?: string;
  question: string;
  options: QuizOption[]; // 2..6
  multiSelect?: boolean;
  explanation: string; // shown after answering
  correctAnswer: string | string[]; // option value(s)
  misconceptions?: { value: string; misconception: string }[];
}

/** What the learner submits. `selected` holds option values; empty when dontKnow. */
export interface QuizAnswer {
  selected: string[];
  dontKnow: boolean;
  confidence?: number; // 0..100
  note?: string;
  thinkingWhileReading?: string;
}

export interface QuizResult extends QuizAnswer {
  correct: boolean;
}

export interface AskInput {
  question: string;
  details?: string;
  options?: { label: string; value?: string; description?: string }[]; // none = free text
  multiSelect?: boolean;
}

export interface AskAnswer {
  selected: string[]; // option values
  other?: string; // "Altro" free text, or the whole answer when there are no options
}

export interface ProposePlanInput {
  /** Lesson topic: also the cards' topic tag (the deck is the course's, Proxi::<chapter title>). */
  topic: string;
  /** Only for target "free": the lesson title (the insight). Ignored for planned/existing targets. */
  title?: string;
  approach: string; // markdown, prose
  dag: string; // mermaid `graph TD`, node labels `N1["Nodo 1: ..."]`
  goal: string;
}

export interface ProposeCurriculumInput {
  chapter: string;
  slug: string; // course dir
  destination: string; // 1-2 sentences
  /** Full MISSION.md (Why / Success looks like / Constraints / Out of scope). Required when the course dir is new. */
  mission?: string;
  /** `slug` only for legacy entries whose file slug differs from the title slug. */
  lessons: { title: string; plannedNodes?: number; slug?: string }[];
}

export interface Decision {
  approved: boolean;
  feedback?: string;
}

export interface WriteNodeInput {
  /** Plan node this node realises ("N3"). */
  planNode: string;
  node: CognitiveNodePayload;
}

export interface WriteExerciseInput {
  /** Directory name under courses/<course>/exercises, e.g. "13-provide-inject". */
  name: string;
  /** LeetCode-style statement, Italian markdown: goal, examples, constraints, the steps (= test groups) in order. */
  instructions: string;
  files: { path: string; content: string }[]; // paths relative to the exercise dir
  /** Progressive hints, weakest first (nudge → concept/API → near-solution). Revealed one at a time by the learner. */
  hints?: string[];
  /** The lesson's final semi-guided exercise (weaves new and earlier concepts). */
  final?: boolean;
}

export interface ProblemInput {
  statement: string; // markdown
  mode: "recall" | "discrimination" | "composition";
  nodeKeys: string[]; // MapNode.key values targeted
  modelSolution: string; // hidden from the learner until conclude
}

export interface ConcludeInput {
  outcome: "solved" | "solvedWithHints" | "movedOn" | "surrendered";
  verdict: string;
  rung: "R0" | "R1" | "R2" | "R3" | "R4" | "R5" | null;
  missClass: "cue" | "discrimination" | "execution" | "careless" | "structural" | null;
  retest: boolean;
}

export interface FinishLessonInput {
  recordTitle: string;
  summary: string;
  evidence: string; // markdown
  implications: string; // markdown
  notesAddendum?: string; // markdown appended to the course NOTES.md
}

export interface FinishResult {
  lessonPath?: string;
  referencePath?: string;
  recordPath: string;
  notesUpdated: boolean;
  anki?: string;
}

// ───────────────────────────── plan / exercises / map ─────────────────────────────

export type PlanNodeStatus = "todo" | "current" | "done" | "missed";

export interface PlanState {
  topic: string;
  goal: string;
  dag: string;
  approved: boolean;
  /** Keyed by DAG id ("N1"). Lesson node i (0-based, capture order) realises N(i+1). */
  nodeStatus: Record<string, PlanNodeStatus>;
}

export interface ExerciseRunResult {
  ok: boolean;
  passed: number;
  failed: number;
  total: number;
  failures: { name: string; message: string }[];
  /** Every test in file order (the milestones); absent on results from older runs. */
  tests?: { name: string; status: "passed" | "failed" | "skipped" }[];
  durationMs: number;
  at: string;
  /** Set when vitest could not run at all (missing node_modules, crash). */
  error?: string;
}

export interface ExerciseInfo {
  course: string;
  name: string; // dir name
  dir: string; // repo-relative
  files: string[];
  instructions?: string;
  lastRun?: ExerciseRunResult;
  /** Progressive hints (SUGGERIMENTI.json); the UI hides them until revealed. */
  hints?: string[];
  /** How many hints the learner has revealed so far (persisted). */
  hintsRevealed?: number;
  final?: boolean;
}

/** GET /api/exercises/:course/:name → the workspace. Test files and helpers the learner must not touch are readOnly. */
export interface ExerciseWorkspace {
  info: ExerciseInfo;
  files: { path: string; content: string; readOnly: boolean; /** content hash; send it back as baseVersion on save */ version: string }[];
}

export type MapNodeStatus = "taught" | "solid" | "shaky" | "missed";

export interface MapNode {
  /** `${course}/${lessonSlug}#${nodeId}` */
  key: string;
  course: string;
  lessonSlug: string;
  lessonTitle: string;
  lessonNumber: number;
  lessonPath: string;
  nodeId: string;
  title: string;
  tier: PedagogicalTier;
  status: MapNodeStatus;
  attempts: number;
  lastAt?: string;
}

export interface KnowledgeMap {
  nodes: MapNode[];
  edges: { from: string; to: string }[]; // MapNode.key → MapNode.key (dependency → dependent)
}

/** One line of courses/<course>/progress.jsonl: one graded lesson check (probe/predict/check) or Anki review. */
export interface ProgressRecord {
  at: string;
  sessionId: string;
  course: string;
  lessonSlug?: string;
  /** Plan node id ("N3"), node slug, or MapNode key. */
  nodeRefs: string[];
  kind: "probe" | "predict" | "check" | "review";
  question?: string;
  correct?: boolean;
  dontKnow?: boolean;
  confidence?: number;
  /** Progress record source; "anki" for imported flashcard reviews. */
  source?: "anki";
  /** Anki revlog id (epoch ms). */
  reviewId?: number;
  /** Anki card id. */
  cardId?: number;
  /** Anki ease score: 1 Again, 2 Hard, 3 Good, 4 Easy. */
  ease?: number;
  /** Anki lapse count at import time. */
  lapses?: number;
  /** Anki card interval in days at import time; negative means seconds, while still learning. */
  interval?: number;
}

// ───────────────────────────── courses overview ─────────────────────────────

export interface CourseLesson {
  title: string;
  slug: string;
  plannedNodes?: number;
  /** Present once taught. */
  lessonPath?: string;
  lessonNumber?: number;
  nodeCount?: number;
  referencePath?: string;
  /** Taught but planned nodes not all captured, or no learning record yet. */
  inProgress?: boolean;
}

export interface CourseOverview {
  slug: string;
  title: string;
  lessons: CourseLesson[]; // planned order
  orphans: CourseLesson[]; // "Fuori dal piano"
  hasExercises: boolean;
  hasMission: boolean;
  /** First paragraph of MISSION.md `## Why`, markdown stripped, ≤ 240 chars. */
  destination?: string;
}

/** Anki counts of one course's deck (`Proxi::<chapter title>`). */
export interface AnkiCourseStats {
  due: number;
  learning: number;
  newCards: number;
}

export interface AnkiStatusResponse {
  online: boolean;
  due?: number;
  newCards?: number;
  learning?: number;
  total?: number;
  error?: string;
  /** Set only when the request asked for a course. */
  deck?: string;
  /** Set only when the request asked for a course. */
  deckExists?: boolean;
  /** Per-course counts, keyed by course slug. Only without `?course=`; missing decks are zeros. */
  byCourse?: Record<string, AnkiCourseStats>;
}

export interface AnkiImportResult {
  online: boolean;
  imported: number;
  skipped: number;
  error?: string;
}

// ───────────────────────────── server → web events (SSE) ─────────────────────────────

export type ServerEvent =
  | { type: "snapshot"; state: SessionState }
  | { type: "item"; item: ChatItem } // upsert by id
  | { type: "delta"; itemId: string; text: string } // append to a streaming teacher item
  | { type: "status"; status: SessionStatus; error?: string; retryable?: boolean }
  | { type: "pending"; requestId: string | null }
  | { type: "lesson"; lessonPath: string; nodeId?: string } // artifact changed on disk: reload pane, scroll to node
  | { type: "plan"; plan: PlanState }
  | { type: "exercises"; exercises: ExerciseInfo[] }
  | { type: "usage"; usage: Usage };

/** One subscription quota window as `claude` reports it: utilization 0..1, resetsAt in epoch seconds. */
export interface QuotaWindow {
  utilization: number;
  resetsAt?: number;
}
/** The Claude subscription's 5-hour and 7-day windows (from claude's rate_limit_event); `at` = when reported. */
export interface QuotaLimits {
  fiveHour?: QuotaWindow;
  sevenDay?: QuotaWindow;
  at: string;
}

// ───────────────────────────── app updates ─────────────────────────────

export type UpdateState = "idle" | "checking" | "up_to_date" | "available" | "updating" | "relaunching" | "error";
export interface UpdateCommitInfo {
  hash: string;
  subject: string;
  relativeTime: string;
}
export interface UpdateStatus {
  state: UpdateState;
  behind: number;
  commits?: UpdateCommitInfo[];
  error?: string;
  checkedAt?: string;
}
export interface UpdateStatusResponse extends UpdateStatus {
  activeSessions: number;
}
export interface UpdateApplyResponse {
  ok: boolean;
  relaunching: boolean;
  error?: string;
}

/** App-wide events: GET /api/events (SSE). Clients refetch what moved. */
export type GlobalEvent =
  | { type: "job"; job: JobRecord }
  | { type: "graph"; revision: number }
  | { type: "courses" }
  | { type: "sessions" }
  | { type: "sync"; status: SyncStatus }
  | { type: "limits"; limits: QuotaLimits }
  | { type: "update"; status: UpdateStatus };

/** GET/POST /api/settings: the settings plus the repository root and the data folder the server works on. */
export type SettingsResponse = LearnerSettings & { repoRoot: string; dataRoot?: string };

// ───────────────────────────── HTTP API (web → server) ─────────────────────────────
// All JSON. POSTs and DELETEs require header `x-learner-client: 1` (CSRF guard) and are bound to 127.0.0.1.
//
// GET  /api/courses                          → CourseOverview[]
// GET  /api/standalone-lessons               → CourseLesson[]
// GET  /api/anki                             → AnkiStatusResponse
// POST /api/anki/import   { course }   AnkiImportResult
// GET  /api/limits                           → QuotaLimits | null (last reported subscription quota)
// GET  /api/map                              → KnowledgeMap
// GET  /api/sessions                         → SessionSummary[] (newest first)
// POST /api/sessions        SessionConfig    → { id: string }
// GET  /api/sessions/:id                     → SessionState
// GET  /api/sessions/:id/events              → SSE of ServerEvent (first event: snapshot)
// POST /api/sessions/:id/message   { text }  → { ok: true }
// POST /api/sessions/:id/answer    AnswerBody → { ok: true }
// POST /api/sessions/:id/interrupt           → { ok: true }
// POST /api/sessions/:id/close               → { ok: true }
// DELETE /api/sessions/:id                  → { ok: true } (stops it, removes .learner/sessions/<id>; lesson files stay)
// POST /api/exercises/run   { course, name, sessionId? } → ExerciseRunResult (with sessionId: also reported to the teacher as a learner message)
// POST /api/exercises/open  { course, name } → { ok: true } (opens VS Code)
// GET  /api/exercises/:course/:name          → ExerciseWorkspace (in-app editor)
// POST /api/exercises/save  { course, name, path, content, baseVersion? } → { ok: true, version } (editable files only; 409 when baseVersion no longer matches the file on disk)
// POST /api/exercises/hint  { course, name, sessionId? } → ExerciseInfo (reveals the next hint; with sessionId the teacher is told)
// GET  /api/lesson?path=<repo-relative lesson html> → FullLessonDocument (in-app lesson reader)
// GET  /files/<repo-relative path>           → static file under courses/ or assets/ (lesson iframe)
// POST /mcp/:sessionId  (header x-learner-token) → MCP JSON-RPC (used by the claude process only)
// POST /mcp/job/:jobId  (header x-learner-token) → MCP JSON-RPC for a background subagent
// GET  /api/events                           → SSE of GlobalEvent
// GET  /api/settings                         → SettingsResponse
// POST /api/settings  Partial<LearnerSettings> → SettingsResponse
// GET  /api/sync                              → SyncStatus
// POST /api/sync/run   { confirmDeletions? }  → SyncStatus (one pass now)
// POST /api/sync/github/start                 → SyncStatus (device flow: github.pending has the code)
// POST /api/sync/github/cancel                → SyncStatus
// POST /api/sync/google/start                 → { url } (open it in the system browser)
// GET  /oauth/google/callback?code&state      → HTML page (loopback redirect)
// POST /api/sync/disconnect { provider }      → SyncStatus (deletes the local token)
// POST /api/sync/data-root { dataRoot }       → SyncStatus (restartRequired when it changed)
// GET  /api/sync/conflicts                    → SyncConflict[]
// POST /api/sync/conflicts/resolve { id, choice: "local"|"remote"|"both" } → { ok: true }
// GET  /api/sync/trash                        → SyncTrashEntry[]
// POST /api/sync/trash/restore { id }         → { ok: true }
// GET  /api/jobs                             → JobRecord[] (newest first, WITHOUT result)
// GET  /api/jobs/:id                         → JobRecord (with result)
// POST /api/jobs/:id/cancel                  → { ok: true }
// GET  /api/graph                            → GraphSnapshot (app/shared/graph)
// GET  /api/graph/sources                    → GraphSource[]
// POST /api/graph/extract  { sources?: string[] } → { jobs: JobRecord[] } (no sources = every uncovered/stale lesson)
// POST /api/graph/positions { positions: Record<string, {x,y}> } → { ok: true }
// POST /api/graph/nodes/:id/delete           → { ok: true }

export type AnswerBody =
  | { requestId: string; kind: "quiz"; answer: QuizAnswer }
  | { requestId: string; kind: "ask"; answer: AskAnswer }
  | { requestId: string; kind: "plan"; answer: Decision }
  | { requestId: string; kind: "curriculum"; answer: Decision };

export const DEFAULT_PORT = 4317;
/** The exact Phase 1c question (skills/teach). */
export const ANKI_OPT_IN_QUESTION = "Vuoi che vengano create e sincronizzate le flashcard Anki per questa sessione? (Sì / No)";

// ───────────────────────────── settings ─────────────────────────────
// Stored in <repo>/.learner/settings.json (runtime, gitignored). Read with readSettings() (app/server/settings.ts).

export const PROVIDERS = ["claude", "agy", "opencode"] as const;
export type Provider = (typeof PROVIDERS)[number];

export const CLAUDE_AUTH_MODES = ["subscription", "apiKey"] as const;
export type ClaudeAuthMode = (typeof CLAUDE_AUTH_MODES)[number];

export type ClaudeInstallationState =
  | "not-found"
  | "update-required"
  | "version-unchecked"
  | "meets-minimum";

export interface ClaudeStatusResponse {
  status: ClaudeInstallationState;
  version?: string;
  minVersion: string;
  binPath?: string;
  error?: string;
}

export interface LearnerSettings {
  /** Engine of teacher sessions and background subagents: `claude`, `agy` (Antigravity), or `opencode`. */
  provider: Provider;
  /** Claude auth mode: "subscription" (interactive login) or "apiKey" (Anthropic Console API key). */
  claudeAuth: ClaudeAuthMode;
  /** Anthropic API key when claudeAuth is "apiKey". */
  anthropicApiKey: string;
  /** agy model ids per role (used when provider is "agy"; the aliases above are then ignored). */
  agyModels: { teacher: string; subagent: string; map: string };
  /** opencode model ids per role (used when provider is "opencode"). */
  opencodeModels: { teacher: string; subagent: string; map: string };
  /** Default model of teach/plan/practice sessions. */
  teacherModel: ModelAlias;
  /** research / visual / demo subagents. */
  subagentModel: ModelAlias;
  /** The map extractor subagent. */
  mapModel: ModelAlias;
  /** Effort level per role for Claude Code ("default" = CLI flag omitted). */
  effort: RoleEffort;
  /** Run the map extractor automatically when a lesson is finished. */
  autoMap: boolean;
  /** STT provider for voice dictation: 'groq' (default, permanent free tier) or 'deepgram'. */
  sttProvider: "groq" | "deepgram";
  /** API key for speech-to-text dictation (Groq or Deepgram). */
  sttApiKey: string;
  /** Language code for speech-to-text dictation (e.g. 'it', 'en', 'es'). Default 'it'. */
  dictationLanguage: string;
  /** Where learner data syncs: "local" (no remote), "github", "gdrive". */
  syncProvider: SyncProviderId;
  /** OAuth client id overrides ("" = built-in constant). */
  githubClientId: string;
  googleClientId: string;
  googleClientSecret: string;
}

export interface DictationLanguageOption {
  code: string;
  label: string;
}

export const DICTATION_LANGUAGES: readonly DictationLanguageOption[] = [
  { code: "it", label: "Italiano" },
  { code: "en", label: "Inglese" },
  { code: "es", label: "Spagnolo" },
  { code: "fr", label: "Francese" },
  { code: "de", label: "Tedesco" },
  { code: "pt", label: "Portoghese" },
  { code: "ru", label: "Russo" },
  { code: "zh", label: "Cinese" },
  { code: "ja", label: "Giapponese" },
  { code: "nl", label: "Olandese" },
] as const;

export const DEFAULT_SETTINGS: LearnerSettings = {
  provider: "claude",
  claudeAuth: "subscription",
  anthropicApiKey: "",
  agyModels: { teacher: "gemini-3.8-flash-high", subagent: "gemini-3.8-flash-medium", map: "gemini-3.8-flash-medium" },
  opencodeModels: { teacher: "omniroute/auto/best-coding", subagent: "omniroute/auto/fast", map: "omniroute/auto/fast" },
  teacherModel: "sonnet",
  subagentModel: "sonnet",
  mapModel: "sonnet",
  effort: { teacher: "default", subagent: "default", map: "default" },
  autoMap: true,
  sttProvider: "groq",
  sttApiKey: "",
  dictationLanguage: "it",
  syncProvider: "local",
  githubClientId: "",
  googleClientId: "",
  googleClientSecret: "",
};

export const SYNC_PROVIDERS = ["local", "github", "gdrive"] as const;
export type SyncProviderId = (typeof SYNC_PROVIDERS)[number];

export interface SyncConflict { id: string; rel: string; side: "locale" | "remoto"; at: string }
export interface SyncTrashEntry { id: string; rel: string; deletedAt: string; size: number }

/** GET /api/sync and every /api/sync/* POST. */
export interface SyncStatus {
  provider: SyncProviderId;
  dataRoot: string;
  /** Set by LEARNER_DATA_ROOT: not changeable from the UI. */
  dataRootFromEnv: boolean;
  /** dataRoot has neither courses/ nor curriculum.json: the UI shows only the first-run screen. */
  needsData: boolean;
  state: "idle" | "syncing" | "error" | "confirm";
  at?: string; pushed?: number; pulled?: number; deleted?: number; conflicts?: number; error?: string;
  pendingDeletions?: { count: number; total: number };
  /** Tokens encrypted with safeStorage (desktop); false = plaintext in .learner/secrets.json. */
  encrypted: boolean;
  github: { connected: boolean; login?: string; repo?: string; pending?: { userCode: string; verificationUri: string; expiresAt: string }; error?: string };
  gdrive: { connected: boolean; email?: string; pending?: boolean; error?: string };
  /** The data folder pointer changed: the server must restart to use it. */
  restartRequired?: boolean;
  conflictCount: number;
  trashCount: number;
}

// ───────────────────────────── background jobs (subagents) ─────────────────────────────
// Each job is one headless `claude -p` process with its own system prompt, a restricted tool set and a per-job MCP
// endpoint (/mcp/job/:id) exposing the role's tools; one of them delivers the result. Records: .learner/jobs/<id>.json.

export type JobKind = "research" | "visual" | "demo" | "map";
export type JobStatus = "running" | "done" | "error" | "cancelled";

export type JobResult =
  | { kind: "research"; brief: string; sources: { title: string; url: string }[] } // brief = markdown
  | { kind: "visual"; svg: string; caption?: string }
  | { kind: "demo"; title: string; html: string; notes?: string } // single-file HTML, rendered in a sandboxed iframe
  | { kind: "map"; source: string; added: number; linked: number; chains: number };

export interface JobRecord {
  id: string;
  kind: JobKind;
  /** Short label for the activity dock ("Ricerca: …", "Mappa: 0003 …"). */
  title: string;
  /** Owning session: the result is shown there and handed to its teacher. Present on auto-map jobs started from a session
   *  too (their card is shown, but a map job is never delivered to the teacher). Absent for jobs started by the map UI. */
  sessionId?: string;
  /** Claude alias, or an agy model id. */
  model: string;
  status: JobStatus;
  /** Last activity line ("cerca: vue watch flush", "render SVG", …). */
  activity: string;
  createdAt: string;
  updatedAt: string;
  costUsd: number;
  /** Not carried by GET /api/jobs nor by the global `job` event (a demo can be 500 KB): fetch GET /api/jobs/:id. Session chat cards have it. */
  result?: JobResult;
  error?: string;
  /** The result has been handed to the owning session's teacher. */
  delivered?: boolean;
}

/** Teacher tool inputs (mcp__app__research / visual / demo): they return an ack at once; the result arrives later. */
export interface ResearchInput {
  question: string;
  context?: string;
}
export interface VisualInput {
  /** Everything the visual-maker needs: it sees nothing else of the lesson. */
  brief: string;
}
export interface DemoInput {
  title: string;
  brief: string;
}

// ───────────────────────────── map sources ─────────────────────────────
// The graph types (GraphNode, GraphChain, GraphSnapshot, …) live in app/shared/graph/.

/** A lesson as input of the map extractor. */
export interface GraphSource {
  /** `${course}/${lesson file stem}`, e.g. "vue-vetrine/0003-il-template-decide-cosa-esiste-…". Also ProvenancePoint.chatId. */
  id: string;
  course: string;
  lessonTitle: string;
  lessonNumber: number;
  lessonPath: string; // repo-relative
  /** Extracted at least once. */
  covered: boolean;
  /** Covered, but the lesson changed since (content fingerprint differs). */
  stale: boolean;
  /** Propositions born in this lesson that no chain uses; > 0 makes the lesson pending again (repair). */
  isolated: number;
}

/** Not extracted yet, changed since, or holding ideas no chain uses (the map must not show isolated points). */
export const needsMapping = (s: GraphSource) => !s.covered || s.stale || s.isolated > 0;
