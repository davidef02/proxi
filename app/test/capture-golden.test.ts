// The committed golden fixtures must be reproducible from the Node server and free of timestamps, keys and personal paths.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { GOOGLE_CLIENT_ID } from "../server/sync/gdrive.ts";
import { ANKI_ERROR, ANKI_GOLDEN, captureAnkiGolden } from "./capture-anki-golden.ts";
import { captureGolden, GOLDEN_DIR, ISO_RE, normalise, normaliseSession, rootVariants } from "./capture-golden.ts";
import { captureLessonGolden, LESSON_GOLDEN } from "./capture-lesson-golden.ts";
import { captureMcpGolden, MCP_GOLDEN } from "./capture-mcp-golden.ts";
import { REPO_ROOT } from "./harness.ts";

const ROUTES = [
  "settings",
  "limits",
  "events",
  "courses",
  "lesson",
  "graph",
  "exercises",
  "sync",
  "sync-gdrive",
  "home",
  "sessions",
  "sessions-agy",
  "sessions-opencode",
];
const ALL_GOLDENS = [...ROUTES, LESSON_GOLDEN, ANKI_GOLDEN, MCP_GOLDEN];

const dirs: string[] = [];
function tempDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  dirs.push(dir);
  return dir;
}

after(() => {
  for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
});

function goldenText(route: string): string {
  return fs.readFileSync(path.join(GOLDEN_DIR, `${route}.json`), "utf-8");
}

function walkStrings(value: unknown, visit: (s: string) => void): void {
  if (typeof value === "string") return visit(value);
  if (Array.isArray(value)) {
    for (const v of value) walkStrings(v, visit);
    return;
  }
  if (value && typeof value === "object") for (const v of Object.values(value)) walkStrings(v, visit);
}

describe("normalise", () => {
  it("replaces roots and ISO timestamps at any depth, keeping the rest and the key order", () => {
    const roots = { repoRoot: ["/a/repo"], dataRoot: ["/a/data"] };
    const input = {
      b: "/a/repo",
      a: { nested: ["/a/data", "plain", 7, null], iso: "2026-11-12T10:00:00.000Z" },
      flag: false,
    };
    const out = normalise(input, roots);
    assert.deepEqual(out, {
      b: "<repoRoot>",
      a: { nested: ["<dataRoot>", "plain", 7, null], iso: "<iso>" },
      flag: false,
    });
    assert.deepEqual(Object.keys(out as object), ["b", "a", "flag"]);
    assert.deepEqual(Object.keys((out as { a: object }).a), ["nested", "iso"]);
    assert.equal(input.b, "/a/repo"); // deep copy: the input is untouched
  });

  it("maps every root variant and prefers repoRoot", () => {
    const roots = { repoRoot: ["/a/repo", "/b/repo"], dataRoot: ["/a/data", "/b/repo"] };
    assert.equal(normalise("/b/repo", roots), "<repoRoot>");
    assert.equal(normalise("/a/data", roots), "<dataRoot>");
    assert.equal(normalise("2026-11-12T10:00:00Z", roots), "<iso>");
    assert.equal(normalise("/elsewhere", roots), "/elsewhere");
  });
});

describe("normaliseSession", () => {
  it("replaces the session id first, then other UUIDs and fake engine counters, keeping the rest and the order", () => {
    const roots = { repoRoot: ["/a/repo"], dataRoot: ["/a/data"] };
    const session = "11111111-1111-4111-8111-111111111111";
    const other = "22222222-2222-4222-8222-222222222222";
    const input = {
      path: `/api/sessions/${session}/events`,
      itemId: "t:msg_1234_1:1",
      tool: "toolu_fake_5678_2",
      learner: `l:${other}`,
      plain: "Ciao",
      at: "2026-11-12T10:00:00.000Z",
      root: "/a/repo",
      nested: [session, other],
    };
    const out = normaliseSession(input, roots, session);
    assert.deepEqual(out, {
      path: "/api/sessions/<session>/events",
      itemId: "t:msg_<pid>_1:1",
      tool: "toolu_fake_<pid>_2",
      learner: "l:<uuid>",
      plain: "Ciao",
      at: "<iso>",
      root: "<repoRoot>",
      nested: ["<session>", "<uuid>"],
    });
    assert.deepEqual(Object.keys(out as object), Object.keys(input));
    assert.equal(input.learner, `l:${other}`); // deep copy: the input is untouched
  });

  it("replaces a root only when it is the whole string", () => {
    const roots = { repoRoot: ["/a/repo"], dataRoot: ["/a/data"] };
    assert.equal(normaliseSession("/a/repo/file.json", roots, "x"), "/a/repo/file.json");
    assert.equal(normaliseSession("/a/data", roots, "x"), "<dataRoot>");
  });
});

