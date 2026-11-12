// Tests for the mode-selecting server helper (PROXI_TEST_SERVER) and its small utilities.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, describe, it } from "node:test";
import { fakeFetch, MODE_ENV, proxiBinary, serverMode, startServer } from "./harness.ts";

const dirs: string[] = [];
function tempDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  dirs.push(dir);
  return dir;
}

after(() => {
  for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
});

describe("serverMode", () => {
  it("defaults to node and accepts both modes", () => {
    assert.equal(serverMode({}), "node");
    assert.equal(serverMode({ [MODE_ENV]: "" }), "node");
    assert.equal(serverMode({ [MODE_ENV]: "node" }), "node");
    assert.equal(serverMode({ [MODE_ENV]: "binary" }), "binary");
  });

  it("rejects an unknown mode", () => {
    assert.throws(() => serverMode({ [MODE_ENV]: "rust" }), /must be "node" or "binary"/);
  });
});

describe("proxiBinary", () => {
  const exe = process.platform === "win32" ? "proxi.exe" : "proxi";

  it("returns the newest target build", () => {
    const root = tempDir("harness-bin-");
    const release = path.join(root, "target", "release", exe);
    const debug = path.join(root, "target", "debug", exe);
    for (const file of [release, debug]) {
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, "");
    }

    fs.utimesSync(release, new Date(1_700_000_000_000), new Date(1_700_000_000_000));
    fs.utimesSync(debug, new Date(1_800_000_000_000), new Date(1_800_000_000_000));
    assert.equal(proxiBinary(root, {}), debug);

    fs.utimesSync(release, new Date(1_900_000_000_000), new Date(1_900_000_000_000));
    assert.equal(proxiBinary(root, {}), release);
  });

  it("honours PROXI_BIN and explains a missing binary", () => {
    const root = tempDir("harness-bin-env-");
    const pinned = path.join(root, "custom-proxi");
    fs.writeFileSync(pinned, "");
    assert.equal(proxiBinary(root, { PROXI_BIN: pinned }), pinned);
    assert.throws(() => proxiBinary(root, { PROXI_BIN: path.join(root, "missing") }), /PROXI_BIN not found/);
    assert.throws(() => proxiBinary(tempDir("harness-bin-empty-"), {}), /cargo build --release --locked/);
  });
});

describe("fakeFetch", () => {
  const recorder = () => {
    const seen: string[] = [];
    const base = (async (input: string | URL | Request) => {
      seen.push(typeof input === "string" ? input : input instanceof URL ? input.href : input.url);
      return new Response("ok");
    }) as typeof fetch;
    return { seen, base };
  };

  it("rewrites the real origins and leaves loopback alone", async () => {
    const { seen, base } = recorder();
    const f = fakeFetch(
      { github: "http://127.0.0.1:4100/", google: "http://127.0.0.1:4200", transcription: "http://127.0.0.1:4300" },
      base,
    );

    await f("https://api.github.com/repos/a/b?x=1");
    assert.equal(seen.at(-1), "http://127.0.0.1:4100/repos/a/b?x=1");
    await f("https://github.com/login/device/code");
    assert.equal(seen.at(-1), "http://127.0.0.1:4100/login/device/code");
    await f("https://www.googleapis.com/upload/drive/v3/files");
    assert.equal(seen.at(-1), "http://127.0.0.1:4200/upload/drive/v3/files");
    await f("https://api.groq.com/openai/v1/audio/transcriptions");
    assert.equal(seen.at(-1), "http://127.0.0.1:4300/openai/v1/audio/transcriptions");
    await f("http://127.0.0.1:9/x");
    assert.equal(seen.at(-1), "http://127.0.0.1:9/x");
  });

  it("rejects an origin with no fake", async () => {
    const { base } = recorder();
    const f = fakeFetch({ github: "http://127.0.0.1:4100" }, base);
    await assert.rejects(() => f("https://example.com/"), /no fake/);
    await assert.rejects(() => f("https://huggingface.co/x"), /no fake/);
  });
});

describe("startServer", () => {
  it("starts in the selected mode and serves the roots", async () => {
    const repo = tempDir("harness-repo-");
    const data = tempDir("harness-data-");
    const server = await startServer({ repoRoot: repo, dataRoot: data });
    try {
      assert.equal(server.mode, serverMode());
      assert.match(server.url, /^http:\/\/127\.0\.0\.1:\d+$/);
      const res = await fetch(`${server.url}/api/settings`);
      assert.equal(res.status, 200);
      const body = (await res.json()) as { repoRoot: string; dataRoot: string };
      assert.equal(body.repoRoot, path.resolve(repo));
      assert.equal(body.dataRoot, path.resolve(data));
    } finally {
      await server.close();
    }
    await assert.rejects(fetch(`${server.url}/api/settings`));
    await server.close();
  });

  it("maps the Anki fake onto LEARNER_ANKI_URL and restores it", { skip: serverMode() !== "node" && "node mode only" }, async () => {
    const repo = tempDir("harness-anki-");
    const prev = process.env.LEARNER_ANKI_URL;
    const fake = "http://127.0.0.1:9";
    const server = await startServer({ repoRoot: repo, fakes: { anki: fake } });
    try {
      assert.equal(process.env.LEARNER_ANKI_URL, fake);
    } finally {
      await server.close();
    }
    assert.equal(process.env.LEARNER_ANKI_URL, prev);
  });
});
