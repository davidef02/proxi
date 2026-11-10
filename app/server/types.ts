// Internal contracts between server modules. Each module owner codes against these.
import type { z } from "zod";
import type {
  AppToolName,
  ChatItem,
  InteractiveItem,
  PlanState,
  ProgressRecord,
  ServerEvent,
  SessionConfig,
  ExerciseInfo,
  AnswerBody,
  JobKind,
  JobRecord,
  JobResult,
  Provider,
  QuotaLimits,
  EffortLevel,
} from "../shared/protocol.ts";

// ───────────── claude.ts / stream.ts : the claude process ─────────────

export type { Provider };
/** Executable of an engine (tests: node + a fake script). */
export interface EngineBin {
  cmd: string;
  args?: string[];
}
/** Built-in capabilities a session or job may use, engine-neutral; each adapter maps them to its own tool names.
 *  read = read/list/search files, web = web search (+ fetch where the engine allows it). */
export type Grant = "read" | "web";

/** What a session or job asks of an engine (intent, not flags). spawnEngine() (engine.ts) picks the adapter. */
export interface EngineLaunch {
  provider: Provider;
  bin: EngineBin;
  /** Repo root: readable by the engine. */
  repoRoot: string;
  /** Data folder (courses, graph, decks, curriculum.json): readable by the engine; undefined = repoRoot. */
  dataRoot?: string;
  /** App-owned dir of this session/job; the adapter writes its prompt/config files here (and agy runs in it). */
  scratchDir: string;
  /** Engine model id, from modelFor() (engine.ts). */
  model: string;
  effort?: EffortLevel;
  systemPrompt: string;
  grant: Grant[];
  /** The app's MCP endpoint ("app" for sessions, "job" for jobs: tools appear as mcp__<name>__<tool>). */
  mcp: { name: "app" | "job"; url: string; token: string };
  /** Extra readable dirs. */
  addDirs: string[];
  /** Tool steps per turn before the turn is cut (background jobs). */
  maxTurns?: number;
  /** Resume token from an earlier run's init/result event (opaque, adapter-owned); undefined = new conversation. */
  resume?: string;
}

/** Normalised engine output (stream.ts for claude, parseAgyLine in agy.ts). `sessionId` of init/result is the resume token. */
export type EngineEvent =
  | { type: "init"; sessionId: string; tools: string[]; mcpServers: { name: string; status: string }[] }
  /** content_block_start of a text block (partial messages). */
  | { type: "text-start"; messageId: string; index: number }
  | { type: "text-delta"; messageId: string; index: number; text: string }
  /** Complete assistant message: authoritative text per block index and tool uses. */
  | {
      type: "assistant";
      messageId: string;
      texts: { index: number; text: string }[];
      toolUses: { index: number; id: string; name: string; input: unknown }[];
    }
  | { type: "tool-result"; toolUseId: string; isError: boolean; text: string }
  | {
      type: "result";
      isError: boolean;
      subtype: string;
      text?: string;
      sessionId: string;
      inputTokens: number;
      outputTokens: number;
      cacheReadTokens: number;
      cacheCreationTokens: number;
      costUsd: number;
    }
  | { type: "control-response"; requestId: string; ok: boolean; error?: string }
  /** The resume token was refused: the process exits next (onExit follows); the next launch must be fresh. */
  | { type: "resume-failed" }
  /** Authentication failed: invalid token, not logged in, or expired session. */
  | { type: "auth-failed"; message?: string }
  | { type: "limits"; limits: QuotaLimits }
  /** The engine refused a tool action on its own (agy permission rules): the turn may end without it. */
  | { type: "action-denied"; action: string }
  | { type: "ignored" };

export interface EngineHandle {
  /** Write one user turn. Turns are processed in order. */
  send(text: string): void;
  /** Abort the running turn; it ends with a result event (isError). The handle stays usable. */
  interrupt(): void;
  /** Kill the process tree (Windows: taskkill /T /F). Resolves on exit, or after 5 s at most. */
  stop(): Promise<void>;
  /** The live process, if any (agy: undefined between an interrupt and the next send). */
  readonly pid: number | undefined;
}

export interface EngineCallbacks {
  onEvent(e: EngineEvent): void;
  /** stderr lines (diagnostics). */
  onStderr(line: string): void;
  /** Exactly once, when the handle is dead and every process it started has exited: on its own, or after stop(). */
  onExit(code: number | null, signal: string | null): void;
}

// spawnEngine(launch: EngineLaunch, cb: EngineCallbacks): EngineHandle   — engine.ts (spawnClaude / spawnAgy)
// parseStreamLine(line: string, state: StreamParseState): EngineEvent[]  — stream.ts (pure, tested)

// ───────────── tools/ + mcp.ts : app tools over MCP ─────────────

export interface ToolOutcome {
  text: string;
  isError?: boolean;
  /** Extra MCP image content (base64), e.g. a rendered SVG or a demo screenshot the subagent must look at. */
  images?: { data: string; mimeType: string }[];
}

type DistOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

/** Everything a tool may touch. Implemented by the session (sessions.ts). */
export interface ToolContext {
  repoRoot: string; // absolute: code + .learner/ (settings)
  dataRoot: string; // absolute: courses/, graph/, decks/, curriculum.json live here
  sessionId: string;
  config: SessionConfig;
  /** Repo-relative lesson artifact path once it exists. */
  getLessonPath(): string | undefined;
  setLessonPath(p: string): void;
  getPlan(): PlanState | null;
  setPlan(p: PlanState): void; // also emits {type:"plan"}
  /** Push an interactive item and wait for the learner's answer. Rejects with Error("cancelled") on interrupt/close. */
  request<T extends AnswerBody["answer"]>(item: DistOmit<InteractiveItem, "id" | "at" | "requestId">): Promise<T>;
  /** Push / upsert a non-blocking item (id and at are filled in when missing). */
  emit(item: DistOmit<ChatItem, "id" | "at"> & { id?: string }): string;
  emitEvent(e: ServerEvent): void;
  logProgress(rec: Omit<ProgressRecord, "at" | "sessionId" | "course">): Promise<void>;
  setExercises(list: ExerciseInfo[]): void; // also emits {type:"exercises"}
  getExercises(): ExerciseInfo[];
  /** Background subagents. The job is owned by this session: its card appears in the chat and the result is handed
   *  to the teacher as a learner-side turn once the teacher is idle. */
  jobs: JobStarter;
}

