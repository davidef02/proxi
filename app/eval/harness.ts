// Drives the real engine through a scenario: temp repo + temp data folder, the `proxi` binary over HTTP/SSE,
// the scripted learner on the other side of the tool cards. Outside CI: it costs a real engine run.
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { AnswerBody, AskAnswer, InteractiveItem, Provider, QuizAnswer, QuizInput, SessionState } from "../shared/protocol.ts";
import { spawnProxi, type RunningProxi } from "../test/binary.ts";
import { isPureVue, targetsOf, type ProbeLog } from "./checks.ts";
import { buildFixture } from "./fixture.ts";
import type { Scenario } from "./scenarios.ts";

export interface RunResult {
  probes: ProbeLog[];
  curriculumTitles?: string[];
  reachedStop: boolean;
  transcript: string[];
  /** SSE events received (the snapshot alone counts). */
  events: number;
}

const REPO = path.resolve(import.meta.dirname, "../..");
const NUDGE = "Vai avanti tu: scegli il prossimo passo.";
const ASK_TEXT = "Voglio saper costruire una piccola app Nuxt con pagine, layout e dati caricati dal server.";

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** Copy the learner's settings into the temp repo; `--engine` patches only the copy. */
export function writeEvalSettings(repoRoot: string, settingsFile: string, engine?: Provider): void {
  const base = fs.existsSync(settingsFile) ? JSON.parse(fs.readFileSync(settingsFile, "utf-8")) : undefined;
  if (base === undefined && !engine) return;
  const settings: Record<string, unknown> = { ...(base ?? {}), ...(engine ? { provider: engine } : {}) };
  // The binary syncs on startup when syncProvider is set and the keychain holds a token: the temp
  // data folder must never reach the learner's real remote.
  delete settings.syncProvider;
  const dest = path.join(repoRoot, ".learner", "settings.json");
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, JSON.stringify(settings, null, 2) + "\n");
}

/** `proxi serve` for one scenario: the temp folder pair, no update check, an Anki port nobody listens on. */
export function evalServeArgs(repoRoot: string, dataRoot: string, claude?: { cmd: string; args: string[] }): string[] {
  const args = [
    "serve",
    "--port", "0",
    "--repo-root", repoRoot,
    "--data-root", dataRoot,
    "--no-update-check",
    "--anki-url", "http://127.0.0.1:9",
  ];
  if (claude) {
    args.push("--claude-bin", claude.cmd);
    for (const arg of claude.args) args.push("--claude-bin-arg", arg);
  }
  return args;
}

