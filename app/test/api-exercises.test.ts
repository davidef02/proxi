// The /api/exercises routes over HTTP/SSE, identical against the Node server and the proxi binary.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import type { GlobalEvent } from "../shared/protocol.ts";
import { startServer, type TestServer } from "./harness.ts";

/** A fresh repo and a separate data folder per test: the files on disk are the fixture. */
async function withServer(fn: (server: TestServer, repo: string, data: string) => Promise<void>): Promise<void> {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "api-exercises-repo-"));
  const data = fs.mkdtempSync(path.join(os.tmpdir(), "api-exercises-data-"));
  let server: TestServer | undefined;
  try {
    server = await startServer({ repoRoot: repo, dataRoot: data });
    await fn(server, repo, data);
  } finally {
    await server?.close();
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(data, { recursive: true, force: true, maxRetries: 3 });
  }
}

/** Writes `rel` under `root`, creating the parents. */
function write(root: string, rel: string, content: string): void {
  fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
  fs.writeFileSync(path.join(root, rel), content);
}

/** The demo course fixture every case starts from. */
function seed(data: string): void {
  write(data, "courses/demo/exercises/package.json", "{}");
  write(data, "courses/demo/exercises/01-a/Esercizio.vue", "stub");
  write(data, "courses/demo/exercises/01-a/Esercizio.test.js", "// t");
  write(data, "courses/demo/exercises/01-a/ISTRUZIONI.md", "testo");
  write(data, "courses/demo/exercises/01-a/node_modules/x.js", "x");
  write(data, "courses/demo/exercises/misc/x.js", "x");
}

async function withExercises(fn: (server: TestServer, data: string) => Promise<void>): Promise<void> {
  await withServer(async (server, _repo, data) => {
    seed(data);
    await fn(server, data);
  });
}

const sha1 = (text: string): string => createHash("sha1").update(text).digest("hex");

