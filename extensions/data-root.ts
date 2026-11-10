/**
 * Data folder resolver: where learner data (courses/, graph/, decks/, curriculum.json, index.html) lives.
 *
 * One resolver for the server, the CLIs and the docs. Order (first hit wins):
 *   1. env LEARNER_DATA_ROOT
 *   2. <repo>/.learner/config.json { dataRoot }
 *   3. ~/Documents/proxi-data
 *
 * `.learner/` (settings, sessions, jobs, manifests, secrets, config) always stays in the code repo.
 */

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { APP_NAME, DATA_DIR_NAME } from "../app/shared/identity.mjs";
import { main } from "./cli.ts";

export const DATA_ROOT_ENV = "LEARNER_DATA_ROOT";

/** The code repo root (this file lives in <repo>/extensions). */
export const CODE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

export type DataRootSource = "env" | "config" | "default";

export function defaultDataRoot(home: string = os.homedir()): string {
  return path.join(home, "Documents", DATA_DIR_NAME);
}

/** True when `dir` already holds learner data (courses/ or curriculum.json). */
export function hasData(dir: string): boolean {
  try {
    if (fs.statSync(path.join(dir, "courses")).isDirectory()) return true;
  } catch {
    // no courses/: fall through
  }
  return fs.existsSync(path.join(dir, "curriculum.json"));
}

function configFile(repoRoot: string): string {
  return path.join(repoRoot, ".learner", "config.json");
}

/** The pointer written by the settings UI, as an absolute path, or undefined. */
export function readDataRootPointer(repoRoot: string): string | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(fs.readFileSync(configFile(repoRoot), "utf8"));
  } catch {
    return undefined;
  }
  if (!parsed || typeof parsed !== "object") return undefined;
  const dataRoot = (parsed as { dataRoot?: unknown }).dataRoot;
  if (typeof dataRoot !== "string" || !dataRoot.trim() || !path.isAbsolute(dataRoot)) return undefined;
  return path.resolve(dataRoot);
}

/** Merges `{dataRoot}` into `.learner/config.json`, keeping the other keys. Atomic, like settings.ts. */
export function writeDataRootPointer(repoRoot: string, dataRoot: string): void {
  const f = configFile(repoRoot);
  fs.mkdirSync(path.dirname(f), { recursive: true });
  let current: Record<string, unknown> = {};
  try {
    const parsed = JSON.parse(fs.readFileSync(f, "utf8"));
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) current = parsed;
  } catch {
    // missing or invalid: start from an empty object
  }
  const body = JSON.stringify({ ...current, dataRoot: path.resolve(dataRoot) }, null, 2) + "\n";
  const tmp = `${f}.${process.pid}.tmp`;
  try {
    fs.writeFileSync(tmp, body);
    fs.renameSync(tmp, f);
  } catch {
    // Windows: rename over a file another process holds open can fail with EPERM/EBUSY: write in place instead.
    fs.rmSync(tmp, { force: true });
    fs.writeFileSync(f, body);
  }
}

/** Why `dir` cannot be the data folder, or undefined. */
export function dataRootProblem(repoRoot: string, dir: string): string | undefined {
  if (!path.isAbsolute(dir)) return "La cartella dei dati deve essere un percorso assoluto.";
  const a = path.resolve(repoRoot).toLowerCase() + path.sep;
  const b = path.resolve(dir).toLowerCase() + path.sep;
  if (a.startsWith(b) || b.startsWith(a)) {
    return `La cartella dei dati non può stare dentro il codice di ${APP_NAME} (né contenerlo).`;
  }
  return undefined;
}

/** Resolves the data folder. Never creates anything. */
export function resolveDataRoot(
  repoRoot: string = CODE_ROOT,
  env: NodeJS.ProcessEnv = process.env,
  home: string = os.homedir()
): { dataRoot: string; source: DataRootSource } {
  const fromEnv = env[DATA_ROOT_ENV]?.trim();
  if (fromEnv) return { dataRoot: path.resolve(fromEnv), source: "env" };
  const pointer = readDataRootPointer(repoRoot);
  if (pointer) return { dataRoot: pointer, source: "config" };
  return { dataRoot: defaultDataRoot(home), source: "default" };
}

/** CLI entrypoints only: resolve, create, and chdir so documented relative paths keep working. */
export function enterDataRoot(): string {
  const { dataRoot } = resolveDataRoot();
  fs.mkdirSync(dataRoot, { recursive: true });
  process.chdir(dataRoot);
  return dataRoot;
}

const USAGE = `Usage: npm run data -- <command>

  where                  print the data folder and where it was resolved from
  help                   show this message

Resolution: LEARNER_DATA_ROOT, then .learner/config.json, then ~/Documents/${DATA_DIR_NAME}.`;

export async function runCli(argv: string[]): Promise<number> {
  try {
    const { positionals } = parseArgs({ args: argv, allowPositionals: true, options: {} });
    const command = positionals[0] ?? "help";

    if (command === "where") {
      const { dataRoot, source } = resolveDataRoot();
      console.log(`${dataRoot} (${source})`);
      return 0;
    }
    if (command === "help") {
      console.log(USAGE);
      return 0;
    }
    console.error(`Unknown command: ${command}`);
    console.error(USAGE);
    return 1;
  } catch (err: any) {
    console.error("Error: " + err.message);
    return 1;
  }
}

main(import.meta.url, runCli);
