// Adapted from LearnHarness (github.com/amosblomqvist/learnharness), AGPL-3.0-only.
// Map ledgers in <repo>/graph/: nodes.jsonl, chains.jsonl, coverage.jsonl (written by extract.ts), layout.json
// (positions; it replaced positions.json when the map became bottom-up per course: the old file is dropped unread).
// Append-only JSONL, corrupt-line tolerant; every write goes through one process-wide FIFO lock.
import { randomBytes } from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";
import { scanLessons } from "../../../extensions/homepage.ts";
import { readCurriculum } from "../../../extensions/chapter.ts";
import { buildKnowledgeMap } from "../knowledge.ts";
import type { MapNodeStatus } from "../../shared/protocol.ts";
import {
  buildSnapshot,
  chainKey,
  getGraph,
  birthSessionOf,
  layeredLayout,
  simplifyChains,
  type GraphChain,
  type GraphNode,
  type GraphSnapshot,
  type Position,
  type ProvenancePoint,
} from "../../shared/graph/index.ts";
import type { Findings } from "../types.ts";

/** tmp + rename; Windows can refuse the rename (EPERM, AV/indexer): fall back to a direct write. */
function writeAtomic(f: string, data: string): void {
  const tmp = f + ".tmp";
  fs.writeFileSync(tmp, data);
  try {
    fs.renameSync(tmp, f);
  } catch {
    fs.writeFileSync(f, data);
    fs.rmSync(tmp, { force: true });
  }
}

const dir = (repoRoot: string) => path.join(path.resolve(repoRoot), "graph");
const file = (repoRoot: string, name: string) => path.join(dir(repoRoot), name);

// ───────────── lock ─────────────
// ponytail: one global FIFO for every repo; per-repo locks if two repos ever share a process under load.
let tail: Promise<unknown> = Promise.resolve();
export function withGraphLock<T>(fn: () => Promise<T> | T): Promise<T> {
  const run = tail.then(fn, fn);
  tail = run.catch(() => undefined);
  return run;
}

// ───────────── raw ledgers ─────────────
const isPoint = (p: any): p is ProvenancePoint => p && typeof p.chatId === "string" && typeof p.messageId === "string";
const strs = (a: unknown): a is string[] => Array.isArray(a) && a.every((s) => typeof s === "string");

function isNode(o: any): o is GraphNode {
  return (
    o && typeof o.id === "string" && typeof o.name === "string" && typeof o.statement === "string" &&
    strs(o.aliases) && typeof o.createdAt === "string" && Array.isArray(o.provenance) && o.provenance.every(isPoint)
  );
}
function isChain(o: any): o is GraphChain {
  return (
    o && typeof o.id === "string" && strs(o.premises) && o.premises.length > 0 && typeof o.conclusion === "string" &&
    typeof o.createdAt === "string" && Array.isArray(o.provenance) && o.provenance.every(isPoint)
  );
}

function readJsonl<T>(f: string, guard: (o: unknown) => o is T): T[] {
  let raw: string;
  try {
    raw = fs.readFileSync(f, "utf-8");
  } catch {
    return [];
  }
  const out: T[] = [];
  for (const line of raw.split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const o = JSON.parse(line);
      if (guard(o)) out.push(o);
    } catch {
      /* corrupt line: skipped */
    }
  }
  return out;
}

export const readNodeLines = (repoRoot: string) => readJsonl(file(repoRoot, "nodes.jsonl"), isNode);
export const readChainLines = (repoRoot: string) => readJsonl(file(repoRoot, "chains.jsonl"), isChain);

function append(repoRoot: string, name: string, rows: object[]): void {
  if (rows.length === 0) return;
  fs.mkdirSync(dir(repoRoot), { recursive: true });
  const f = file(repoRoot, name);
  // a crashed write can leave the last line unterminated: start on a fresh line
  let lead = "";
  try {
    const raw = fs.readFileSync(f, "utf-8");
    if (raw && !raw.endsWith("\n")) lead = "\n";
  } catch {
    /* new file */
  }
  fs.appendFileSync(f, lead + rows.map((r) => JSON.stringify(r)).join("\n") + "\n");
}

