import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { CONFLICT, getWorkspace, listExercises, parseVitestReport, revealHint, runExercise, saveExerciseFile } from "../server/exercises.ts";

test("listExercises: filters dirs, lists files, reads ISTRUZIONI.md", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ex-"));
  const ex = path.join(root, "courses/c/exercises");
  for (const d of ["01-a", "zz-verifica", "node_modules", "misc"]) fs.mkdirSync(path.join(ex, d), { recursive: true });
  fs.mkdirSync(path.join(ex, "01-a/sub"));
  fs.writeFileSync(path.join(ex, "01-a/Esercizio.vue"), "x");
  fs.writeFileSync(path.join(ex, "01-a/ISTRUZIONI.md"), "fai questo");
  fs.writeFileSync(path.join(ex, "package.json"), "{}");
  const list = await listExercises(root, "c");
  assert.deepEqual(list.map((e) => e.name), ["01-a", "zz-verifica"]);
  assert.deepEqual(list[0].files, ["Esercizio.vue", "ISTRUZIONI.md"]);
  assert.equal(list[0].instructions, "fai questo");
  assert.equal(list[0].dir, "courses/c/exercises/01-a");
  assert.deepEqual(await listExercises(root, "none"), []);
});

test("parseVitestReport: failing report is ok:false with failures, ANSI stripped, truncated", () => {
  const json = JSON.stringify({
    numTotalTests: 3, numPassedTests: 2, numFailedTests: 1, numFailedTestSuites: 1,
    testResults: [{ assertionResults: [
      { status: "passed", fullName: "a" },
      { status: "failed", fullName: "b ok", failureMessages: ["\u001b[31mAssertionError\u001b[39m: " + "x".repeat(2000)] },
      { status: "passed", fullName: "c" },
    ] }],
  });
  const r = parseVitestReport(json, 5);
  assert.equal(r.ok, false);
  assert.equal(r.error, undefined);
  assert.equal(r.passed, 2);
  assert.equal(r.failed, 1);
  assert.equal(r.failures[0].name, "b ok");
  assert.ok(r.failures[0].message.startsWith("AssertionError"));
  assert.equal(r.failures[0].message.length, 1000);
});

test("parseVitestReport: all green is ok", () => {
  const r = parseVitestReport(JSON.stringify({ numTotalTests: 1, numPassedTests: 1, numFailedTests: 0, testResults: [] }), 1);
  assert.equal(r.ok, true);
});

test("runExercise: unknown name and missing node_modules", async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ex-"));
  fs.mkdirSync(path.join(root, "courses/c/exercises/01-a"), { recursive: true });
  assert.match((await runExercise(root, "c", "99-nope")).error!, /sconosciuto/);
  assert.match((await runExercise(root, "c", "../x")).error!, /sconosciuto/);
  const r = await runExercise(root, "c", "01-a");
  assert.equal(r.ok, false);
  assert.match(r.error!, /package.json/);
});

function tempEx() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ex-"));
  const dir = path.join(root, "courses/c/exercises/01-a");
  fs.mkdirSync(path.join(dir, "node_modules"), { recursive: true });
  fs.writeFileSync(path.join(dir, "Esercizio.vue"), "stub");
  fs.writeFileSync(path.join(dir, "Esercizio.test.js"), "// t");
  fs.writeFileSync(path.join(dir, "ISTRUZIONI.md"), "testo");
  fs.writeFileSync(path.join(dir, "node_modules/x.js"), "x");
  return { root, dir };
}

test("workspace: tests readOnly, hidden files and node_modules skipped", async () => {
  const { root, dir } = tempEx();
  fs.writeFileSync(path.join(dir, "SUGGERIMENTI.json"), JSON.stringify({ hints: ["a", "b"], final: true, revealed: 1 }));
  const ws = (await getWorkspace(root, "c", "01-a"))!;
  assert.deepEqual(ws.files.map((f) => [f.path, f.readOnly]), [["Esercizio.test.js", true], ["Esercizio.vue", false]]);
  assert.deepEqual([ws.info.hints, ws.info.hintsRevealed, ws.info.final], [["a", "b"], 1, true]);
  assert.equal(ws.info.files.includes("SUGGERIMENTI.json"), false);
  assert.equal(await getWorkspace(root, "c", "../x"), null);
  assert.equal(await getWorkspace(root, "c", "09-none"), null);
});

test("saveExerciseFile: validates paths, refuses tests and escapes", () => {
  const { root, dir } = tempEx();
  const save = (p: string, c = "x") => saveExerciseFile(root, "c", "01-a", p, c);
  assert.equal(save("Esercizio.vue", "nuovo"), null);
  assert.equal(fs.readFileSync(path.join(dir, "Esercizio.vue"), "utf-8"), "nuovo");
  assert.equal(save("helpers/uno.js"), null);
  assert.equal(save("stile.css"), null);
  for (const bad of ["../x.js", "a/../../x.js", "/abs.js", "C:/x.js", "C:x.js", "a\\b.js", "node_modules/x.js", "Esercizio.test.js", "x.spec.ts", "ISTRUZIONI.md", "SUGGERIMENTI.json", "run.exe", ".app-written.json", ""]) {
    assert.notEqual(save(bad), null, bad);
  }
  assert.notEqual(save("Esercizio.vue", "x".repeat(200 * 1024 + 1)), null);
  assert.notEqual(saveExerciseFile(root, "c", "../01-a", "Esercizio.vue", "x"), null);
  assert.equal(fs.readFileSync(path.join(dir, "Esercizio.test.js"), "utf-8"), "// t");
});

