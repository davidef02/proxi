// Courses, lesson reading and /files over HTTP, identical against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { LESSON_FILE_CSP } from "../shared/csp.ts";
import { startServer, type TestServer } from "./harness.ts";

/** A fresh repo and a separate data folder per test: the files on disk are the fixture. */
async function withServer(fn: (server: TestServer, repo: string, data: string) => Promise<void>): Promise<void> {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "api-courses-repo-"));
  const data = fs.mkdtempSync(path.join(os.tmpdir(), "api-courses-data-"));
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

function lessonHtml(num: number, slug: string, title: string, nodes: number, dag = "", extra: Record<string, unknown> = {}): string {
  const doc = {
    metadata: { lessonNumber: num, topic: title, title, slug, dateCreated: "", dateModified: "", version: "1", initialDag: dag },
    nodes: Array.from({ length: nodes }, (_, i) => ({ nodeId: `n${i + 1}`, title: `Nodo ${i + 1}`, tier: "axiom", content: "x" })),
    ...extra,
  };
  return `<html><head><title>T</title></head><body><script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script></body></html>`;
}

/** Raw request: lets the test send un-normalised paths (fetch would resolve `..`). */
function raw(port: number, p: string, o: { method?: string; headers?: Record<string, string> } = {}): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: "127.0.0.1", port, path: p, method: o.method ?? "GET", headers: o.headers }, (res) => {
      let body = "";
      res.on("data", (d) => (body += d));
      res.on("end", () => resolve({ status: res.statusCode!, body }));
    });
    req.on("error", reject);
    req.end();
  });
}

/** The demo course fixture shared by the /api/courses cases. */
function seedDemo(data: string): void {
  write(data, "curriculum.json", JSON.stringify({
    chapters: [{ title: "Demo", slug: "demo", lessons: [
      { title: "Prima", slug: "prima", plannedNodes: 2 },
      { title: "Seconda", slug: "seconda" },
    ] }],
  }));
  write(data, "courses/demo/MISSION.md", "# M\n\n## Why\n\nImparare **Vue** `ref`\nin due righe.\n");
  write(data, "courses/demo/exercises/package.json", "{}");
  write(data, "courses/demo/lessons/0001-prima.html", lessonHtml(1, "prima", "Prima", 1, "graph TD\nN1[a] --> N2[b]\nN2 --> N3[c]"));
  write(data, "courses/demo/reference/prima.html", "<html></html>");
  write(data, "courses/demo/lessons/0002-extra.html", lessonHtml(2, "extra", "Extra", 1));
  write(data, "courses/loose/lessons/0001-solo.html", lessonHtml(1, "solo", "Solo", 1));
}

describe("GET /api/courses", () => {
  it("answers [] on an empty data folder and does not create courses/", async () => {
    await withServer(async (server, _repo, data) => {
      const res = await fetch(`${server.url}/api/courses`);
      assert.equal(res.status, 200);
      assert.deepEqual(await res.json(), []);
      assert.deepEqual(fs.readdirSync(data), []);
    });
  });

  it("matches the plan onto the taught lessons, with flags, destination and orphans", async () => {
    await withServer(async (server, _repo, data) => {
      seedDemo(data);
      const res = await fetch(`${server.url}/api/courses`);
      assert.equal(res.status, 200);
      const expected = [
        {
          slug: "demo",
          title: "Demo",
          lessons: [
            {
              title: "Prima",
              slug: "prima",
              lessonPath: "courses/demo/lessons/0001-prima.html",
              lessonNumber: 1,
              nodeCount: 1,
              referencePath: "courses/demo/reference/prima.html",
              inProgress: true,
              plannedNodes: 2,
            },
            { title: "Seconda", slug: "seconda" },
          ],
          orphans: [
            { title: "Extra", slug: "extra", lessonPath: "courses/demo/lessons/0002-extra.html", lessonNumber: 2, nodeCount: 1, inProgress: false },
          ],
          hasExercises: true,
          hasMission: true,
          destination: "Imparare Vue ref in due righe.",
        },
        {
          slug: "loose",
          title: "loose",
          lessons: [],
          orphans: [
            { title: "Solo", slug: "solo", lessonPath: "courses/loose/lessons/0001-solo.html", lessonNumber: 1, nodeCount: 1, inProgress: false },
          ],
          hasExercises: false,
          hasMission: false,
        },
      ];
      // Serialised string: key order is part of the response.
      assert.equal(JSON.stringify(await res.json()), JSON.stringify(expected));
      assert.equal(fs.existsSync(path.join(data, "courses", "lezioni-singole", "MISSION.md")), true);
    });
  });

  it("a learning record closes the lesson", async () => {
    await withServer(async (server, _repo, data) => {
      seedDemo(data);
      write(data, "courses/demo/learning-records/0007-y.md", "# y\n\n<!-- lesson: prima -->\n");
      const body = (await (await fetch(`${server.url}/api/courses`)).json()) as Array<Record<string, any>>;
      assert.equal(body[0].lessons[0].slug, "prima");
      assert.equal(body[0].lessons[0].inProgress, false);
    });
  });
});

