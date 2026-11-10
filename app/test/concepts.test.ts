import { after, describe, test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  BROWSE_LIMIT,
  SEARCH_LIMIT,
  SEMANTIC_UNAVAILABLE,
  browseConcepts,
  loadCatalog,
  openConcept,
  searchConcepts,
} from "../server/graph/concepts.ts";
import { EMBEDDING, modelPresent, rankSemantic } from "../server/graph/embeddings.ts";
import { CODE_ROOT } from "../../extensions/data-root.ts";
import { deleteNode } from "../server/graph/store.ts";

interface Prop {
  id: string;
  name: string;
  statement: string;
  aliases: string[];
  createdAt: string;
  provenance: { chatId: string; messageId: string }[];
}

interface Chain {
  id: string;
  premises: string[];
  conclusion: string;
  createdAt: string;
  provenance: { chatId: string; messageId: string }[];
}

const prop = (
  id: string,
  name: string,
  provenance: Prop["provenance"] = [{ chatId: "demo/0001-demo", messageId: "" }],
): Prop => ({
  id,
  name,
  statement: `${name} vale.`,
  aliases: [],
  createdAt: "2026-01-01T00:00:00.000Z",
  provenance,
});

const chain = (id: string, premises: string[], conclusion: string): Chain => ({
  id,
  premises,
  conclusion,
  createdAt: "2026-01-01T00:00:00.000Z",
  provenance: [],
});

const roots: string[] = [];

function makeData(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "concepts-"));
  roots.push(root);
  fs.writeFileSync(
    path.join(root, "curriculum.json"),
    JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [] }] }),
  );
  return root;
}

function writeGraph(root: string, nodes: Prop[], chains: Chain[] = []): void {
  const dir = path.join(root, "graph");
  fs.mkdirSync(dir, { recursive: true });
  const jsonl = (rows: object[]) => rows.map((r) => JSON.stringify(r)).join("\n") + (rows.length ? "\n" : "");
  fs.writeFileSync(path.join(dir, "nodes.jsonl"), jsonl(nodes));
  fs.writeFileSync(path.join(dir, "chains.jsonl"), jsonl(chains));
}

function writeLesson(
  root: string,
  course: string,
  filename: string,
  slug: string,
  title: string,
  nodes: { nodeId: string; title: string }[],
  lessonNumber = 1,
): void {
  const dir = path.join(root, "courses", course, "lessons");
  fs.mkdirSync(dir, { recursive: true });
  const doc = {
    metadata: {
      lessonNumber,
      topic: title,
      title,
      slug,
      dateCreated: "2026-01-01T00:00:00.000Z",
      dateModified: "2026-01-02T00:00:00.000Z",
      version: "1",
    },
    nodes: nodes.map((n) => ({ nodeId: n.nodeId, title: n.title, tier: "axiom", content: "x" })),
  };
  fs.writeFileSync(path.join(dir, filename), `<script type="application/json" id="lesson-data">${JSON.stringify(doc)}</script>`);
}

