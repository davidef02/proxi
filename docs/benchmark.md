# Benchmark baseline

`docs/benchmark-baseline.json` is the Electron + Node baseline for M05 and M06. Compare a run only with a baseline
generated on the same machine: the `machine` and `os` fields record what it ran on.

## Metric

Idle RAM is the sum of the Windows Private Working Set (`WorkingSetPrivate`, from
`Win32_PerfRawData_PerfProc_Process`) over the Electron main process and all its descendants. The driver samples it
`10 s` after the course page becomes interactive. MB = bytes / 2^20 (1 decimal). `app.getAppMetrics()` is used only
to check that every Electron process is inside the sampled tree; its `privateBytes` and `workingSetSize` are never
summed.

## Interactive moment and cold start

Cold start runs from the launch of `electron.exe app/desktop/main.mjs` to the moment
`[data-testid="next-action"]` is visible and enabled on `#/c/demo`: the fixture course `demo`, opened directly with
`LEARNER_OPEN_HASH`. The UI is prebuilt; a stale `app/web/dist` is rebuilt before timing starts. Time is measured by
the driver, so Playwright/CDP overhead is included and constant across runs.

## Runs

Each execution discards 1 warm-up launch, then measures 5 runs. The file stores those 5 runs and their medians:
`ram_idle_mb` and `cold_start_ms`. Every run is isolated: a temp copy of the fixture repo, a temp userData folder,
fake claude and an unreachable Anki URL.

## Rerun

On Windows, with other heavy apps closed (about 2 minutes):

```bash
npm run bench:baseline
```

This writes `docs/benchmark-baseline.json`. To check repeatability, run twice and compare the medians:

```bash
npm run bench:baseline -- --out <tmp>/first.json
npm run bench:baseline -- --compare <tmp>/first.json
```

The second command exits 0 only when both medians drift less than 10 %. `--runs <n>` is for quick tries only; the
committed baseline must be a default 5-run execution.

`commit` is HEAD when the file was generated, `base_commit` is the merge-base with `main` (it survives a squash
merge), and `dirty` records whether the worktree had uncommitted changes.
