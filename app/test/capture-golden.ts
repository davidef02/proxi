// Captures golden HTTP/SSE outputs from the Node server into crates/proxi/tests/fixtures/golden (A12). Regenerate only from Node, before the cutover.
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { driveBody, installFakeDrive, type FakeDriveSeed } from "./fake-drive.ts";
import type { FakeGithubSeed } from "./fake-github.ts";
import { installFakeGithub } from "./fake-github.ts";
import { FakeHttpServer, type RecordedRequest } from "./fake-servers.ts";
import { REPO_ROOT, startServer, type ServerMode } from "./harness.ts";

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
  /** Present on a `remote` case: the non-GET requests the fake GitHub received while it ran. */
  remote?: { method: string; path: string; body: unknown }[];
}

export interface GoldenFile {
  route: string;
  seed: Record<string, unknown>;
  /** Raw file contents written verbatim under the data folder (lessons, JSONL ledgers): the replay's input, never normalised. */
  dataSeed?: Record<string, string>;
  github?: FakeGithubSeed;
  drive?: FakeDriveSeed;
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
  /** Record what the fake GitHub received: a `github` seed must be set. */
  remote?: boolean;
}

export interface Script {
  route: string;
  seed: Record<string, unknown>;
  /** Raw file contents written verbatim under the data folder (lessons, JSONL ledgers): the replay's input, never normalised. */
  dataSeed?: Record<string, string>;
  /** Start a fake GitHub with this seed and point the server at it. */
  github?: FakeGithubSeed;
  /** Start a fake Google Drive with this seed and point the server at it. */
  drive?: FakeDriveSeed;
  cases: ScriptedCase[];
}

/** The hostname a commit message ends with, before normalisation. */
const HOST_SUFFIX = `(${os.hostname()})`;