/** Rewrite a ledger dropping only the lines `drop` rejects; lines readers cannot parse are kept verbatim (never lose data). */
function rewriteWithout<T>(repoRoot: string, name: string, guard: (o: unknown) => o is T, drop: (o: T) => boolean): void {
  const f = file(repoRoot, name);
  let raw: string;
  try {
    raw = fs.readFileSync(f, "utf-8");
  } catch {
    return;
  }
  const out: string[] = [];
  for (const line of raw.split(/\r?\n/)) {
    if (!line.trim()) continue;
    let o: unknown;
    try {
      o = JSON.parse(line);
    } catch {
      out.push(line);
      continue;
    }
    if (!(guard(o) && drop(o))) out.push(line);
  }
  writeAtomic(f, out.map((l) => l + "\n").join(""));
}

/** Monotonic revision shared by every {type:"graph"} event. */
let revision = 0;
export const nextRevision = (): number => ++revision;

export function readPositions(repoRoot: string): Record<string, Position> {
  try {
    const o = JSON.parse(fs.readFileSync(file(repoRoot, LAYOUT_FILE), "utf-8"));
    const out: Record<string, Position> = {};
    for (const [k, v] of Object.entries(o ?? {})) {
      const p = v as Position;
      if (p && Number.isFinite(p.x) && Number.isFinite(p.y)) out[k] = { x: p.x, y: p.y };
    }
    return out;
  } catch {
    return {};
  }
}

const LAYOUT_FILE = "layout.json";

function writePositionsFile(repoRoot: string, positions: Record<string, Position>): void {
  fs.mkdirSync(dir(repoRoot), { recursive: true });
  const f = file(repoRoot, LAYOUT_FILE);
  fs.rmSync(file(repoRoot, "positions.json"), { force: true }); // pre-v4 free layout
  writeAtomic(f, JSON.stringify(positions));
}

/** Merge dragged positions; ids that are neither a proposition nor a chain (implication node) are ignored. */
export function savePositions(repoRoot: string, update: Record<string, Position>): Promise<void> {
  return withGraphLock(() => {
    const merged = readPositions(repoRoot);
    const known = new Set(readNodeLines(repoRoot).map((n) => n.id));
    for (const c of readChainLines(repoRoot)) known.add(c.id);
    for (const [k, p] of Object.entries(update)) {
      if (known.has(k) && p && Number.isFinite(p.x) && Number.isFinite(p.y)) merged[k] = { x: p.x, y: p.y };
    }
    writePositionsFile(repoRoot, merged);
  });
}

// ───────────── contract (types.ts) ─────────────

/** Existing propositions, grouped by the lesson they were born in (oldest lesson first): "n_id | name | statement". */
export async function graphDigest(repoRoot: string): Promise<string> {
  const g = getGraph({ readNodes: () => readNodeLines(repoRoot), readChains: () => readChainLines(repoRoot) });
  const labels = lessonLabels(repoRoot);
  const groups = new Map<string, string[]>();
  for (const n of g.nodes) {
    const src = birthSessionOf(n) ?? "";
    const rows = groups.get(src) ?? [];
    rows.push(`${n.id} | ${n.name} | ${n.statement}`);
    groups.set(src, rows);
  }
  return [...groups.keys()]
    .sort()
    .map((src) => `## ${labels[src] ?? (src || "(senza lezione)")}\n${groups.get(src)!.join("\n")}`)
    .join("\n\n");
}

/** Propositions no chain uses (neither premise nor conclusion), by birth lesson: the map must never hold them. */
export function isolatedBySource(repoRoot: string): Map<string, { id: string; name: string }[]> {
  const g = getGraph({ readNodes: () => readNodeLines(repoRoot), readChains: () => readChainLines(repoRoot) });
  const used = new Set(g.chains.flatMap((c) => [...c.premises, c.conclusion]));
  const out = new Map<string, { id: string; name: string }[]>();
  for (const n of g.nodes) {
    if (used.has(n.id)) continue;
    const src = birthSessionOf(n) ?? "";
    out.set(src, [...(out.get(src) ?? []), { id: n.id, name: n.name }]);
  }
  return out;
}

