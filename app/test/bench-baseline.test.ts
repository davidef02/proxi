// Pure logic behind `npm run bench:baseline`: medians, the PowerShell process-tree sample, the baseline file
// shape and the repeatability check. No Electron here.
import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { pathToFileURL } from "node:url";

const here = import.meta.dirname;
const bench = () => import(pathToFileURL(path.join(here, "../desktop/bench-lib.mjs")).href);

const SHA = "a".repeat(40);
const BASE_SHA = "b".repeat(40);

/** A document that passes validateBaseline; individual tests break one field at a time. */
const validBaseline = () => ({
  schema: 1,
  app: "electron+node",
  date: "2026-11-12T10:00:00.000Z",
  commit: SHA,
  base_commit: BASE_SHA,
  dirty: false,
  machine: { cpu: "Test CPU", cores: 8, ram_gb: 16 },
  os: { platform: "win32", release: "10.0.26100", version: "10.0.26100", arch: "x64" },
  runtime: { electron: "45.0.0" },
  method: {
    fixture: "demo",
    open_hash: "#/c/demo",
    settle_ms: 10000,
    warmup_runs: 1,
    ram_metric: "Win32 WorkingSetPrivate summed over the Electron process tree (MB = 2^20 bytes)",
    interactive: "[data-testid=next-action] visible and enabled on #/c/demo",
    cold_start: "launch to interactive, prebuilt UI",
  },
  runs: [
    { ram_idle_mb: 300.1, cold_start_ms: 2100, processes: 5 },
    { ram_idle_mb: 310.4, cold_start_ms: 2050, processes: 5 },
    { ram_idle_mb: 305.2, cold_start_ms: 2300, processes: 5 },
    { ram_idle_mb: 299.9, cold_start_ms: 1990, processes: 5 },
    { ram_idle_mb: 320, cold_start_ms: 2200, processes: 5 },
  ],
  ram_idle_mb: 305.2,
  cold_start_ms: 2100,
});

test("median: odd, even, single, unsorted, empty throws, input untouched", async () => {
  const { median } = await bench();
  assert.equal(median([3, 1, 2]), 2);
  assert.equal(median([4, 1, 3, 2]), 2.5);
  assert.equal(median([7]), 7);
  assert.equal(median([9, 1, 5, 3, 7]), 5);
  assert.throws(() => median([]), /median of empty list/);
  const input = [5, 1, 4];
  median(input);
  assert.deepEqual(input, [5, 1, 4]);
});

test("toMb: bytes become mebibytes with one decimal", async () => {
  const { toMb } = await bench();
  assert.equal(toMb(1048576 * 123.456), 123.5);
  assert.equal(toMb(0), 0);
  assert.equal(toMb(1048576), 1);
});

test("parseProcessSample: an array of rows sums private bytes", async () => {
  const { parseProcessSample } = await bench();
  const out = parseProcessSample(
    JSON.stringify([
      { pid: 10, name: "electron.exe", wsPrivate: 1048576 },
      { pid: 11, name: "electron.exe", wsPrivate: 2 * 1048576 },
    ]),
  );
  assert.deepEqual(out, { pids: [10, 11], bytes: 3145728, processes: 2 });
});

test("parseProcessSample: a single row is wrapped in an array", async () => {
  const { parseProcessSample } = await bench();
  const out = parseProcessSample(JSON.stringify({ pid: 10, name: "electron.exe", wsPrivate: 100 }));
  assert.deepEqual(out, { pids: [10], bytes: 100, processes: 1 });
});

test("parseProcessSample: an empty sample or garbage throws with the stdout excerpt", async () => {
  const { parseProcessSample } = await bench();
  assert.throws(() => parseProcessSample("[]"), /no processes sampled/);
  assert.throws(() => parseProcessSample("<html>nope</html>"), /no processes sampled.*<html>nope/s);
});

test("processSampleScript: walks the process tree and reads WorkingSetPrivate", async () => {
  const { processSampleScript } = await bench();
  const script = processSampleScript(1234);
  for (const part of ["$root = 1234", "WorkingSetPrivate", "ParentProcessId", "-InputObject"]) {
    assert.ok(script.includes(part), `script lacks ${part}`);
  }
  for (const bad of [0, -1, 1.5, "1; rm"]) {
    assert.throws(() => processSampleScript(bad), JSON.stringify(bad));
  }
});

