import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { createHash } from "node:crypto";
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import type { ExerciseInfo, ExerciseRunResult, ExerciseWorkspace } from "../shared/protocol.ts";

const NAME_RE = /^(\d{2}-|zz-)/;
const ANSI_RE = /\u001b\[[0-9;]*[A-Za-z]/g;
const NAME_SAFE = /^[A-Za-z0-9._-]+$/;
const EXT = new Set([".vue", ".js", ".ts", ".json", ".css", ".md"]);
const TEST_RE = /\.(test|spec)\.[^/]+$/i; // NTFS is case-insensitive: Esercizio.TEST.ts is still a test
const LASTRUN_FILE = ".app-lastrun.json";
/** saveExerciseFile error when the file changed on disk since the version the editor loaded (HTTP 409, code "stale"). */
export const CONFLICT = "Il file è cambiato fuori dall'editor";
export const HINTS_FILE = "SUGGERIMENTI.json";
export const INSTRUCTIONS_FILE = "ISTRUZIONI.md";
const MAX_FILE = 200 * 1024;
const exDir = (repoRoot: string, course: string) => path.join(path.resolve(repoRoot), "courses", course, "exercises");

/** Validate a learner-exercise file path (relative, forward slashes, allowed extension); returns an error string or null. */
export function badExercisePath(p: string): string | null {
  if (!p || p.includes("\\") || p.includes("\0")) return `Il percorso "${p}" deve essere relativo, con barre dirette (/)`;
  if (p.startsWith("/") || /^[a-zA-Z]:/.test(p)) return `Il percorso "${p}" deve essere relativo (niente percorsi assoluti o lettere di unità)`;
  if (p.split("/").some((s) => s === ".." || s === "." || s === "")) return `Il percorso "${p}" non può contenere "..", "." o segmenti vuoti`;
  if (!EXT.has(path.extname(p).toLowerCase())) return `Il percorso "${p}": l'estensione deve essere una tra ${[...EXT].join(" ")}`;
  return null;
}

export interface HintsFile { hints: string[]; final: boolean; revealed: number }

export function readHints(dir: string): HintsFile | null {
  try {
    const h = JSON.parse(fs.readFileSync(path.join(dir, HINTS_FILE), "utf-8"));
    if (!Array.isArray(h.hints)) return null;
    return { hints: h.hints.map(String), final: !!h.final, revealed: Math.max(0, Math.min(h.hints.length, Number(h.revealed) || 0)) };
  } catch {
    return null;
  }
}

export function writeHints(dir: string, h: HintsFile): void {
  fs.writeFileSync(path.join(dir, HINTS_FILE), JSON.stringify(h, null, 2) + "\n", "utf-8");
}

/** Absolute dir of an existing exercise, or null (bad name, missing). */
function exerciseDirOf(repoRoot: string, course: string, name: string): string | null {
  if (!NAME_SAFE.test(name) || name === "." || name === ".." || name === "node_modules") return null;
  const dir = path.join(exDir(repoRoot, course), name);
  return fs.existsSync(dir) && fs.statSync(dir).isDirectory() ? dir : null;
}

const isHidden = (rel: string) => [INSTRUCTIONS_FILE, HINTS_FILE].some((f) => f.toLowerCase() === rel.toLowerCase());

/** Content hash of a file ("" when missing): the editor sends it back as baseVersion on save. */
export function fileVersion(abs: string): string {
  try {
    return createHash("sha1").update(fs.readFileSync(abs)).digest("hex");
  } catch {
    return "";
  }
}

function walk(dir: string, rel = "", depth = 0): string[] {
  const out: string[] = [];
  for (const e of fs.readdirSync(path.join(dir, rel), { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (e.name.startsWith(".") || e.name === "node_modules") continue;
    const r = rel ? `${rel}/${e.name}` : e.name;
    if (e.isDirectory()) {
      if (depth < 3) out.push(...walk(dir, r, depth + 1));
    } else if (e.isFile() && EXT.has(path.extname(e.name).toLowerCase()) && !isHidden(r)) out.push(r);
  }
  return out;
}

/** Files for the in-app editor: tests are readOnly, everything else is editable. */
export async function getWorkspace(repoRoot: string, course: string, name: string): Promise<ExerciseWorkspace | null> {
  const dir = exerciseDirOf(repoRoot, course, name);
  const info = dir && (await listExercises(repoRoot, course)).find((e) => e.name === name);
  if (!dir || !info) return null;
  const files = walk(dir)
    .filter((p) => fs.statSync(path.join(dir, p)).size <= MAX_FILE)
    .map((p) => ({ path: p, content: fs.readFileSync(path.join(dir, p), "utf-8"), version: fileVersion(path.join(dir, p)), readOnly: TEST_RE.test(p) }));
  return { info, files };
}

/** Save one learner file; returns an error message or null. Editable files only, or new files with allowed extensions. */
export function saveExerciseFile(repoRoot: string, course: string, name: string, rel: string, content: string, baseVersion?: string): string | null {
  const dir = exerciseDirOf(repoRoot, course, name);
  if (!dir) return "Esercizio sconosciuto";
  const bad = badExercisePath(rel);
  if (bad) return bad;
  if (rel.split("/").some((s) => s === "node_modules" || s.startsWith("."))) return "Percorso non consentito";
  if (TEST_RE.test(rel) || isHidden(rel)) return "File di sola lettura";
  if (Buffer.byteLength(content) > MAX_FILE) return "File troppo grande";
  const target = path.resolve(dir, rel);
  if (path.relative(dir, target).startsWith("..")) return "Percorso non consentito";
  if (baseVersion !== undefined && fileVersion(target) !== baseVersion) return CONFLICT;
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, content, "utf-8");
  return null;
}

/** Reveal the next hint (capped at hints.length); null when the exercise has none. */
export async function revealHint(repoRoot: string, course: string, name: string): Promise<ExerciseInfo | null> {
  const dir = exerciseDirOf(repoRoot, course, name);
  const h = dir && readHints(dir);
  if (!dir || !h || !h.hints.length) return null;
  if (h.revealed < h.hints.length) writeHints(dir, { ...h, revealed: h.revealed + 1 });
  return (await listExercises(repoRoot, course)).find((e) => e.name === name) ?? null;
}

export async function listExercises(repoRoot: string, course: string): Promise<ExerciseInfo[]> {
  const base = exDir(repoRoot, course);
  if (!fs.existsSync(base)) return [];
  const out: ExerciseInfo[] = [];
  for (const e of fs.readdirSync(base, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    if (!e.isDirectory() || !NAME_RE.test(e.name)) continue;
    const dir = path.join(base, e.name);
    const files = fs.readdirSync(dir, { withFileTypes: true }).filter((f) => f.isFile() && !f.name.startsWith(".") && f.name.toLowerCase() !== HINTS_FILE.toLowerCase()).map((f) => f.name).sort();
    const instr = path.join(dir, INSTRUCTIONS_FILE);
    const h = readHints(dir);
    let lastRun: ExerciseRunResult | undefined;
    try { lastRun = JSON.parse(fs.readFileSync(path.join(dir, LASTRUN_FILE), "utf-8")); } catch { /* never run */ }
    out.push({
      course,
      name: e.name,
      dir: `courses/${course}/exercises/${e.name}`,
      files,
      ...(lastRun ? { lastRun } : {}),
      ...(h ? { hints: h.hints, hintsRevealed: h.revealed, final: h.final } : {}),
      ...(fs.existsSync(instr) ? { instructions: fs.readFileSync(instr, "utf-8") } : {}),
    });
  }
  return out;
}

/** Pure: vitest JSON report text to result. Exported for tests. */
export function parseVitestReport(json: string, durationMs: number): ExerciseRunResult {
  const r = JSON.parse(json);
  const failures: ExerciseRunResult["failures"] = [];
  const tests: NonNullable<ExerciseRunResult["tests"]> = [];
  for (const f of r.testResults ?? []) {
    // a suite that fails to load (syntax error, bad import) has no assertions: surface its message as the failure
    if (f.status === "failed" && !(f.assertionResults ?? []).length) {
      failures.push({ name: "Il file non si carica", message: String(f.message ?? "").replace(ANSI_RE, "").slice(0, 1000) });
      tests.push({ name: "Il file non si carica", status: "failed" });
    }
    for (const a of f.assertionResults ?? []) {
      tests.push({ name: a.fullName ?? a.title ?? "?", status: a.status === "passed" ? "passed" : a.status === "failed" ? "failed" : "skipped" });
      if (a.status === "failed") {
        failures.push({
          name: a.fullName ?? a.title ?? "?",
          message: (a.failureMessages ?? []).join("\n").replace(ANSI_RE, "").slice(0, 1000),
        });
      }
    }
  }
  const failed = r.numFailedTests ?? failures.length;
  return {
    ok: failed === 0 && (r.numTotalTests ?? 0) > 0 && !(r.numFailedTestSuites > 0),
    passed: r.numPassedTests ?? 0,
    failed,
    total: r.numTotalTests ?? 0,
    failures,
    tests,
    durationMs,
    at: new Date().toISOString(),
  };
}

const killTree = (child: ChildProcess) => {
  if (process.platform === "win32" && child.pid !== undefined) spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
  else child.kill("SIGKILL");
};

const installs = new Map<string, Promise<string | null>>();
/** First run in a course: `npm install` in its exercises dir, one at a time per dir. Resolves an error message or null.
 *  LEARNER_NPM_BIN (tests): a node script run instead of npm. */
function installOnce(base: string): Promise<string | null> {
  let p = installs.get(base);
  if (!p) {
    p = new Promise<string | null>((resolve) => {
      if (!fs.existsSync(path.join(base, "package.json"))) return resolve(`Manca package.json in ${base}`);
      const fake = process.env.LEARNER_NPM_BIN;
      const child = fake
        ? spawn(process.execPath, [fake, "install"], { cwd: base, stdio: "ignore" })
        : spawn(process.platform === "win32" ? "npm.cmd" : "npm", ["install"], { cwd: base, stdio: "ignore", shell: process.platform === "win32" });
      const timer = setTimeout(() => { killTree(child); resolve("Timeout: npm install ha superato 5 minuti"); }, 300_000);
      child.on("error", (e) => { clearTimeout(timer); resolve("npm install non è partito: " + e.message); });
      child.on("close", (code) => { clearTimeout(timer); resolve(code === 0 ? null : `npm install è fallito (codice ${code})`); });
    }).finally(() => installs.delete(base));
    installs.set(base, p);
  }
  return p;
}

export async function runExercise(repoRoot: string, course: string, name: string): Promise<ExerciseRunResult> {
  const base = exDir(repoRoot, course);
  const fail = (error: string): ExerciseRunResult => ({
    ok: false, passed: 0, failed: 0, total: 0, failures: [], durationMs: 0, at: new Date().toISOString(), error,
  });
  const dir = path.join(base, name);
  if (!NAME_RE.test(name) || name.includes("/") || name.includes("\\") || !fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) {
    return fail(`Esercizio sconosciuto: ${name}`);
  }
  const vitest = path.join(base, "node_modules", "vitest", "vitest.mjs");
  if (!fs.existsSync(vitest)) {
    const err = await installOnce(base);
    if (err || !fs.existsSync(vitest)) return fail(err ?? `Installazione non riuscita: esegui npm install in courses/${course}/exercises`);
  }

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "learner-ex-"));
  const outFile = path.join(tmpDir, "report.json");
  const start = Date.now();
  try {
    const stderr = await new Promise<string>((resolve, reject) => {
      // The name is a path filter; the trailing slash keeps "01-a" from matching "01-ab".
      const child = spawn(process.execPath, [vitest, "run", name + "/", "--reporter=json", `--outputFile=${outFile}`], {
        cwd: base,
        stdio: ["ignore", "ignore", "pipe"],
      });
      let err = "";
      child.stderr.on("data", (d) => (err += d));
      const timer = setTimeout(() => {
        killTree(child);
        reject(new Error("Timeout: i test hanno superato 120 s"));
      }, 120_000);
      child.on("error", (e) => { clearTimeout(timer); reject(e); });
      child.on("close", () => { clearTimeout(timer); resolve(err); });
    });
    if (!fs.existsSync(outFile)) return fail(("vitest non ha prodotto un report. " + stderr).replace(ANSI_RE, "").slice(0, 1000));
    const res = parseVitestReport(fs.readFileSync(outFile, "utf-8"), Date.now() - start);
    try { fs.writeFileSync(path.join(dir, LASTRUN_FILE), JSON.stringify(res), "utf-8"); } catch { /* best effort */ }
    return res;
  } catch (e: any) {
    return fail(String(e?.message ?? e));
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

export function openInEditor(absDir: string): void {
  if (absDir.includes('"')) return;
  try {
    // shell:true joins args unquoted: quote the path so spaces survive.
    const c = spawn("code", [`"${absDir}"`], { shell: true, detached: true, stdio: "ignore" });
    c.on("error", () => {});
    c.unref();
  } catch {
    /* ignore */
  }
}
