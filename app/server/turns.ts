// One session's turn state machine: launch / send / relaunch / interrupt / watchdog, and the job results owed to the
// teacher riding along with the next turn. Driven only by EngineHandle + EngineEvent: an in-memory handle tests it.
import type { AttachmentMeta, JobRecord, SessionStatus } from "../shared/protocol.ts";
import { jobTurnText } from "./jobs/deliver.ts";
import { CLAUDE_AUTH_ERROR_MESSAGE, CLAUDE_AUTH_ERROR_RE } from "./claude.ts";
import type { EngineCallbacks, EngineEvent, EngineHandle } from "./types.ts";

const JOB_SEP = "\n\n---\n\n";
const BUSY = new Set<SessionStatus>(["running", "waiting", "starting"]);
/** An interrupted turn that never reports its result is given up after this long. */
export const WATCHDOG_MS = 10_000;

export interface LastTurnInput {
  text?: string;
  show: string[];
  attachments?: AttachmentMeta[];
  turnText: string;
  pushedLearner?: boolean;
}

/** What the turn module needs from its session (SessionManager implements it). */
export interface TurnHost {
  /** Starts the engine; resume = token from an earlier init/result (undefined: new conversation). Throws when it cannot. */
  spawn(resume: string | undefined, cb: EngineCallbacks): Promise<EngineHandle>;
  /** Engine events to render. late = result of a turn already given up: count it, leave the current turn alone. */
  event(e: EngineEvent, late: boolean): void;
  status(): SessionStatus;
  setStatus(status: SessionStatus, error?: string): void;
  notice(level: "info" | "error", text: string): void;
  /** A learner bubble: pushed only once its turn really goes out. */
  learner(text: string, attachments?: AttachmentMeta[]): void;
  /** Interactive cards blocking the teacher; cancelPending() rejects them all. */
  pending(): number;
  cancelPending(): void;
  /** This session's jobs (the one source of what is owed) and the ack once a result went out. */
  jobs: { list(): JobRecord[]; markDelivered(id: string): void };
}

/** A settled, non-map job whose result the teacher has not received yet. */
export const isOwed = (j: JobRecord) => j.kind !== "map" && !j.delivered && (j.status === "done" || j.status === "error");

export class Turns {
  private handle?: EngineHandle;
  private stopped = false;
  /** Turns sent / results received by the current handle: results < sends = a given-up turn's result is still due. */
  private sends = 0;
  private results = 0;
  private interrupting = false;
  private watchdog?: NodeJS.Timeout;
  /** The last turn sent: re-sent on a fresh conversation when the engine refuses the resume token. */
  private lastSent?: string;
  private lastTurn?: LastTurnInput;
  private _canRetry = false;
  private refused = false;
  private authFailed = false;
  private authNoticeEmitted = false;
  /** Last stderr lines of the current process. */
  readonly stderr: string[] = [];

  /** resume: the persisted token (undefined = the first launch starts a new conversation). */
  constructor(
    private host: TurnHost,
    public resume?: string,
    lastTurn?: LastTurnInput,
    canRetry = false,
  ) {
    this.lastTurn = lastTurn;
    this._canRetry = canRetry;
  }

  get pid(): number | undefined {
    return this.handle?.pid;
  }
  /** An engine process is up (or being relaunched by the adapter). */
  get live(): boolean {
    return !!this.handle;
  }

  get canRetry(): boolean {
    return this._canRetry && !this.stopped && !!this.lastTurn;
  }

  get savedLastTurn(): LastTurnInput | undefined {
    return this.lastTurn;
  }