test("buildBaseline: medians of the runs, key order, and a valid document", async () => {
  const { buildBaseline, validateBaseline } = await bench();
  const runs = validBaseline().runs;
  const baseline = buildBaseline({
    runs,
    date: "2026-11-12T10:00:00.000Z",
    commit: SHA,
    baseCommit: BASE_SHA,
    dirty: false,
    machine: { cpu: "Test CPU", cores: 8, ram_gb: 16 },
    os: { platform: "win32", release: "10.0.26100", version: "10.0.26100", arch: "x64" },
    electron: "45.0.0",
    warmupRuns: 1,
  });
  assert.equal(baseline.ram_idle_mb, 305.2);
  assert.equal(baseline.cold_start_ms, 2100);
  assert.deepEqual(Object.keys(baseline), [
    "schema",
    "app",
    "date",
    "commit",
    "base_commit",
    "dirty",
    "machine",
    "os",
    "runtime",
    "method",
    "runs",
    "ram_idle_mb",
    "cold_start_ms",
  ]);
  assert.equal(baseline.method.warmup_runs, 1);
  assert.equal(baseline.method.fixture, "demo");
  assert.deepEqual(validateBaseline(baseline), []);
});

test("buildBaseline: a missing base commit stays null", async () => {
  const { buildBaseline } = await bench();
  const { runs, ...rest } = validBaseline();
  const baseline = buildBaseline({ runs, baseCommit: undefined, ...rest, electron: "45.0.0", warmupRuns: 1 });
  assert.equal(baseline.base_commit, null);
});

test("validateBaseline: rejects a wrong run count", async () => {
  const { validateBaseline } = await bench();
  const obj = validBaseline();
  obj.runs = obj.runs.slice(0, 4);
  assert.notDeepEqual(validateBaseline(obj), []);
});

test("validateBaseline: rejects a median that does not match the runs", async () => {
  const { validateBaseline } = await bench();
  const obj = validBaseline();
  obj.ram_idle_mb = 999;
  assert.notDeepEqual(validateBaseline(obj), []);
});

test("validateBaseline: rejects another platform", async () => {
  const { validateBaseline } = await bench();
  const obj = validBaseline();
  obj.os.platform = "linux";
  assert.notDeepEqual(validateBaseline(obj), []);
});

test("validateBaseline: rejects a short commit", async () => {
  const { validateBaseline } = await bench();
  const obj = validBaseline();
  obj.commit = "abc";
  assert.notDeepEqual(validateBaseline(obj), []);
});

test("validateBaseline: rejects a missing CPU name", async () => {
  const { validateBaseline } = await bench();
  const obj = validBaseline();
  delete (obj.machine as { cpu?: string }).cpu;
  assert.notDeepEqual(validateBaseline(obj), []);
});

test("validateBaseline: null is not an object", async () => {
  const { validateBaseline } = await bench();
  assert.deepEqual(validateBaseline(null), ["not an object"]);
});

test("drift: 9.9 % is ok, 10 % on either metric is not", async () => {
  const { drift } = await bench();
  const small = drift({ ram_idle_mb: 100, cold_start_ms: 1000 }, { ram_idle_mb: 109.9, cold_start_ms: 1099 });
  assert.ok(Math.abs(small.ram - 0.099) < 1e-9);
  assert.ok(Math.abs(small.cold - 0.099) < 1e-9);
  assert.equal(small.ok, true);
  assert.equal(drift({ ram_idle_mb: 100, cold_start_ms: 1000 }, { ram_idle_mb: 110, cold_start_ms: 1000 }).ok, false);
  assert.equal(drift({ ram_idle_mb: 100, cold_start_ms: 1000 }, { ram_idle_mb: 100, cold_start_ms: 1100 }).ok, false);
});

test("parseArgs: defaults, all three flags, every error case", async () => {
  const { parseArgs } = await bench();
  assert.deepEqual(parseArgs([]), { runs: 5, out: "docs/benchmark-baseline.json", compare: undefined });
  assert.deepEqual(parseArgs(["--runs", "2", "--out", "x.json", "--compare", "y.json"]), {
    runs: 2,
    out: "x.json",
    compare: "y.json",
  });
  assert.throws(() => parseArgs(["--runs", "0"]), /--runs needs a positive integer/);
  assert.throws(() => parseArgs(["--runs", "1.5"]), /--runs needs a positive integer/);
  assert.throws(() => parseArgs(["--runs", "nope"]), /--runs needs a positive integer/);
  assert.throws(() => parseArgs(["--out"]), /--out needs a path/);
  assert.throws(() => parseArgs(["--compare"]), /--compare needs a path/);
  assert.throws(() => parseArgs(["--wat"]), /unknown flag --wat/);
});
