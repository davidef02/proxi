// Captures golden HTTP/SSE outputs from the Node server into crates/proxi/tests/fixtures/golden (A12). Regenerate only from Node, before the cutover.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { REPO_ROOT, startServer } from "./harness.ts";

export interface GoldenRequest {
  method: "GET" | "POST";
  path: string;
  body?: unknown;
  rawBody?: string;
}

export interface GoldenCase {
  name: string;
  request: GoldenRequest;
  response: { status: number; body: unknown };
  firstEvent?: unknown;
}

export interface GoldenFile {
  route: string;
  seed: Record<string, unknown>;
  /** Raw text files written verbatim under the data folder: the replay's input, never normalised (L16). */
  dataSeed?: Record<string, string>;
  cases: GoldenCase[];
}

export const GOLDEN_DIR = path.join(REPO_ROOT, "crates", "proxi", "tests", "fixtures", "golden");
export const ISO_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/;

interface Roots {
  repoRoot: string[];
  dataRoot: string[];
}

/** Every form the server may report for `dir`: the resolved path and, when it exists, the native realpath. */
export function rootVariants(dir: string): string[] {
  const out = [path.resolve(dir)];
  if (fs.existsSync(dir)) {
    const real = fs.realpathSync.native(dir);
    if (!out.includes(real)) out.push(real);
  }
  return out;
}

/** Deep copy with the volatile bits replaced, so the fixture does not depend on this machine or this run. */
export function normalise(value: unknown, roots: Roots): unknown {
  if (typeof value === "string") {
    if (roots.repoRoot.includes(value)) return "<repoRoot>";
    if (roots.dataRoot.includes(value)) return "<dataRoot>";
    return ISO_RE.test(value) ? "<iso>" : value;
  }
  if (Array.isArray(value)) return value.map((v) => normalise(v, roots));
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [key, v] of Object.entries(value)) out[key] = normalise(v, roots);
    return out;
  }
  return value;
}

interface ScriptedCase {
  name: string;
  request: GoldenRequest;
  /** Open GET /api/events first and record the first data event the request triggers. */
  awaitEvent?: boolean;
}

interface Script {
  route: string;
  seed: Record<string, unknown>;
  /** Raw text files written verbatim under the data folder (no added newline), before the server starts. */
  dataSeed?: Record<string, string>;
  cases: ScriptedCase[];
}

// Low entropy and no `sk` prefix: neither gitleaks nor the JSON fixtures may read it as a key (L12/L13).
const KEY = "x".repeat(16) + "abcd";

/** A lesson artifact as the engine writes it: the document as one JSON data island. */
function islandHtml(doc: unknown): string {
  return `<html><body><script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script></body></html>`;
}

/** A minimal FullLessonDocument with `count` one-line nodes. */
function lessonDoc(num: number, slug: string, title: string, count: number, dag = "", topic = title) {
  return {
    metadata: { lessonNumber: num, topic, title, slug, dateCreated: "", dateModified: "", version: "1", initialDag: dag },
    nodes: Array.from({ length: count }, (_, i) => ({
      nodeId: `n${i + 1}`,
      title: `Nodo ${i + 1}`,
      tier: "axiom",
      content: "x",
      math: [],
    })),
  };
}

// 13 lines, one of them entity-escaped: the reader truncates the snippet and unescapes it.
const pythonLines = Array.from({ length: 13 }, (_, i) => `l${i + 1}`);
pythonLines[2] = "l3 &lt;";

const secondaDoc = {
  metadata: {
    lessonNumber: 4,
    topic: "Seconda",
    title: "",
    slug: "seconda",
    dateCreated: "",
    dateModified: "",
    version: "1",
    initialDag: "graph TD\nN1[a]",
  },
  nodes: [
    { nodeId: "n1", title: "Nodo 1", tier: "axiom", content: "", summary: "  Fatto uno  ", math: ["a^2"] },
    {
      nodeId: "n2",
      title: "Nodo 2",
      tier: "axiom",
      content: "",
      ankiParams: { targetFact: "Fatto due" },
      steps: [{ stepId: "s1", content: "<p>x</p>", math: ["b"], codeBlocks: [{ language: "js", code: "1" }] }],
    },
    { nodeId: "n3", title: "Nodo 3", tier: "axiom", content: "<p><strong>La verità da tenere senza caveat.</strong> Il punto fermo.</p>" },
    {
      nodeId: "n4",
      title: "Nodo 4",
      tier: "axiom",
      content: `<p>Prima frase. Seconda frase! Terza.</p><pre><code class="language-python">${pythonLines.join("\n")}</code></pre>`,
    },
    { nodeId: "n5", title: "Nodo 5", tier: "axiom", content: "Testo\n```rust\nfn x() {}\n```" },
    { nodeId: "n6", tier: "axiom", content: "" },
  ],
};

