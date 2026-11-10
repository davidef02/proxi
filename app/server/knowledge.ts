import * as fs from "node:fs";
import * as path from "node:path";
import { listLessons } from "./lesson.ts";
import type { KnowledgeMap, MapNode, MapNodeStatus, ProgressRecord } from "../shared/protocol.ts";

const PROGRESS_KINDS = new Set(["probe", "predict", "check", "review"]);

export const SHAKY_LAPSES = 3;
export const MATURE_DAYS = 21;

export function ankiStatus(reviews: ProgressRecord[]): MapNodeStatus {
  return shakyCards(reviews).length > 0 ? "shaky" : "solid";
}

// Latest review of every card that still looks shaky. ankiStatus and ankiCause both read this,
// so the stated cause can't disagree with the status.
function shakyCards(reviews: ProgressRecord[]): ProgressRecord[] {
  const groups = new Map<number | string, ProgressRecord[]>();
  for (const r of reviews) {
    const key = r.cardId ?? r.reviewId ?? "default";
    const group = groups.get(key);
    if (group) group.push(r);
    else groups.set(key, [r]);
  }

  const shaky: ProgressRecord[] = [];
  for (const group of groups.values()) {
    let latest = group[0];
    for (let i = 1; i < group.length; i++) {
      const cur = group[i];
      const cmp = cur.at.localeCompare(latest.at);
      if (cmp > 0 || (cmp === 0 && (cur.reviewId ?? 0) > (latest.reviewId ?? 0))) {
        latest = cur;
      }
    }
    const isShaky =
      latest.ease === 1 ||
      ((latest.lapses ?? 0) >= SHAKY_LAPSES && (latest.interval ?? 0) < MATURE_DAYS);
    if (isShaky) shaky.push(latest);
  }
  return shaky;
}

/** Why Anki still calls this node shaky, and the review that says so. */
export function ankiCause(reviews: ProgressRecord[]): { cause: string; at: string } | undefined {
  let latest: ProgressRecord | undefined;
  for (const r of shakyCards(reviews)) {
    if (!latest) {
      latest = r;
      continue;
    }
    const cmp = r.at.localeCompare(latest.at);
    if (cmp > 0 || (cmp === 0 && (r.reviewId ?? 0) > (latest.reviewId ?? 0))) latest = r;
  }
  if (!latest) return undefined;
  return { cause: latest.ease === 1 ? "Anki: Again" : `Anki: ${latest.lapses} lapses`, at: latest.at };
}

const progressFile = (repoRoot: string, course: string) =>
  path.join(path.resolve(repoRoot), "courses", course, "progress.jsonl");

export async function readProgress(repoRoot: string, course: string): Promise<ProgressRecord[]> {
  let raw: string;
  try {
    raw = fs.readFileSync(progressFile(repoRoot, course), "utf-8");
  } catch {
    return [];
  }
  const out: ProgressRecord[] = [];
  for (const line of raw.split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const rec = JSON.parse(line);
      if (rec && typeof rec === "object" && Array.isArray(rec.nodeRefs) && PROGRESS_KINDS.has(rec.kind)) out.push(rec);
    } catch {
      /* skip bad line */
    }
  }
  return out;
}

export async function appendProgress(repoRoot: string, rec: ProgressRecord): Promise<void> {
  fs.appendFileSync(progressFile(repoRoot, rec.course), JSON.stringify(rec) + "\n");
}

function isGraded(r: ProgressRecord): boolean {
  return r.correct !== undefined || r.dontKnow === true;
}

function statusOf(r: ProgressRecord): MapNodeStatus {
  if (!r.correct || r.dontKnow) return "missed";
  return (r.confidence ?? 70) >= 50 ? "solid" : "shaky";
}

// All progress records of one node, oldest first. Shared so the map and the reinforce list
// can't drift on how a record names its node.
function recordsForNode(records: ProgressRecord[], course: string, slug: string, nodeId: string, index: number): ProgressRecord[] {
  const key = `${course}/${slug}#${nodeId}`;
  const mine = records.filter(
    (r) =>
      r.nodeRefs.includes(key) ||
      (r.lessonSlug === slug && (r.nodeRefs.includes(nodeId) || r.nodeRefs.includes(`N${index + 1}`))),
  );
  return mine
    .map((r, idx) => ({ r, idx }))
    .sort((a, b) => a.r.at.localeCompare(b.r.at) || a.idx - b.idx)
    .map((x) => x.r);
}

