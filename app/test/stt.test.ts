import assert from "node:assert/strict";
import test from "node:test";
import { transcribeAudio } from "../server/stt.ts";
import { DEFAULT_SETTINGS } from "../shared/protocol.ts";

test("transcribeAudio throws when no API key is provided", async () => {
  const blob = new Blob(["fake-audio"], { type: "audio/webm" });
  await assert.rejects(
    () => transcribeAudio(blob, { ...DEFAULT_SETTINGS, sttApiKey: "" }),
    /Nessuna API key Groq configurata/
  );

  await assert.rejects(
    () => transcribeAudio(blob, { ...DEFAULT_SETTINGS, sttProvider: "deepgram", sttApiKey: "" }),
    /Nessuna API key Deepgram configurata/
  );
});

test("transcribeAudio successfully calls Groq endpoint and returns text", async () => {
  const blob = new Blob(["fake-audio"], { type: "audio/webm" });
  let calledUrl = "";
  let authHeader = "";
  let receivedFormData: any = null;

  const mockFetch: typeof fetch = async (input, init) => {
    calledUrl = String(input);
    authHeader = (init?.headers as Record<string, string>)?.Authorization || "";
    receivedFormData = init?.body as FormData;
    return new Response(JSON.stringify({ text: "  Ciao, questo è un test in italiano.  " }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };

  const res = await transcribeAudio(
    blob,
    { ...DEFAULT_SETTINGS, sttProvider: "groq", sttApiKey: "gsk_test123" },
    mockFetch
  );

  assert.equal(res.text, "Ciao, questo è un test in italiano.");
  assert.equal(calledUrl, "https://api.groq.com/openai/v1/audio/transcriptions");
  assert.equal(authHeader, "Bearer gsk_test123");
  assert.ok(receivedFormData);
  assert.equal(receivedFormData.get("model"), "whisper-large-v3-turbo");
  assert.equal(receivedFormData.get("language"), "it");
});

test("transcribeAudio handles Groq error responses gracefully", async () => {
  const blob = new Blob(["fake-audio"], { type: "audio/webm" });
  const mockFetch: typeof fetch = async () => {
    return new Response(JSON.stringify({ error: { message: "Invalid API Key" } }), {
      status: 401,
      headers: { "Content-Type": "application/json" },
    });
  };

  await assert.rejects(
    () => transcribeAudio(blob, { ...DEFAULT_SETTINGS, sttApiKey: "gsk_invalid" }, mockFetch),
    /Groq: Invalid API Key/
  );
});

test("transcribeAudio successfully calls Deepgram endpoint and returns text", async () => {
  const blob = new Blob(["fake-audio"], { type: "audio/webm" });
  let calledUrl = "";
  let authHeader = "";

  const mockFetch: typeof fetch = async (input, init) => {
    calledUrl = String(input);
    authHeader = (init?.headers as Record<string, string>)?.Authorization || "";
    return new Response(
      JSON.stringify({
        results: {
          channels: [{ alternatives: [{ transcript: "Risposta corretta." }] }],
        },
      }),
      { status: 200, headers: { "Content-Type": "application/json" } }
    );
  };

  const res = await transcribeAudio(
    blob,
    { ...DEFAULT_SETTINGS, sttProvider: "deepgram", sttApiKey: "dg_test123" },
    mockFetch
  );

  assert.equal(res.text, "Risposta corretta.");
  assert.ok(calledUrl.includes("https://api.deepgram.com/v1/listen"));
  assert.ok(calledUrl.includes("language=it"));
  assert.equal(authHeader, "Token dg_test123");
});

test("transcribeAudio forwards custom dictationLanguage to Groq and Deepgram", async () => {
  const blob = new Blob(["fake-audio"], { type: "audio/webm" });

  // Groq with English
  let groqFormData: any = null;
  const mockGroqFetch: typeof fetch = async (_input, init) => {
    groqFormData = init?.body as FormData;
    return new Response(JSON.stringify({ text: "Hello world" }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };

  const groqRes = await transcribeAudio(
    blob,
    { ...DEFAULT_SETTINGS, sttProvider: "groq", sttApiKey: "gsk_test", dictationLanguage: "en" },
    mockGroqFetch
  );
  assert.equal(groqRes.text, "Hello world");
  assert.equal(groqFormData.get("language"), "en");

  // Deepgram with Spanish
  let deepgramUrl = "";
  const mockDeepgramFetch: typeof fetch = async (input) => {
    deepgramUrl = String(input);
    return new Response(
      JSON.stringify({ results: { channels: [{ alternatives: [{ transcript: "Hola mundo" }] }] } }),
      { status: 200, headers: { "Content-Type": "application/json" } }
    );
  };

  const deepgramRes = await transcribeAudio(
    blob,
    { ...DEFAULT_SETTINGS, sttProvider: "deepgram", sttApiKey: "dg_test", dictationLanguage: "es" },
    mockDeepgramFetch
  );
  assert.equal(deepgramRes.text, "Hola mundo");
  assert.ok(deepgramUrl.includes("language=es"));
});

test("readSettings and writeSettings support dictationLanguage", async () => {
  const { readSettings, writeSettings } = await import("../server/settings.ts");
  const os = await import("node:os");
  const fs = await import("node:fs");
  const path = await import("node:path");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-settings-lang-"));
  try {
    const initial = readSettings(root);
    assert.equal(initial.dictationLanguage, "it");

    const updated = writeSettings(root, { dictationLanguage: "fr" });
    assert.equal(updated.dictationLanguage, "fr");

    const reloaded = readSettings(root);
    assert.equal(reloaded.dictationLanguage, "fr");
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("POST /api/transcribe rejects requests without audio file", async () => {
  const { createServer } = await import("../server/main.ts");
  const os = await import("node:os");
  const fs = await import("node:fs");
  const path = await import("node:path");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-stt-"));
  const srv = await createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false });
  try {
    const res = await fetch(`${srv.url}/api/transcribe`, {
      method: "POST",
      headers: { "x-learner-client": "1" },
      body: new FormData(),
    });
    assert.equal(res.status, 400);
    const json = (await res.json()) as any;
    assert.match(json.error, /mancante/);
  } finally {
    await srv.close();
    fs.rmSync(root, { recursive: true, force: true });
  }
});