const primaDoc = {
  metadata: {
    lessonNumber: 1,
    topic: "Prima",
    title: "Prima",
    slug: "prima",
    dateCreated: "",
    dateModified: "",
    version: "1",
    initialDag: "",
  },
  nodes: [{ nodeId: "n1", title: "Nodo 1", tier: "axiom", content: "<p>La <b>verità</b> pesa </b> poco.</p>", math: [] }],
  weight: "__float__",
  zeta: "sconosciuta",
};

const primaReferenceDoc = { title: "Sintesi prima", slug: "prima", nodes: [], formulas: [], misconceptions: [] };

const SCRIPTS: Script[] = [
  {
    route: "settings",
    seed: {},
    cases: [
      { name: "get-defaults", request: { method: "GET", path: "/api/settings" } },
      { name: "post-api-key", request: { method: "POST", path: "/api/settings", body: { anthropicApiKey: KEY } } },
      { name: "get-after-post", request: { method: "GET", path: "/api/settings" } },
      { name: "post-nested", request: { method: "POST", path: "/api/settings", body: { effort: { teacher: "high" }, agyModels: { teacher: "t" } } } },
      { name: "post-invalid-json", request: { method: "POST", path: "/api/settings", rawBody: "{" } },
      { name: "post-unknown-key", request: { method: "POST", path: "/api/settings", body: { nope: 1 } } },
    ],
  },
  {
    route: "limits",
    seed: { ".learner/limits.json": { fiveHour: { utilization: 0.42, resetsAt: 1700000000 }, at: "2026-11-12T10:00:00.000Z" } },
    cases: [
      { name: "get-seeded", request: { method: "GET", path: "/api/limits" } },
      { name: "post-provider-agy", request: { method: "POST", path: "/api/settings", body: { provider: "agy" } } },
      { name: "get-after-agy", request: { method: "GET", path: "/api/limits" } },
    ],
  },
  {
    route: "events",
    seed: {},
    cases: [
      { name: "limits-off", request: { method: "POST", path: "/api/settings", body: { provider: "opencode" } }, awaitEvent: true },
    ],
  },
  {
    route: "courses",
    seed: {},
    dataSeed: {
      "curriculum.json": JSON.stringify({
        chapters: [
          {
            title: "Algebra",
            slug: "algebra",
            lessons: [
              { title: "Vettori nel piano", slug: "vettori-nel-piano", plannedNodes: 3 },
              { title: "Prodotto scalare", slug: "prodotto-scalare" },
              { title: "Matrici", slug: "matrici", plannedNodes: 2 },
            ],
          },
          { title: "Lezioni singole", slug: "lezioni-singole", lessons: [] },
          { title: "Vuoto", slug: "vuoto", lessons: [] },
        ],
      }),
      "courses/algebra/MISSION.md": "# Algebra\n\n## Why\n\nCapire **vettori** e `matrici` con [esempi](http://x) concreti.\n\nSecondo paragrafo.\n",
      "courses/algebra/exercises/package.json": "{}",
      "courses/algebra/lessons/0001-vettori-nel-piano.html": islandHtml(
        lessonDoc(1, "vettori-nel-piano", "Vettori", 1, "graph TD\nN1[a] --> N2[b]\nN2 --> N3[c]"),
      ),
      "courses/algebra/reference/vettori-nel-piano.html": "<html></html>",
      "courses/algebra/lessons/0002-prodotto-scalare.html": islandHtml(
        lessonDoc(2, "prodotto-scalare", "Prodotto scalare", 2, "graph TD\nN1[a] --> N2[b]\nN2 --> N3[c]"),
      ),
      // Marker-less record: closes 0002 by number.
      "courses/algebra/learning-records/0002-chiusa.md": "# chiusa\n",
      "courses/algebra/lessons/0003-extra.html": islandHtml(lessonDoc(3, "extra", "Extra", 1)),
      "courses/algebra/lessons/0004-prosa.html": "<html>solo prosa</html>",
      "courses/algebra/lessons/0005-rotta.html": '<script type="application/json" id="lesson-data">{nope</script>',
      "courses/sciolto/MISSION.md": "# Sciolto\n\nIntro senza sezione Why: " + "parola ".repeat(50) + "\n",
      "courses/sciolto/lessons/0001-solo.html": islandHtml(lessonDoc(1, "solo", "", 1, "", "Solo topic")),
      "courses/lezioni-singole/lessons/0001-fourier.html": islandHtml(lessonDoc(1, "fourier", "Fourier", 1)),
      "courses/lezioni-singole/reference/fourier.html": "<html></html>",
      "courses/lezioni-singole/learning-records/0001-a.md": "# a\n\n<!-- lesson: fourier -->\n",
      "courses/lezioni-singole/lessons/0002-entropia.html": islandHtml(lessonDoc(2, "entropia", "Entropia", 0, "graph TD\nN1[a] --> N2[b]")),
    },
    cases: [
      { name: "courses", request: { method: "GET", path: "/api/courses" } },
      { name: "standalone", request: { method: "GET", path: "/api/standalone-lessons" } },
      { name: "courses-again", request: { method: "GET", path: "/api/courses" } },
    ],
  },
  {
    route: "lesson",
    seed: {},
    dataSeed: {
      // JSON.stringify would print 1: the island writes the float literally, so a replay must normalise it to 1.
      "courses/demo/lessons/0001-prima.html": `<html><body><script type="application/json" id="lesson-data">${JSON.stringify(primaDoc).replace('"__float__"', "1.0")}</script></body></html>`,
      "courses/demo/lessons/0002-prosa.html": "<html>solo prosa</html>",
      "courses/demo/lessons/0003-rotta.html": '<script type="application/json" id="lesson-data">{nope</script>',
      "courses/demo/reference/prima.html": [
        "<!doctype html>",
        "<html><head>",
        "<title> Sintesi prima </title>",
        "<style>a{}</style>",
        "<link rel=x>",
        "</head><body>",
        "<script>1</script>",
        "<main><h2>Ciao</h2></main>",
        `<script type="application/json" id="reference-data">${JSON.stringify(primaReferenceDoc)}</script>`,
        "</body></html>",
      ].join("\n"),
      "courses/demo/lessons/0004-seconda.html": islandHtml(secondaDoc),
      "courses/demo/learning-records/0004-x.md":
        "# x\r\n\r\n## Evidence\r\n\r\n- Confonde il Nodo 2 con il 3\r\n* Slip sul node abc-1\r\n\r\n<!-- lesson: seconda -->\r\n",
      "courses/demo/lessons/0005-terza.html": islandHtml(lessonDoc(5, "terza", "Terza", 1)),
      "courses/demo/learning-records/0005-y.md": "# y\n\n## Evidence\n\nTutto bene. Un errore sul segno. Fine\n\n## Next\n",
    },
    cases: [
      { name: "lesson-ok", request: { method: "GET", path: "/api/lesson?path=courses/demo/lessons/0001-prima.html" } },
      { name: "lesson-missing", request: { method: "GET", path: "/api/lesson?path=courses/demo/lessons/0099-no.html" } },
      { name: "lesson-prose", request: { method: "GET", path: "/api/lesson?path=courses/demo/lessons/0002-prosa.html" } },
      { name: "lesson-malformed", request: { method: "GET", path: "/api/lesson?path=courses/demo/lessons/0003-rotta.html" } },
      { name: "lesson-bad-path", request: { method: "GET", path: "/api/lesson?path=courses/demo/MISSION.md" } },
      { name: "doc-reference", request: { method: "GET", path: "/api/doc?path=courses/demo/reference/prima.html" } },
      { name: "doc-lesson", request: { method: "GET", path: "/api/doc?path=courses/demo/lessons/0002-prosa.html" } },
      { name: "doc-missing", request: { method: "GET", path: "/api/doc?path=courses/demo/reference/nope.html" } },
      { name: "doc-bad-path", request: { method: "GET", path: "/api/doc?path=courses/demo/exercises/x.html" } },
      { name: "reference-island", request: { method: "GET", path: "/api/reference?path=courses/demo/reference/prima.html" } },
      { name: "reference-fallback", request: { method: "GET", path: "/api/reference?path=courses/demo/reference/seconda.html" } },
      { name: "reference-fallback-sentences", request: { method: "GET", path: "/api/reference?path=courses/demo/reference/terza.html" } },
      { name: "reference-missing", request: { method: "GET", path: "/api/reference?path=courses/demo/reference/nope.html" } },
      { name: "reference-lesson-path", request: { method: "GET", path: "/api/reference?path=courses/demo/lessons/0004-seconda.html" } },
    ],
  },
];