describe("GET /api/standalone-lessons", () => {
  it("lists the standalone lessons in lesson order, with reference and status", async () => {
    await withServer(async (server, _repo, data) => {
      write(data, "curriculum.json", JSON.stringify({ chapters: [] }));
      write(data, "courses/lezioni-singole/MISSION.md", "# Mio contenitore\n\n## Why\n\nMarker.\n");
      write(
        data,
        "courses/lezioni-singole/lessons/0001-trasformata-di-fourier.html",
        lessonHtml(1, "trasformata-di-fourier", "La trasformata di Fourier scompone un segnale", 2),
      );
      write(data, "courses/lezioni-singole/reference/trasformata-di-fourier.html", "<html></html>");
      write(data, "courses/lezioni-singole/learning-records/0001-chiusa.md", "# chiusa\n\n<!-- lesson: trasformata-di-fourier -->\n");
      write(
        data,
        "courses/lezioni-singole/lessons/0002-entropia.html",
        lessonHtml(2, "entropia", "L'entropia di Shannon misura la sorpresa media", 0, "graph TD\nN1[a] --> N2[b]"),
      );

      const res = await fetch(`${server.url}/api/standalone-lessons`);
      assert.equal(res.status, 200);
      const expected = [
        {
          title: "La trasformata di Fourier scompone un segnale",
          slug: "trasformata-di-fourier",
          lessonPath: "courses/lezioni-singole/lessons/0001-trasformata-di-fourier.html",
          lessonNumber: 1,
          nodeCount: 2,
          referencePath: "courses/lezioni-singole/reference/trasformata-di-fourier.html",
          inProgress: false,
        },
        {
          title: "L'entropia di Shannon misura la sorpresa media",
          slug: "entropia",
          lessonPath: "courses/lezioni-singole/lessons/0002-entropia.html",
          lessonNumber: 2,
          nodeCount: 0,
          inProgress: true,
        },
      ];
      assert.equal(JSON.stringify(await res.json()), JSON.stringify(expected));
      // Create-only: the learner's own MISSION.md is never overwritten.
      assert.match(fs.readFileSync(path.join(data, "courses", "lezioni-singole", "MISSION.md"), "utf-8"), /Marker\./);
    });
  });
});

describe("GET /api/lesson", () => {
  it("echoes the data island as it is, key order included", async () => {
    await withServer(async (server, _repo, data) => {
      const island = {
        metadata: { topic: "Prima", title: "Prima", slug: "prima", lessonNumber: 1, dateCreated: "", dateModified: "", version: "1", initialDag: "" },
        nodes: [{ nodeId: "n1", title: "Nodo 1", tier: "axiom", content: "x" }],
        zeta: { b: 1, a: 2 },
      };
      write(data, "courses/demo/lessons/0001-prima.html", `<html><script type="application/json" id="lesson-data">${JSON.stringify(island)}</script></html>`);
      const res = await fetch(`${server.url}/api/lesson?path=courses/demo/lessons/0001-prima.html`);
      assert.equal(res.status, 200);
      assert.equal(JSON.stringify(await res.json()), JSON.stringify(island));
    });
  });

  it("answers missing, prose and malformed lessons with the Italian errors", async () => {
    await withServer(async (server, _repo, data) => {
      write(data, "courses/demo/lessons/0002-prosa.html", "<html>prosa</html>");
      write(data, "courses/demo/lessons/0003-rotta.html", `<html><script type="application/json" id="lesson-data">{nope</script></html>`);

      const missing = await fetch(`${server.url}/api/lesson?path=courses/demo/lessons/0099-no.html`);
      assert.equal(missing.status, 404);
      assert.deepEqual(await missing.json(), { error: "Lezione non trovata" });

      const prose = await fetch(`${server.url}/api/lesson?path=courses/demo/lessons/0002-prosa.html`);
      assert.equal(prose.status, 422);
      assert.deepEqual(await prose.json(), { error: "La lezione non ha dati strutturati" });

      const malformed = await fetch(`${server.url}/api/lesson?path=courses/demo/lessons/0003-rotta.html`);
      assert.equal(malformed.status, 422);
      assert.equal(typeof ((await malformed.json()) as Record<string, unknown>).error, "string");
    });
  });

  it("rejects a path outside the lesson rule, and a missing path", async () => {
    await withServer(async (server) => {
      for (const p of ["../secret.txt", "courses/demo/MISSION.md", "courses/demo/lessons/..%2f..%2fMISSION.md", "courses/demo/lessons/../../../secret.html", ""]) {
        const res = await fetch(`${server.url}/api/lesson?path=${p}`);
        assert.equal(res.status, 400, p);
        assert.deepEqual(await res.json(), { error: "Percorso della lezione non valido" }, p);
      }
      const noPath = await fetch(`${server.url}/api/lesson`);
      assert.equal(noPath.status, 400);
      assert.deepEqual(await noPath.json(), { error: "Percorso della lezione non valido" });
    });
  });
});