describe("rootVariants", () => {
  it("lists the resolved and the native realpath once, and only the resolved one when missing", () => {
    const dir = tempDir("golden-roots-");
    const variants = rootVariants(dir);
    assert.equal(variants[0], path.resolve(dir));
    assert.deepEqual([...new Set(variants)], [...new Set([path.resolve(dir), fs.realpathSync.native(dir)])]);
    assert.equal(variants.length, new Set(variants).size);
    assert.deepEqual(rootVariants(path.join(dir, "missing")), [path.resolve(dir, "missing")]);
  });
});

it("committed golden fixtures match a fresh Node capture", async () => {
  const out = tempDir("golden-fresh-");
  await captureGolden(out);
  await captureLessonGolden(out);
  await captureAnkiGolden(out);
  await captureMcpGolden(out);
  for (const route of ALL_GOLDENS) {
    const committed = path.join(GOLDEN_DIR, `${route}.json`);
    assert.ok(fs.existsSync(committed), `missing ${committed}: run npm run golden:capture`);
    // Parsed, not bytes: with core.autocrlf the checkout is CRLF while a fresh capture is LF.
    assert.deepStrictEqual(
      JSON.parse(fs.readFileSync(committed, "utf-8")),
      JSON.parse(fs.readFileSync(path.join(out, `${route}.json`), "utf-8")),
      `run npm run golden:capture (${route})`,
    );
  }
});

it("events golden holds the normalised first event", () => {
  const file = JSON.parse(goldenText("events")) as { cases: Array<{ name: string; firstEvent?: unknown }> };
  assert.equal(file.cases[0]?.name, "limits-off");
  assert.deepStrictEqual(file.cases[0]?.firstEvent, { type: "limits", limits: { at: "<iso>" } });
});

it("courses golden holds the provisioned standalone exclusion and the lesson echo", () => {
  const courses = JSON.parse(goldenText("courses")) as {
    cases: Array<{ name: string; response: { body: Array<{ slug: string }> } }>;
  };
  const list = courses.cases.find((c) => c.name === "courses");
  assert.deepStrictEqual(
    list?.response.body.map((c) => c.slug),
    ["algebra", "vuoto", "sciolto"],
  );

  const lesson = JSON.parse(goldenText("lesson")) as {
    cases: Array<{ name: string; response: { body: Record<string, unknown> } }>;
  };
  const ok = lesson.cases.find((c) => c.name === "lesson-ok");
  assert.ok(ok, "missing lesson-ok case");
  assert.ok("zeta" in ok.response.body, "the lesson echo keeps the unknown island key");
  assert.equal(ok.response.body.weight, 1);
});

it("graph golden holds the graph event and JS-shaped positions", () => {
  interface GraphBody {
    nodes: Array<{ id: string }>;
    positions: Record<string, unknown>;
  }
  const file = JSON.parse(goldenText("graph")) as { cases: Array<{ name: string; firstEvent?: unknown; response: { body: GraphBody } }> };
  const byName = (name: string) => {
    const found = file.cases.find((c) => c.name === name);
    assert.ok(found, `missing graph case ${name}`);
    return found;
  };
  assert.deepStrictEqual(byName("delete-node").firstEvent, { type: "graph", revision: 1 });
  assert.deepStrictEqual(byName("get-graph-after-positions").response.body.positions.n_a, { x: 5, y: 6.5 });
  const body = byName("get-graph").response.body;
  for (const node of body.nodes) assert.ok(node.id in body.positions, `no position for ${node.id}`);
  assert.match(goldenText("graph"), /"dataSeed"/);
});

