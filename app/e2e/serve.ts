// Playwright webServer: fixture repo copy served by the built `proxi` binary with fake-claude on a fixed port. Run `cargo build --release --locked` first.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { proxiBinary, spawnProxi } from "../test/binary.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // never reach the real Anki
const here = path.dirname(fileURLToPath(import.meta.url));
const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-e2e-"));
fs.cpSync(path.join(here, "../test/fixtures/repo"), root, { recursive: true });
const p = await spawnProxi(
  [
    "serve",
    "--port",
    String(Number(process.env.E2E_PORT) || 4399),
    "--repo-root",
    root,
    "--data-root",
    root,
    "--dist-dir",
    path.join(here, "../../app/web/dist"),
    "--claude-bin",
    process.execPath,
    "--claude-bin-arg",
    path.join(here, "../test/fake-claude.mjs"),
    "--anki-url",
    "http://127.0.0.1:9",
    "--memory-secrets",
    "--no-update-check",
  ],
  proxiBinary(),
);
console.log(`e2e server ${p.url} repo ${root}`);
const stop = async () => {
  await p.close().catch(() => {});
  fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
  process.exit(0);
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
