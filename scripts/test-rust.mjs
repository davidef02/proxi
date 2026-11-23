// Runs the HTTP/SSE tests of the areas ported to Rust against the spawned `proxi` binary.
// Each porting issue appends its test files to RUST_PORTED.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

export const RUST_PORTED = [
  "app/test/harness.test.ts",
  "app/test/api-settings.test.ts",
  "app/test/api-limits.test.ts",
  "app/test/api-events.test.ts",
  "app/test/api-transcribe.test.ts",
  "app/test/api-update.test.ts",
  "app/test/api-courses.test.ts",
  "app/test/api-sessions.test.ts",
  "app/test/attachments.test.ts",
  "app/test/sessions.test.ts",
  "app/test/api-anki.test.ts",
  "app/test/graph-api.test.ts",
  "app/test/api-sync.test.ts",
  "app/test/api-exercises.test.ts",
  "app/test/agy.test.ts",
  "app/test/opencode.test.ts",
  "app/test/claude-version.test.ts",
  "app/test/api-engines.test.ts",
  "app/test/api-engine-models.test.ts",
  "app/test/eval-binary.test.ts",
  "app/test/api-jobs.test.ts",
  "app/test/api-extract.test.ts",
];

export function rustTestEnv(base = process.env) {
  return { ...base, PROXI_TEST_SERVER: "binary" };
}

// A hung test or hook (a server or child that never goes away) fails after 2 min instead of stalling
// the gate until the CI job timeout. No --test-force-exit: on Windows it can abort node with a libuv
// assertion (async.c, UV_HANDLE_CLOSING). Keep in step with the test:app script in package.json.
export const TEST_GUARDS = ["--test-timeout=120000"];

export function rustTestArgs(files = RUST_PORTED) {
  return ["--import", "tsx", "--test", ...TEST_GUARDS, ...files];
}

export function missingFiles(root, files = RUST_PORTED) {
  return files.filter((file) => !fs.existsSync(path.join(root, file)));
}

function main() {
  const missing = missingFiles(ROOT);
  if (missing.length > 0) {
    for (const file of missing) console.error(`test:rust: missing ${file}`);
    process.exit(1);
  }
  // no shell: the Windows .exe path would need quoting otherwise (L15)
  const result = spawnSync(process.execPath, rustTestArgs(), {
    cwd: ROOT,
    stdio: "inherit",
    env: rustTestEnv(),
  });
  process.exit(result.status ?? 1);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