  /** One turn: launches the engine if there is none (status "starting"; on failure "error" + throw), pushes the
   *  learner bubbles, sends owed job results + `text` as ONE turn ("running"), marks those results delivered.
   *  The caller has checked the session is free. text undefined = only the owed results. */
  async send(text: string | undefined, show: string[], attachments?: AttachmentMeta[]): Promise<void> {
    const owed = this.host.jobs.list().filter(isOwed);
    let turnText = text;
    if (attachments && attachments.length > 0) {
      const attLines = attachments.map((a) => `- ${a.name} (percorso: ${a.path})`).join("\n");
      const attBlock = `[Allegati forniti dallo studente:\n${attLines}]`;
      turnText = turnText !== undefined ? `${attBlock}\n\n${turnText}` : attBlock;
    }
    const fullText = [...owed.map(jobTurnText), ...(turnText === undefined ? [] : [turnText])].join(JOB_SEP);
    this.lastTurn = { text, show, attachments, turnText: fullText, pushedLearner: false };
    this._canRetry = false;

    if (!this.handle) {
      this.host.setStatus("starting");
      try {
        await this.launch();
      } catch (e: any) {
        this._canRetry = true;
        if (!this.stopped) this.host.setStatus("error", e.message);
        throw e;
      }
    }
    for (let i = 0; i < show.length; i++) {
      const atts = i === show.length - 1 ? attachments : undefined;
      this.host.learner(show[i], atts);
    }
    this.lastTurn.pushedLearner = true;
    this.write(fullText);
    this.host.setStatus("running");
    for (const j of owed) this.host.jobs.markDelivered(j.id);
  }

  /** Re-submits the last failed turn without duplicating learner bubbles. */
  async retry(): Promise<void> {
    if (this.stopped) throw new Error("Session is closed");
    if (!this.canRetry || !this.lastTurn) throw new Error("Nessun turno fallito da riprovare");

    this._canRetry = false;
    const input = this.lastTurn;

    if (!this.handle) {
      this.host.setStatus("starting");
      try {
        await this.launch();
      } catch (e: any) {
        this._canRetry = true;
        if (!this.stopped) this.host.setStatus("error", e.message);
        throw e;
      }
    }

    if (!input.pushedLearner) {
      for (let i = 0; i < input.show.length; i++) {
        const atts = i === input.show.length - 1 ? input.attachments : undefined;
        this.host.learner(input.show[i], atts);
      }
      input.pushedLearner = true;
    }

    this.write(input.turnText);
    this.host.setStatus("running");
  }

  /** Hands owed job results to an idle teacher (relaunching the engine if needed); no-op when busy or nothing is owed.
   *  A failed launch leaves status "error" and the results owed: they ride along with the next learner message. */
  deliver(): void {
    if (this.stopped || this.host.status() !== "idle" || !this.host.jobs.list().some(isOwed)) return;
    void this.send(undefined, []).catch(() => {});
  }

  /** Aborts the running turn and cancels its cards; the turn ends with its (error) result, or after WATCHDOG_MS. */
  interrupt(): void {
    const st = this.host.status();
    const busy = (st === "running" || st === "waiting") && !!this.handle; // no process (Anki card only): nothing to abort
    if (busy) this.interrupting = true;
    this._canRetry = false;
    this.lastTurn = undefined;
    this.handle?.interrupt();
    this.host.cancelPending();
    if (!busy) return;
    clearTimeout(this.watchdog);
    this.watchdog = setTimeout(() => {
      if (!this.interrupting) return;
      this.interrupting = false;
      this.host.notice("info", "Turno interrotto.");
      if (BUSY.has(this.host.status())) this.host.setStatus("idle");
    }, WATCHDOG_MS);
    this.watchdog.unref();
  }

  /** Close/shutdown: no launch ever again; kills the engine (resolves on its exit, 5 s at most). */
  async stop(): Promise<void> {
    this.stopped = true;
    clearTimeout(this.watchdog);
    const h = this.handle;
    this.handle = undefined;
    await h?.stop().catch(() => {});
  }

  private write(text: string) {
    this.lastSent = text;
    this.sends++;
    this.handle!.send(text);
  }

