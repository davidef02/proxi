// Locate and spawn the built `proxi` binary; implementation lives in the shared module so the
// Electron main and the tests/eval use the same code.
export { proxiBinary, spawnProxi } from "../shared/proxi-bin.mjs";
export type { RunningProxi } from "../shared/proxi-bin.d.mts";