it("exercises golden holds the workspace order, the stale conflict and the courses event", () => {
  interface FileEntry {
    path: string;
    readOnly: boolean;
  }
  const file = JSON.parse(goldenText("exercises")) as {
    cases: Array<{ name: string; firstEvent?: unknown; response: { status: number; body: unknown } }>;
  };
  const byName = (name: string) => {
    const found = file.cases.find((c) => c.name === name);
    assert.ok(found, `missing exercises case ${name}`);
    return found;
  };
  const detail = byName("detail").response.body as { files: FileEntry[] };
  assert.deepStrictEqual(
    detail.files.map((f) => [f.path, f.readOnly]),
    [
      ["app.js", false],
      ["Esercizio.js", false],
      ["Esercizio.test.js", true],
      ["sub/a/b/ok.js", false],
    ],
  );
  const stale = byName("save-stale").response;
  assert.equal(stale.status, 409);
  assert.deepStrictEqual(stale.body, { error: "Il file è cambiato fuori dall'editor", code: "stale" });
  assert.deepStrictEqual(byName("save").firstEvent, { type: "courses" });
  assert.deepStrictEqual(byName("hint").firstEvent, { type: "courses" });
});

it("sync golden records one commit with the normalised host", () => {
  const file = JSON.parse(goldenText("sync")) as {
    cases: Array<{ name: string; remote?: { method: string; path: string; body: unknown }[] }>;
  };
  const first = file.cases.find((c) => c.name === "run-first");
  const again = file.cases.find((c) => c.name === "run-again");
  assert.ok(first?.remote, "run-first has no remote log");
  const base = "/repos/tester/proxi-data";
  assert.deepEqual(
    first.remote.map((r) => `${r.method} ${r.path}`),
    [
      `POST ${base}/git/blobs`,
      `POST ${base}/git/blobs`,
      `POST ${base}/git/blobs`,
      `POST ${base}/git/trees`,
      `POST ${base}/git/commits`,
      `PATCH ${base}/git/refs/heads/main`,
    ],
  );
  const commit = first.remote.find((r) => r.path === `${base}/git/commits`);
  assert.equal((commit?.body as { message?: string } | undefined)?.message, "sync: 3 sent, 0 deleted (<host>)");
  assert.deepEqual(again?.remote, []);
});

it("sync-gdrive golden uploads the local files and refreshes the token", () => {
  interface Remote {
    method: string;
    path: string;
    body: unknown;
  }
  interface UploadBody {
    meta: { modifiedTime?: string };
    data: string;
  }
  const file = JSON.parse(goldenText("sync-gdrive")) as { cases: Array<{ name: string; remote?: Remote[] }> };
  const first = file.cases.find((c) => c.name === "run-first");
  assert.ok(first?.remote, "run-first has no remote log");
  const token = first.remote.find((r) => r.path === "/token");
  assert.deepEqual(token?.body, { client_id: GOOGLE_CLIENT_ID, refresh_token: "rt-1", grant_type: "refresh_token" });
  const uploads = first.remote.filter((r) => r.path.startsWith("/upload/drive/v3/files"));
  assert.ok(uploads.length > 0, "no upload recorded");
  for (const upload of uploads) {
    const body = upload.body as UploadBody;
    assert.equal(body.meta.modifiedTime, "<iso>");
    assert.equal(typeof body.data, "string"); // the boundary is parsed away, the bytes stay utf8
  }
  // A pass builds a fresh provider, so it refreshes the token even when it writes nothing.
  const again = file.cases.find((c) => c.name === "run-again");
  assert.ok(again?.remote?.some((r) => r.path === "/token"), "run-again has no token refresh");
  assert.equal(again?.remote?.some((r) => r.path.startsWith("/upload/")), false);
  for (const c of file.cases) if (c.remote) assert.doesNotMatch(JSON.stringify(c.remote), /boundary/);
});