/** Writes `<outDir>/<route>.json` for every scripted route and returns the file paths. Always Node mode. */
export async function captureGolden(outDir: string = GOLDEN_DIR): Promise<string[]> {
  fs.mkdirSync(outDir, { recursive: true });
  const written: string[] = [];
  for (const script of SCRIPTS) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), `golden-${script.route}-`));
    const repo = path.join(dir, "repo");
    const data = path.join(dir, "data");
    fs.mkdirSync(repo);
    fs.mkdirSync(data);
    for (const [rel, content] of Object.entries(script.seed)) {
      const file = path.join(repo, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, JSON.stringify(content, null, 2) + "\n");
    }
    for (const [rel, content] of Object.entries(script.dataSeed ?? {})) {
      const file = path.join(data, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, content); // verbatim: no added newline
    }
    const roots = { repoRoot: rootVariants(repo), dataRoot: rootVariants(data) };
    const server = await startServer({ repoRoot: repo, dataRoot: data, mode: "node" });
    try {
      const cases: GoldenCase[] = [];
      for (const c of script.cases) cases.push(await captureCase(server.url, c, roots));
      const target = path.join(outDir, `${script.route}.json`);
      // The seed is the replay's input, so it stays raw; only the captured responses are normalised.
      const file: GoldenFile = {
        route: script.route,
        seed: script.seed,
        ...(script.dataSeed ? { dataSeed: script.dataSeed } : {}),
        cases,
      };
      fs.writeFileSync(target, JSON.stringify(file, null, 2) + "\n"); // always LF, never os.EOL
      written.push(target);
    } finally {
      await server.close();
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
    }
  }
  return written;
}

