// Pure helpers behind `npm run bench:baseline`: medians, the PowerShell process-tree sample and its parser,
// the baseline JSON shape and the repeatability check. No Electron or Playwright imports here.
export const RUNS = 5;
export const SETTLE_MS = 10_000;
export const DRIFT_LIMIT = 0.10;
export const RAM_TARGET_FACTOR = 0.85;
export const COLD_TARGET_FACTOR = 1.0;
export const OPEN_HASH = "#/c/demo";
export const INTERACTIVE_SELECTOR = '[data-testid="next-action"]';

const round1 = (n) => Math.round(n * 10) / 10;

export function median(nums) {
  if (nums.length === 0) throw new Error("median of empty list");
  const sorted = [...nums].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

export function toMb(bytes) {
  return Math.round((bytes / 1048576) * 10) / 10;
}

export function parseProcessSample(stdout) {
  const excerpt = String(stdout).slice(0, 200);
  let rows;
  try {
    rows = JSON.parse(stdout);
  } catch {
    throw new Error(`no processes sampled: ${excerpt}`);
  }
  // ConvertTo-Json emits a bare object when the tree has a single process.
  if (rows !== null && !Array.isArray(rows)) rows = [rows];
  if (!Array.isArray(rows) || rows.length === 0) throw new Error(`no processes sampled: ${excerpt}`);
  if (!rows.every((r) => r && Number.isInteger(r.pid) && r.pid > 0 && Number.isFinite(r.wsPrivate) && r.wsPrivate >= 0)) {
    throw new Error(`no processes sampled: ${excerpt}`);
  }
  return {
    pids: rows.map((r) => r.pid),
    bytes: rows.reduce((sum, r) => sum + r.wsPrivate, 0),
    processes: rows.length,
  };
}

// Walks the descendants of the Electron main pid through Win32_Process, then reads the Private Working Set of
// every pid in that tree. One PowerShell call per sample.
export function processSampleScript(rootPid) {
  if (!Number.isInteger(rootPid) || rootPid <= 0) throw new Error("rootPid must be a positive integer");
  return `$root = ${rootPid}
$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
$tree = [System.Collections.Generic.HashSet[int]]::new(); [void]$tree.Add($root)
do { $added = $false; foreach ($p in $all) { if ($p.ProcessId -ne $p.ParentProcessId -and $tree.Contains([int]$p.ParentProcessId) -and $tree.Add([int]$p.ProcessId)) { $added = $true } } } while ($added)
$rows = @(Get-CimInstance Win32_PerfRawData_PerfProc_Process | Where-Object { $tree.Contains([int]$_.IDProcess) } | ForEach-Object { [pscustomobject]@{ pid = [int]$_.IDProcess; name = [string]$_.Name; wsPrivate = [int64]$_.WorkingSetPrivate } })
ConvertTo-Json -InputObject $rows -Compress`;
}

export function buildBaseline({ app, runs, date, commit, baseCommit, dirty, machine, os, tauri, warmupRuns }) {
  return {
    schema: 1,
    app,
    date,
    commit,
    base_commit: baseCommit ?? null,
    dirty,
    machine,
    os,
    runtime: { tauri },
    method: {
      fixture: "demo",
      open_hash: OPEN_HASH,
      settle_ms: SETTLE_MS,
      warmup_runs: warmupRuns,
      ram_metric: "Win32 WorkingSetPrivate summed over the Tauri process tree (MB = 2^20 bytes)",
      interactive: "[data-testid=next-action] visible and enabled on #/c/demo",
      cold_start: "launch to interactive, prebuilt UI",
    },
    runs,
    ram_idle_mb: round1(median(runs.map((r) => r.ram_idle_mb))),
    cold_start_ms: Math.round(median(runs.map((r) => r.cold_start_ms))),
  };
}

const nonEmpty = (v) => typeof v === "string" && v.trim() !== "";
const positive = (v) => typeof v === "number" && Number.isFinite(v) && v > 0;
const isSha = (v) => typeof v === "string" && /^[0-9a-f]{40}$/.test(v);

// Returns the list of problems so the CLI can print them all; never throws on malformed input.
export function validateBaseline(obj, { runs = RUNS } = {}) {
  if (!obj || typeof obj !== "object" || Array.isArray(obj)) return ["not an object"];
  const errors = [];

  if (obj.schema !== 1) errors.push("schema must be 1");
  if (!nonEmpty(obj.date) || !/^\d{4}-\d{2}-\d{2}T/.test(obj.date) || Number.isNaN(Date.parse(obj.date))) {
    errors.push("date must be an ISO 8601 string");
  }
  if (!isSha(obj.commit)) errors.push("commit must be a 40-character hex sha");
  if (obj.base_commit !== null && !isSha(obj.base_commit)) errors.push("base_commit must be null or a 40-character hex sha");
  if (typeof obj.dirty !== "boolean") errors.push("dirty must be a boolean");
  if (!nonEmpty(obj.machine?.cpu)) errors.push("machine.cpu must be a non-empty string");
  if (!positive(obj.machine?.cores)) errors.push("machine.cores must be a positive number");
  if (!positive(obj.machine?.ram_gb)) errors.push("machine.ram_gb must be a positive number");
  if (obj.os?.platform !== "win32") errors.push('os.platform must be "win32"');
  for (const key of ["release", "version", "arch"]) {
    if (!nonEmpty(obj.os?.[key])) errors.push(`os.${key} must be a non-empty string`);
  }
  if (!nonEmpty(obj.runtime?.tauri) && !nonEmpty(obj.runtime?.electron)) {
    errors.push("runtime.tauri must be a non-empty string (legacy runtime.electron accepted for M05 baselines)");
  }
  if (obj.method?.settle_ms !== SETTLE_MS) errors.push(`method.settle_ms must be ${SETTLE_MS}`);

  if (!Array.isArray(obj.runs) || obj.runs.length !== runs) {
    errors.push(`runs must contain exactly ${runs} samples`);
    return errors;
  }
  let runErrors = 0;
  obj.runs.forEach((r, i) => {
    if (!positive(r?.ram_idle_mb)) {
      errors.push(`runs[${i}].ram_idle_mb must be a positive number`);
      runErrors++;
    }
    if (!positive(r?.cold_start_ms)) {
      errors.push(`runs[${i}].cold_start_ms must be a positive number`);
      runErrors++;
    }
    if (!Number.isInteger(r?.processes) || r.processes <= 0) {
      errors.push(`runs[${i}].processes must be a positive integer`);
      runErrors++;
    }
  });
  if (runErrors === 0) {
    if (obj.ram_idle_mb !== round1(median(obj.runs.map((r) => r.ram_idle_mb)))) {
      errors.push("ram_idle_mb must be the median of runs[].ram_idle_mb");
    }
    if (obj.cold_start_ms !== Math.round(median(obj.runs.map((r) => r.cold_start_ms)))) {
      errors.push("cold_start_ms must be the median of runs[].cold_start_ms");
    }
  }
  return errors;
}

/** Problems with a `--compare` file: it must be a baseline this command wrote, at any run count. */
export function validateCompareFile(obj) {
  // A real baseline has at least one run; validate against its own count so `--runs 1` files compare too.
  const runs = Array.isArray(obj?.runs) && obj.runs.length > 0 ? obj.runs.length : RUNS;
  return validateBaseline(obj, { runs });
}

export function targetThresholds(prev, { ramFactor = RAM_TARGET_FACTOR, coldFactor = COLD_TARGET_FACTOR } = {}) {
  return {
    ram_idle_mb: round1(prev.ram_idle_mb * ramFactor),
    cold_start_ms: Math.round(prev.cold_start_ms * coldFactor),
  };
}

export function drift(prev, next, limit = DRIFT_LIMIT, targets = undefined) {
  const ram = Math.abs(next.ram_idle_mb - prev.ram_idle_mb) / prev.ram_idle_mb;
  const cold = Math.abs(next.cold_start_ms - prev.cold_start_ms) / prev.cold_start_ms;
  if (targets) {
    const ramOk = next.ram_idle_mb <= targets.ram_idle_mb;
    const coldOk = next.cold_start_ms <= targets.cold_start_ms;
    return { ram, cold, ramOk, coldOk, ok: ramOk && coldOk };
  }
  return { ram, cold, ok: ram < limit && cold < limit };
}

export function parseArgs(argv) {
  const opts = { runs: RUNS, out: "docs/benchmark-baseline.json", compare: undefined, driftLimit: DRIFT_LIMIT, drift: false };
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i];
    if (flag === "--runs") {
      const value = argv[++i];
      if (value === undefined || !/^\d+$/.test(value) || Number(value) <= 0) {
        throw new Error("--runs needs a positive integer");
      }
      opts.runs = Number(value);
    } else if (flag === "--drift-limit") {
      const value = argv[++i];
      if (value === undefined || !/^\d+(\.\d+)?$/.test(value)) throw new Error("--drift-limit needs a non-negative number");
      opts.driftLimit = Number(value);
    } else if (flag === "--drift") {
      opts.drift = true;
    } else if (flag === "--out" || flag === "--compare") {
      const value = argv[++i];
      if (value === undefined || value === "") throw new Error(`${flag} needs a path`);
      opts[flag.slice(2)] = value;
    } else {
      throw new Error(`unknown flag ${flag}`);
    }
  }
  return opts;
}
