import type { LearnerSettings } from "../shared/protocol.ts";

export interface TranscribeResult {
  text: string;
}

export async function transcribeAudio(
  audioBlob: Blob,
  settings: LearnerSettings,
  fetchFn: typeof fetch = fetch,
): Promise<TranscribeResult> {
  const provider = settings.sttProvider || "groq";
  const apiKey =
    settings.sttApiKey?.trim() ||
    (provider === "groq" ? process.env.GROQ_API_KEY : process.env.DEEPGRAM_API_KEY) ||
    "";

  if (!apiKey) {
    throw new Error(
      provider === "groq"
        ? "Nessuna API key Groq configurata. Inseriscila nelle Impostazioni (gratuita su console.groq.com)."
        : "Nessuna API key Deepgram configurata. Inseriscila nelle Impostazioni."
    );
  }

  const language = settings.dictationLanguage?.trim() || "it";

  if (provider === "groq") {
    const fd = new FormData();
    fd.append("file", audioBlob, "dictation.webm");
    fd.append("model", "whisper-large-v3-turbo");
    fd.append("language", language);
    fd.append("response_format", "json");
    fd.append("temperature", "0");

    const res = await fetchFn("https://api.groq.com/openai/v1/audio/transcriptions", {
      method: "POST",
      headers: {
        Authorization: `Bearer ${apiKey}`,
      },
      body: fd,
    });

    if (!res.ok) {
      const errText = await res.text().catch(() => "");
      let msg = `Errore Groq API (${res.status})`;
      try {
        const json = JSON.parse(errText);
        if (json?.error?.message) msg = `Groq: ${json.error.message}`;
      } catch {}
      throw new Error(msg);
    }

    const data = (await res.json()) as { text?: string };
    return { text: (data.text ?? "").trim() };
  } else {
    // Deepgram REST endpoint
    const res = await fetchFn(
      `https://api.deepgram.com/v1/listen?model=nova-3&language=${encodeURIComponent(language)}&smart_format=true&punctuate=true`,
      {
        method: "POST",
        headers: {
          Authorization: `Token ${apiKey}`,
          "Content-Type": audioBlob.type || "audio/webm",
        },
        body: audioBlob,
      }
    );

    if (!res.ok) {
      const errText = await res.text().catch(() => "");
      let msg = `Errore Deepgram API (${res.status})`;
      try {
        const json = JSON.parse(errText);
        if (json?.err_msg) msg = `Deepgram: ${json.err_msg}`;
      } catch {}
      throw new Error(msg);
    }

    const data = (await res.json()) as any;
    const transcript =
      data?.results?.channels?.[0]?.alternatives?.[0]?.transcript ?? "";
    return { text: transcript.trim() };
  }
}