it("sessions golden holds the snapshot, the live turn and the state file", () => {
  interface SessionCase {
    name: string;
    request: { path: string };
    response: { body: any };
    events?: any[];
  }
  const file = JSON.parse(goldenText("sessions")) as {
    cases: SessionCase[];
    stateFile: Record<string, unknown>;
  };
  const message = file.cases.find((c) => c.name === "message");
  assert.ok(message);
  assert.ok(message.events, "the message case has no events");
  const [snapshot, ...live] = message.events;
  assert.equal(snapshot.type, "snapshot");
  assert.equal(snapshot.state.id, "<session>");
  assert.ok(snapshot.state.items.some((i: any) => i.kind === "teacher" && i.text === "Ciao, sono il docente."));
  assert.deepEqual(
    live.map((e) => (e.type === "item" ? `item:${e.item.kind}` : e.type)),
    ["item:learner", "status", "item:teacher", "delta", "delta", "delta", "item:teacher", "usage", "status"],
  );
  assert.equal(live[0].item.text, "Ciao");
  assert.equal(live[1].status, "running");
  assert.equal(live[2].item.streaming, true);
  assert.equal(live.slice(3, 6).map((e) => e.text).join(""), "Ciao, sono il docente.");
  assert.equal(live[6].item.streaming, false);
  assert.equal(live[7].usage.turns, 2);
  assert.equal(live[8].status, "idle");

  const create = file.cases.find((c) => c.name === "create");
  assert.deepEqual(create?.response.body, { id: "<session>" });
  const get = file.cases.find((c) => c.name === "get");
  assert.equal(get?.request.path, "/api/sessions/<session>");
  const list = file.cases.find((c) => c.name === "list");
  assert.deepEqual(list?.response.body.map((s: any) => s.id), ["<session>"]);

  // No machine- or run-dependent id survives the capture, in the cases and in the saved state.
  const uuid = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/;
  walkStrings({ cases: file.cases, stateFile: file.stateFile }, (s) => {
    assert.doesNotMatch(s, ISO_RE, s);
    assert.doesNotMatch(s, uuid, s);
  });

  // A successful turn clears pendingStart and lastTurn: only the live fields keep their Node order.
  assert.deepEqual(Object.keys(file.stateFile), ["state", "provider", "resume", "canRetry"]);
});

it("agy and opencode session goldens pin the engine, its token and the turn", () => {
  interface SessionCase {
    name: string;
    events?: any[];
  }
  interface TeacherItem {
    kind: string;
    text: string;
  }
  const tokens = { agy: "conv-<pid>", opencode: "ses-<pid>" } as const;
  const uuid = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/;

  for (const engine of ["agy", "opencode"] as const) {
    const token = tokens[engine];
    const file = JSON.parse(goldenText(`sessions-${engine}`)) as {
      cases: SessionCase[];
      stateFile: Record<string, unknown>;
    };

    assert.equal(file.stateFile.provider, engine);
    assert.equal(file.stateFile.resume, token);
    assert.deepEqual(Object.keys(file.stateFile), ["state", "provider", "resume", "canRetry"]);

    const message = file.cases.find((c) => c.name === "message");
    assert.ok(message?.events, `${engine}: the message case has no events`);
    const [snapshot, ...live] = message.events;
    assert.equal(snapshot.type, "snapshot", engine);
    assert.ok(
      snapshot.state.items.some((i: TeacherItem) => i.kind === "teacher" && i.text === `Ciao, sono il docente (${token}).`),
      `${engine}: the snapshot has no teacher item naming ${token}`,
    );
    const kinds = live.map((e) => (e.type === "item" ? `item:${e.item.kind}` : e.type));
    assert.equal(kinds[0], "item:learner", engine);
    assert.equal(live[live.length - 1].type, "status", engine);
    assert.equal(live[live.length - 1].status, "idle", engine);

    // No machine- or run-dependent id survives the capture, in the cases and in the saved state.
    walkStrings({ cases: file.cases, stateFile: file.stateFile }, (s) => {
      assert.doesNotMatch(s, ISO_RE, s);
      assert.doesNotMatch(s, uuid, s);
      assert.doesNotMatch(s, /(?:conv|ses)-\d+/, s);
    });
  }
});

it("golden fixtures hold no raw timestamps", () => {
  // Only the captured cases: the seed is the replay's input and keeps its fixed literal timestamp.
  for (const route of ROUTES) {
    const file = JSON.parse(goldenText(route)) as { cases: unknown };
    walkStrings(file.cases, (s) => assert.doesNotMatch(s, ISO_RE, `${route}: ${s}`));
  }
  assert.match(goldenText("events"), /"<iso>"/);
});