export interface AppTool<I = unknown> {
  name: AppToolName;
  /** Mechanical description only; teaching craft lives in the system prompt. */
  description: string;
  schema: z.ZodType<I>;
  run(input: I, ctx: ToolContext): Promise<ToolOutcome>;
  /** Writes lessons, the plan, exercises or records: toolsFor raises coursesChanged after a successful run. */
  writes?: true;
}

// toolsFor(config: SessionConfig, dataRoot: string): AppTool[]                         — tools/index.ts
// handleMcp(body: unknown, tools: AppTool[], ctx: ToolContext): Promise<object | null> — mcp.ts (null = notification → HTTP 202)

// ───────────── context.ts : system prompt ─────────────
// buildSystemPrompt(opts: { config: SessionConfig; repoRoot: string; dataRoot?: string; lessonPath?: string; sessionId: string; today: string }): Promise<string>
// openingMessage(config: SessionConfig): string   — first user turn sent automatically

// ───────────── domain helpers ─────────────
// The domain helpers take the data root (repoRoot is only for .learner/ and docs/): dataRoot = the folder holding courses/.
// courses.ts:    listCourses(dataRoot): Promise<CourseOverview[]>
// knowledge.ts:  buildKnowledgeMap(dataRoot): Promise<KnowledgeMap>; readProgress(dataRoot, course): Promise<ProgressRecord[]>; appendProgress(dataRoot, rec: ProgressRecord): Promise<void>; purgeProgress(dataRoot): void (drops practice/exercise lines once at server start)
// exercises.ts:  listExercises(dataRoot, course): Promise<ExerciseInfo[]>; runExercise(dataRoot, course, name): Promise<ExerciseRunResult>; openInEditor(absDir): void
// planstate.ts:  derivePlanState(base: {topic, goal, dag, approved}, lessonNodeCount: number, progress: ProgressRecord[]): PlanState; parseDagIds(dag): string[]

// ───────────── jobs/ : background subagents ─────────────

/** What a job tool may touch. Implemented by the job manager (jobs/index.ts). */
export interface JobContext {
  repoRoot: string;
  dataRoot: string; // absolute: courses/, graph/, decks/, curriculum.json live here
  jobId: string;
  /** Updates JobRecord.activity (shown live in the activity dock). */
  setActivity(text: string): void;
  /** Delivers the result: the job becomes "done"; the claude process is stopped after its turn. */
  finish(result: JobResult): void;
}

export interface JobTool<I = unknown> {
  /** Name as the subagent sees it: mcp__job__<name>. */
  name: string;
  description: string;
  schema: z.ZodType<I>;
  run(input: I, ctx: JobContext): Promise<ToolOutcome>;
}

/** One subagent kind. research/visual/demo live in jobs/roles/, map in graph/extract.ts. */
export interface JobRole<I = unknown> {
  kind: JobKind;
  /** Built-in capabilities; [] = only the MCP tools. The model comes from modelFor() (engine.ts). */
  grant: Grant[];
  /** Tool steps before the job's turn is cut. */
  maxTurns: number;
  systemPrompt(input: I, dataRoot: string): Promise<string>;
  /** The single user turn the job starts with. */
  firstMessage(input: I): string;
  tools(input: I): JobTool<any>[];
  /** Called once when the job settles in any state: drop per-job in-memory state (drafts). */
  cleanup?(jobId: string): void;
}

export interface JobStarter {
  /** Starts at once (bounded concurrency: queued jobs show activity "in coda"). Throws on an unknown kind. */
  start<I>(kind: JobKind, input: I, opts: { title: string; sessionId?: string }): JobRecord;
  list(): JobRecord[];
  get(id: string): JobRecord | undefined;
  cancel(id: string): void;
  /** The finished result was handed to the owning session's teacher. */
  markDelivered(id: string): void;
}

// ───────────── graph/ : the map ledgers (store.ts), used by the map extractor role ─────────────

/** What the map extractor submits for one lesson (mcp__job__submit_findings). Local ids "p1", "p2"…; existing
 *  propositions are referenced by their "n_…" id. */
export interface Findings {
  propositions: {
    id: string; // local "pN"
    name: string;
    statement: string;
    aliases?: string[];
    /** Lesson node id the idea comes from (ProvenancePoint.messageId). */
    nodeId?: string;
    /** Same idea as this existing proposition: link instead of creating. */
    linkTo?: string;
  }[];
  chains: { premises: string[]; conclusion: string }[]; // ids: local "pN" or existing "n_…"
  /** Repair only: isolated "n_…" ideas listed to the extractor that are not load-bearing; they leave the map. */
  remove?: string[];
}

// store.ts (graph):
//   graphDigest(repoRoot): Promise<string>                                   one line per proposition: "n_id | name | statement"
//   validateFindings(f: Findings, existingIds: Set<string>): string[]         errors, [] = ok
//   mergeFindings(repoRoot, sourceId: string, f: Findings): Promise<{ added: number; linked: number; chains: number }>
//   readSnapshot(repoRoot): Promise<GraphSnapshot>
