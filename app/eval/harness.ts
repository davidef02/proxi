// Drives the real engine through a scenario: temp repo + temp data folder, the server in-process, the
// scripted learner on the other side of the tool cards. Outside CI: it costs a real engine run.
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { createServer, type RunningServer } from "../server/main.ts";
import type { AskAnswer, InteractiveItem, Provider, QuizAnswer, QuizInput, SessionState } from "../shared/protocol.ts";
import { isPureVue, targetsOf, type ProbeLog } from "./checks.ts";
import { buildFixture } from "./fixture.ts";
import type { Scenario } from "./scenarios.ts";

export interface RunResult {
  probes: ProbeLog[];
  curriculumTitles?: string[];
  reachedStop: boolean;
  transcript: string[];
}

const REPO = path.resolve(import.meta.dirname, "../..");
const NUDGE = "Vai avanti tu: scegli il prossimo passo.";
const ASK_TEXT = "Voglio saper costruire una piccola app Nuxt con pagine, layout e dati caricati dal server.";

export async function runScenario(
  s: Scenario,
  opts: { engine?: Provider; settingsFile: string; timeoutMs: number; log: (line: string) => void }
): Promise<RunResult> {
  process.env.LEARNER_ANKI_URL ??= "http://127.0.0.1:9"; // a real Anki must never answer the eval

  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "eval-repo-"));
  const dataRoot = fs.mkdtempSync(path.join(os.tmpdir(), "eval-data-"));
  let srv: RunningServer | undefined;
  let id: string | undefined;

  try {
    for (const rel of ["docs/agents/pedagogy.md", "docs/agents/anki-cards.md", "assets/lesson-template.html"]) {
      const dest = path.join(repoRoot, rel);
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.copyFileSync(path.join(REPO, rel), dest);
    }
    // The engine is whatever the app is configured with; --engine only patches the temp copy.
    const base = fs.existsSync(opts.settingsFile) ? JSON.parse(fs.readFileSync(opts.settingsFile, "utf-8")) : undefined;
    const settings = { ...(base ?? {}), ...(opts.engine ? { provider: opts.engine } : {}) };
    if (base !== undefined || opts.engine) {
      const dest = path.join(repoRoot, ".learner", "settings.json");
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.writeFileSync(dest, JSON.stringify(settings, null, 2) + "\n");
    }
    opts.log(`repo ${repoRoot}`);
    opts.log(`data ${dataRoot}`);

    await buildFixture(dataRoot, { graph: s.graph });
    srv = await createServer({ repoRoot, dataRoot, port: 0, openBrowser: false });
    const server = srv;
    id = await server.manager.create(s.config);
    const sessionId = id;

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
      const st: SessionState | undefined = server.manager.get(sessionId);
      if (!st) throw new Error("session disappeared");
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
          server.manager.answer(sessionId, { requestId: item.requestId, kind: "quiz", answer });
        } else if (item.kind === "ask") {
          const first = item.input.options?.[0];
          const answer: AskAnswer = first
            ? { selected: [first.value ?? first.label] }
            : { selected: [], other: ASK_TEXT };
          server.manager.answer(sessionId, { requestId: item.requestId, kind: "ask", answer });
        } else if (item.kind === "plan") {
          if (s.stopAt === "plan") {
            reachedStop = true;
            break;
          }
          server.manager.answer(sessionId, { requestId: item.requestId, kind: "plan", answer: { approved: true } });
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
        await server.manager.message(sessionId, NUDGE).catch(() => {});
      }

      if (reachedStop) break;
      await new Promise((r) => setTimeout(r, 500));
    }

    return { probes, curriculumTitles, reachedStop, transcript };
  } finally {
    if (srv && id) await srv.manager.close(id).catch(() => {});
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
