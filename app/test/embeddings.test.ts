import { after, describe, test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { CODE_ROOT } from "../../extensions/data-root.ts";
import {
  EMBEDDING,
  cacheFile,
  conceptText,
  modelPresent,
  modelsDir,
  rankSemantic,
  readCache,
  withLoadLock,
} from "../server/graph/embeddings.ts";

const tmpDirs: string[] = [];

function tmpDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "embeddings-"));
  tmpDirs.push(dir);
  return dir;
}

after(() => {
  for (const dir of tmpDirs) fs.rmSync(dir, { recursive: true, force: true });
});

const MODELLED = { skip: !modelPresent(CODE_ROOT) && "embedding model not downloaded" };

const nodes = [
  {
    id: "n_computed",
    name: "Proprietà calcolate",
    statement: "In Vue una proprietà calcolata ricalcola il valore solo quando cambiano le sue dipendenze reattive.",
    aliases: [],
  },
  { id: "n_props", name: "Props", statement: "Dati passati da un componente genitore a un componente figlio.", aliases: [] },
  { id: "n_for", name: "Ciclo for", statement: "Ripete un blocco di codice un numero fissato di volte.", aliases: [] },
];

describe("embeddings", () => {
  test("conceptText joins the fields and skips empty aliases", () => {
    assert.equal(
      conceptText({ name: "Proprietà calcolate", statement: "Valore derivato e in cache", aliases: ["computed", "valore derivato"] }),
      "Proprietà calcolate. Valore derivato e in cache. computed, valore derivato",
    );
    assert.equal(conceptText({ name: "Props", statement: "Dati passati dal genitore", aliases: [] }), "Props. Dati passati dal genitore");
  });

  test("modelsDir is <repoRoot>/.learner/models", () => {
    assert.ok(modelsDir(path.join("finto", "repo")).endsWith(path.join(".learner", "models")));
  });

  test("rankSemantic without the model is unavailable and makes no request", async () => {
    const repo = tmpDir(); // no .learner/models
    const urls: string[] = [];
    const blocked = (input: string | URL): Promise<Response> => {
      urls.push(String(input));
      return Promise.reject(new Error("network blocked"));
    };
    const prevFetch = globalThis.fetch;
    globalThis.fetch = blocked as unknown as typeof fetch;
    try {
      const out = await rankSemantic(repo, "computed", nodes, { cacheFile: path.join(repo, "vectors.json") });
      assert.deepEqual(out, { ok: false, reason: "model missing" });
      assert.deepEqual(urls, []);
    } finally {
      globalThis.fetch = prevFetch;
    }
  });

  // first model-backed test in the file on purpose: the load must happen here, so that the
  // allowRemoteModels assertion below sees what getExtractor set
  test("semantic: works offline from the cache", MODELLED, async () => {
    const tf = await import("@huggingface/transformers");
    const env = tf.env;
    const prev = { fetch: globalThis.fetch, envFetch: env.fetch, remoteHost: env.remoteHost };
    const urls: string[] = [];
    const blocked = (input: string | URL): Promise<Response> => {
      urls.push(String(input));
      return Promise.reject(new Error("network blocked"));
    };
    globalThis.fetch = blocked as unknown as typeof fetch;
    env.fetch = blocked;
    env.remoteHost = "http://127.0.0.1:9/";
    try {
      const out = await rankSemantic(CODE_ROOT, "computed", nodes, {
        cacheFile: path.join(tmpDir(), "offline.json"),
      });
      assert.ok(out.ok, out.ok ? "" : out.reason);
      assert.deepEqual(
        urls.filter((u) => /^https?:/.test(u)),
        [],
      );
      assert.equal(env.allowRemoteModels, false);
    } finally {
      globalThis.fetch = prev.fetch;
      env.fetch = prev.envFetch;
      env.remoteHost = prev.remoteHost;
    }
  });

  test("rankSemantic returns one cosine per node", MODELLED, async () => {
    const out = await rankSemantic(CODE_ROOT, "computed", nodes, { cacheFile: path.join(tmpDir(), "cosines.json") });
    assert.ok(out.ok, out.ok ? "" : out.reason);
    assert.deepEqual(
      out.hits.map((h) => h.id),
      nodes.map((n) => n.id),
    );
    for (const hit of out.hits) assert.ok(Number.isFinite(hit.cosine) && Math.abs(hit.cosine) <= 1.001, `${hit.id}: ${hit.cosine}`);
  });

  test("withLoadLock serializes loads, and a rejection does not block the next one", async () => {
    const order: string[] = [];
    const first = withLoadLock(async () => {
      order.push("s1");
      await new Promise((r) => setTimeout(r, 20));
      order.push("e1");
    });
    const second = withLoadLock(async () => {
      order.push("s2");
      await new Promise((r) => setTimeout(r, 1));
      order.push("e2");
    });
    await Promise.all([first, second]);
    assert.deepEqual(order, ["s1", "e1", "s2", "e2"]);

    await assert.rejects(withLoadLock(async () => {
      throw new Error("boom");
    }));
    assert.equal(
      await withLoadLock(async () => "ok"),
      "ok",
    );
  });

  test("cache ignored when the config changes", () => {
    const repo = tmpDir();
    const base = cacheFile(repo, EMBEDDING);
    assert.notEqual(cacheFile(repo, { ...EMBEDDING, dtype: "fp32" }), base);
    assert.notEqual(cacheFile(repo, { ...EMBEDDING, passagePrefix: "passage: " }), base);

    const file = path.join(repo, "vectors.json");
    const vectors = { abc123: [0.1, 0.2, 0.3] };
    fs.writeFileSync(file, JSON.stringify({ ...EMBEDDING, dtype: "other", vectors }));
    assert.deepEqual(readCache(file, EMBEDDING), {});
    fs.writeFileSync(file, JSON.stringify({ ...EMBEDDING, vectors }));
    assert.deepEqual(readCache(file, EMBEDDING), vectors);
  });
});
