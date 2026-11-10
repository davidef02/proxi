// Every engine adapter hands the server's data folder to the agent process as LEARNER_DATA_ROOT, so the
// `npm run lesson|anki|home` CLIs the agent runs write into the folder the server reads (review r2 finding 2).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, it } from "node:test";
import { spawnAgy } from "../server/agy.ts";
import { spawnClaude } from "../server/claude.ts";
import { spawnOpencode } from "../server/opencode.ts";
import { writeSettings } from "../server/settings.ts";
import type { EngineCallbacks, EngineHandle, EngineLaunch, Provider } from "../server/types.ts";

type Spawn = (l: EngineLaunch, cb: EngineCallbacks) => EngineHandle;

/** A stand-in engine binary: prints the env var on stderr and exits. */
const ECHO = `console.error("DATA_ROOT=" + (process.env.LEARNER_DATA_ROOT ?? "UNSET")); process.exit(0);`;

const ENGINES: { provider: Provider; spawn: Spawn; perTurn: boolean }[] = [
  { provider: "claude", spawn: spawnClaude, perTurn: false },
  { provider: "agy", spawn: spawnAgy, perTurn: false },
  // opencode starts one process per turn: the env is checked on the process a send() launches
  { provider: "opencode", spawn: spawnOpencode, perTurn: true },
];

/** Runs the adapter against the echo script and returns the value the child saw. */
async function childDataRoot(tmp: string, e: (typeof ENGINES)[number], dataRoot: string | undefined): Promise<string> {
  const script = path.join(tmp, "echo-env.mjs");
  fs.writeFileSync(script, ECHO);
  const launch: EngineLaunch = {
    provider: e.provider,
    bin: { cmd: process.execPath, args: [script] },
    repoRoot: tmp,
    ...(dataRoot ? { dataRoot } : {}),
    scratchDir: path.join(tmp, "scratch"),
    model: "m",
    systemPrompt: "sys",
    grant: [],
    mcp: { name: "app", url: "http://127.0.0.1:9/mcp", token: "tok" },
    addDirs: [],
  };
  const stderr: string[] = [];
  let handle: EngineHandle | undefined;
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${e.provider}: no exit; stderr=${JSON.stringify(stderr)}`)), 10_000);
    const done = () => { clearTimeout(timer); resolve(); };
    handle = e.spawn(launch, {
      onEvent: (ev) => { if (e.perTurn && ev.type === "result") done(); },
      onStderr: (line) => stderr.push(line),
      onExit: () => done(),
    });
    if (e.perTurn) handle.send("ciao");
  });
  await handle?.stop();
  const line = stderr.find((l) => l.startsWith("DATA_ROOT="));
  assert.ok(line, `${e.provider}: the child printed nothing; stderr=${JSON.stringify(stderr)}`);
  return line.slice("DATA_ROOT=".length);
}

describe("engine adapters pass LEARNER_DATA_ROOT to the agent process", () => {
  let tmp = "";
  let saved: string | undefined;
  beforeEach(() => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "engine-data-root-env-"));
    writeSettings(tmp, { claudeAuth: "subscription" });
    saved = process.env.LEARNER_DATA_ROOT;
  });
  afterEach(() => {
    if (saved === undefined) delete process.env.LEARNER_DATA_ROOT;
    else process.env.LEARNER_DATA_ROOT = saved;
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  for (const e of ENGINES) {
    it(`${e.provider}: the launch's dataRoot is set in the child env`, async () => {
      delete process.env.LEARNER_DATA_ROOT;
      const dataRoot = path.join(tmp, "data");
      assert.equal(await childDataRoot(tmp, e, dataRoot), dataRoot);
    });

    it(`${e.provider}: the launch's dataRoot wins over a different LEARNER_DATA_ROOT in the server env`, async () => {
      // e.g. the pointer changed after the server resolved its folder, or createServer({ dataRoot }) was passed explicitly
      process.env.LEARNER_DATA_ROOT = path.join(tmp, "stale");
      const dataRoot = path.join(tmp, "data");
      assert.equal(await childDataRoot(tmp, e, dataRoot), dataRoot);
    });

    it(`${e.provider}: without a dataRoot the variable is not invented`, async () => {
      delete process.env.LEARNER_DATA_ROOT;
      assert.equal(await childDataRoot(tmp, e, undefined), "UNSET");
      // and the server's own value is inherited unchanged
      process.env.LEARNER_DATA_ROOT = path.join(tmp, "from-env");
      assert.equal(await childDataRoot(tmp, e, undefined), path.join(tmp, "from-env"));
    });
  }
});
