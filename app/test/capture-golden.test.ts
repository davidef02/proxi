// The committed golden fixtures must be reproducible from the Node server and free of timestamps, keys and personal paths.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { captureGolden, GOLDEN_DIR, ISO_RE, normalise, rootVariants } from "./capture-golden.ts";
import { REPO_ROOT } from "./harness.ts";

const ROUTES = ["settings", "limits", "events", "home"];

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
  for (const route of ROUTES) {
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
  for (const route of ROUTES) {
    const text = goldenText(route);
    assert.doesNotMatch(text, keyLike, route);
    // Walk the parsed values: JSON's escaping of Windows backslashes would hide a path in the raw text.
    walkStrings(JSON.parse(text), (s) => {
      for (const needle of personal) assert.equal(s.includes(needle), false, `${route}: ${s} contains ${needle}`);
    });
  }
  assert.match(goldenText("settings"), /"<repoRoot>"/);
});