async function captureCase(base: string, c: ScriptedCase, roots: Roots): Promise<GoldenCase> {
  const events = c.awaitEvent ? openEvents(base) : undefined;
  try {
    if (events) await events.ready;
    const res = await send(base, c.request);
    const body = normalise(JSON.parse(await res.text()), roots);
    const out: GoldenCase = { name: c.name, request: c.request, response: { status: res.status, body } };
    // Node stamps the event with the current time: normalise it like any other field (app/server/main.ts:293).
    if (events) out.firstEvent = normalise(await events.first(5000), roots);
    return out;
  } finally {
    events?.close();
  }
}

function send(base: string, request: GoldenRequest): Promise<Response> {
  if (request.method === "GET") return fetch(base + request.path);
  return fetch(base + request.path, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: request.rawBody ?? JSON.stringify(request.body),
  });
}

/** SSE client on /api/events: `ready` resolves once the headers arrive, `first` waits for the first data event. */
function openEvents(base: string) {
  const events: unknown[] = [];
  const ac = new AbortController();
  let started!: () => void;
  let failed!: (error: unknown) => void;
  const ready = new Promise<void>((resolve, reject) => {
    started = resolve;
    failed = reject;
  });
  void (async () => {
    const res = await fetch(`${base}/api/events`, { signal: ac.signal });
    if (!res.ok) throw new Error(`GET /api/events: ${res.status}`);
    started();
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
  })().catch(failed);
  return {
    ready,
    async first(ms: number): Promise<unknown> {
      const end = Date.now() + ms;
      for (;;) {
        if (events.length > 0) return events[0];
        if (Date.now() > end) throw new Error(`no event on /api/events within ${ms} ms`);
        await new Promise((r) => setTimeout(r, 25));
      }
    },
    close: () => ac.abort(),
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const at = process.argv.indexOf("--out");
  const outDir = at === -1 ? GOLDEN_DIR : path.resolve(process.argv[at + 1] ?? GOLDEN_DIR);
  captureGolden(outDir)
    .then((files) => {
      for (const file of files) console.log(file);
    })
    .catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exit(1);
    });
}