export async function runScenario(
  s: Scenario,
  opts: {
    bin: string;
    engine?: Provider;
    settingsFile: string;
    timeoutMs: number;
    claude?: { cmd: string; args: string[] };
    log: (line: string) => void;
  }
): Promise<RunResult> {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "eval-repo-"));
  const dataRoot = fs.mkdtempSync(path.join(os.tmpdir(), "eval-data-"));
  let srv: RunningProxi | undefined;
  let id: string | undefined;
  let base = "";
  const stream = new AbortController();

  const post = (p: string, body: unknown = {}) =>
    fetch(base + p, {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  const postOk = async (p: string, body: AnswerBody) => {
    const res = await post(p, body);
    if (!res.ok) throw new Error(`${p.split("/").pop()} ${res.status}: ${(await res.json().catch(() => ({}))).error ?? ""}`);
  };
  const getState = async (sessionId: string): Promise<SessionState> => {
    const res = await fetch(`${base}/api/sessions/${sessionId}`);
    if (!res.ok) throw new Error(res.status === 404 ? "session disappeared" : `session ${res.status}`);
    return (await res.json()) as SessionState;
  };

  try {
    for (const rel of ["docs/agents/pedagogy.md", "docs/agents/anki-cards.md", "assets/lesson-template.html"]) {
      const dest = path.join(repoRoot, rel);
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.copyFileSync(path.join(REPO, rel), dest);
    }
    // The engine is whatever the app is configured with; --engine only patches the temp copy.
    writeEvalSettings(repoRoot, opts.settingsFile, opts.engine);
    opts.log(`repo ${repoRoot}`);
    opts.log(`data ${dataRoot}`);

    await buildFixture(dataRoot, { graph: s.graph });
    srv = await spawnProxi(evalServeArgs(repoRoot, dataRoot, opts.claude), opts.bin);
    base = srv.url;

    const created = await post("/api/sessions", s.config);
    if (!created.ok) throw new Error(`create ${created.status}: ${(await created.json().catch(() => ({}))).error ?? ""}`);
    id = ((await created.json()) as { id: string }).id;
    const sessionId = id;
    opts.log(`session ${sessionId}`);

    // The stream only wakes the loop early: the GET below stays authoritative.
    let events = 0;
    let wake: () => void = () => {};
    const nextEvent = () =>
      new Promise<void>((resolve) => {
        wake = () => resolve();
      });
    void (async () => {
      const res = await fetch(`${base}/api/sessions/${sessionId}/events`, { signal: stream.signal });
      const dec = new TextDecoder();
      let buf = "";
      for await (const chunk of res.body as unknown as AsyncIterable<Uint8Array>) {
        buf += dec.decode(chunk, { stream: true });
        let i: number;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, i);
          buf = buf.slice(i + 2);
          const data = block.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join("");
          if (!data) continue;
          events++;
          wake();
        }
      }
    })().catch(() => {});

    const probes: ProbeLog[] = [];
    const transcript: string[] = [];
    let reachedStop = false;
    let curriculumTitles: string[] | undefined;
    const answered = new Set<string>();
    const finalised = new Set<string>();
    let nudges = 0;
    let fallbackProbes = 0;

    const quizAnswer = (input: QuizInput): { answer: QuizAnswer; label: string } => {
      const targets = targetsOf(input.question);
      const correct: QuizAnswer = {
        selected: Array.isArray(input.correctAnswer) ? [...input.correctAnswer] : [input.correctAnswer],
        dontKnow: false,
        confidence: 90,
      };
      if (isPureVue(input.question) && targets.length > 0 && targets.every((c) => c.status === "solid" || c.status === "taught")) {
        return { answer: correct, label: "corretto (90)" };
      }
      if (targets.some((c) => c.status === "shaky")) return { answer: { ...correct, confidence: 20 }, label: "corretto (20)" };
      if (targets.some((c) => c.status === "missed")) return { answer: { selected: [], dontKnow: true }, label: "non lo so" };
      // Nuxt question, or nothing the fixture knows: bracket the edge on the first one, then stop guessing.
      if (input.kind !== "probe") return { answer: { ...correct, confidence: 80 }, label: "corretto (80)" };
      if (fallbackProbes++ === 0) return { answer: { ...correct, confidence: 80 }, label: "corretto (80)" };
      return { answer: { selected: [], dontKnow: true }, label: "non lo so" };
    };

    const deadline = Date.now() + opts.timeoutMs;
    while (!reachedStop) {
      if (Date.now() > deadline) throw new Error("timeout");
      const st = await getState(sessionId);
      if (st.status === "error") throw new Error(st.lastError ?? "engine error");
      if (st.status === "closed") break;

      for (const it of st.items) {
        if (it.kind !== "teacher" || it.streaming || finalised.has(it.id) || !it.text.trim()) continue;
        finalised.add(it.id);
        transcript.push(it.text);
        opts.log(`  · ${it.text.slice(0, 200).replace(/\s+/g, " ")}`);
      }

      const item = st.pendingRequestId
        ? (st.items.find((x) => "requestId" in x && x.requestId === st.pendingRequestId) as InteractiveItem | undefined)
        : undefined;

      if (item && !answered.has(item.requestId) && !isAnswered(item)) {
        answered.add(item.requestId);
        if (item.kind === "quiz") {
          const { answer, label } = quizAnswer(item.input);
          if (item.input.kind === "probe") {
            probes.push({
              question: item.input.question,
              options: item.input.options.map((o) => o.label),
              answer: label,
            });
            const where = targetsOf(item.input.question).map((c) => c.id).join(",") || "nuxt";
            opts.log(`PROBE ${probes.length} [${where}] ${item.input.question.replace(/\s+/g, " ")} -> ${label}`);
          }
          await postOk(`/api/sessions/${sessionId}/answer`, { requestId: item.requestId, kind: "quiz", answer });
        } else if (item.kind === "ask") {
          const first = item.input.options?.[0];
          const answer: AskAnswer = first
            ? { selected: [first.value ?? first.label] }
            : { selected: [], other: ASK_TEXT };
          await postOk(`/api/sessions/${sessionId}/answer`, { requestId: item.requestId, kind: "ask", answer });
        } else if (item.kind === "plan") {
          if (s.stopAt === "plan") {
            reachedStop = true;
            break;
          }
          await postOk(`/api/sessions/${sessionId}/answer`, {
            requestId: item.requestId,
            kind: "plan",
            answer: { approved: true },
          });
        } else {
          curriculumTitles = item.input.lessons.map((l) => l.title);
          opts.log(`CURRICULUM ${curriculumTitles.join(" | ")}`);
          reachedStop = true;
          break;
        }
      } else if (!item && st.status === "idle") {
        if (nudges >= 6) break;
        nudges++;
        opts.log(`nudge ${nudges}/6`);
        await post(`/api/sessions/${sessionId}/message`, { text: NUDGE }).catch(() => {});
      }

      if (reachedStop) break;
      await Promise.race([sleep(500), nextEvent()]);
    }

    opts.log(`sse events: ${events}`);
    return { probes, curriculumTitles, reachedStop, transcript, events };
  } finally {
    stream.abort();
    if (srv && id) await post(`/api/sessions/${id}/close`).then((r) => r.ok && opts.log(`closed ${id}`)).catch(() => {});
    if (srv) await srv.close().catch(() => {});
    if (process.env.EVAL_KEEP) {
      opts.log(`kept ${repoRoot} and ${dataRoot}`);
    } else {
      fs.rmSync(repoRoot, { recursive: true, force: true, maxRetries: 3 });
      fs.rmSync(dataRoot, { recursive: true, force: true, maxRetries: 3 });
    }
  }
}

function isAnswered(it: InteractiveItem): boolean {
  if (it.kind === "quiz") return !!it.result || !!it.cancelled;
  if (it.kind === "ask") return !!it.answer || !!it.cancelled;
  return !!it.decision || !!it.cancelled;
}
