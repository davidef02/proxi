// Local-only bookkeeping of a sync: the trash (files a remote deletion removed here) and the conflict copies (the
// loser of a both-changed file). Nothing here talks to a provider.
import * as fs from "node:fs";
import * as path from "node:path";
import { inSyncSet, TMP } from "./engine.ts";
import type { SyncConflict, SyncTrashEntry } from "./types.ts";

export const TRASH_DAYS = 30;
const DAY_MS = 24 * 60 * 60 * 1000;
const CONFLICT_RE = /^(.+)\.(locale|remoto|drive)-(\d+)$/;
const TRASH_ID_RE = /^(\d+)\/(.+)$/;

/** Is `child` strictly inside `parent` (both resolved)? */
function inside(parent: string, child: string): boolean {
  const p = path.resolve(parent);
  return path.resolve(child).startsWith(p + path.sep);
}

function walkFiles(dir: string, rel = "", out: string[] = []): string[] {
  if (!fs.existsSync(dir)) return out;
  for (const name of fs.readdirSync(path.join(dir, rel))) {
    const r = path.join(rel, name);
    if (fs.statSync(path.join(dir, r)).isDirectory()) walkFiles(dir, r, out);
    else out.push(r.split(path.sep).join("/"));
  }
  return out;
}

/** Removes `dir` and its empty ancestors, stopping at (and never removing) `stop`. */
function pruneUp(dir: string, stop: string): void {
  const s = path.resolve(stop);
  let d = path.resolve(dir);
  while (d !== s && d.startsWith(s + path.sep)) {
    try {
      fs.rmdirSync(d);
    } catch {
      return;
    }
    d = path.dirname(d);
  }
}

async function moveFileSync(from: string, to: string): Promise<void> {
  fs.mkdirSync(path.dirname(to), { recursive: true });
  try {
    fs.renameSync(from, to);
  } catch {
    fs.copyFileSync(from, to);
    fs.unlinkSync(from);
  }
}

/** Deletes trash folders (`<trashDir>/<ms>`) older than TRASH_DAYS. Non-numeric names are left alone. */
export function purgeTrash(trashDir: string, now = Date.now()): void {
  if (!fs.existsSync(trashDir)) return;
  for (const name of fs.readdirSync(trashDir)) {
    if (!/^\d+$/.test(name) || Number(name) >= now - TRASH_DAYS * DAY_MS) continue;
    fs.rmSync(path.join(trashDir, name), { recursive: true, force: true });
  }
}

/** Trash files, newest first. The id is `<ms>/<rel>`: the folder the file was moved to. */
export function listTrash(trashDir: string): SyncTrashEntry[] {
  if (!fs.existsSync(trashDir)) return [];
  const out: SyncTrashEntry[] = [];
  for (const ms of fs.readdirSync(trashDir)) {
    if (!/^\d+$/.test(ms)) continue;
    const base = path.join(trashDir, ms);
    if (!fs.statSync(base).isDirectory()) continue;
    for (const rel of walkFiles(base)) {
      out.push({
        id: `${ms}/${rel}`,
        rel,
        deletedAt: new Date(Number(ms)).toISOString(),
        size: fs.statSync(path.join(base, rel)).size,
      });
    }
  }
  return out.sort((a, b) => b.deletedAt.localeCompare(a.deletedAt) || a.id.localeCompare(b.id));
}

/** Moves a trashed file back into dataRoot. Refuses to overwrite. */
export async function restoreTrash(trashDir: string, dataRoot: string, id: string): Promise<void> {
  const m = TRASH_ID_RE.exec(id);
  if (!m) throw new Error("Identificatore non valido.");
  const rel = m[2];
  if (!inSyncSet(rel)) throw new Error("Identificatore non valido.");
  const src = path.resolve(trashDir, id);
  if (!inside(trashDir, src)) throw new Error("Identificatore non valido.");
  const dst = path.join(dataRoot, rel);
  if (fs.existsSync(dst)) throw new Error(`Esiste già un file con questo nome: ${rel}`);
  await moveFileSync(src, dst);
  pruneUp(path.dirname(src), trashDir);
}

/** Conflict copies, newest first. A copy is named `<rel>.<side>-<ms>`; legacy `drive` is read as `remoto`. */
export function listConflicts(conflictsDir: string): SyncConflict[] {
  const out: SyncConflict[] = [];
  for (const id of walkFiles(conflictsDir)) {
    const m = CONFLICT_RE.exec(id);
    if (!m) continue;
    out.push({
      id,
      rel: m[1],
      side: m[2] === "locale" ? "locale" : "remoto",
      at: new Date(Number(m[3])).toISOString(),
    });
  }
  return out.sort((a, b) => b.at.localeCompare(a.at) || a.id.localeCompare(b.id));
}

/**
 * Settles one conflict copy:
 * - `local` / `remote` keeps the side the user picked. When that side is the copy, the copy is written back into
 *   dataRoot as a fresh local change (pushed by the next sync); otherwise the copy is discarded.
 * - `both` moves the copy to `<dataRoot>/conflitti/<id>`, outside SYNC_ROOTS, so it stays on this PC.
 */
export async function resolveConflict(
  conflictsDir: string,
  dataRoot: string,
  id: string,
  choice: "local" | "remote" | "both",
): Promise<void> {
  if (choice !== "local" && choice !== "remote" && choice !== "both") throw new Error("Scelta non valida.");
  const m = CONFLICT_RE.exec(id);
  const copy = path.resolve(conflictsDir, id);
  if (!m || !inside(conflictsDir, copy)) throw new Error("Conflitto non valido.");
  const rel = m[1];
  if (!inSyncSet(rel)) throw new Error("Conflitto non valido.");
  if (!fs.existsSync(copy)) throw new Error("Conflitto non trovato.");
  const side = m[2] === "locale" ? "locale" : "remoto";

  if (choice === "both") {
    await moveFileSync(copy, path.join(dataRoot, "conflitti", id));
  } else if ((choice === "local" && side === "locale") || (choice === "remote" && side === "remoto")) {
    const dst = path.join(dataRoot, rel);
    fs.mkdirSync(path.dirname(dst), { recursive: true });
    fs.copyFileSync(copy, dst + TMP);
    fs.renameSync(dst + TMP, dst);
    fs.unlinkSync(copy);
  } else {
    fs.unlinkSync(copy);
  }
  pruneUp(path.dirname(copy), conflictsDir);
}
