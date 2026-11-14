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
  "app/test/api-courses.test.ts",
];

export function rustTestEnv(base = process.env) {
  return { ...base, PROXI_TEST_SERVER: "binary" };
}

export function rustTestArgs(files = RUST_PORTED) {
  return ["--import", "tsx", "--test", ...files];
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
