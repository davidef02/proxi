// Playwright webServer: fixture repo copy + createServer with fake-claude on a fixed port. Run `npm run app:build` first.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "../server/main.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // never reach the real Anki
const here = path.dirname(fileURLToPath(import.meta.url));
const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-e2e-"));
fs.cpSync(path.join(here, "../test/fixtures/repo"), root, { recursive: true });
const srv = await createServer({
  repoRoot: root,
  dataRoot: root,
  port: Number(process.env.E2E_PORT) || 4399,
  claudeBin: process.execPath,
  claudeBinArgs: [path.join(here, "../test/fake-claude.mjs")],
  addDirs: [],
  openBrowser: false,
});
console.log(`e2e server ${srv.url} repo ${root}`);
const stop = async () => {
  await srv.close().catch(() => {});
  fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 });
  process.exit(0);
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