it("home golden normalises the stamp and covers every AC case", () => {
  const file = JSON.parse(goldenText("home")) as {
    cases: Array<{
      name: string;
      result: { exitCode: number; stdout: string; stderr: string };
      ankiRequests?: unknown[];
      files: Record<string, string>;
    }>;
  };
  const byName = (name: string) => {
    const found = file.cases.find((entry) => entry.name === name);
    assert.ok(found, `missing home case ${name}`);
    return found;
  };

  const anki = byName("generate-anki");
  const ankiHtml = anki.files["index.html"] ?? "";
  assert.match(ankiHtml, /Dati Anki al <it-IT>/);
  assert.match(ankiHtml, /rigenera con <code>npm run home<\/code>/);
  assert.equal(anki.ankiRequests?.length, 2);
  assert.deepEqual(anki.ankiRequests?.[0], { action: "deckNamesAndIds", version: 6 });

  const noAnki = byName("generate-no-anki");
  assert.equal(
    noAnki.result.stdout,
    "<dataRoot>/index.html — 2 chapters, 6 lessons taught, 3 outside the plan, without Anki\n"
  );
  const html = noAnki.files["index.html"] ?? "";
  assert.ok(html.includes('href="courses/docker/lessons/0002-con%20spazio.html"'));
  const fuori = html.indexOf("Fuori dal piano");
  assert.ok(fuori >= 0, "the orphan section is rendered");
  const hrefs = [
    'href="courses/docker/lessons/0002-con%20spazio.html"',
    'href="courses/docker/lessons/0004-image-e-container-bis.html"',
    'href="courses/probabilita/lessons/0002-dadi.html"',
  ].map((href) => html.indexOf(href, fuori));
  assert.ok(hrefs[0] >= 0, "first orphan");
  assert.ok(hrefs[1] > hrefs[0], "docker 0004 after docker 0002");
  assert.ok(hrefs[2] > hrefs[1], "probabilita 0002 after docker 0004");

  const options = byName("generate-options");
  assert.equal(
    options.result.stdout,
    "<dataRoot>/board.html — 3 chapters, 2 lessons taught, 1 outside the plan, without Anki\n"
  );
  assert.deepEqual(Object.keys(options.files), ["board.html"]);
  assert.ok(options.files["board.html"]?.includes('href="courses/rust/"'));
  assert.ok(options.files["board.html"]?.includes("0001-ownership.html"));

  assert.deepEqual(Object.keys(byName("plan-curriculum").files), ["plan.json"]);

  const optionsListing = byName("chapters-options").result.stdout;
  assert.match(optionsListing, /Ownership/);
  assert.match(optionsListing, /Borrow/);
  assert.notEqual(optionsListing, byName("chapters").result.stdout);

  const unreachable = byName("generate-anki-unreachable");
  assert.equal(unreachable.result.exitCode, 1);
  assert.deepEqual(unreachable.files, {});
  assert.match(unreachable.result.stderr, /AnkiConnect non raggiungibile \(fetch failed\)/);

  assert.deepEqual(byName("chapters").files, {});
});

it("golden fixtures hold no key-like or personal data", () => {
  const keyLike = new RegExp("(?<![A-Za-z0-9_-])" + "sk" + "-");
  const personal = [os.tmpdir(), fs.realpathSync.native(os.tmpdir()), REPO_ROOT];
  const host = `(${os.hostname()})`;
  for (const route of ALL_GOLDENS) {
    const text = goldenText(route);
    assert.doesNotMatch(text, keyLike, route);
    // Walk the parsed values: JSON's escaping of Windows backslashes would hide a path in the raw text.
    walkStrings(JSON.parse(text), (s) => {
      for (const needle of personal) assert.equal(s.includes(needle), false, `${route}: ${s} contains ${needle}`);
      assert.equal(s.includes(host), false, `${route}: ${s} contains the hostname`);
    });
  }
  assert.match(goldenText("settings"), /"<repoRoot>"/);
});

