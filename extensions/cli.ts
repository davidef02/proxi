/**
 * Shared CLI seam for the three entrypoints.
 *
 * Flag parsing itself is `node:util.parseArgs` called inline by each entrypoint
 * (so TypeScript infers the flag types); this module only owns the one thing
 * they must agree on: when to self-execute, and how a non-zero status becomes
 * an exit code. `runCli` returns a status so tests can drive it in-process
 * without killing the runner.
 */

import * as path from "node:path";
import { fileURLToPath } from "node:url";

/** Runs `runCli` only when this module is the script node was started with. */
export function main(moduleUrl: string, runCli: (argv: string[]) => Promise<number>): void {
  const script = (process.argv[1] || "").replace(/\\/g, "/");
  if (!script || script.includes(".test.")) return;
  const stem = (p: string) => path.basename(p).replace(/\.[jt]s$/, "");
  if (stem(script) !== stem(fileURLToPath(moduleUrl))) return;

  runCli(process.argv.slice(2)).then(
    (code) => {
      if (code) process.exit(code);
    },
    (err) => {
      console.error("Fatal error:", err);
      process.exit(1);
    }
  );
}