/** POST as the web UI does: the CSRF header plus JSON. A string body is sent raw. */
function post(server: TestServer, p: string, body: unknown): Promise<Response> {
  return fetch(`${server.url}${p}`, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

/** Same poll as api-events.test.ts. */
async function waitFor<T>(fn: () => T | undefined | false, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Minimal SSE client: `ready` resolves when the headers arrive, `close` aborts the stream. */
function sse(url: string) {
  const events: GlobalEvent[] = [];
  const ac = new AbortController();
  const ready = fetch(url, { signal: ac.signal });
  void ready
    .then(async (res) => {
      const dec = new TextDecoder();
      let buf = "";
      for await (const chunk of res.body as unknown as AsyncIterable<Uint8Array>) {
        buf += dec.decode(chunk, { stream: true });
        let i: number;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, i);
          buf = buf.slice(i + 2);
          const data = block.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join("");
          if (data) events.push(JSON.parse(data));
        }
      }
    })
    .catch(() => {});
  return { events, ready, close: () => ac.abort() };
}

/** A vitest stand-in: records argv/cwd, then writes the report vitest would. */
const RUN_FAKE = `import fs from "node:fs";
const args = process.argv.slice(2);
fs.appendFileSync("argv.json", JSON.stringify({ args, cwd: process.cwd() }));
const out = args.find((a) => a.startsWith("--outputFile=")).slice("--outputFile=".length);
fs.writeFileSync(out, JSON.stringify({ numTotalTests: 1, numPassedTests: 1, numFailedTests: 0, testResults: [{ assertionResults: [{ status: "passed", fullName: "somma" }] }] }));
`;

/** An npm stand-in: logs the subcommand and installs the vitest fake. */
const INSTALL_FAKE = `import fs from "node:fs";
fs.appendFileSync("install.log", process.argv[2] + "\\n");
fs.mkdirSync("node_modules/vitest", { recursive: true });
fs.writeFileSync("node_modules/vitest/vitest.mjs", ${JSON.stringify(RUN_FAKE)});
`;

describe("GET /api/exercises/:course", () => {
  it("lists the exercises with files, hints, the last run and instructions", async () => {
    await withExercises(async (server, data) => {
      const plain = await fetch(`${server.url}/api/exercises/demo`);
      assert.equal(plain.status, 200);
      const list = (await plain.json()) as Record<string, any>[];
      assert.deepEqual(list.map((e) => e.name), ["01-a"]);
      assert.deepEqual(Object.keys(list[0]), ["course", "name", "dir", "files", "instructions"]);
      assert.equal(list[0].dir, "courses/demo/exercises/01-a");
      assert.deepEqual(list[0].files, ["Esercizio.test.js", "Esercizio.vue", "ISTRUZIONI.md"]);

      write(data, "courses/demo/exercises/01-a/SUGGERIMENTI.json", JSON.stringify({ hints: ["a"], revealed: 0 }));
      write(data, "courses/demo/exercises/01-a/.app-lastrun.json", JSON.stringify({ ok: true }));
      const withMeta = (await (await fetch(`${server.url}/api/exercises/demo`)).json()) as Record<string, any>[];
      assert.deepEqual(Object.keys(withMeta[0]), ["course", "name", "dir", "files", "lastRun", "hints", "hintsRevealed", "final", "instructions"]);

      const none = await fetch(`${server.url}/api/exercises/nessuno`);
      assert.equal(none.status, 200);
      assert.deepEqual(await none.json(), []);

      const bad = await fetch(`${server.url}/api/exercises/Bad`);
      assert.equal(bad.status, 400);
      assert.deepEqual(await bad.json(), { error: ": Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/" });
    });
  });
});

describe("GET /api/exercises/:course/:name", () => {
  it("serves the workspace with readOnly tests, hashes and hidden files skipped", async () => {
    await withExercises(async (server) => {
      const res = await fetch(`${server.url}/api/exercises/demo/01-a`);
      assert.equal(res.status, 200);
      const body = (await res.json()) as Record<string, any>;
      assert.deepEqual(Object.keys(body), ["info", "files"]);
      assert.deepEqual(Object.keys(body.files[0]), ["path", "content", "version", "readOnly"]);
      assert.deepEqual(body.files.map((f: any) => [f.path, f.readOnly]), [["Esercizio.test.js", true], ["Esercizio.vue", false]]);
      assert.equal(body.files[1].version, sha1("stub"));

      for (const name of ["09-none", "misc"]) {
        const missing = await fetch(`${server.url}/api/exercises/demo/${name}`);
        assert.equal(missing.status, 404);
        assert.deepEqual(await missing.json(), { error: "Esercizio non trovato" });
      }
    });
  });
});

describe("POST /api/exercises/save", () => {
  it("saves an editable file and returns its new version", async () => {
    await withExercises(async (server, data) => {
      const res = await post(server, "/api/exercises/save", { course: "demo", name: "01-a", path: "Esercizio.vue", content: "nuovo" });
      assert.equal(res.status, 200);
      assert.deepEqual(await res.json(), { ok: true, version: sha1("nuovo") });
      assert.equal(fs.readFileSync(path.join(data, "courses/demo/exercises/01-a/Esercizio.vue"), "utf-8"), "nuovo");
    });
  });

  it("rejects a stale baseVersion and leaves the file alone", async () => {
    await withExercises(async (server, data) => {
      const file = path.join(data, "courses/demo/exercises/01-a/Esercizio.vue");
      const version = sha1("stub");
      fs.writeFileSync(file, "da VS Code");
      const res = await post(server, "/api/exercises/save", { course: "demo", name: "01-a", path: "Esercizio.vue", content: "mio", baseVersion: version });
      assert.equal(res.status, 409);
      assert.deepEqual(await res.json(), { error: "Il file è cambiato fuori dall'editor", code: "stale" });
      assert.equal(fs.readFileSync(file, "utf-8"), "da VS Code");
    });
  });

  it("refuses read-only files, unknown exercises and bad bodies", async () => {
    await withExercises(async (server) => {
      const readonly = await post(server, "/api/exercises/save", { course: "demo", name: "01-a", path: "Esercizio.test.js", content: "x" });
      assert.equal(readonly.status, 400);
      assert.deepEqual(await readonly.json(), { error: "File di sola lettura" });

      const missing = await post(server, "/api/exercises/save", { course: "demo", name: "09-none", path: "Esercizio.vue", content: "x" });
      assert.equal(missing.status, 404);
      assert.deepEqual(await missing.json(), { error: "Esercizio non trovato" });

      const malformed = await post(server, "/api/exercises/save", "{");
      assert.equal(malformed.status, 400);
      assert.deepEqual(await malformed.json(), { error: "Invalid JSON body" });

      const empty = await post(server, "/api/exercises/save", {});
      assert.equal(empty.status, 400);
      assert.deepEqual(await empty.json(), {
        error: "course: Invalid input: expected string, received undefined; name: Invalid input: expected string, received undefined; path: Invalid input: expected string, received undefined; content: Invalid input: expected string, received undefined",
      });

      const shortPath = await post(server, "/api/exercises/save", { course: "demo", name: "01-a", path: "", content: "x" });
      assert.equal(shortPath.status, 400);
      assert.deepEqual(await shortPath.json(), { error: "path: Too small: expected string to have >=1 characters" });

      const huge = await post(server, "/api/exercises/save", { course: "demo", name: "01-a", path: "Esercizio.vue", content: "x".repeat(600_001) });
      assert.equal(huge.status, 413);
      assert.deepEqual(await huge.json(), { error: "File troppo grande" });
    });
  });

  it("requires the x-learner-client header", async () => {
    await withExercises(async (server) => {
      const res = await fetch(`${server.url}/api/exercises/save`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ course: "demo", name: "01-a", path: "Esercizio.vue", content: "x" }),
      });
      assert.equal(res.status, 403);
      assert.deepEqual(await res.json(), { error: "Missing x-learner-client header" });
    });
  });

  it("emits one courses event for two quick saves", async () => {
    await withExercises(async (server) => {
      const stream = sse(`${server.url}/api/events`);
      try {
        assert.equal((await stream.ready).status, 200);
        const first = await post(server, "/api/exercises/save", { course: "demo", name: "01-a", path: "Esercizio.vue", content: "uno" });
        assert.equal(first.status, 200);
        await waitFor(() => stream.events.find((e) => e.type === "courses"));
        const second = await post(server, "/api/exercises/save", { course: "demo", name: "01-a", path: "Esercizio.vue", content: "due" });
        assert.equal(second.status, 200);
        await new Promise((r) => setTimeout(r, 300));
        assert.deepEqual(stream.events.filter((e) => e.type === "courses"), [{ type: "courses" }]);
      } finally {
        stream.close();
      }
    });
  });
});

