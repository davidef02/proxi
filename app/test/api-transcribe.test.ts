// POST /api/transcribe over plain HTTP: the same upstream request, text and errors against both server modes.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, afterEach, before, describe, it } from "node:test";
import { startFakeServers, type FakeServers } from "./fake-servers.ts";
import { startServer, type TestServer } from "./harness.ts";

// Built at runtime and low entropy: a literal or key-like key would trip the secret preflight and gitleaks (L12/L13).
const key = ["test", "harness".repeat(3)].join("-");

let fakes: FakeServers;
let saved: { groq: string | undefined; deepgram: string | undefined };

before(async () => {
  fakes = await startFakeServers();
  saved = { groq: process.env.GROQ_API_KEY, deepgram: process.env.DEEPGRAM_API_KEY };
  delete process.env.GROQ_API_KEY;
  delete process.env.DEEPGRAM_API_KEY;
});

after(async () => {
  await fakes.close();
  restore("GROQ_API_KEY", saved.groq);
  restore("DEEPGRAM_API_KEY", saved.deepgram);
});

afterEach(() => fakes.reset());

function restore(name: string, value: string | undefined): void {
  if (value === undefined) delete process.env[name];
  else process.env[name] = value;
}

/** A fresh repo per test: settings written by one test must not reach another. */
async function withServer(fn: (server: TestServer) => Promise<void>, settings?: unknown): Promise<void> {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "api-transcribe-repo-"));
  let server: TestServer | undefined;
  try {
    server = await startServer({ repoRoot: repo, fakes: fakes.urls });
    if (settings !== undefined) assert.equal((await saveSettings(server, settings)).status, 200);
    await fn(server);
  } finally {
    await server?.close();
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
  }
}

function saveSettings(server: TestServer, patch: unknown): Promise<Response> {
  return fetch(`${server.url}/api/settings`, {
    method: "POST",
    headers: { "x-learner-client": "1", "content-type": "application/json" },
    body: JSON.stringify(patch),
  });
}

/** The browser's dictation upload: one `audio` file field, with the client header the server requires. */
function postAudio(server: TestServer, { withAudio = true } = {}): Promise<Response> {
  const form = new FormData();
  if (withAudio) form.append("audio", new Blob(["fake-audio"], { type: "audio/webm" }), "a.webm");
  return fetch(`${server.url}/api/transcribe`, {
    method: "POST",
    headers: { "x-learner-client": "1" },
    body: form,
  });
}

const GROQ_PATH = "/openai/v1/audio/transcriptions";
const GROQ_NO_KEY = "Nessuna API key Groq configurata. Inseriscila nelle Impostazioni (gratuita su console.groq.com).";
const DEEPGRAM_NO_KEY = "Nessuna API key Deepgram configurata. Inseriscila nelle Impostazioni.";

describe("POST /api/transcribe", () => {
  it("rejects a request without an audio file", async () => {
    await withServer(async (server) => {
      const res = await postAudio(server, { withAudio: false });
      assert.equal(res.status, 400);
      assert.deepEqual(await res.json(), { error: "File audio mancante" });
    });
  });

  it("refuses Groq without a key before calling the service", async () => {
    await withServer(async (server) => {
      const res = await postAudio(server);
      assert.equal(res.status, 400);
      assert.deepEqual(await res.json(), { error: GROQ_NO_KEY });
      assert.equal(fakes.transcription.requests.length, 0);
    });
  });

  it("refuses Deepgram without a key before calling the service", async () => {
    await withServer(
      async (server) => {
        const res = await postAudio(server);
        assert.equal(res.status, 400);
        assert.deepEqual(await res.json(), { error: DEEPGRAM_NO_KEY });
        assert.equal(fakes.transcription.requests.length, 0);
      },
      { sttProvider: "deepgram" },
    );
  });

  it("Groq: forwards the form and trims the text", async () => {
    await withServer(
      async (server) => {
        fakes.transcription.on("POST", GROQ_PATH, { body: { text: "  Ciao, questo è un test in italiano.  " } });

        const res = await postAudio(server);
        assert.equal(res.status, 200);
        assert.deepEqual(await res.json(), { text: "Ciao, questo è un test in italiano." });

        assert.equal(fakes.transcription.requests.length, 1);
        const req = fakes.transcription.requests[0];
        assert.equal(req.method, "POST");
        assert.equal(req.path, GROQ_PATH);
        assert.equal(req.headers.authorization, `Bearer ${key}`);
        for (const part of [
          'name="model"',
          "whisper-large-v3-turbo",
          'name="language"',
          'name="response_format"',
          'name="temperature"',
          'filename="dictation.webm"',
          "fake-audio",
        ]) {
          assert.ok(req.body.includes(part), `missing ${part} in ${req.body}`);
        }
      },
      { sttProvider: "groq", sttApiKey: key },
    );
  });

  it("Groq: uses the service message and falls back to the status", async () => {
    await withServer(
      async (server) => {
        fakes.transcription.on("POST", GROQ_PATH, { status: 401, body: { error: { message: "Invalid API Key" } } });
        let res = await postAudio(server);
        assert.equal(res.status, 400);
        assert.deepEqual(await res.json(), { error: "Groq: Invalid API Key" });

        fakes.transcription.on("POST", GROQ_PATH, { status: 500, body: "boom" });
        res = await postAudio(server);
        assert.equal(res.status, 400);
        assert.deepEqual(await res.json(), { error: "Errore Groq API (500)" });
      },
      { sttProvider: "groq", sttApiKey: key },
    );
  });

  it("Deepgram: sends the raw audio and the language", async () => {
    await withServer(
      async (server) => {
        fakes.transcription.on("POST", "/v1/listen", {
          body: { results: { channels: [{ alternatives: [{ transcript: "Hola mundo " }] }] } },
        });

        const res = await postAudio(server);
        assert.equal(res.status, 200);
        assert.deepEqual(await res.json(), { text: "Hola mundo" });

        assert.equal(fakes.transcription.requests.length, 1);
        const req = fakes.transcription.requests[0];
        assert.equal(req.method, "POST");
        assert.equal(req.path, "/v1/listen?model=nova-3&language=es&smart_format=true&punctuate=true");
        assert.equal(req.headers.authorization, `Token ${key}`);
        assert.equal(req.headers["content-type"], "audio/webm");
        assert.equal(req.body, "fake-audio");
      },
      { sttProvider: "deepgram", sttApiKey: key, dictationLanguage: "es" },
    );
  });

  it("Deepgram: uses err_msg from the service", async () => {
    await withServer(
      async (server) => {
        fakes.transcription.on("POST", "/v1/listen", { status: 400, body: { err_msg: "Bad audio" } });
        const res = await postAudio(server);
        assert.equal(res.status, 400);
        assert.deepEqual(await res.json(), { error: "Deepgram: Bad audio" });
      },
      { sttProvider: "deepgram", sttApiKey: key },
    );
  });

  it("falls back to GROQ_API_KEY", async () => {
    process.env.GROQ_API_KEY = key;
    try {
      await withServer(async (server) => {
        fakes.transcription.on("POST", GROQ_PATH, { body: { text: "env key" } });
        const res = await postAudio(server);
        assert.equal(res.status, 200);
        assert.deepEqual(await res.json(), { text: "env key" });
        assert.equal(fakes.transcription.requests[0].headers.authorization, `Bearer ${key}`);
      });
    } finally {
      delete process.env.GROQ_API_KEY;
    }
  });
});