export function purgeProgress(dataRoot: string): void {
  const coursesDir = path.join(path.resolve(dataRoot), "courses");
  try {
    if (!fs.statSync(coursesDir).isDirectory()) return;
  } catch {
    return;
  }

  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(coursesDir, { withFileTypes: true });
  } catch {
    return;
  }

  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const file = path.join(coursesDir, entry.name, "progress.jsonl");
    if (!fs.existsSync(file)) continue;

    let tmp: string | null = null;
    try {
      const raw = fs.readFileSync(file, "utf-8");
      const segments = raw.match(/[^\n]*\n|[^\n]+$/g) ?? [];
      let dropped = 0;
      const kept: string[] = [];
      for (const segment of segments) {
        let drop = false;
        try {
          const parsed = JSON.parse(segment.replace(/\r?\n$/, ""));
          if (parsed && typeof parsed === "object" && (parsed.kind === "practice" || parsed.kind === "exercise")) {
            drop = true;
          }
        } catch {
          // Empty segments and parse errors are kept
        }
        if (drop) {
          dropped++;
        } else {
          kept.push(segment);
        }
      }
      if (dropped === 0) continue;
      tmp = file + ".purge-" + process.pid + ".tmp";
      fs.writeFileSync(tmp, kept.join(""), "utf-8");
      fs.renameSync(tmp, file);
    } catch (e) {
      if (tmp) {
        try {
          fs.rmSync(tmp, { force: true });
        } catch {}
      }
      console.warn(`progress purge skipped for ${entry.name}: ${(e as Error).message}`);
    }
  }
}

export async function buildKnowledgeMap(repoRoot: string): Promise<KnowledgeMap> {
  const root = path.resolve(repoRoot);
  const nodes: MapNode[] = [];
  const edges: KnowledgeMap["edges"] = [];
  const progress = new Map<string, ProgressRecord[]>();

  for (const { summary: l, doc, rel } of listLessons(root)) {
    if (doc.nodes.length === 0) continue;
    if (!progress.has(l.course)) progress.set(l.course, await readProgress(root, l.course));
    const records = progress.get(l.course)!;
    const keyOf = (id: string) => `${l.course}/${l.slug}#${id}`;
    const ids = new Set(doc.nodes.map((n) => n.nodeId));

    doc.nodes.forEach((n, i) => {
      const key = keyOf(n.nodeId);
      const sortedMine = recordsForNode(records, l.course, l.slug, n.nodeId, i);
      const reviews = sortedMine.filter((r) => r.kind === "review");
      const nonReviews = sortedMine.filter((r) => r.kind !== "review");
      const graded = nonReviews.filter(isGraded);
      const latestGraded = graded[graded.length - 1];
      const status: MapNodeStatus = reviews.length
        ? ankiStatus(reviews)
        : latestGraded
        ? statusOf(latestGraded)
        : "taught";
      const attempts = graded.length + reviews.length;
      nodes.push({
        key,
        course: l.course,
        lessonSlug: l.slug,
        lessonTitle: l.title,
        lessonNumber: l.lessonNumber,
        lessonPath: rel,
        nodeId: n.nodeId,
        title: n.title,
        tier: n.tier,
        status,
        attempts,
        ...(sortedMine.length ? { lastAt: sortedMine[sortedMine.length - 1].at } : {}),
      });
      for (const d of n.dependencies ?? []) if (ids.has(d)) edges.push({ from: keyOf(d), to: key });
    });
  }
  return { nodes, edges };
}

export interface ReinforceEntry {
  key: string;
  title: string;
  lessonNumber: number;
  lessonTitle: string;
  cause: string;
  at: string;
}

/** The course's Anki-shaky concepts, newest evidence first. Empty when Anki never floored a card. */
export async function reinforceList(dataRoot: string, course: string, limit = 10): Promise<ReinforceEntry[]> {
  const root = path.resolve(dataRoot);
  const progress = await readProgress(root, course);
  if (progress.length === 0) return [];

  const out: ReinforceEntry[] = [];
  for (const { summary: l, doc } of listLessons(root)) {
    if (l.course !== course || doc.nodes.length === 0) continue;
    doc.nodes.forEach((n, i) => {
      const reviews = recordsForNode(progress, l.course, l.slug, n.nodeId, i).filter((r) => r.kind === "review");
      const cause = ankiCause(reviews);
      if (!cause) return;
      out.push({
        key: `${l.course}/${l.slug}#${n.nodeId}`,
        title: n.title,
        lessonNumber: l.lessonNumber,
        lessonTitle: l.title,
        cause: cause.cause,
        at: cause.at,
      });
    });
  }

  // stable sort: equal `at` keeps lesson/node order
  out.sort((a, b) => b.at.localeCompare(a.at));
  return out.slice(0, limit);
}
