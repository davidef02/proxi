import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api.ts";

export type DictationStatus = "idle" | "recording" | "transcribing" | "error";

export interface UseDictationOptions {
  onTranscribed: (text: string) => void;
  onError?: (err: string) => void;
}

export function useDictation({ onTranscribed, onError }: UseDictationOptions) {
  const [status, setStatus] = useState<DictationStatus>("idle");
  const [error, setError] = useState<string | null>(null);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const cancelledRef = useRef(false);

  const cleanup = useCallback(() => {
    if (streamRef.current) {
      for (const track of streamRef.current.getTracks()) {
        try {
          track.stop();
        } catch {}
      }
      streamRef.current = null;
    }
    mediaRecorderRef.current = null;
    chunksRef.current = [];
  }, []);

  const cancel = useCallback(() => {
    cancelledRef.current = true;
    if (mediaRecorderRef.current && mediaRecorderRef.current.state !== "inactive") {
      try {
        mediaRecorderRef.current.stop();
      } catch {}
    }
    cleanup();
    setStatus("idle");
    setError(null);
  }, [cleanup]);

  const start = useCallback(async () => {
    if (!navigator.mediaDevices?.getUserMedia) {
      const msg = "Microfono non supportato in questo ambiente.";
      setError(msg);
      onError?.(msg);
      setStatus("error");
      return;
    }

    try {
      cancelledRef.current = false;
      chunksRef.current = [];
      setError(null);

      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
      streamRef.current = stream;

      const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
        ? "audio/webm;codecs=opus"
        : MediaRecorder.isTypeSupported("audio/webm")
        ? "audio/webm"
        : "";

      const mr = mimeType ? new MediaRecorder(stream, { mimeType }) : new MediaRecorder(stream);
      mediaRecorderRef.current = mr;

      mr.ondataavailable = (e) => {
        if (e.data && e.data.size > 0) {
          chunksRef.current.push(e.data);
        }
      };

      mr.onstop = async () => {
        if (cancelledRef.current) {
          cleanup();
          return;
        }

        const chunks = chunksRef.current;
        const type = mr.mimeType || "audio/webm";
        cleanup();

        if (chunks.length === 0) {
          setStatus("idle");
          return;
        }

        const audioBlob = new Blob(chunks, { type });
        setStatus("transcribing");

        try {
          const res = await api.transcribe(audioBlob);
          const t = res.text?.trim();
          if (t) {
            onTranscribed(t);
          }
          setStatus("idle");
        } catch (e: any) {
          const msg = e?.message || "Errore durante la trascrizione audio";
          setError(msg);
          onError?.(msg);
          setStatus("error");
        }
      };

      mr.start(250); // timeslice 250ms
      setStatus("recording");
    } catch (e: any) {
      cleanup();
      const msg =
        e.name === "NotAllowedError" || e.name === "PermissionDeniedError"
          ? "Permesso microfono negato."
          : e?.message || "Impossibile accedere al microfono.";
      setError(msg);
      onError?.(msg);
      setStatus("error");
    }
  }, [cleanup, onTranscribed, onError]);

  const stop = useCallback(() => {
    if (mediaRecorderRef.current && mediaRecorderRef.current.state === "recording") {
      try {
        mediaRecorderRef.current.stop();
      } catch (e) {
        cleanup();
        setStatus("idle");
      }
    }
  }, [cleanup]);

  const toggle = useCallback(() => {
    if (status === "recording") {
      stop();
    } else if (status === "idle" || status === "error") {
      void start();
    }
  }, [status, start, stop]);

  // Clean up if component unmounts while recording
  useEffect(() => {
    return () => {
      cancelledRef.current = true;
      if (mediaRecorderRef.current && mediaRecorderRef.current.state !== "inactive") {
        try {
          mediaRecorderRef.current.stop();
        } catch {}
      }
      cleanup();
    };
  }, [cleanup]);

  return {
    status,
    isRecording: status === "recording",
    isTranscribing: status === "transcribing",
    error,
    start,
    stop,
    cancel,
    toggle,
  };
}
