# Benchmark baseline

`docs/benchmark-tauri.json` is the Tauri v2 + Rust baseline. Compare a run only with a baseline generated on
the same machine: the `machine` and `os` fields record what it ran on.

## Metric

Idle RAM is the sum of the Windows Private Working Set (`WorkingSetPrivate`, from
`Win32_PerfRawData_PerfProc_Process`) over the Tauri process tree (`proxi-app.exe` and all its descendants,
including the WebView2 browser processes and the spawned `proxi` backend). The driver samples it `10 s`
after the course page becomes interactive. MB = bytes / 2^20 (1 decimal).

## Interactive moment and cold start

Cold start runs from the launch of `src-tauri/target/release/proxi-app.exe` to the moment
`[data-testid="next-action"]:enabled` is visible on `#/c/demo`: the fixture course `demo`, opened directly with
`LEARNER_OPEN_HASH`. The UI is prebuilt; a stale `app/web/dist` is rebuilt before timing starts. Time is measured by
the driver from launch to selector visible over CDP (`--remote-debugging-port`), so CDP connect overhead is
included and constant across runs.

## Runs

Each execution discards 1 warm-up launch, then measures 5 runs. The file stores those 5 runs and their medians:
`ram_idle_mb` and `cold_start_ms`. Every run is isolated: a temp copy of the fixture repo with `app/web/dist`, an
isolated temp `WEBVIEW2_USER_DATA_FOLDER`, a temp `LEARNER_USER_DATA` folder, fake claude and an unreachable Anki URL.
Foreign WebView2 instances never join the measured tree.

The embedding model stays runtime-downloaded to `.learner/models` and out of the application bundle, protecting the
15 MB installer cap (enforced by `scripts/verify-release.mjs` in the release pipeline; not recorded in the
benchmark JSON).

## Rerun

On Windows, with other heavy apps closed (about 2 minutes):

```bash
npm run bench:baseline
```

This writes `docs/benchmark-tauri.json` and asserts against `docs/benchmark-rust.json` via
`--compare docs/benchmark-rust.json`: it exits 0 only when both target thresholds are met (`ram_idle_mb <= 144.6`
and `cold_start_ms <= 740`, deriving targets via `targetThresholds(prev)` where `ram = round1(170.1 * 0.85) = 144.6`
and `cold = 740 * 1.0 = 740`). Above either target, the command exits 1.

To check repeatability between two runs without the one-sided target assert:

```bash
npm run bench:baseline -- --out <tmp>/first.json
npm run bench:baseline -- --compare <tmp>/first.json --drift [--drift-limit <frac>]
```

With `--drift`, the comparison performs a symmetric drift check (default 10 %, or overridden by
`--drift-limit <frac>`). `--runs <n>` is for quick tries only; the committed baseline must be a default 5-run execution.

`commit` is HEAD when the file was generated, `base_commit` is the merge-base with `main` (it survives a squash
merge), and `dirty` records whether the worktree had uncommitted changes.