describe("POST /api/exercises/hint", () => {
  it("reveals the next hint, caps at the total and writes Node's bytes", async () => {
    await withExercises(async (server, data) => {
      const file = path.join(data, "courses/demo/exercises/01-a/SUGGERIMENTI.json");
      write(data, "courses/demo/exercises/01-a/SUGGERIMENTI.json", JSON.stringify({ hints: ["a", "b"], final: false, revealed: 0 }));
      write(data, "courses/demo/exercises/02-b/README.md", "senza\n");

      const stream = sse(`${server.url}/api/events`);
      try {
        assert.equal((await stream.ready).status, 200);
        const first = await post(server, "/api/exercises/hint", { course: "demo", name: "01-a" });
        assert.equal(first.status, 200);
        assert.equal(((await first.json()) as Record<string, any>).hintsRevealed, 1);
        assert.equal(fs.readFileSync(file, "utf-8"), JSON.stringify({ hints: ["a", "b"], final: false, revealed: 1 }, null, 2) + "\n");
        await waitFor(() => stream.events.find((e) => e.type === "courses"));

        const second = await post(server, "/api/exercises/hint", { course: "demo", name: "01-a" });
        assert.equal(((await second.json()) as Record<string, any>).hintsRevealed, 2);
        const third = await post(server, "/api/exercises/hint", { course: "demo", name: "01-a" });
        assert.equal(((await third.json()) as Record<string, any>).hintsRevealed, 2);

        const withSession = await post(server, "/api/exercises/hint", { course: "demo", name: "01-a", sessionId: "nessuna" });
        assert.equal(withSession.status, 200);

        const noHints = await post(server, "/api/exercises/hint", { course: "demo", name: "02-b" });
        assert.equal(noHints.status, 404);
        assert.deepEqual(await noHints.json(), { error: "Nessun suggerimento per questo esercizio" });
      } finally {
        stream.close();
      }
    });
  });
});