function parseBody(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

/** Non-GET requests with their body, mapped by the fake that received them. */
function remoteRequests(
  requests: RecordedRequest[],
  body: (r: RecordedRequest) => unknown,
): { method: string; path: string; body: unknown }[] {
  return requests
    .filter((r) => r.method !== "GET")
    .map((r) => ({ method: r.method, path: r.path, body: body(r) }));
}

/** A JSON body, with the commit hostname swapped for `<host>` so the fixture is portable. */
function githubBody(r: RecordedRequest): unknown {
  const body = parseBody(r.body);
  if (body && typeof body === "object" && "message" in body) {
    const { message } = body as { message?: unknown };
    if (typeof message === "string" && message.endsWith(HOST_SUFFIX)) {
      (body as { message: string }).message = message.slice(0, -HOST_SUFFIX.length) + "(<host>)";
    }
  }
  return body;
}

type RemoteLog = { server: FakeHttpServer; body: (r: RecordedRequest) => unknown };

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

const GRAPH_AT = "2026-01-01T00:00:00.000Z";

/** sha1 of the `app.js` the exercises script seeds: the version the editor sends back on its first save. */
const APP_JS_SHA1 = createHash("sha1").update("// app\n").digest("hex");

/** One graph/nodes.jsonl line: a proposition with one provenance point. `extra` lands after `provenance`. */
function nodeLine(id: string, name: string, statement: string, chatId: string, messageId: string, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({ id, name, statement, aliases: [], createdAt: GRAPH_AT, provenance: [{ chatId, messageId }], ...extra });
}

/** One graph/chains.jsonl line: a lesson-level assertion (empty messageId). */
function chainLine(id: string, premises: string[], conclusion: string, chatId: string): string {
  return JSON.stringify({ id, premises, conclusion, createdAt: GRAPH_AT, provenance: [{ chatId, messageId: "" }] });
}

/** A lesson artifact: the HTML shell around its #lesson-data island. */
function lessonHtml(metadata: Record<string, unknown>, nodes: unknown[]): string {
  return `<html><body><script type="application/json" id="lesson-data">${JSON.stringify({ metadata, nodes })}</script></body></html>`;
}

export const SCRIPTS: Script[] = [
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
  {
    route: "graph",
    seed: {},
    dataSeed: {
      "curriculum.json": JSON.stringify({ chapters: [{ title: "Corso Demo", slug: "demo", lessons: [] }] }),
      "courses/demo/lessons/0001-t.html": lessonHtml(
        { lessonNumber: 1, topic: "T", title: "Tema", slug: "t", dateCreated: "", dateModified: "", version: "1" },
        [
          { nodeId: "alpha", title: "Alfa", tier: "axiom", content: "x" },
          { nodeId: "beta", title: "Beta", tier: "derivation", content: "x", dependencies: ["alpha", "ghost"] },
          { nodeId: "gamma", title: "Gamma", tier: "derivation", content: "x", dependencies: ["beta"] },
        ],
      ),
      "courses/demo/lessons/0002-u.html": lessonHtml(
        { lessonNumber: 2, topic: "Topic U", title: "", slug: "u", dateCreated: "", dateModified: "", version: "1" },
        [{ nodeId: "delta", title: "Delta", tier: "axiom", content: "x" }],
      ),
      "courses/demo/lessons/notes.html": "<html><body>appunti</body></html>",
      "courses/demo/progress.jsonl": [
        JSON.stringify({ at: "2026-01-02T00:00:00Z", sessionId: "s", course: "demo", lessonSlug: "t", nodeRefs: ["alpha"], kind: "check", correct: true, confidence: 20 }),
        JSON.stringify({ at: "2026-01-02T00:00:00Z", sessionId: "s", course: "demo", lessonSlug: "t", nodeRefs: ["beta"], kind: "check", correct: false }),
        JSON.stringify({ at: "2026-01-02T00:00:00Z", sessionId: "s", course: "demo", lessonSlug: "u", nodeRefs: ["N1"], kind: "check", correct: true, confidence: 90 }),
        JSON.stringify({ kind: "review", source: "anki", sessionId: "anki", course: "demo", lessonSlug: "t", nodeRefs: ["demo/t#gamma"], cardId: 7, reviewId: 100, ease: 3, interval: 30, lapses: 0, at: "2026-01-03T00:00:00Z" }),
        "not json",
      ]
        .map((line) => line + "\n")
        .join(""),
      "graph/nodes.jsonl": [
        nodeLine("n_a", "A", "A vale.", "demo/0001-t", "alpha"),
        nodeLine("n_b", "B", "B vale.", "demo/0001-t", "beta"),
        nodeLine("n_c", "C", "C vale.", "demo/0001-t", "gamma"),
        nodeLine("n_g", "G", "G vale.", "demo/0001-t", "alpha", { note: "kept" }),
        nodeLine("n_d", "D", "D vale.", "demo/0002-u", "delta"),
        nodeLine("n_e", "E", "E vale.", "other/0001-x", ""),
        nodeLine("n_f", "F", "F vale.", "other/0001-x", ""),
        JSON.stringify({ id: "n_a", name: "A bis", statement: "A riformulato", aliases: ["A2"], createdAt: GRAPH_AT, provenance: [{ chatId: "demo/0002-u", messageId: "delta" }] }),
        "{not json",
        '{"id":"n_bad","name":"x"}',
      ]
        .map((line) => line + "\n")
        .join(""),
      "graph/chains.jsonl": [
        chainLine("arg_abc", ["n_a", "n_b"], "n_c", "demo/0001-t"),
        chainLine("arg_cd", ["n_c"], "n_d", "demo/0002-u"),
        chainLine("arg_dup", ["n_b", "n_a"], "n_c", "other/0001-x"),
        chainLine("arg_ef", ["n_e"], "n_f", "other/0001-x"),
        chainLine("arg_dangling", ["n_missing"], "n_a", "other/0001-x"),
        chainLine("arg_gad", ["n_g", "n_a"], "n_d", "demo/0002-u"),
        '{"id":"arg_cut","premises":["n_a"]', // unterminated: a crashed append, kept verbatim
      ].join("\n"),
      "graph/positions.json": JSON.stringify({ n_a: { x: 1, y: 1 } }),
    },
    cases: [
      { name: "get-map", request: { method: "GET", path: "/api/map" } },
      { name: "get-graph", request: { method: "GET", path: "/api/graph" } },
      {
        name: "post-positions",
        request: {
          method: "POST",
          path: "/api/graph/positions",
          body: { positions: { n_a: { x: 5, y: 6.5 }, arg_ef: { x: 1, y: 2 }, ghost: { x: 1, y: 2 }, n_c: { x: "7", y: 1 } } },
        },
      },
      { name: "get-graph-after-positions", request: { method: "GET", path: "/api/graph" } },
      { name: "post-positions-missing", request: { method: "POST", path: "/api/graph/positions", body: {} } },
      { name: "post-positions-invalid-json", request: { method: "POST", path: "/api/graph/positions", rawBody: "{" } },
      { name: "delete-node", request: { method: "POST", path: "/api/graph/nodes/n_b/delete", body: {} }, awaitEvent: true },
      { name: "get-graph-after-delete", request: { method: "GET", path: "/api/graph" } },
      { name: "delete-chain", request: { method: "POST", path: "/api/graph/nodes/arg_gad/delete", body: {} } },
      { name: "delete-invalid-id", request: { method: "POST", path: "/api/graph/nodes/x_1/delete", body: {} } },
      { name: "get-map-after", request: { method: "GET", path: "/api/map" } },
    ],
  },
  {
    route: "exercises",
    seed: {},
    dataSeed: {
      "courses/demo/exercises/package.json": "{}",
      "courses/demo/exercises/01-somma/Esercizio.js": "export const somma = (a, b) => 0;\n",
      "courses/demo/exercises/01-somma/Esercizio.test.js": "// test\n",
      // Lowercase first letter: localeCompare puts it before Esercizio.js, byte order would not.
      "courses/demo/exercises/01-somma/app.js": "// app\n",
      "courses/demo/exercises/01-somma/ISTRUZIONI.md": "# Somma\n\nScrivi somma.\n",
      "courses/demo/exercises/01-somma/SUGGERIMENTI.json": JSON.stringify({ hints: ["Usa +", "Ritorna a + b"], final: true, revealed: 1 }),
      "courses/demo/exercises/01-somma/.app-lastrun.json": JSON.stringify({
        ok: true,
        passed: 1,
        failed: 0,
        total: 1,
        failures: [],
        tests: [{ name: "somma", status: "passed" }],
        durationMs: 12,
        at: "2026-01-01T00:00:00.000Z",
      }),
      "courses/demo/exercises/01-somma/sub/a/b/ok.js": "// depth 3\n",
      "courses/demo/exercises/01-somma/sub/a/b/c/no.js": "// too deep\n",
      // In `files` (any extension is listed) but not in the workspace (walk keeps the editor extensions only).
      "courses/demo/exercises/01-somma/note.txt": "x",
      "courses/demo/exercises/02-vuoto/README.md": "vuoto\n",
      "courses/demo/exercises/02-vuoto/SUGGERIMENTI.json": JSON.stringify({ hints: ["a", "b", "c"], revealed: 1.5 }),
      // Falsy: listExercises omits lastRun.
      "courses/demo/exercises/02-vuoto/.app-lastrun.json": "0",
      "courses/demo/exercises/zz-verifica/Esercizio.vue": "<template />\n",
      "courses/demo/exercises/zz-verifica/SUGGERIMENTI.json": JSON.stringify({ hints: ["uno", 2], final: "si", revealed: "x" }),
      // Malformed: listExercises omits lastRun.
      "courses/demo/exercises/zz-verifica/.app-lastrun.json": "{nope",
      "courses/demo/exercises/misc/x.js": "x",
    },
    cases: [
      { name: "list", request: { method: "GET", path: "/api/exercises/demo" } },
      { name: "list-missing", request: { method: "GET", path: "/api/exercises/nessuno" } },
      { name: "list-bad", request: { method: "GET", path: "/api/exercises/Bad" } },
      { name: "detail", request: { method: "GET", path: "/api/exercises/demo/01-somma" } },
      { name: "detail-missing", request: { method: "GET", path: "/api/exercises/demo/09-no" } },
      { name: "detail-not-exercise", request: { method: "GET", path: "/api/exercises/demo/misc" } },
      {
        name: "save",
        request: {
          method: "POST",
          path: "/api/exercises/save",
          body: { course: "demo", name: "01-somma", path: "app.js", content: "// nuovo\n", baseVersion: APP_JS_SHA1 },
        },
        awaitEvent: true,
      },
      {
        name: "save-stale",
        request: {
          method: "POST",
          path: "/api/exercises/save",
          body: { course: "demo", name: "01-somma", path: "app.js", content: "x", baseVersion: "0".repeat(40) },
        },
      },
      {
        name: "save-readonly",
        request: {
          method: "POST",
          path: "/api/exercises/save",
          body: { course: "demo", name: "01-somma", path: "Esercizio.test.js", content: "x" },
        },
      },
      {
        name: "hint",
        request: { method: "POST", path: "/api/exercises/hint", body: { course: "demo", name: "zz-verifica" } },
        awaitEvent: true,
      },
      { name: "detail-after", request: { method: "GET", path: "/api/exercises/demo/01-somma" } },
    ],
  },
  {
    route: "sync",
    // "local" on purpose: the 1 s start pass of startAuto/spawn_auto then does nothing. The script switches to
    // github through POST /api/settings, which emits no courses/graph event, so no auto pass races the runs.
    seed: {
      ".learner/settings.json": { syncProvider: "local" },
      ".learner/secrets.json": {
        v: 1,
        encrypted: false,
        data: JSON.stringify({
          github: { token: "t".repeat(12), login: "tester", repo: "tester/proxi-data", branch: "main" },
        }),
      },
    },
    dataSeed: {
      "curriculum.json": "{\"chapters\":[]}\n",
      "courses/algebra/notes.md": "# algebra\n",
      "courses/algebra/lessons/0001-intro.html": "<p>locale</p>\n",
      "graph/nodes.jsonl": "{\"id\":\"n1\"}\n",
    },
    github: {
      login: "tester",
      files: {
        "README.md": "# proxi-data\n",
        "courses/algebra/lessons/0001-intro.html": "<p>remoto</p>\n",
        "decks/algebra.tsv": "q\ta\n",
      },
    },
    cases: [
      { name: "status-connected", request: { method: "GET", path: "/api/sync" } },
      { name: "provider-github", request: { method: "POST", path: "/api/settings", body: { syncProvider: "github" } } },
      { name: "run-first", request: { method: "POST", path: "/api/sync/run", body: {} }, remote: true },
      { name: "run-again", request: { method: "POST", path: "/api/sync/run", body: {} }, remote: true },
      { name: "disconnect", request: { method: "POST", path: "/api/sync/disconnect", body: { provider: "github" } } },
    ],
  },
  {
    route: "sync-gdrive",
    // "local" on purpose: the 1 s start pass of startAuto then does nothing, and POST /api/settings emits no event.
    seed: {
      ".learner/settings.json": { syncProvider: "local" },
      ".learner/secrets.json": {
        v: 1,
        encrypted: false,
        data: JSON.stringify({
          gdrive: { refreshToken: "rt-1", folderId: "folder-1", email: "tester@example.com" },
        }),
      },
    },
    dataSeed: {
      "curriculum.json": "{\"chapters\":[]}\n",
      "courses/algebra/notes.md": "# algebra\n",
      "courses/algebra/lessons/0001-intro.html": "<p>locale</p>\n",
      "graph/nodes.jsonl": "{\"id\":\"n1\"}\n",
    },
    drive: {
      folderId: "folder-1",
      files: {
        "courses/algebra/lessons/0001-intro.html": "<p>remoto</p>\n",
        "decks/algebra.tsv": "q\ta\n",
      },
    },
    cases: [
      { name: "status-connected", request: { method: "GET", path: "/api/sync" } },
      { name: "provider-gdrive", request: { method: "POST", path: "/api/settings", body: { syncProvider: "gdrive" } } },
      { name: "run-first", request: { method: "POST", path: "/api/sync/run", body: {} }, remote: true },
      { name: "run-again", request: { method: "POST", path: "/api/sync/run", body: {} }, remote: true },
      // No remote log: a late auto pass could add a token request.
      { name: "disconnect", request: { method: "POST", path: "/api/sync/disconnect", body: { provider: "gdrive" } } },
    ],
  },
];

/** Writes `<outDir>/<route>.json` for every scripted route and returns the file paths. Always Node mode. */
export async function captureGolden(outDir: string = GOLDEN_DIR): Promise<string[]> {
  fs.mkdirSync(outDir, { recursive: true });
  const written: string[] = [];
  for (const script of SCRIPTS) {
    const target = path.join(outDir, `${script.route}.json`);
    const file = await captureScript(script, "node");
    fs.writeFileSync(target, JSON.stringify(file, null, 2) + "\n"); // always LF, never os.EOL
    written.push(target);
  }
  return written;
}

/** Runs one script against a fresh temp repo + data folder and returns the golden, un-normalised seeds kept raw. */
export async function captureScript(script: Script, mode: ServerMode = "node"): Promise<GoldenFile> {
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
  for (const [rel, text] of Object.entries(script.dataSeed ?? {})) {
    const file = path.join(data, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  }
  const roots = { repoRoot: rootVariants(repo), dataRoot: rootVariants(data) };
  const github = script.github;
  const drive = script.drive;
  const fake = github ? new FakeHttpServer("github") : undefined;
  const google = drive ? new FakeHttpServer("google") : undefined;
  try {
    if (fake && github) {
      await fake.start();
      installFakeGithub(fake, github);
    }
    if (google && drive) {
      await google.start();
      installFakeDrive(google, drive);
    }
    const fakes = { ...(fake ? { github: fake.url } : {}), ...(google ? { google: google.url } : {}) };
    const server = await startServer({ repoRoot: repo, dataRoot: data, mode, fakes: fake || google ? fakes : undefined });
    const remote: RemoteLog | undefined =
      fake && github ? { server: fake, body: githubBody } : google && drive ? { server: google, body: driveBody } : undefined;
    try {
      const cases: GoldenCase[] = [];
      for (const c of script.cases) cases.push(await captureCase(server.url, c, roots, remote));
      // The seed and the data files are the replay's input, so they stay raw; only the captured output is normalised.
      return {
        route: script.route,
        seed: script.seed,
        ...(script.dataSeed ? { dataSeed: script.dataSeed } : {}),
        ...(github ? { github } : {}),
        ...(drive ? { drive } : {}),
        cases,
      };
    } finally {
      await server.close();
    }
  } finally {
    await fake?.close();
    await google?.close();
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
  }
}

async function captureCase(base: string, c: ScriptedCase, roots: Roots, remote?: RemoteLog): Promise<GoldenCase> {
  const events = c.awaitEvent ? openEvents(base) : undefined;
  const seen = c.remote && remote ? remote.server.requests.length : 0;
  try {
    if (events) await events.ready;
    const res = await send(base, c.request);
    const body = normalise(JSON.parse(await res.text()), roots);
    const out: GoldenCase = { name: c.name, request: c.request, response: { status: res.status, body } };
    // Node stamps the event with the current time: normalise it like any other field (app/server/main.ts:293).
    if (events) out.firstEvent = normalise(await events.first(5000), roots);
    // Read the log only after the response: the run has committed by then.
    if (c.remote && remote) out.remote = normalise(remoteRequests(remote.server.requests.slice(seen), remote.body), roots) as GoldenCase["remote"];
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