describe("GET /api/doc", () => {
  it("serves the title and the main/body markup without scripts, styles or links", async () => {
    await withServer(async (server, _repo, data) => {
      write(
        data,
        "courses/c/reference/x.html",
        `<html><head><title> T </title><style>a{}</style><link rel=x></head><body><script>1</script><main><h2>Ciao</h2></main></body></html>`,
      );
      write(data, "courses/c/lessons/0001-a.html", "<html><head></head><body><p>x</p></body></html>");

      const ref = await fetch(`${server.url}/api/doc?path=courses/c/reference/x.html`);
      assert.equal(ref.status, 200);
      assert.equal(JSON.stringify(await ref.json()), JSON.stringify({ title: "T", html: "<h2>Ciao</h2>" }));

      const lesson = await fetch(`${server.url}/api/doc?path=courses/c/lessons/0001-a.html`);
      assert.equal(lesson.status, 200);
      const body = (await lesson.json()) as Record<string, unknown>;
      assert.equal("title" in body, false);
      assert.equal(body.html, "<p>x</p>");
    });
  });

  it("answers a missing document and a path outside the document rule", async () => {
    await withServer(async (server) => {
      const missing = await fetch(`${server.url}/api/doc?path=courses/c/reference/nope.html`);
      assert.equal(missing.status, 404);
      assert.deepEqual(await missing.json(), { error: "Documento non trovato" });

      const bad = await fetch(`${server.url}/api/doc?path=courses/c/exercises/x.html`);
      assert.equal(bad.status, 400);
      assert.deepEqual(await bad.json(), { error: "Percorso del documento non valido" });
    });
  });
});

describe("GET /api/reference", () => {
  it("serves the reference island as it is", async () => {
    await withServer(async (server, _repo, data) => {
      const island = { title: "Sintesi", slug: "x", nodes: [], formulas: [], misconceptions: [] };
      write(
        data,
        "courses/c/reference/x.html",
        `<html><body><script type="application/json" id="reference-data">${JSON.stringify(island)}</script></body></html>`,
      );
      const res = await fetch(`${server.url}/api/reference?path=courses/c/reference/x.html`);
      assert.equal(res.status, 200);
      assert.equal(JSON.stringify(await res.json()), JSON.stringify(island));
    });
  });

  it("falls back to the lesson island and its learning record", async () => {
    await withServer(async (server, _repo, data) => {
      const dag = "graph TD\nN1[a] --> N2[b]";
      write(data, "courses/c/lessons/0002-b.html", lessonHtml(2, "b", "b", 2, dag));
      write(data, "courses/c/learning-records/0002-b.md", "# Rec\n\n## Evidence\n\n- Learner slip A\n\n<!-- lesson: b -->\n");

      const res = await fetch(`${server.url}/api/reference?path=courses/c/reference/b.html`);
      assert.equal(res.status, 200);
      const body = (await res.json()) as Record<string, any>;
      assert.equal(body.title, "b");
      assert.equal(body.nodes.length, 2);
      assert.equal(body.initialDag, dag);
      assert.ok(body.misconceptions.some((m: Record<string, any>) => m.description.includes("Learner slip A")));
    });
  });

  it("answers a missing reference and a lesson path that is not a reference", async () => {
    await withServer(async (server, _repo, data) => {
      write(data, "courses/c/lessons/0002-b.html", lessonHtml(2, "b", "b", 1));
      for (const p of ["courses/c/reference/nope.html", "courses/c/lessons/0002-b.html"]) {
        const res = await fetch(`${server.url}/api/reference?path=${p}`);
        assert.equal(res.status, 404, p);
        assert.deepEqual(await res.json(), { error: "Sintesi non trovata" }, p);
      }
    });
  });
});

