// The committed golden fixtures must be reproducible from the Node server and free of timestamps, keys and personal paths.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { captureGolden, GOLDEN_DIR, ISO_RE, normalise, rootVariants } from "./capture-golden.ts";
import { REPO_ROOT } from "./harness.ts";

const ROUTES = ["settings", "limits", "events", "graph"];

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

it("golden fixtures hold no raw timestamps", () => {
  // Only the captured cases: the seed is the replay's input and keeps its fixed literal timestamp.
  for (const route of ROUTES) {
    const file = JSON.parse(goldenText(route)) as { cases: unknown };
    walkStrings(file.cases, (s) => assert.doesNotMatch(s, ISO_RE, `${route}: ${s}`));
  }
  assert.match(goldenText("events"), /"<iso>"/);
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