/** Errors the extractor model can act on. [] = ok. Every new proposition must sit in a chain of the submission, and so
 *  must every `mustConnect` id (this lesson's propositions left isolated by an earlier extraction) unless the
 *  submission removes it (`remove`: an isolated idea that turned out not to be load-bearing). */
export function validateFindings(f: Findings, existingIds: Set<string>, mustConnect: string[] = []): string[] {
  const errors: string[] = [];
  if (!f || !Array.isArray(f.propositions) || !Array.isArray(f.chains)) return ["propositions e chains devono essere array (anche vuoti)"];
  const local = new Set<string>();
  for (const [i, p] of f.propositions.entries()) {
    const at = `propositions[${i}]`;
    if (!p || typeof p.id !== "string" || !/^p\d+$/.test(p.id)) errors.push(`${at}.id deve essere "p1", "p2", ...`);
    else if (local.has(p.id)) errors.push(`${at}.id "${p.id}" duplicato`);
    else local.add(p.id);
    if (!p || typeof p.name !== "string" || !p.name.trim()) errors.push(`${at}.name vuoto`);
    if (!p || typeof p.statement !== "string" || !p.statement.trim()) errors.push(`${at}.statement vuoto`);
    if (p?.linkTo !== undefined && !existingIds.has(p.linkTo)) errors.push(`${at}.linkTo "${p.linkTo}" non esiste nel grafo`);
  }
  const known = (id: string) => local.has(id) || existingIds.has(id);
  for (const [i, c] of f.chains.entries()) {
    const at = `chains[${i}]`;
    if (!c || !Array.isArray(c.premises) || c.premises.length === 0) {
      errors.push(`${at}.premises vuoto: una catena ha almeno una premessa`);
      continue;
    }
    for (const id of [...c.premises, c.conclusion]) if (!known(id)) errors.push(`${at}: "${id}" non è un id locale pN né un n_ esistente`);
    if (c.premises.includes(c.conclusion)) errors.push(`${at}: la conclusione compare tra le premesse`);
  }
  const inChain = new Set(f.chains.flatMap((c) => (c && Array.isArray(c.premises) ? [...c.premises, c.conclusion] : [])));
  const linkOf = new Map(f.propositions.filter((p) => p?.linkTo !== undefined).map((p) => [p.linkTo!, p.id]));
  for (const [i, p] of f.propositions.entries())
    if (p && typeof p.id === "string" && p.linkTo === undefined && !inChain.has(p.id))
      errors.push(`propositions[${i}] "${p.id}" non è in nessuna catena: ogni idea della mappa regge o deriva da un'altra (mettila in una catena, o toglila se non è portante)`);
  const removed = new Set(f.remove ?? []);
  for (const id of removed)
    if (!mustConnect.includes(id)) errors.push(`remove: "${id}" non è tra le idee isolate elencate (si possono togliere solo quelle)`);
    else if (inChain.has(id)) errors.push(`remove: "${id}" è anche in una catena: o la colleghi o la togli`);
  for (const id of mustConnect)
    if (existingIds.has(id) && !removed.has(id) && !inChain.has(id) && !inChain.has(linkOf.get(id) ?? ""))
      errors.push(`"${id}" è ancora isolata: mettila in una catena come premessa o conclusione, o in remove se non è portante`);
  return errors;
}

const newNodeId = () => `n_${randomBytes(6).toString("hex")}`;
const newChainId = () => `arg_${randomBytes(6).toString("hex")}`;