describe("GET /files", () => {
  it("serves course files and assets with their type, nosniff and CORS", async () => {
    await withServer(async (server, repo, data) => {
      write(repo, "assets/x.css", "body{}");
      write(repo, "assets/f.woff2", "");
      write(repo, "secret.txt", "TOPSECRET");
      write(data, "courses/demo/MISSION.md", "# Demo\n\nCorpo è qui.\n");
      write(data, "secret.txt", "TOPSECRET");

      const md = await fetch(`${server.url}/files/courses/demo/MISSION.md`);
      assert.equal(md.status, 200);
      assert.equal(md.headers.get("content-type"), "text/markdown; charset=utf-8");
      assert.equal(md.headers.get("x-content-type-options"), "nosniff");
      assert.equal(md.headers.get("content-security-policy"), null);
      assert.equal(md.headers.get("access-control-allow-origin"), null);
      assert.equal(await md.text(), "# Demo\n\nCorpo è qui.\n");

      const css = await fetch(`${server.url}/files/assets/x.css`);
      assert.equal(css.status, 200);
      assert.equal(css.headers.get("content-type"), "text/css; charset=utf-8");
      assert.equal(css.headers.get("access-control-allow-origin"), "*");

      const font = await fetch(`${server.url}/files/assets/f.woff2`);
      assert.equal(font.status, 200);
      assert.equal(font.headers.get("content-type"), "font/woff2");
      assert.equal(font.headers.get("access-control-allow-origin"), "*");
    });
  });

  it("sandboxes lesson HTML with the shared CSP and the storage shim", async () => {
    await withServer(async (server, _repo, data) => {
      write(data, "courses/demo/lessons/0001-a.html", `<!doctype html><html><head><meta charset="utf-8"></head><body>a</body></html>`);
      write(data, "courses/demo/lessons/0002-b.html", `<!doctype html><p>b</p>`);

      const res = await fetch(`${server.url}/files/courses/demo/lessons/0001-a.html`);
      assert.equal(res.status, 200);
      assert.equal(res.headers.get("content-type"), "text/html; charset=utf-8");
      assert.equal(res.headers.get("content-security-policy"), LESSON_FILE_CSP);
      assert.equal(res.headers.get("x-content-type-options"), "nosniff");
      assert.equal(res.headers.get("access-control-allow-origin"), null);
      const body = await res.text();
      assert.equal(body.indexOf("<head>") + "<head>".length, body.indexOf("<script>try{localStorage"));

      const noHead = await (await fetch(`${server.url}/files/courses/demo/lessons/0002-b.html`)).text();
      assert.equal(noHead.indexOf("<!doctype html>") + "<!doctype html>".length, noHead.indexOf("<script>try{localStorage"));
    });
  });

  it("refuses paths outside the served roots", async () => {
    await withServer(async (server, repo, data) => {
      write(repo, "assets/x.css", "body{}");
      write(repo, "secret.txt", "TOPSECRET");
      write(repo, "courses/repo-only/MISSION.md", "# repo\n");
      write(data, "courses/demo/MISSION.md", "# Demo\n");
      write(data, "curriculum.json", JSON.stringify({ chapters: [] }));
      write(data, "secret.txt", "TOPSECRET");

      for (const p of [
        "/files/secret.txt",
        "/files/curriculum.json",
        "/files/..%2fsecret.txt",
        "/files/courses/..%2f..%2fsecret.txt",
        "/files/courses/..%5csecret.txt",
        "/files/courses/%00",
        "/files/courses/repo-only/MISSION.md",
        "/files/",
        "/files/courses/demo",
      ]) {
        const r = await raw(server.port, p);
        assert.equal(r.status, 404, p);
        assert.deepEqual(JSON.parse(r.body), { error: "Not found" }, p);
      }

      const bad = await raw(server.port, "/files/%E0%A4%A");
      assert.equal(bad.status, 400);
      assert.deepEqual(JSON.parse(bad.body), { error: "Bad request" });

      for (const p of ["/files/../secret.txt", "/files/%2e%2e/secret.txt"]) {
        assert.ok(!(await raw(server.port, p)).body.includes("TOPSECRET"), p);
      }
    });
  });

  it("refuses a symlink that leads outside the served roots", async () => {
    const outside = fs.mkdtempSync(path.join(os.tmpdir(), "api-courses-outside-"));
    try {
      write(outside, "leak.txt", "LEAKED");
      await withServer(async (server, _repo, data) => {
        write(data, "courses/demo/keep.txt", "keep");
        fs.symlinkSync(outside, path.join(data, "courses", "demo", "link"), "junction");
        const res = await fetch(`${server.url}/files/courses/demo/link/leak.txt`);
        assert.equal(res.status, 404);
        assert.ok(!(await res.text()).includes("LEAKED"));
      });
    } finally {
      fs.rmSync(outside, { recursive: true, force: true, maxRetries: 3 });
    }
  });
});
