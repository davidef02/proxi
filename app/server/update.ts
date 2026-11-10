// Git-based update manager: checks for upstream commits and applies fast-forward updates.
import { execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { promisify } from "node:util";
import { emitGlobal } from "./events.ts";
import { HttpError, type SessionManager } from "./sessions.ts";
import type { JobManager } from "./jobs/index.ts";
import type { UpdateApplyResponse, UpdateCommitInfo, UpdateStatus } from "../shared/protocol.ts";
import { SKIP_MODEL_ENV } from "../shared/identity.mjs";

const execFileAsync = promisify(execFile);

let currentStatus: UpdateStatus = {
  state: "idle",
  behind: 0,
};

export function getStatus(): UpdateStatus {
  return { ...currentStatus };
}

/** For tests: set status directly. */
export function _setStatusForTest(s: UpdateStatus): void {
  currentStatus = { ...s };
}

async function runGit(
  args: string[],
  cwd: string,
  opts?: { gitBin?: string; timeout?: number },
): Promise<{ stdout: string; stderr: string }> {
  const gitBin = opts?.gitBin ?? "git";
  return await execFileAsync(gitBin, args, {
    cwd,
    timeout: opts?.timeout,
    windowsHide: true,
    encoding: "utf8",
    env: { ...process.env, GIT_TERMINAL_PROMPT: "0" },
  });
}

let inFlightCheck: Promise<UpdateStatus> | null = null;

export function checkUpdate(repoRoot: string, opts?: { gitBin?: string }): Promise<UpdateStatus> {
  if (inFlightCheck) return inFlightCheck;
  inFlightCheck = doCheckUpdate(repoRoot, opts).finally(() => {
    inFlightCheck = null;
  });
  return inFlightCheck;
}

async function doCheckUpdate(repoRoot: string, opts?: { gitBin?: string }): Promise<UpdateStatus> {
  const gitBin = opts?.gitBin ?? "git";

  // Fast check: if neither .git dir nor .git file exists, repoRoot is not a git worktree
  if (!fs.existsSync(path.join(repoRoot, ".git"))) {
    const status: UpdateStatus = { state: "idle", behind: 0, checkedAt: new Date().toISOString() };
    currentStatus = status;
    emitGlobal({ type: "update", status });
    return status;
  }

  // Check if repo has git: git rev-parse --is-inside-work-tree
  try {
    await runGit(["rev-parse", "--is-inside-work-tree"], repoRoot, { gitBin });
  } catch {
    const status: UpdateStatus = { state: "idle", behind: 0, checkedAt: new Date().toISOString() };
    currentStatus = status;
    emitGlobal({ type: "update", status });
    return status;
  }

  // Check if upstream exists: git rev-parse --symbolic-full-name @{u}
  // If no upstream configured for current branch, fallback to origin/main or origin/HEAD so working on feature branches still tracks updates.
  let targetRef = "@{u}";
  try {
    await runGit(["rev-parse", "--symbolic-full-name", "@{u}"], repoRoot, { gitBin });
  } catch {
    targetRef = "";
    for (const fallback of ["origin/main", "origin/master", "origin/HEAD"]) {
      try {
        await runGit(["rev-parse", "--verify", fallback], repoRoot, { gitBin });
        targetRef = fallback;
        break;
      } catch {
        // try next
      }
    }
    if (!targetRef) {
      const status: UpdateStatus = { state: "idle", behind: 0, checkedAt: new Date().toISOString() };
      currentStatus = status;
      emitGlobal({ type: "update", status });
      return status;
    }
  }

  // git fetch --quiet with a 10-second timeout. If offline/error, catch silently and return idle/up_to_date.
  try {
    await runGit(["fetch", "--quiet"], repoRoot, { gitBin, timeout: 10_000 });
  } catch {
    // offline or error: catch silently
  }

  let behind = 0;
  try {
    const { stdout } = await runGit(["rev-list", "--count", `HEAD..${targetRef}`], repoRoot, { gitBin });
    behind = parseInt(stdout.trim(), 10) || 0;
  } catch {
    const status: UpdateStatus = { state: "idle", behind: 0, checkedAt: new Date().toISOString() };
    currentStatus = status;
    emitGlobal({ type: "update", status });
    return status;
  }

  if (behind > 0) {
    let commits: UpdateCommitInfo[] = [];
    try {
      const { stdout } = await runGit(["log", "-n", "10", "--pretty=format:%h%x09%s%x09%cr", `HEAD..${targetRef}`], repoRoot, { gitBin });
      commits = stdout
        .split("\n")
        .map((l) => l.trim())
        .filter(Boolean)
        .map((line) => {
          const [hash, subject, relativeTime] = line.split("\t");
          return {
            hash: hash ?? "",
            subject: subject ?? "",
            relativeTime: relativeTime ?? "",
          };
        });
    } catch {
      // fallback if log fails
    }
    const status: UpdateStatus = {
      state: "available",
      behind,
      commits,
      checkedAt: new Date().toISOString(),
    };
    currentStatus = status;
    emitGlobal({ type: "update", status });
    return status;
  }

  const status: UpdateStatus = {
    state: "up_to_date",
    behind: 0,
    checkedAt: new Date().toISOString(),
  };
  currentStatus = status;
  emitGlobal({ type: "update", status });
  return status;
}

export function countActiveWork(manager: SessionManager, jobs: JobManager): number {
  const activeSessions = (manager.list?.() ?? []).filter((s) => s.status === "running" || s.status === "starting" || s.status === "waiting").length;
  const sessionPids = typeof manager.livePids === "function" ? manager.livePids().length : 0;
  const sessionsCount = Math.max(activeSessions, sessionPids);
  const jobsCount = typeof jobs.livePids === "function" ? jobs.livePids().length : 0;
  return sessionsCount + jobsCount;
}

export async function applyUpdate(
  repoRoot: string,
  opts: {
    force?: boolean;
    manager: SessionManager;
    jobs: JobManager;
    relaunch?: () => Promise<void> | void;
    gitBin?: string;
    npmBin?: string;
  },
): Promise<UpdateApplyResponse> {
  const gitBin = opts.gitBin ?? "git";

  // Check countActiveWork. If > 0 and !opts.force, throw HttpError(409).
  // If opts.force is true, cleanly terminate running sessions and jobs before updating.
  const active = countActiveWork(opts.manager, opts.jobs);
  if (active > 0) {
    if (!opts.force) {
      throw new HttpError(409, "C'è una sessione in corso. Chiudila o conferma per interromperla prima di aggiornare.", "sessions_running");
    }
    await Promise.all([opts.manager.shutdown(), opts.jobs.shutdown()]);
  }

  // Check git status --porcelain. If output is non-empty, throw HttpError(409, "Modifiche locali non salvate nel repository. Impossibile aggiornare senza rischiare di sovrascrivere il tuo lavoro.", "dirty_tree")
  let statusOut = "";
  try {
    const { stdout } = await runGit(["status", "--porcelain"], repoRoot, { gitBin });
    statusOut = stdout.trim();
  } catch (err: any) {
    throw new HttpError(500, "Errore verifica repository: " + (err?.message ?? String(err)));
  }
  if (statusOut.length > 0) {
    throw new HttpError(409, "Modifiche locali non salvate nel repository. Impossibile aggiornare senza rischiare di sovrascrivere il tuo lavoro.", "dirty_tree");
  }

  // Detect current branch. If on a feature/different branch, switch to main if tree is clean.
  let currentBranch = "";
  try {
    const { stdout } = await runGit(["rev-parse", "--abbrev-ref", "HEAD"], repoRoot, { gitBin });
    currentBranch = stdout.trim();
  } catch {
    // detached or error
  }

  if (currentBranch && currentBranch !== "main" && currentBranch !== "master") {
    // Check if main or master branch exists locally
    let mainBranch = "main";
    try {
      await runGit(["rev-parse", "--verify", "main"], repoRoot, { gitBin });
    } catch {
      try {
        await runGit(["rev-parse", "--verify", "master"], repoRoot, { gitBin });
        mainBranch = "master";
      } catch {
        mainBranch = "";
      }
    }
    if (mainBranch) {
      try {
        await runGit(["checkout", mainBranch], repoRoot, { gitBin });
      } catch (err: any) {
        throw new HttpError(409, `Impossibile passare al branch ${mainBranch}: ` + (err?.stderr?.trim() || err?.message || String(err)));
      }
    }
  }

  // Fetch upstream before inspecting fast-forward state
  try {
    await runGit(["fetch", "--quiet"], repoRoot, { gitBin, timeout: 15_000 });
  } catch {
    // If offline or network error, proceed; if pull fails later it will report network error
  }

  // Check upstream exists: git rev-parse --symbolic-full-name @{u}. If not, throw HttpError(400, "Nessun branch upstream configurato.")
  try {
    await runGit(["rev-parse", "--symbolic-full-name", "@{u}"], repoRoot, { gitBin });
  } catch {
    throw new HttpError(400, "Nessun branch upstream configurato.");
  }

  // Check fast-forward: git merge-base --is-ancestor HEAD @{u}. If exit code != 0, throw HttpError(409, "I commit locali sono divergenti da quelli remoti. Impossibile eseguire un aggiornamento fast-forward.", "diverged")
  try {
    await runGit(["merge-base", "--is-ancestor", "HEAD", "@{u}"], repoRoot, { gitBin });
  } catch {
    throw new HttpError(409, "I commit locali sono divergenti da quelli remoti. Impossibile eseguire un aggiornamento fast-forward.", "diverged");
  }

  // Capture oldHead = git rev-parse HEAD
  let oldHead = "";
  try {
    const { stdout } = await runGit(["rev-parse", "HEAD"], repoRoot, { gitBin });
    oldHead = stdout.trim();
  } catch (err: any) {
    throw new HttpError(500, "Impossibile determinare il commit corrente: " + (err?.message ?? String(err)));
  }

  // Update state to updating
  currentStatus = { ...currentStatus, state: "updating" };
  emitGlobal({ type: "update", status: currentStatus });

  // Run git pull --ff-only with 60s timeout. If fails, throw HttpError(500, "git pull fallito: " + stderr)
  try {
    await runGit(["pull", "--ff-only"], repoRoot, { gitBin, timeout: 60_000 });
  } catch (err: any) {
    const stderr = err?.stderr?.trim() || err?.message || String(err);
    currentStatus = { ...currentStatus, state: "error", error: "git pull fallito: " + stderr };
    emitGlobal({ type: "update", status: currentStatus });
    throw new HttpError(500, "git pull fallito: " + stderr);
  }

  // Capture newHead = git rev-parse HEAD
  let newHead = "";
  try {
    const { stdout } = await runGit(["rev-parse", "HEAD"], repoRoot, { gitBin });
    newHead = stdout.trim();
  } catch (err: any) {
    throw new HttpError(500, "Impossibile determinare il nuovo commit: " + (err?.message ?? String(err)));
  }

  // Check diff: git diff --name-only <oldHead> <newHead>. If package-lock.json or package.json in diff:
  // Run npm install (using npmBin or platform npm.cmd / npm) with timeout (e.g. 180s).
  if (oldHead && newHead && oldHead !== newHead) {
    let diffFiles: string[] = [];
    try {
      const { stdout } = await runGit(["diff", "--name-only", oldHead, newHead], repoRoot, { gitBin });
      diffFiles = stdout.split("\n").map((f) => f.trim()).filter(Boolean);
    } catch {
      // diff failed or empty
    }

    if (diffFiles.includes("package.json") || diffFiles.includes("package-lock.json")) {
      const npmBin = opts.npmBin ?? (process.platform === "win32" ? "npm.cmd" : "npm");
      try {
        if (process.platform === "win32") {
          await execFileAsync("cmd.exe", ["/c", npmBin, "install"], {
            cwd: repoRoot,
            timeout: 180_000,
            windowsHide: true,
            encoding: "utf8",
            env: { ...process.env, [SKIP_MODEL_ENV]: "1" },
          });
        } else {
          await execFileAsync(npmBin, ["install"], {
            cwd: repoRoot,
            timeout: 180_000,
            windowsHide: true,
            encoding: "utf8",
            env: { ...process.env, [SKIP_MODEL_ENV]: "1" },
          });
        }
      } catch (err: any) {
        const stderr = err?.stderr?.trim() || err?.message || String(err);
        currentStatus = { ...currentStatus, state: "error", error: "npm install fallito: " + stderr };
        emitGlobal({ type: "update", status: currentStatus });
        throw new HttpError(500, "npm install fallito: " + stderr);
      }
    }
  }

  // Set status { state: "relaunching", behind: 0 }, emit global event
  const status: UpdateStatus = {
    state: "relaunching",
    behind: 0,
    checkedAt: new Date().toISOString(),
  };
  currentStatus = status;
  emitGlobal({ type: "update", status });

  // If opts.relaunch is provided, call it asynchronously after 500ms so the HTTP reply is sent first.
  // If not provided, in headless mode, schedule a graceful shutdown/restart or exit.
  if (opts.relaunch) {
    setTimeout(() => {
      void opts.relaunch!();
    }, 500);
  } else if (process.env.NODE_ENV !== "test") {
    setTimeout(() => {
      process.exit(0);
    }, 500);
  }

  return { ok: true, relaunching: true };
}
