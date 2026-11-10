import * as fs from "node:fs";
import * as path from "node:path";
import {
  cardsInfo,
  deckQuery,
  DEFAULT_ANKI_URL,
  findCards,
  getReviewsOfCards,
  notesInfo,
} from "../../extensions/anki-client.ts";
import { courseDeckNameFor } from "../../extensions/anki-sync.ts";
import { listLessons } from "./lesson.ts";
import { readProgress } from "./knowledge.ts";
import type { AnkiImportResult, ProgressRecord } from "../shared/protocol.ts";

const inflight = new Map<string, Promise<AnkiImportResult>>();

export async function importAnkiReviews(
  dataRoot: string,
  course: string,
  opts: { ankiUrl?: string } = {}
): Promise<AnkiImportResult> {
  const key = `${path.resolve(dataRoot)}|${course}`;
  const existing = inflight.get(key);
  if (existing) return existing;

  const promise = (async (): Promise<AnkiImportResult> => {
    try {
      return await doImport(dataRoot, course, opts);
    } catch (err: any) {
      return {
        online: false,
        imported: 0,
        skipped: 0,
        error: err?.message || String(err),
      };
    }
  })().finally(() => {
    inflight.delete(key);
  });

  inflight.set(key, promise);
  return promise;
}

async function doImport(
  dataRoot: string,
  course: string,
  opts: { ankiUrl?: string }
): Promise<AnkiImportResult> {
  const url = opts.ankiUrl || process.env.LEARNER_ANKI_URL || DEFAULT_ANKI_URL;
  const deck = courseDeckNameFor(dataRoot, course);

  const cardsRes = await findCards(url, deckQuery(deck));
  if (cardsRes.error !== null || cardsRes.result === null) {
    return {
      online: false,
      imported: 0,
      skipped: 0,
      error: cardsRes.error ?? "Failed to find cards",
    };
  }
  const cardIds = cardsRes.result;
  if (cardIds.length === 0) {
    return { online: true, imported: 0, skipped: 0 };
  }

  const infoRes = await cardsInfo(url, cardIds);
  if (infoRes.error !== null || infoRes.result === null) {
    return {
      online: false,
      imported: 0,
      skipped: 0,
      error: infoRes.error ?? "Failed to fetch card info",
    };
  }
  const cards = infoRes.result;

  const noteIds = Array.from(new Set(cards.map((c) => c.note)));
  const notesRes = await notesInfo(url, noteIds);
  if (notesRes.error !== null || notesRes.result === null) {
    return {
      online: false,
      imported: 0,
      skipped: 0,
      error: notesRes.error ?? "Failed to fetch note info",
    };
  }
  const noteTagsMap = new Map<number, string[]>();
  for (const n of notesRes.result) {
    noteTagsMap.set(n.noteId, n.tags || []);
  }

  const revsRes = await getReviewsOfCards(url, cardIds);
  if (revsRes.error !== null || revsRes.result === null) {
    return {
      online: false,
      imported: 0,
      skipped: 0,
      error: revsRes.error ?? "Failed to fetch reviews",
    };
  }
  const reviewsByCard = revsRes.result;

  const existing = await readProgress(dataRoot, course);
  const known = new Set(
    existing
      .filter((r) => r.source === "anki" && typeof r.reviewId === "number")
      .map((r) => r.reviewId!)
  );

  const lessons = listLessons(dataRoot)
    .filter((l) => l.summary.course === course)
    .map((l) => ({
      slug: l.summary.slug,
      nodeIds: new Set(l.doc.nodes.map((n) => n.nodeId)),
    }));

  let skipped = 0;
  interface MappedCard {
    card: (typeof cards)[number];
    nodeId: string;
    lessonSlug: string;
  }
  const mappedCards: MappedCard[] = [];

  for (const card of cards) {
    const tags = noteTagsMap.get(card.note) || [];
    const nodeTags = new Set<string>();

    for (const tag of tags) {
      if (tag.toLowerCase().startsWith("node::")) {
        nodeTags.add(tag.slice(6));
      }
    }

    if (nodeTags.size !== 1) {
      skipped++;
      continue;
    }

    const nodeId = Array.from(nodeTags)[0];

    let lessonTag: string | undefined;
    for (const tag of tags) {
      if (tag.toLowerCase().startsWith("lesson::")) {
        lessonTag = tag.slice(8);
        break;
      }
    }

    let resolvedLesson = lessonTag
      ? lessons.find((l) => l.slug === lessonTag && l.nodeIds.has(nodeId))
      : undefined;

    if (!resolvedLesson) {
      const candidates = lessons.filter((l) => l.nodeIds.has(nodeId));
      if (candidates.length === 1) {
        resolvedLesson = candidates[0];
      } else {
        skipped++;
        continue;
      }
    }

    mappedCards.push({ card, nodeId, lessonSlug: resolvedLesson.slug });
  }

  const newRecords: ProgressRecord[] = [];
  for (const { card, nodeId, lessonSlug } of mappedCards) {
    const reviews = reviewsByCard[String(card.cardId)] || [];
    for (const r of reviews) {
      if (typeof r.ease === "number" && r.ease >= 1 && r.ease <= 4 && !known.has(r.id)) {
        const rec: ProgressRecord = {
          at: new Date(r.id).toISOString(),
          sessionId: "anki",
          course,
          lessonSlug,
          nodeRefs: [`${course}/${lessonSlug}#${nodeId}`],
          kind: "review",
          source: "anki",
          reviewId: r.id,
          cardId: card.cardId,
          ease: r.ease,
          lapses: card.lapses,
          interval: card.interval,
          correct: r.ease > 1,
        };
        newRecords.push(rec);
        known.add(r.id);
      }
    }
  }

  const courseDir = path.join(path.resolve(dataRoot), "courses", course);
  const courseExists = fs.existsSync(courseDir);

  if (newRecords.length > 0 && courseExists) {
    newRecords.sort((a, b) => (a.reviewId ?? 0) - (b.reviewId ?? 0));
    const lines = newRecords.map((r) => JSON.stringify(r) + "\n");
    const progressFile = path.join(courseDir, "progress.jsonl");
    fs.appendFileSync(progressFile, lines.join(""));
    return { online: true, imported: newRecords.length, skipped };
  }

  return { online: true, imported: 0, skipped };
}
