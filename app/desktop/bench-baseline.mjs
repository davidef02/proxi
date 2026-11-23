// Measures the real desktop app: idle RAM (Windows Private Working Set summed over the Tauri process tree) and
// cold start (launch to the course page's next action). One discarded warm-up plus N measured runs, medians only.
// Run: npm run bench:baseline [-- --runs n] [--out file] [--compare file] [--drift-limit frac] [--drift]
import { chromium } from "playwright-core";
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { performance } from "node:perf_hooks";
import { uiIsStale } from "./startup.mjs";
import { proxiBinaryPath } from "./backend.mjs";
import { proxiBinary } from "../shared/proxi-bin.mjs";
import {
  INTERACTIVE_SELECTOR,
  OPEN_HASH,
  SETTLE_MS,
  buildBaseline,
  drift,
  parseArgs,
  parseProcessSample,
  processSampleScript,
  targetThresholds,
  toMb,
  validateBaseline,
  validateCompareFile,
} from "./bench-lib.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "../..");

const alive = (pid) => execFileSync("tasklist", ["/FI", `PID eq ${pid}`, "/NH"], { encoding: "utf8" }).includes(String(pid));
const git = (...args) => execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();

function freePort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const port = s.address().port;
      s.close(() => resolve(port));
    });
  });
}

try {
  const opts = parseArgs(process.argv.slice(2));
  if (process.platform !== "win32") {
    console.error("bench:baseline measures Windows Private Working Set: run it on Windows");
    process.exit(1);
  }

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

  const tauriExe = path.join(root, "src-tauri", "target", "release", "proxi-app.exe");
  if (!fs.existsSync(tauriExe)) throw new Error(`Tauri app missing at ${tauriExe}: run \`npm run tauri:build\` first.`);

  // Fails fast with the `cargo build --release --locked` message instead of a mid-run error.
  proxiBinaryPath(root);

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
    fs.cpSync(path.join(appRoot, "web", "dist"), path.join(repo, "app", "web", "dist"), { recursive: true });

    const cdpPort = await freePort();
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
      // The app resolves the binary under LEARNER_REPO_ROOT (the temp fixture repo): point it at the real one.
      PROXI_BIN: proxiBinary(root, process.env),
      WEBVIEW2_USER_DATA_FOLDER: path.join(tmp, "webview2"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${cdpPort}`,
    };

    let child, browser, pid;
    try {
      const t0 = performance.now();
      let spawnError;
      child = spawn(tauriExe, [], { env, stdio: "ignore" });
      child.once("error", (e) => {
        spawnError = e;
      });
      pid = child.pid;

      const deadlineCdp = Date.now() + 60_000;
      while (true) {
        if (spawnError) throw spawnError;
        let ok = false;
        try {
          const res = await fetch(`http://127.0.0.1:${cdpPort}/json/version`);
          ok = res.ok;
        } catch {}
        if (ok) break;
        if (Date.now() > deadlineCdp) throw new Error(`${label}: DevTools endpoint did not answer within 60 s`);
        if (!alive(pid)) throw new Error(`${label}: Tauri app process exited prematurely`);
        await new Promise((r) => setTimeout(r, 50));
      }

      browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);

      let page;
      const deadlinePage = Date.now() + 90_000;
      while (true) {
        if (spawnError) throw spawnError;
        page = (browser.contexts() ?? []).flatMap((c) => c.pages()).find((p) => /^http:\/\/127\.0\.0\.1:\d+/.test(p.url()));
        if (page) break;
        if (Date.now() > deadlinePage) throw new Error(`${label}: app page not found within 90 s`);
        if (!alive(pid)) throw new Error(`${label}: Tauri app process exited prematurely`);
        await new Promise((r) => setTimeout(r, 50));
      }

      await page.locator(INTERACTIVE_SELECTOR + ":enabled").waitFor({ state: "visible", timeout: 90_000 });
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

      await browser.close();
      browser = undefined;
      const ram = toMb(sample.bytes);
      const coldMs = Math.round(cold);
      console.log(`${label}: cold_start_ms=${coldMs} ram_idle_mb=${ram} processes=${sample.processes}`);
      return { ram_idle_mb: ram, cold_start_ms: coldMs, processes: sample.processes };
    } finally {
      if (browser) await browser.close().catch(() => {});
      if (pid && alive(pid)) {
        try {
          execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
        } catch {}
      }
      for (let attempt = 1; attempt <= 10; attempt++) {
        try {
          fs.rmSync(tmp, { recursive: true, force: true, maxRetries: 3 });
          break;
        } catch (e) {
          if (attempt === 10) throw e;
          await new Promise((r) => setTimeout(r, 200));
        }
      }
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

  const tauriConf = JSON.parse(fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"));
  const tauriVersion = tauriConf.version;

  const baseline = buildBaseline({
    app: "tauri+rust",
    runs: measured.map(({ ram_idle_mb, cold_start_ms, processes }) => ({ ram_idle_mb, cold_start_ms, processes })),
    date: new Date().toISOString(),
    commit,
    baseCommit,
    dirty,
    machine: { cpu: os.cpus()[0].model.trim(), cores: os.cpus().length, ram_gb: Math.round(os.totalmem() / 2 ** 30) },
    os: { platform: process.platform, release: os.release(), version: os.version(), arch: process.arch },
    tauri: tauriVersion,
    warmupRuns: 1,
  });

  const errors = validateBaseline(baseline, { runs: opts.runs });
  if (errors.length > 0) throw new Error(`invalid baseline: ${errors.join("; ")}`);

  const outPath = path.resolve(root, opts.out);
  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  fs.writeFileSync(outPath, JSON.stringify(baseline, null, 2) + "\n");
  console.log(`wrote ${path.relative(root, outPath) || outPath}: ram_idle_mb=${baseline.ram_idle_mb} cold_start_ms=${baseline.cold_start_ms}`);

  if (prev) {
    if (opts.drift) {
      const d = drift(prev, baseline, opts.driftLimit);
      console.log(`drift ram=${(d.ram * 100).toFixed(1)}% cold=${(d.cold * 100).toFixed(1)}% (limit ${(opts.driftLimit * 100).toFixed(0)}%)`);
      if (!d.ok) process.exitCode = 1;
    } else {
      const targets = targetThresholds(prev);
      const d = drift(prev, baseline, opts.driftLimit, targets);
      console.log(`targets ram_idle_mb<=${targets.ram_idle_mb} cold_start_ms<=${targets.cold_start_ms} (from --compare ${opts.compare})`);
      console.log(`drift ram=${(d.ram * 100).toFixed(1)}% cold=${(d.cold * 100).toFixed(1)}%`);
      if (!d.ok) process.exitCode = 1;
    }
  }
} catch (e) {
  console.error(`bench:baseline failed: ${e?.message || e}`);
  process.exitCode = 1;
}