test("revealHint: increments up to the total, none without hints", async () => {
  const { root, dir } = tempEx();
  assert.equal(await revealHint(root, "c", "01-a"), null);
  fs.writeFileSync(path.join(dir, "SUGGERIMENTI.json"), JSON.stringify({ hints: ["a", "b"], final: false, revealed: 0 }));
  assert.equal((await revealHint(root, "c", "01-a"))!.hintsRevealed, 1);
  assert.equal((await revealHint(root, "c", "01-a"))!.hintsRevealed, 2);
  assert.equal((await revealHint(root, "c", "01-a"))!.hintsRevealed, 2);
});

test("parseVitestReport lists every test in order", () => {
  const r = parseVitestReport(JSON.stringify({ numTotalTests: 2, numPassedTests: 1, numFailedTests: 1, testResults: [{ assertionResults: [{ status: "passed", fullName: "1. a" }, { status: "failed", fullName: "2. b", failureMessages: ["x"] }] }] }), 1);
  assert.deepEqual(r.tests, [{ name: "1. a", status: "passed" }, { name: "2. b", status: "failed" }]);
});

test("saveExerciseFile: stale baseVersion is a conflict and leaves the file alone", async () => {
  const { root, dir } = tempEx();
  const v = (await getWorkspace(root, "c", "01-a"))!.files.find((f) => f.path === "Esercizio.vue")!.version;
  fs.writeFileSync(path.join(dir, "Esercizio.vue"), "da VS Code");
  assert.equal(saveExerciseFile(root, "c", "01-a", "Esercizio.vue", "mio", v), CONFLICT);
  assert.equal(fs.readFileSync(path.join(dir, "Esercizio.vue"), "utf-8"), "da VS Code");
  const v2 = (await getWorkspace(root, "c", "01-a"))!.files.find((f) => f.path === "Esercizio.vue")!.version;
  assert.equal(saveExerciseFile(root, "c", "01-a", "Esercizio.vue", "mio", v2), null);
  assert.equal(saveExerciseFile(root, "c", "01-a", "nuovo.js", "x", ""), null); // new file: base is ""
});

test("read-only checks are case-insensitive", async () => {
  const { root, dir } = tempEx();
  for (const bad of ["Esercizio.TEST.ts", "x.Spec.js", "istruzioni.md", "suggerimenti.json"]) assert.notEqual(saveExerciseFile(root, "c", "01-a", bad, "x"), null, bad);
  fs.writeFileSync(path.join(dir, "Istruzioni.MD"), "x");
  fs.writeFileSync(path.join(dir, "Foo.TEST.ts"), "x");
  const ws = (await getWorkspace(root, "c", "01-a"))!;
  assert.equal(ws.files.some((f) => /istruzioni/i.test(f.path)), false);
  assert.equal(ws.files.find((f) => f.path === "Foo.TEST.ts")!.readOnly, true);
});

test("lastRun is persisted by runExercise and returned by listExercises", async () => {
  const { root, dir } = tempEx();
  const vitest = path.join(root, "courses/c/exercises/node_modules/vitest/vitest.mjs");
  fs.mkdirSync(path.dirname(vitest), { recursive: true });
  fs.writeFileSync(vitest, `import fs from "node:fs";const o=process.argv.find(a=>a.startsWith("--outputFile=")).slice(13);fs.writeFileSync(o,JSON.stringify({numTotalTests:1,numPassedTests:1,numFailedTests:0,testResults:[]}));`);
  assert.equal((await listExercises(root, "c"))[0].lastRun, undefined);
  const r = await runExercise(root, "c", "01-a");
  assert.equal(r.ok, true);
  assert.deepEqual((await listExercises(root, "c"))[0].lastRun, r);
  assert.equal((await getWorkspace(root, "c", "01-a"))!.info.lastRun!.ok, true);
  assert.equal(fs.existsSync(path.join(dir, ".app-lastrun.json")), true);
  assert.equal((await listExercises(root, "c"))[0].files.includes(".app-lastrun.json"), false);
});

test("runExercise installs the dependencies once when vitest is missing", async () => {
  const { root } = tempEx();
  const base = path.join(root, "courses/c/exercises");
  fs.writeFileSync(path.join(base, "package.json"), "{}");
  const fake = path.join(root, "fake-npm.mjs");
  fs.writeFileSync(fake, `import fs from "node:fs";fs.appendFileSync("install.log", process.argv[2] + "\\n");fs.mkdirSync("node_modules/vitest", { recursive: true });fs.writeFileSync("node_modules/vitest/vitest.mjs", ${JSON.stringify(`import fs from "node:fs";const o=process.argv.find(a=>a.startsWith("--outputFile=")).slice(13);fs.writeFileSync(o,JSON.stringify({numTotalTests:1,numPassedTests:1,numFailedTests:0,testResults:[]}));`)});`);
  process.env.LEARNER_NPM_BIN = fake;
  try {
    const [a, b] = await Promise.all([runExercise(root, "c", "01-a"), runExercise(root, "c", "01-a")]);
    assert.equal(a.ok && b.ok, true, a.error ?? b.error);
    assert.equal(fs.readFileSync(path.join(base, "install.log"), "utf-8"), "install\n");
  } finally {
    delete process.env.LEARNER_NPM_BIN;
  }
});
