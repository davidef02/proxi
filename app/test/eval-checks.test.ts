import { test, after } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  checkSolidSkipped,
  checkTaughtGroups,
  checkCurriculum,
  checkMinProbes,
  checkStrands,
  type ProbeLog,
} from "../eval/checks.ts";
import { buildFixture } from "../eval/fixture.ts";
import { courseIndexBlock } from "../server/courseindex.ts";
import { readSnapshot } from "../server/graph/store.ts";

const dirs: string[] = [];
after(() => dirs.forEach((d) => fs.rmSync(d, { recursive: true, force: true })));

/** Only the question matters for the checks; options and answer are required by the type. */
const probe = (question: string): ProbeLog => ({ question, options: [], answer: "ok" });

test("checkSolidSkipped: a usage hit counts even in Nuxt wording, a bare mention does not", () => {
  assert.deepEqual(checkSolidSkipped([probe("Cosa restituisce ref(0).value?")]), [
    'solid concept probed: "Cosa restituisce ref(0).value?"',
  ]);
  assert.deepEqual(
    checkSolidSkipped([probe("In Nuxt, cosa stampa console.log(ref(1).value) dentro una pagina?")]),
    ['solid concept probed: "In Nuxt, cosa stampa console.log(ref(1).value) dentro una pagina?"'],
  );
  // Nuxt question that only names a solid concept: allowed, the lesson is about Nuxt
  assert.deepEqual(checkSolidSkipped([probe("Come si usa useFetch in Nuxt con un ref?")]), []);
  // pure Vue question aimed only at solid concepts: not allowed
  assert.deepEqual(checkSolidSkipped([probe("Quando si aggiorna un ref?")]), [
    'solid concept probed: "Quando si aggiorna un ref?"',
  ]);
});

test("checkSolidSkipped: a taught concept alongside the solid one excuses a pure-Vue question", () => {
  // defineProps (taught) is exercised; ref only named: the question still has something to teach him
  assert.deepEqual(checkSolidSkipped([probe("Come si passa un ref dentro defineProps?")]), []);
  // a taught API used on its own is not a solid concept either
  assert.deepEqual(checkSolidSkipped([probe("A cosa serve defineProps?")]), []);
});

test("checkTaughtGroups: one confirmation per group of taught concepts", () => {
  const one = [probe("Come si dichiarano le props di un componente?")];
  assert.deepEqual(checkTaughtGroups(one), []);

  const two = [
    probe("Come si dichiarano le props di un componente?"),
    probe("A cosa serve v-model in un campo di input?"),
  ];
  assert.deepEqual(checkTaughtGroups(two), ["group contratto confirmed 2 times"]);

  // Nuxt questions do not confirm the Vue group
  const nuxt = [
    probe("In Nuxt come passi le props a una pagina?"),
    probe("In Nuxt v-model funziona su una pagina?"),
  ];
  assert.deepEqual(checkTaughtGroups(nuxt), []);

  // a group holding a fragile and a missed concept is not "taught": revisit it as often as needed
  const tempo = [
    probe("Come si usa watch() dentro onMounted()?"),
    probe("watch() e onMounted() insieme: cosa cambia?"),
  ];
  assert.deepEqual(checkTaughtGroups(tempo), []);
});

test("checkMinProbes: the empty-graph scenario still asks a real probe", () => {
  assert.deepEqual(checkMinProbes([], 2), ["only 0 probes, need 2"]);
  assert.deepEqual(checkMinProbes([probe("A cosa serve un middleware?")], 2), ["only 1 probes, need 2"]);
  assert.deepEqual(
    checkMinProbes([probe("A cosa serve un middleware?"), probe("Come si caricano i dati?")], 2),
    [],
  );
});

test("checkCurriculum: no lesson whose subject is a solid concept", () => {
  assert.deepEqual(checkCurriculum(["Il computed ricalcola solo quando cambia"]), [
    'lesson on a solid concept: "Il computed ricalcola solo quando cambia"',
  ]);
  // Nuxt wording does not excuse a solid-only subject in a plan
  assert.deepEqual(checkCurriculum(["Il ref in Nuxt"]), ['lesson on a solid concept: "Il ref in Nuxt"']);
  assert.deepEqual(checkCurriculum(["In Nuxt computed() nei componenti"]), [
    'lesson on a solid concept: "In Nuxt computed() nei componenti"',
  ]);
  assert.deepEqual(checkCurriculum(["Nuxt carica i dati con useFetch"]), []);
  // watch is shaky, so it is fair game
  assert.deepEqual(checkCurriculum(["useFetch e watch in Nuxt"]), []);
});

test("checkStrands: probes span more than one Nuxt strand", () => {
  const same = [
    probe("Come si caricano i dati con useFetch?"),
    probe("Quando conviene useFetch invece di useAsyncData?"),
  ];
  assert.deepEqual(checkStrands(same, 2), ["probes cover 1 Nuxt strands (dati), need 2"]);

  const spread = [probe("Come si caricano i dati con useFetch?"), probe("A cosa serve un middleware in Nuxt?")];
  assert.deepEqual(checkStrands(spread, 2), []);

  assert.deepEqual(checkStrands([], 2), ["probes cover 0 Nuxt strands (none), need 2"]);
});

test("buildFixture: the course index shows the four statuses", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "eval-checks-"));
  dirs.push(root);
  await buildFixture(root, { graph: true });

  const block = await courseIndexBlock(root);
  const line = block.split("\n").find((l) => l.startsWith("- Fondamenti di Vue "));
  assert.ok(line, "the Vue chapter has a line in the course index");
  assert.match(line, /solido 2, fragile 1, insegnato 3, mancato 1/);
});

test("buildFixture: the graph carries one proposition per concept with its fixture status", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "eval-checks-"));
  dirs.push(root);
  await buildFixture(root, { graph: true });

  const snap = await readSnapshot(root);
  assert.equal(snap.propositions.length, 7, "one graph proposition per concept");
  const status = Object.fromEntries(snap.propositions.map((p) => [p.name, p.status]));
  assert.deepEqual(status, {
    ref: "solid",
    computed: "solid",
    props: "taught",
    emit: "taught",
    "v-model": "taught",
    watch: "shaky",
    "on-mounted": "missed",
  });
  // provenance points at the lesson files, otherwise the statuses above could never be matched
  for (const p of snap.propositions) {
    assert.match(p.provenance[0].chatId, /^vue-base\//);
    assert.match(p.provenance[0].messageId, /^(ref|computed|props|emit|v-model|watch|on-mounted)$/);
  }
});

test("buildFixture: no graph means nothing to reuse", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "eval-checks-"));
  dirs.push(root);
  await buildFixture(root, { graph: false });

  const block = await courseIndexBlock(root);
  assert.ok(!block.includes("vue-base"));
  assert.equal((await readSnapshot(root)).propositions.length, 0);
});