  private async launch() {
    if (this.stopped) throw new Error("Session is closed");
    let h: EngineHandle | undefined;
    const mine = () => !!h && this.handle === h; // a stale or stopped handle is ignored
    this.stderr.length = 0;
    const handle = await this.host.spawn(this.resume, {
      onEvent: (e) => mine() && this.onEvent(e),
      onStderr: (line) => {
        if (!mine()) return;
        this.stderr.push(line);
        if (this.stderr.length > 20) this.stderr.shift();
        if (CLAUDE_AUTH_ERROR_RE.test(line)) this.authFailed = true;
      },
      onExit: (code, signal) => mine() && this.onExit(code, signal),
    });
    if (this.stopped) {
      void handle.stop();
      throw new Error("Session is closed");
    }
    h = this.handle = handle;
    this.sends = this.results = 0;
    this.refused = false;
    this.authFailed = false;
    this.authNoticeEmitted = false;
  }

  private async relaunchFresh(text: string) {
    this.host.notice("info", "Ripresa della conversazione non riuscita: riparto da una sessione nuova con il contesto della lezione.");
    this.host.setStatus("starting");
    try {
      await this.launch();
      this.write(text);
      this.host.setStatus("running");
    } catch (e: any) {
      if (!this.stopped) this.host.setStatus("error", e.message);
    }
  }

  private onEvent(e: EngineEvent) {
    if (e.type === "auth-failed") {
      this.authFailed = true;
      const msg = e.message || CLAUDE_AUTH_ERROR_MESSAGE;
      if (!this.authNoticeEmitted) {
        this.authNoticeEmitted = true;
        this.host.notice("error", msg);
      }
      this.host.setStatus("error", msg);
      void this.stop();
      return;
    }
    if (e.type === "resume-failed") {
      this.refused = true;
      this.resume = undefined; // the next launch is fresh, so this cannot loop
      return;
    }
    if (e.type === "action-denied") return this.host.notice("info", `Il motore ha bloccato un'azione (${e.action}).`);
    if ((e.type === "init" || e.type === "result") && e.sessionId) this.resume = e.sessionId;
    if (e.type !== "result") return this.host.event(e, false);
    const late = ++this.results < this.sends;
    this.host.event(e, late);
    if (late) return;
    if (this.interrupting) {
      this.host.notice("info", "Turno interrotto.");
      this._canRetry = false;
      this.lastTurn = undefined;
    } else if (e.isError && !this.authFailed) {
      const errMsg = `Errore del docente (${e.subtype})${e.text ? ": " + e.text : ""}`;
      this.host.notice("error", errMsg);
      this._canRetry = true;
      this.interrupting = false;
      clearTimeout(this.watchdog);
      if (this.host.pending() === 0) {
        this.host.setStatus("error", errMsg);
      }
      return;
    } else {
      this._canRetry = false;
      this.lastTurn = undefined;
    }
    this.interrupting = false;
    clearTimeout(this.watchdog);
    if (this.host.pending() === 0 && !this.authFailed) {
      this.host.setStatus("idle");
      this.deliver();
    }
  }

  private onExit(code: number | null, signal: string | null) {
    this.handle = undefined;
    this.interrupting = false;
    clearTimeout(this.watchdog);
    this.host.cancelPending();
    if (this.authFailed || CLAUDE_AUTH_ERROR_RE.test(this.stderr.join(" "))) {
      this.authFailed = true;
      if (!this.authNoticeEmitted) {
        this.authNoticeEmitted = true;
        this.host.notice("error", CLAUDE_AUTH_ERROR_MESSAGE);
      }
      this.host.setStatus("error", CLAUDE_AUTH_ERROR_MESSAGE);
      return;
    }
    if (this.refused && this.lastSent !== undefined && !this.stopped) return void this.relaunchFresh(this.lastSent);
    if (this.sends > this.results && !this.stopped) {
      this._canRetry = true;
    }
    const tail = this.stderr.slice(-5).join("\n");
    this.host.setStatus("error", `teacher process exited (${signal ?? code})` + (tail ? ": " + tail : ""));
  }
}