it("lesson golden holds no raw timestamps or dates", () => {
  // Only the captured cases: the seed is the replay's input and holds no timestamps.
  const file = JSON.parse(goldenText(LESSON_GOLDEN)) as { cases: unknown };
  const text = JSON.stringify(file.cases);
  assert.doesNotMatch(text, /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/);
  assert.doesNotMatch(text, /Generated: \d{4}/);
  assert.match(text, /<iso>/);
  assert.match(text, /Generated: <date>/);
});

it("lesson golden pins the slug and the D11 stderr", () => {
  const file = JSON.parse(goldenText(LESSON_GOLDEN)) as {
    cases: Array<{ name: string; stdout: string; stderr: string; files: Record<string, string | null> }>;
  };
  const init = file.cases.find((c) => c.name === "init-default");
  assert.ok(init, "missing init-default");
  assert.match(init.stdout, /"slug": "probabilita-e-davvero-50-ca-va"/);
  for (const c of file.cases) {
    assert.doesNotMatch(c.stderr, /\[home\]/, c.name);
    assert.equal(Object.keys(c.files).includes("index.html"), false, c.name);
  }
});

it("anki golden pins ping, LaTeX and the data/stdin parity", () => {
  const file = JSON.parse(goldenText(ANKI_GOLDEN)) as {
    cases: Array<{ name: string; stdout: string; exitCode: number; files: Record<string, string | null> }>;
  };
  const byName = (name: string) => {
    const found = file.cases.find((c) => c.name === name);
    assert.ok(found, `missing anki case ${name}`);
    return found;
  };
  assert.equal(byName("ping-online").stdout, "✅ AnkiConnect is ONLINE (<ankiUrl>) — Version 6 (<ms>ms)\n");
  // The error after the prefix is platform-dependent: `fetch failed` on a fast refusal, a timeout on a slow one.
  const offline = byName("ping-offline");
  assert.ok(offline.stdout.startsWith("⚠️ AnkiConnect is OFFLINE (<deadUrl>): "));
  assert.equal(offline.exitCode, 0);
  assert.match(byName("ping-timeout").stdout, /Connection timed out/);
  assert.ok(byName("ping-error").stdout.includes(ANKI_ERROR));
  assert.equal(
    byName("export-data-nodes").files["decks/from-data.tsv"],
    byName("export-stdin-nodes").files["decks/from-stdin.tsv"],
  );
  // The JSON-escaped `\(\frac`: the LaTeX pass ran, and the extract dump keeps V8's number printing.
  // normaliseStream turns every backslash into `/`, so the pinned escape reads `/(/frac` in the golden.
  const latex = JSON.stringify("\\(\\frac").slice(1, -1).split("\\\\").join("/").split("\\").join("/");
  const extract = byName("extract-course").stdout;
  assert.ok(extract.includes('"vue-js-vetrine"'));
  assert.ok(extract.includes('"tier::undefined"'));
  assert.ok(extract.includes(latex));
  assert.ok(extract.includes('"lessonNumber": 1,'));
});

