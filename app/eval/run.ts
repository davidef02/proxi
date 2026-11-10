// `npm run eval -- [scenario...]`: the scenarios in app/eval/scenarios.ts, one after the other, against
// the real configured engine. Outside CI: every scenario costs a real engine run.
import * as fs from "node:fs";
import * as path from "node:path";
import { parseArgs } from "node:util";
import { PROVIDERS, type Provider } from "../shared/protocol.ts";
import { runScenario } from "./harness.ts";
import { SCENARIOS, type Scenario } from "./scenarios.ts";

const REPO = path.resolve(import.meta.dirname, "../..");
const DEFAULT_TIMEOUT_MIN = 15;

async function main(): Promise<number> {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      engine: { type: "string" },
      settings: { type: "string" },
      timeout: { type: "string" },
    },
  });

  if (values.engine !== undefined && !PROVIDERS.includes(values.engine as Provider)) {
    console.error(`--engine must be one of: ${PROVIDERS.join(", ")}`);
    return 2;
  }
  const engine = values.engine as Provider | undefined;

  const unknown = positionals.filter((name) => !SCENARIOS.some((s) => s.name === name));
  if (unknown.length > 0) {
    console.error(`unknown scenario: ${unknown.join(", ")}`);
    console.error(`scenarios: ${SCENARIOS.map((s) => s.name).join(", ")}`);
    return 2;
  }
  const run: Scenario[] = positionals.length === 0 ? SCENARIOS : SCENARIOS.filter((s) => positionals.includes(s.name));

  const minutes = values.timeout === undefined ? DEFAULT_TIMEOUT_MIN : Number(values.timeout);
  if (!Number.isFinite(minutes) || minutes <= 0) {
    console.error(`--timeout must be a positive number of minutes, got ${values.timeout}`);
    return 2;
  }

  const settingsFile = values.settings ? path.resolve(values.settings) : path.join(REPO, ".learner", "settings.json");
  console.log(
    `settings: ${settingsFile}${fs.existsSync(settingsFile) ? "" : " (missing: the engine defaults apply)"}`
  );
  console.log(`engine: ${engine ?? "from settings"}`);
  console.log(`timeout: ${minutes} min per scenario`);

  let failed = false;
  let errored = false;
  for (const s of run) {
    console.log(`== ${s.name}`);
    let result;
    try {
      result = await runScenario(s, { engine, settingsFile, timeoutMs: minutes * 60_000, log: (l) => console.log(l) });
    } catch (err) {
      errored = true;
      console.log(`ERROR ${s.name}: ${err instanceof Error ? err.message : String(err)}`);
      continue;
    }

    console.log("Probe log:");
    if (result.probes.length === 0) console.log("  (none)");
    result.probes.forEach((p, i) => console.log(`  ${i + 1}. ${oneLine(p.question)} -> ${p.answer}`));
    if (result.curriculumTitles) console.log(`Curriculum: ${result.curriculumTitles.join(" | ")}`);

    const errors = s.check(result);
    if (!result.reachedStop) errors.push(`did not reach ${s.stopAt}`);
    if (errors.length === 0) {
      console.log(`PASS ${s.name}`);
    } else {
      failed = true;
      console.log(`FAIL ${s.name}`);
      for (const e of errors) console.log(`  - ${e}`);
    }
  }

  return errored ? 2 : failed ? 1 : 0;
}

const oneLine = (text: string) => text.replace(/\s+/g, " ").trim();

process.exitCode = await main();