export function mergeFindings(
  repoRoot: string,
  sourceId: string,
  f: Findings,
  /** Runs inside the lock right after the ledgers are written (coverage). */
  afterWrite?: () => void,
): Promise<{ added: number; linked: number; chains: number }> {
  return withGraphLock(async () => {
    const existing = new Set(readNodeLines(repoRoot).map((n) => n.id));
    const now = new Date().toISOString();
    const mapping = new Map<string, string>();
    const nodeRows: GraphNode[] = [];
    let added = 0;
    let linked = 0;
    for (const p of f.propositions) {
      if (mapping.has(p.id)) continue;
      const provenance = [{ chatId: sourceId, messageId: p.nodeId ?? "" }];
      if (p.linkTo !== undefined && existing.has(p.linkTo)) {
        // link-append: the first line's name/statement stand, phrasings and provenance union at read time
        nodeRows.push({ id: p.linkTo, name: p.name, statement: p.statement, aliases: [...(p.aliases ?? [])], createdAt: now, provenance });
        mapping.set(p.id, p.linkTo);
        linked++;
      } else {
        const id = newNodeId();
        nodeRows.push({ id, name: p.name, statement: p.statement, aliases: [...(p.aliases ?? [])], createdAt: now, provenance });
        existing.add(id);
        mapping.set(p.id, id);
        added++;
      }
    }
    append(repoRoot, "nodes.jsonl", nodeRows);

    // chains: dedupe by (sorted premises, conclusion); a known argument re-stated by another source keeps its id
    const lines = readChainLines(repoRoot);
    const idOfKey = new Map<string, string>();
    const assertedBySource = new Set<string>();
    for (const l of lines) {
      const k = chainKey(l.premises, l.conclusion);
      if (!idOfKey.has(k)) idOfKey.set(k, l.id);
      if (l.provenance.some((pt) => pt.chatId === sourceId)) assertedBySource.add(k);
    }
    const resolve = (e: string) => mapping.get(e) ?? (existing.has(e) ? e : undefined);
    const chainRows: GraphChain[] = [];
    let chains = 0;
    for (const c of f.chains) {
      const conclusion = resolve(c.conclusion);
      if (conclusion === undefined) continue;
      const premises = [...new Set(c.premises.map(resolve))].filter((p): p is string => p !== undefined && p !== conclusion);
      if (premises.length === 0) continue;
      const k = chainKey(premises, conclusion);
      if (assertedBySource.has(k)) continue;
      const id = idOfKey.get(k) ?? newChainId();
      chainRows.push({ id, premises, conclusion, createdAt: now, provenance: [{ chatId: sourceId, messageId: "" }] });
      idOfKey.set(k, id);
      assertedBySource.add(k);
      chains++;
    }
    append(repoRoot, "chains.jsonl", chainRows);
    // Isolated ideas the extractor judged not load-bearing (validated: only ones listed to it, in no chain).
    const removed = new Set(f.remove ?? []);
    if (removed.size) rewriteWithout(repoRoot, "nodes.jsonl", isNode, (n) => removed.has(n.id));

    simplifyLedger(repoRoot, [...lines, ...chainRows]);
    afterWrite?.();
    try {
      settleLayout(repoRoot);
    } catch {
      /* positions are rebuildable: a layout failure must not fail (and so duplicate) a merge that already landed */
    }
    return { added, linked, chains };
  });
}

/** Drop the chains simplifyChains finds redundant (by chain key), leaving unparseable lines alone. */
function simplifyLedger(repoRoot: string, all: GraphChain[]): void {
  const kept = new Set(simplifyChains(all));
  if (kept.size === all.length) return;
  const dropped = new Set(all.filter((c) => !kept.has(c)).map((c) => chainKey(c.premises, c.conclusion)));
  rewriteWithout(repoRoot, "chains.jsonl", isChain, (c) => dropped.has(chainKey(c.premises, c.conclusion)));
}

/** Settle when layout.json is missing or does not cover every node (lost file, failed settle). Never throws. */
export function ensureLayout(repoRoot: string): Promise<void> {
  return withGraphLock(() => {
    try {
      const pos = readPositions(repoRoot);
      const snap = buildSnapshot(readNodeLines(repoRoot), readChainLines(repoRoot), {}, {});
      if (snap.nodes.some((n) => !(n.id in pos))) settleLayout(repoRoot);
    } catch {
      /* rebuildable */
    }
  });
}

/** Layered layout (layeredLayout) of the whole map written to layout.json: bottom-up, courses side by side. Called
 *  after every merge, so a repair that only adds chains moves the nodes it connects. Deterministic.
 *  ponytail: a merge re-lays the whole map (drags are lost); keep untouched courses if that starts to matter. */
export function settleLayout(repoRoot: string): void {
  const snap = buildSnapshot(readNodeLines(repoRoot), readChainLines(repoRoot), {}, {});
  if (snap.nodes.length === 0) return;
  const birth = new Map(snap.propositions.map((n) => [n.id, birthSessionOf(n) ?? ""]));
  const pos = layeredLayout(snap.nodes, snap.logicalEdges, snap.impLinks, (id) => birth.get(id) ?? "");
  writePositionsFile(repoRoot, Object.fromEntries(pos));
}