it("anki golden pins sync", () => {
  interface SyncNote {
    deckName: string;
    modelName: string;
    fields: Record<string, string | null>;
    options: { allowDuplicate: boolean; duplicateScope: string };
  }
  interface SyncRequest {
    action: string;
    params?: { deck?: string; notes?: SyncNote[]; modelName?: string };
  }
  interface SyncCase {
    name: string;
    stdout: string;
    exitCode: number;
    requests: SyncRequest[];
    files: Record<string, string | null>;
  }
  const file = JSON.parse(goldenText(ANKI_GOLDEN)) as { cases: SyncCase[] };
  const byName = (name: string) => {
    const found = file.cases.find((c) => c.name === name);
    assert.ok(found, `missing anki case ${name}`);
    return found;
  };
  const actions = (c: SyncCase) => c.requests.map((r) => r.action);
  const addedCount = (c: SyncCase) => Number(/^✅ Synced (\d+) cards/.exec(c.stdout)?.[1] ?? NaN);
  const duplicateCount = (c: SyncCase) => Number(/\((\d+) duplicates skipped\)/.exec(c.stdout)?.[1] ?? NaN);
  const addNotes = (c: SyncCase) => {
    const request = c.requests.find((r) => r.action === "addNotes");
    assert.ok(request, `${c.name} has no addNotes request`);
    return request.params?.notes ?? [];
  };

  for (const c of file.cases) assert.ok(Array.isArray(c.requests), `${c.name} has no requests array`);

  const lesson = byName("sync-lesson");
  assert.deepEqual(actions(lesson), ["version", "createDeck", "modelNames", "addNotes"]);
  assert.match(lesson.stdout, /^✅ Synced ([1-9]\d*) cards to deck 'Proxi::Vue\.js & Vetrine' \(0 duplicates skipped\)\n📁 Backup TSV saved to /);
  assert.deepEqual(lesson.requests.find((r) => r.action === "createDeck")?.params, { deck: "Proxi::Vue.js & Vetrine" });
  for (const note of addNotes(lesson)) {
    assert.equal(note.deckName, "Proxi::Vue.js & Vetrine");
    assert.deepEqual(note.options, { allowDuplicate: false, duplicateScope: "deck" });
  }
  const firstAdded = addedCount(lesson);

  const again = byName("sync-lesson-again");
  assert.ok(again.stdout.startsWith("✅ Synced 0 cards"), again.stdout);
  assert.equal(duplicateCount(again), firstAdded);

  const duplicates = byName("sync-allow-duplicates");
  assert.equal(addedCount(duplicates), firstAdded);
  for (const note of addNotes(duplicates)) assert.equal(note.options.allowDuplicate, true);

  const deck = byName("sync-deck");
  assert.deepEqual(deck.requests.find((r) => r.action === "createDeck")?.params, { deck: "Proxi::Personale" });
  assert.ok(deck.files["decks/entropia.tsv"]?.includes("#deck:Proxi::Personale"), "sync-deck TSV header");

  const italian = byName("sync-italian");
  assert.deepEqual(actions(italian), ["version", "createDeck", "modelNames", "modelFieldNames", "modelFieldNames", "addNotes"]);
  const italianNotes = addNotes(italian);
  assert.equal(italianNotes.length, 1);
  assert.equal(italianNotes[0]?.modelName, "Basilare");
  assert.deepEqual(Object.keys(italianNotes[0]?.fields ?? {}), ["Fronte", "Retro"]);

  const error = byName("sync-anki-error");
  assert.deepEqual(actions(error), ["version"]);
  assert.equal(error.exitCode, 0);
  assert.ok(error.stdout.includes(`AnkiConnect offline (<errorUrl> - ${ANKI_ERROR})`), error.stdout);

  const offline = byName("sync-offline");
  assert.deepEqual(offline.requests, []);
  assert.ok(offline.stdout.includes("Connection timed out"), offline.stdout);
  assert.ok(offline.stdout.includes("📁 Deck TSV exported to "), offline.stdout);

  const nodes = byName("sync-nodes-only");
  assert.deepEqual(nodes.requests.find((r) => r.action === "addNotes")?.params, { notes: [] });
});

it("lesson golden covers rebuild and compile-ref", () => {
  const file = JSON.parse(goldenText(LESSON_GOLDEN)) as {
    cases: Array<{ name: string; stderr: string; files: Record<string, string | null> }>;
  };
  const byName = (name: string) => {
    const found = file.cases.find((c) => c.name === name);
    assert.ok(found, `missing ${name}`);
    return found;
  };

  const reference = byName("compile-ref-default").files["reference/il-modello-osi.html"];
  assert.ok(reference, "compile-ref-default wrote no reference/il-modello-osi.html");
  assert.match(reference, /id="reference-data"/);
  assert.match(reference, /confonde rete e trasporto/); // the sibling learning-record scan, not the board

  const alias = Object.values(byName("reference-alias-record").files);
  assert.equal(alias.length, 1);
  assert.match(alias[0] as string, /confonde la legge con il caso/); // --record wins over the scan

  assert.equal(byName("rebuild-slug-miss").stderr, "Error: Lesson not found: nope\n");
  assert.equal(byName("compile-ref-slug-miss").stderr, "Error: Cannot compile reference: lesson file not found (undefined)\n");

  for (const c of file.cases) {
    for (const name of Object.keys(c.files)) assert.doesNotMatch(name, /learning-records\//, `${c.name} lists a learning record`);
  }
});
