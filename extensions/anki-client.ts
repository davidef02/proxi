/**
 * anki-client.ts — the one adapter onto AnkiConnect.
 *
 * Part of the Proxi teaching engine.
 * Everything that talks to Anki goes through `invoke`: one fetch, one timeout,
 * one error shape. Callers only ever check whether `.error` is set.
 */

const ANKI_VERSION = 6;
export const DEFAULT_ANKI_URL = "http://127.0.0.1:8765";

/** AnkiConnect always answers with exactly one of the two fields set. */
export interface AnkiResponse<T = any> {
  result: T | null;
  error: string | null;
}

/** Sends a single JSON-RPC action. Never throws: a dead Anki is an `error`, not an exception. */
export async function invoke<T = any>(
  url: string,
  action: string,
  params?: any,
  timeoutMs = 2000
): Promise<AnkiResponse<T>> {
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json; charset=utf-8" },
      body: JSON.stringify({ action, version: ANKI_VERSION, params }),
      signal: AbortSignal.timeout(timeoutMs)
    });

    if (!res.ok) {
      return { result: null, error: `HTTP ${res.status} ${res.statusText}` };
    }

    return (await res.json()) as AnkiResponse<T>;
  } catch (err: any) {
    return {
      result: null,
      error:
        err.name === "TimeoutError" || err.name === "AbortError"
          ? "AnkiConnect request timed out"
          : err.message || "Network error"
    };
  }
}

export interface AnkiCardInfo {
  cardId: number;
  note: number;
  deckName: string;
  interval: number;
  lapses: number;
  factor: number;
}

export interface AnkiNoteInfo {
  noteId: number;
  tags: string[];
}

export interface AnkiReview {
  id: number;
  ease: number;
  ivl: number;
  lastIvl: number;
  factor: number;
  time: number;
  type: number;
}

export function deckQuery(deck: string): string {
  return `deck:"${deck.replaceAll('"', '\\"')}"`;
}

export async function findCards(
  url: string,
  query: string,
  timeoutMs = 2000
): Promise<AnkiResponse<number[]>> {
  return invoke<number[]>(url, "findCards", { query }, timeoutMs);
}

export async function cardsInfo(
  url: string,
  cards: number[],
  timeoutMs = 5000
): Promise<AnkiResponse<AnkiCardInfo[]>> {
  return invoke<AnkiCardInfo[]>(url, "cardsInfo", { cards }, timeoutMs);
}

export async function notesInfo(
  url: string,
  notes: number[],
  timeoutMs = 5000
): Promise<AnkiResponse<AnkiNoteInfo[]>> {
  return invoke<AnkiNoteInfo[]>(url, "notesInfo", { notes }, timeoutMs);
}

export async function getReviewsOfCards(
  url: string,
  cards: number[],
  timeoutMs = 5000
): Promise<AnkiResponse<Record<string, AnkiReview[]>>> {
  return invoke<Record<string, AnkiReview[]>>(url, "getReviewsOfCards", { cards }, timeoutMs);
}

