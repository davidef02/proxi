// Measures the real desktop app: idle RAM (Windows Private Working Set summed over the Electron process tree) and
// cold start (launch to the course page's next action). One discarded warm-up plus N measured runs, medians only.
// Run: npm run bench:baseline [-- --runs n] [--out file] [--compare file]
import { _electron } from "playwright-core";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { performance } from "node:perf_hooks";
import { uiIsStale } from "./startup.mjs";
import {
  INTERACTIVE_SELECTOR,
  OPEN_HASH,
  SETTLE_MS,
  buildBaseline,
  drift,
  parseArgs,
  parseProcessSample,
  processSampleScript,
  toMb,
  validateBaseline,
  validateCompareFile,
} from "./bench-lib.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "../..");

const alive = (pid) => execFileSync("tasklist", ["/FI", `PID eq ${pid}`, "/NH"], { encoding: "utf8" }).includes(String(pid));
const git = (...args) => execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();

try {
  const opts = parseArgs(process.argv.slice(2));
  if (process.platform !== "win32") {
    console.error("bench:baseline measures Windows Private Working Set: run it on Windows");
    process.exit(1);
  }

  const electronExe = path.join(root, "node_modules", "electron", "dist", "electron.exe");
  if (!fs.existsSync(electronExe)) throw new Error("Electron missing: run `npm install` first.");

  let prev;
  if (opts.compare) {
    const prevPath = path.resolve(root, opts.compare);
    try {
      prev = JSON.parse(fs.readFileSync(prevPath, "utf8"));
    } catch (e) {
      throw new Error(`cannot read --compare ${prevPath}: ${e.message}`);
    }
    const prevErrors = validateCompareFile(prev);
    if (prevErrors.length > 0) throw new Error(`--compare ${prevPath} is not a baseline file: ${prevErrors.join("; ")}`);
  }

  const appRoot = path.join(root, "app");
  const distIndex = path.join(appRoot, "web", "dist", "index.html");
  if (uiIsStale(appRoot)) {
    console.log("building UI (not timed)");
    execFileSync(process.execPath, [path.join(root, "node_modules", "vite", "bin", "vite.js"), "build", "--config", path.join(appRoot, "web", "vite.config.ts")], {
      stdio: "inherit",
      cwd: root,
    });
    if (uiIsStale(appRoot)) throw new Error("UI still stale after build");
  }
  const distMtime = fs.statSync(distIndex).mtimeMs;

  async function runOnce(label) {
    await new Promise((r) => setTimeout(r, 2000)); // same pause before every launch, warm-up included
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-bench-"));
    const repo = path.join(tmp, "repo");
    fs.cpSync(path.join(appRoot, "test", "fixtures", "repo"), repo, { recursive: true });
    const env = {
      ...process.env,
      LEARNER_REPO_ROOT: repo,
      LEARNER_DATA_ROOT: repo,
      LEARNER_PORT: "0",
      LEARNER_USER_DATA: path.join(tmp, "userdata"),
      LEARNER_CLAUDE_BIN: process.execPath,
      LEARNER_CLAUDE_BIN_ARGS: JSON.stringify([path.join(appRoot, "test", "fake-claude.mjs")]),
      LEARNER_ANKI_URL: "http://127.0.0.1:9",
      LEARNER_OPEN_HASH: OPEN_HASH,
    };
    delete env.ELECTRON_RUN_AS_NODE;
    let electronApp, pid;
    try {
      const t0 = performance.now();
      electronApp = await _electron.launch({ executablePath: electronExe, args: [path.join(here, "main.mjs")], env, timeout: 60000 });
      pid = electronApp.process().pid;
      const page = await electronApp.firstWindow({ timeout: 60000 });
      await page.locator(INTERACTIVE_SELECTOR + ":enabled").waitFor({ state: "visible", timeout: 60000 });
      const cold = performance.now() - t0;
      if (!page.url().endsWith("/" + OPEN_HASH)) throw new Error(`${label}: window at ${page.url()}, expected ${OPEN_HASH}`);
      if (fs.statSync(distIndex).mtimeMs !== distMtime) throw new Error("UI was rebuilt during a timed run");

      await new Promise((r) => setTimeout(r, SETTLE_MS));
      // WMI perf counters occasionally come back empty for a moment: retry, the app just stays idle.
      let sample;
      for (let attempt = 1; ; attempt++) {
        try {
          sample = parseProcessSample(
            execFileSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", processSampleScript(pid)], { encoding: "utf8" }),
          );
          break;
        } catch (e) {
          if (attempt >= 3) throw e;
          await new Promise((r) => setTimeout(r, 1000));
        }
      }
      // coverage only: the metric comes from PowerShell, not from getAppMetrics
      const metricPids = await electronApp.evaluate(({ app }) => app.getAppMetrics().map((m) => m.pid));
      const missing = metricPids.filter((p) => !sample.pids.includes(p));
      if (missing.length > 0) throw new Error(`${label}: Electron pids ${missing} not in the sampled tree`);
      const electron = await electronApp.evaluate(() => process.versions.electron);

      await electronApp.close();
      electronApp = undefined;
      const ram = toMb(sample.bytes);
      const coldMs = Math.round(cold);
      console.log(`${label}: cold_start_ms=${coldMs} ram_idle_mb=${ram} processes=${sample.processes}`);
      return { ram_idle_mb: ram, cold_start_ms: coldMs, processes: sample.processes, electron };
    } finally {
      if (electronApp) await electronApp.close().catch(() => {});
      if (pid && alive(pid)) execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  }

  await runOnce("warm-up");
  const measured = [];
  for (let i = 1; i <= opts.runs; i++) measured.push(await runOnce(`run ${i}/${opts.runs}`));

  const commit = git("rev-parse", "HEAD");
  let baseCommit = null;
  try {
    baseCommit = git("merge-base", "HEAD", "main");
  } catch {}
  const dirty = git("status", "--porcelain") !== "";

  const baseline = buildBaseline({
    runs: measured.map(({ ram_idle_mb, cold_start_ms, processes }) => ({ ram_idle_mb, cold_start_ms, processes })),
    date: new Date().toISOString(),
    commit,
    baseCommit,
    dirty,
    machine: { cpu: os.cpus()[0].model.trim(), cores: os.cpus().length, ram_gb: Math.round(os.totalmem() / 2 ** 30) },
    os: { platform: process.platform, release: os.release(), version: os.version(), arch: process.arch },
    electron: measured[0].electron,
    warmupRuns: 1,
  });

  const errors = validateBaseline(baseline, { runs: opts.runs });
  if (errors.length > 0) throw new Error(`invalid baseline: ${errors.join("; ")}`);

  const outPath = path.resolve(root, opts.out);
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, JSON.stringify(baseline, null, 2) + "\n");
  console.log(`wrote ${path.relative(root, outPath) || outPath}: ram_idle_mb=${baseline.ram_idle_mb} cold_start_ms=${baseline.cold_start_ms}`);

  if (prev) {
    const d = drift(prev, baseline);
    console.log(`drift ram=${(d.ram * 100).toFixed(1)}% cold=${(d.cold * 100).toFixed(1)}% (limit 10%)`);
    if (!d.ok) process.exitCode = 1;
  }
} catch (e) {
  console.error(`bench:baseline failed: ${e?.message || e}`);
  process.exitCode = 1;
}