/** Delete a proposition (cascades to every chain referencing it) or, for an "arg_" id, that one chain. */
export function deleteNode(repoRoot: string, id: string): Promise<{ nodes: number; chains: number }> {
  return withGraphLock(() => {
    const chains = readChainLines(repoRoot);
    let removedChains = 0;
    let removedNodes = 0;
    if (id.startsWith("arg_")) {
      removedChains = chains.filter((c) => c.id === id).length;
      if (removedChains) rewriteWithout(repoRoot, "chains.jsonl", isChain, (c) => c.id === id);
    } else {
      removedNodes = readNodeLines(repoRoot).filter((n) => n.id === id).length;
      if (removedNodes) rewriteWithout(repoRoot, "nodes.jsonl", isNode, (n) => n.id === id);
      const hit = (c: GraphChain) => c.conclusion === id || c.premises.includes(id);
      removedChains = chains.filter(hit).length;
      if (removedChains) rewriteWithout(repoRoot, "chains.jsonl", isChain, hit);
    }
    const pos = readPositions(repoRoot);
    if (id in pos) {
      delete pos[id];
      writePositionsFile(repoRoot, pos);
    }
    return { nodes: removedNodes, chains: removedChains };
  });
}

/** source id (`<course>/<lesson stem>`) -> "Lezione N · titolo" for every lesson on disk. */
export function lessonLabels(repoRoot: string): Record<string, string> {
  const out: Record<string, string> = {};
  try {
    for (const l of scanLessons({ baseDir: repoRoot })) out[`${l.course}/${l.filename.replace(/\.html$/, "")}`] = `Lezione ${l.lessonNumber} · ${l.title}`;
  } catch {
    /* no courses yet */
  }
  return out;
}

export const STATUS_RANK: Record<MapNodeStatus, number> = { taught: 0, solid: 1, shaky: 2, missed: 3 };

export async function provenanceStatus(repoRoot: string): Promise<Map<string, MapNodeStatus>> {
  const map = new Map<string, MapNodeStatus>();
  try {
    const km = await buildKnowledgeMap(repoRoot);
    for (const n of km.nodes) {
      map.set(`${n.course}/${n.lessonPath.split(/[\\/]/).pop()!.replace(/\.html$/, "")}#${n.nodeId}`, n.status);
    }
  } catch {
    /* empty lookup on error */
  }
  return map;
}

export function applyStatus(nodes: GraphNode[], lookup: Map<string, MapNodeStatus>): void {
  for (const node of nodes) {
    let worst: MapNodeStatus | undefined;
    for (const p of node.provenance) {
      if (p.messageId === "") continue;
      const s = lookup.get(`${p.chatId}#${p.messageId}`);
      if (s !== undefined) {
        if (worst === undefined || STATUS_RANK[s] > STATUS_RANK[worst]) {
          worst = s;
        }
      }
    }
    if (worst !== undefined) {
      node.status = worst;
    } else {
      delete node.status;
    }
  }
}

export async function readSnapshot(repoRoot: string): Promise<GraphSnapshot> {
  const nodes = readNodeLines(repoRoot);
  const chains = readChainLines(repoRoot);
  const labels = lessonLabels(repoRoot);
  const used = new Set<string>();
  for (const l of [...nodes, ...chains]) for (const p of l.provenance) used.add(p.chatId);
  const sessions: Record<string, string> = {};
  // a source whose lesson file is gone keeps its id as label (honest, never guessed)
  for (const id of used) sessions[id] = labels[id] ?? id;
  let courseLabels: Record<string, string> = {};
  try {
    courseLabels = Object.fromEntries(readCurriculum({ baseDir: repoRoot }).chapters.map((c) => [c.slug, c.title]));
  } catch {
    /* malformed curriculum: frames show the course dir */
  }
  const snap = buildSnapshot(nodes, chains, sessions, readPositions(repoRoot), courseLabels);
  const lookup = await provenanceStatus(repoRoot);
  applyStatus(snap.propositions, lookup);
  applyStatus(snap.nodes, lookup);
  return snap;
}