describe("POST /api/exercises/run", () => {
  it("runs vitest, records argv and persists the last run", async () => {
    await withExercises(async (server, data) => {
      const base = path.join(data, "courses/demo/exercises");
      write(data, "courses/demo/exercises/node_modules/vitest/vitest.mjs", RUN_FAKE);

      const res = await post(server, "/api/exercises/run", { course: "demo", name: "01-a" });
      assert.equal(res.status, 200);
      const text = await res.text();
      const body = JSON.parse(text);
      assert.deepEqual(Object.keys(body), ["ok", "passed", "failed", "total", "failures", "tests", "durationMs", "at"]);
      assert.equal(body.ok, true);
      assert.deepEqual(body.tests, [{ name: "somma", status: "passed" }]);
      assert.equal(fs.readFileSync(path.join(base, "01-a/.app-lastrun.json"), "utf-8"), text);

      const argv = JSON.parse(fs.readFileSync(path.join(base, "argv.json"), "utf-8"));
      assert.deepEqual(argv.args.slice(0, 3), ["run", "01-a/", "--reporter=json"]);
      assert.equal(argv.args.length, 4);
      assert.match(argv.args[3], /^--outputFile=.+report\.json$/);
      assert.equal(fs.realpathSync(argv.cwd), fs.realpathSync(base));

      const list = (await (await fetch(`${server.url}/api/exercises/demo`)).json()) as Record<string, any>[];
      assert.deepEqual(list[0].lastRun, body);
    });
  });

  it("fails unknown exercises and a missing package.json with a clear error", async () => {
    await withExercises(async (server, data) => {
      const unknown = await post(server, "/api/exercises/run", { course: "demo", name: "misc" });
      assert.equal(unknown.status, 200);
      const unknownBody = (await unknown.json()) as Record<string, any>;
      assert.deepEqual(Object.keys(unknownBody), ["ok", "passed", "failed", "total", "failures", "durationMs", "at", "error"]);
      assert.equal(unknownBody.error, "Esercizio sconosciuto: misc");

      const missing = await post(server, "/api/exercises/run", { course: "demo", name: "09-none" });
      assert.equal(missing.status, 404);
      assert.deepEqual(await missing.json(), { error: "Esercizio non trovato" });

      fs.rmSync(path.join(data, "courses/demo/exercises/package.json"));
      const noPkg = await post(server, "/api/exercises/run", { course: "demo", name: "01-a" });
      const noPkgBody = (await noPkg.json()) as Record<string, any>;
      assert.equal(noPkgBody.ok, false);
      assert.ok(String(noPkgBody.error).startsWith("Manca package.json in "));
      assert.ok(String(noPkgBody.error).endsWith(path.join("courses", "demo", "exercises")));
    });
  });

  it("installs the dependencies once when vitest is missing", async () => {
    const repo = fs.mkdtempSync(path.join(os.tmpdir(), "api-exercises-repo-"));
    const data = fs.mkdtempSync(path.join(os.tmpdir(), "api-exercises-data-"));
    const fake = path.join(repo, "fake-npm.mjs");
    fs.writeFileSync(fake, INSTALL_FAKE);
    const previous = process.env.LEARNER_NPM_BIN;
    process.env.LEARNER_NPM_BIN = fake;
    let server: TestServer | undefined;
    try {
      seed(data);
      server = await startServer({ repoRoot: repo, dataRoot: data });
      const [a, b] = await Promise.all([
        post(server, "/api/exercises/run", { course: "demo", name: "01-a" }),
        post(server, "/api/exercises/run", { course: "demo", name: "01-a" }),
      ]);
      const bodies = [(await a.json()) as Record<string, any>, (await b.json()) as Record<string, any>];
      assert.equal(bodies.every((r) => r.ok), true, bodies.map((r) => r.error).join("; "));
      assert.equal(fs.readFileSync(path.join(data, "courses/demo/exercises/install.log"), "utf-8"), "install\n");
    } finally {
      if (previous === undefined) delete process.env.LEARNER_NPM_BIN;
      else process.env.LEARNER_NPM_BIN = previous;
      await server?.close();
      fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
      fs.rmSync(data, { recursive: true, force: true, maxRetries: 3 });
    }
  });
});

describe("POST /api/exercises/open", () => {
  it("refuses unknown exercises and bad courses without launching the editor", async () => {
    await withExercises(async (server) => {
      const missing = await post(server, "/api/exercises/open", { course: "demo", name: "09-none" });
      assert.equal(missing.status, 404);
      assert.deepEqual(await missing.json(), { error: "Esercizio non trovato" });

      const bad = await post(server, "/api/exercises/open", { course: "Bad", name: "01-a" });
      assert.equal(bad.status, 400);
      assert.deepEqual(await bad.json(), { error: "course: Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/" });
    });
  });
});