after(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

describe("searchConcepts", () => {
  test("caps at 10, one 5-field line each, status insegnato", async () => {
    const root = makeData();
    writeGraph(
      root,
      Array.from({ length: 15 }, (_, i) => prop(`n_${i + 1}`, `Passo ${i + 1}`)),
    );
    const cat = await loadCatalog(root);

    const out = await searchConcepts(cat, "passo");
    assert.equal(out.isError, undefined);
    const lines = out.text.split("\n");
    assert.equal(lines.length, SEARCH_LIMIT + 1);
    assert.match(lines[0], /^10 concept\(s\), best first \(id \| name \| status \| course \| lesson\):$/);
    for (const line of lines.slice(1)) {
      const fields = line.split(" | ");
      assert.equal(fields.length, 5, line);
      assert.equal(fields[2], "insegnato", line);
      assert.equal(fields[3], "Demo", line);
    }

    assert.equal((await searchConcepts(cat, "passo", 3)).text.split("\n").length, 4);
    // the cap never rises above SEARCH_LIMIT
    assert.equal((await searchConcepts(cat, "passo", 50)).text.split("\n").length, SEARCH_LIMIT + 1);

    const empty = await searchConcepts(cat, "   ");
    assert.equal(empty.isError, true);
    assert.match(empty.text, /^Empty query/);

    const none = await searchConcepts(cat, "zzz");
    assert.equal(none.isError, undefined);
    assert.equal(none.text, 'No concept matches "zzz".');
  });

  test("a proposition with no provenance is searchable and names no lesson", async () => {
    const root = makeData();
    writeGraph(root, [prop("n_x", "Senza lezione", [])]);
    const cat = await loadCatalog(root);

    const out = await searchConcepts(cat, "senza");
    assert.equal(out.isError, undefined);
    const line = out.text.split("\n")[1];
    assert.ok(line.endsWith(" | (no lesson) | -"), line);
  });

  test("matches the statement and the aliases, not only the name", async () => {
    const root = makeData();
    writeGraph(root, [
      {
        ...prop("n_ohm", "Legge fondamentale"),
        statement: "La corrente segue la legge di Ohm.",
        aliases: ["resistenza elettrica"],
      },
    ]);
    const cat = await loadCatalog(root);

    for (const query of ["ohm", "resistenza"]) {
      const out = await searchConcepts(cat, query);
      assert.equal(out.isError, undefined);
      assert.match(out.text, /n_ohm \| Legge fondamentale/, `"${query}" did not find the concept`);
    }
  });

  test("fallback note on hits and no-hit when the ranker is unavailable", async () => {
    const root = makeData();
    writeGraph(root, [prop("n_1", "Primo passo")]);
    const cat = await loadCatalog(root);
    const ranker = async () => ({ ok: false as const, reason: "model missing" });

    const hit = await searchConcepts(cat, "passo", undefined, ranker);
    assert.equal(hit.isError, undefined);
    assert.match(hit.text, /n_1 \| Primo passo/);
    assert.ok(hit.text.endsWith(SEMANTIC_UNAVAILABLE), hit.text);

    const none = await searchConcepts(cat, "zzz", undefined, ranker);
    assert.equal(none.isError, undefined);
    assert.ok(none.text.endsWith(SEMANTIC_UNAVAILABLE), none.text);
    assert.match(none.text, /^No concept matches "zzz"/);
  });

  test("semantic ranking orders by cosine and adds the lexical bonus", async () => {
    const root = makeData();
    writeGraph(root, [
      prop("n_a", "Alfa"),
      prop("n_b", "Beta"),
      prop("n_c", "Passo veloce"),
      prop("n_d", "Zeta"),
      prop("n_e", "Passo lento"),
    ]);
    const cat = await loadCatalog(root);
    const cosine: Record<string, number> = { n_a: 0.8, n_b: 0.5, n_c: 0.4, n_d: 0.1, n_e: 0.1 };
    const ranker = async (_q: string, nodes: { id: string }[]) => ({
      ok: true as const,
      hits: nodes.map((n) => ({ id: n.id, cosine: cosine[n.id] ?? 0 })),
    });

    const out = await searchConcepts(cat, "passo", undefined, ranker);
    const ids = out.text
      .split("\n")
      .slice(1)
      .map((l) => l.replace(/^- /, "").split(" | ")[0]);
    // n_c (0.4 + bonus) beats n_b (0.5); n_d is under minScore and not lexical, so it is dropped;
    // n_e is under minScore too but matches lexically, so it is kept
    assert.deepEqual(ids, ["n_a", "n_c", "n_b", "n_e"]);
  });

  test("AC7: added concept found, deleted concept gone without restart", async () => {
    const root = makeData();
    writeGraph(root, [prop("n_base", "Concetto base")]);
    const ranker = async (_q: string, nodes: { id: string; name: string }[]) => ({
      ok: true as const,
      hits: nodes.map((n) => ({ id: n.id, cosine: n.name.includes("nuovo") ? 1 : 0 })),
    });

    let cat = await loadCatalog(root);
    assert.doesNotMatch((await searchConcepts(cat, "nuovo", undefined, ranker)).text, /n_nuovo/);

    writeGraph(root, [prop("n_base", "Concetto base"), prop("n_nuovo", "Concetto nuovo")]);
    cat = await loadCatalog(root);
    assert.match((await searchConcepts(cat, "nuovo", undefined, ranker)).text, /n_nuovo \| Concetto nuovo/);

    await deleteNode(root, "n_nuovo");
    cat = await loadCatalog(root);
    assert.doesNotMatch((await searchConcepts(cat, "nuovo", undefined, ranker)).text, /n_nuovo/);
  });

  test(
    'semantic: "computed" finds "Proprietà calcolate"',
    { skip: !modelPresent(CODE_ROOT) && "embedding model not downloaded" },
    async () => {
      const root = makeData();
      writeGraph(root, [
        {
          ...prop("n_calc", "Proprietà calcolate"),
          // no "computed" anywhere: the hit must be semantic, not lexical
          statement:
            "Proprietà calcolata in Vue: un valore calcolato dalle dipendenze reattive e messo in cache, ricalcolato solo al loro cambiare.",
        },
        prop("n_for", "Ciclo for"),
        prop("n_promessa", "Promessa"),
        prop("n_template", "Template Vue"),
        prop("n_props", "Props"),
        prop("n_click", "Evento click"),
        prop("n_array", "Array"),
      ]);
      const cat = await loadCatalog(root);
      const cache = path.join(root, "embeddings-cache.json");

      const out = await searchConcepts(cat, "computed", undefined, (q, nodes) =>
        rankSemantic(CODE_ROOT, q, nodes, { cacheFile: cache }),
      );
      assert.ok(!out.text.includes(SEMANTIC_UNAVAILABLE), out.text);
      const top = out.text.split("\n").slice(1, 4);
      assert.ok(
        top.some((r) => r.includes("n_calc")),
        out.text,
      );
    },
  );

  test(
    "AC7 with the real model: added concept found, deleted concept gone",
    { skip: !modelPresent(CODE_ROOT) && "embedding model not downloaded" },
    async () => {
      const root = makeData();
      writeGraph(root, [prop("n_base", "Concetto base"), prop("n_altro", "Altro argomento")]);
      const cache = path.join(root, "embeddings-cache.json");
      const ranker = (q: string, nodes: { id: string; name: string; statement: string; aliases: string[] }[]) =>
        rankSemantic(CODE_ROOT, q, nodes, { cacheFile: cache });

      let cat = await loadCatalog(root);
      assert.doesNotMatch(
        (await searchConcepts(cat, "concetto aggiunto dopo", undefined, ranker)).text,
        /Concetto aggiunto dopo/,
      );

      writeGraph(root, [
        prop("n_base", "Concetto base"),
        prop("n_altro", "Altro argomento"),
        prop("n_nuovo", "Concetto aggiunto dopo"),
      ]);
      cat = await loadCatalog(root);
      const added = await searchConcepts(cat, "concetto aggiunto dopo", undefined, ranker);
      assert.ok(
        added.text
          .split("\n")
          .slice(1, 4)
          .some((r) => r.includes("n_nuovo")),
        added.text,
      );

      await deleteNode(root, "n_nuovo");
      cat = await loadCatalog(root);
      assert.doesNotMatch(
        (await searchConcepts(cat, "concetto aggiunto dopo", undefined, ranker)).text,
        /Concetto aggiunto dopo/,
      );
    },
  );
});

describe("openConcept", () => {
  test("shows the concept, its provenance, MapNode keys and chain neighbours", async () => {
    const root = makeData();
    writeLesson(root, "demo", "0001-demo.html", "demo", "Demo", [
      { nodeId: "alpha", title: "Alfa" },
      { nodeId: "beta", title: "Beta" },
      { nodeId: "gamma", title: "Gamma" },
    ]);
    const chatId = "demo/0001-demo";
    writeGraph(
      root,
      [
        prop("n_a", "Alfa", [{ chatId, messageId: "alpha" }]),
        prop("n_b", "Beta", [{ chatId, messageId: "beta" }]),
        prop("n_c", "Gamma", [{ chatId, messageId: "gamma" }]),
      ],
      [chain("arg_1", ["n_a", "n_b"], "n_c")],
    );
    const cat = await loadCatalog(root);

    const c = openConcept(cat, "n_c");
    assert.equal(c.isError, undefined);
    assert.ok(c.text.startsWith("# Gamma (n_c)"), c.text);
    assert.ok(c.text.includes("Statement: Gamma vale."));
    assert.ok(c.text.includes("Status: insegnato"));
    assert.ok(c.text.includes("Courses: Demo (demo)"));
    assert.ok(c.text.includes("Lessons: Lezione 1 · Demo (demo/0001-demo)"));
    assert.ok(c.text.includes("MapNode keys (for problem.nodeKeys): demo/demo#gamma"));
    assert.ok(c.text.includes("Premises:\n- n_a | Alfa\n- n_b | Beta"));
    assert.ok(c.text.includes("Consequences: (none)"));

    const a = openConcept(cat, "n_a");
    assert.ok(a.text.includes("Premises: (none)"));
    assert.ok(a.text.includes("Consequences:\n- n_c | Gamma"));

    const bad = openConcept(cat, "nope");
    assert.equal(bad.isError, true);
    assert.equal(bad.text, 'Unknown concept id "nope". Use search_concepts or browse_concepts to find ids.');
  });

  test("lists aliases, every distinct course and lesson, and says when no key exists", async () => {
    const root = makeData();
    writeGraph(root, [
      {
        ...prop("n_multi", "Multi", [
          { chatId: "demo/0001-demo", messageId: "missing-node" },
          { chatId: "altra/0002-due", messageId: "" },
          { chatId: "demo/0001-demo", messageId: "missing-node" },
        ]),
        aliases: ["sinonimo uno", "sinonimo due"],
      },
    ]);
    const cat = await loadCatalog(root);

    const out = openConcept(cat, "n_multi");
    assert.equal(out.isError, undefined);
    assert.ok(out.text.includes("Aliases: sinonimo uno, sinonimo due"), out.text);
    // birth point first, duplicates collapsed, unknown slugs keep their slug
    assert.ok(out.text.includes("Courses: Demo (demo); altra (altra)"), out.text);
    assert.ok(out.text.includes("Lessons: demo/0001-demo (demo/0001-demo); altra/0002-due (altra/0002-due)"), out.text);
    assert.ok(out.text.includes("MapNode keys: none"), out.text);
  });
});

describe("browseConcepts", () => {
  async function demoCatalog() {
    const root = makeData();
    writeGraph(root, [
      prop("n1", "Uno"),
      prop("n2", "Due"),
      prop("n3", "Tre", [{ chatId: "demo/0002-secondo", messageId: "" }]),
      prop("n4", "Quattro", [{ chatId: "altra/0001-terza", messageId: "" }]),
      prop("n5", "Cinque", [{ chatId: "lezioni-singole/0001-sola", messageId: "" }]),
    ]);
    return loadCatalog(root);
  }

  test("no input lists courses with counts, standalone last", async () => {
    const cat = await demoCatalog();
    const out = browseConcepts(cat);
    assert.equal(out.isError, undefined);
    const lines = out.text.split("\n");
    assert.equal(lines[0], "Courses (slug | title | concepts):");
    assert.deepEqual(lines.slice(1), [
      "- demo | Demo | 3 concepts",
      "- altra | altra | 1 concepts",
      "- lezioni-singole | Lezioni singole | 1 concepts",
    ]);
  });

  test("course lists its birth lessons with counts", async () => {
    const cat = await demoCatalog();
    const out = browseConcepts(cat, "demo");
    assert.equal(out.isError, undefined);
    const lines = out.text.split("\n");
    assert.equal(lines[0], "Lessons of Demo (lesson | label | concepts):");
    assert.deepEqual(lines.slice(1), [
      "- demo/0001-demo | demo/0001-demo | 2 concepts",
      "- demo/0002-secondo | demo/0002-secondo | 1 concepts",
    ]);
  });

  test("course + lesson lists concepts with status, stem or full id", async () => {
    const cat = await demoCatalog();
    const stem = browseConcepts(cat, "demo", "0001-demo");
    const full = browseConcepts(cat, "demo", "demo/0001-demo");
    assert.equal(stem.isError, undefined);
    assert.equal(stem.text, full.text);
    const lines = stem.text.split("\n");
    assert.equal(lines[0], "Concepts of demo/0001-demo (id | name | status | course | lesson):");
    assert.deepEqual(lines.slice(1), [
      "- n1 | Uno | insegnato | Demo | demo/0001-demo",
      "- n2 | Due | insegnato | Demo | demo/0001-demo",
    ]);
  });

  test("unknown course, unknown lesson and lesson without course are readable errors", async () => {
    const cat = await demoCatalog();
    const noCourse = browseConcepts(cat, "nope");
    assert.equal(noCourse.isError, true);
    assert.equal(noCourse.text, 'No concepts in course "nope". Courses with concepts: demo, altra, lezioni-singole.');

    const noLesson = browseConcepts(cat, "demo", "9999-nope");
    assert.equal(noLesson.isError, true);
    assert.equal(
      noLesson.text,
      'No concepts born in lesson "demo/9999-nope". Call browse_concepts with course "demo" to list its lessons.',
    );

    const danglingLesson = browseConcepts(cat, undefined, "0001-demo");
    assert.equal(danglingLesson.isError, true);
    assert.equal(danglingLesson.text, "Pass course with lesson.");
  });

  test("an empty graph says so", async () => {
    const cat = await loadCatalog(makeData());
    const out = browseConcepts(cat);
    assert.equal(out.isError, undefined);
    assert.equal(out.text, "The knowledge graph is empty: no concept yet.");
  });

  test("a lesson longer than the browse limit is truncated with a pointer to search", async () => {
    const root = makeData();
    writeGraph(
      root,
      Array.from({ length: BROWSE_LIMIT + 2 }, (_, i) => prop(`n_b${i}`, `Concetto ${i}`)),
    );
    const cat = await loadCatalog(root);

    const out = browseConcepts(cat, "demo", "0001-demo");
    assert.equal(out.isError, undefined);
    const lines = out.text.split("\n");
    assert.equal(lines.length, 1 + BROWSE_LIMIT + 1);
    assert.equal(lines.at(-1), "(+2 more: use search_concepts)");
  });
});
