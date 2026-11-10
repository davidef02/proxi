// Two-way sync between the data folder (dataRoot) and one SyncProvider, against a manifest of the last sync.
// Only files that changed since then move. Change detection compares each side with the manifest, never the two
// sides' mtimes: git checkouts stamp files with "now", so a stale clone must not overwrite newer work. Deletions
// propagate through the manifest: an entry whose side is missing is a deletion, and the other side's copy goes to
// the trash. When both sides changed, the newer one wins and the loser is kept in the conflicts folder.
import * as fs from "node:fs";
import * as fsp from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import {
  RemoteMovedError,
  type LocalStat,
  type Manifest,
  type RemoteStat,
  type SyncProvider,
  type SyncResult,
} from "./types.ts";

/** What is synced: the ordinary Proxi data (never .learner/, node_modules, builds). */
export const SYNC_ROOTS = ["courses", "graph", "decks", "curriculum.json"];
export const SKIP_DIRS = new Set(["node_modules", "dist", ".vite", "coverage", ".git"]);
/** Retired files: deletions do not propagate, so without this a provider would hand them back forever. */
export const SKIP_FILES = new Set(["graph/positions.json"]);
export const TMP = ".ia-sync-tmp";
/** Above this share of the manifest, a pass that deletes stops and asks for confirmation. */
const DELETE_GUARD = 0.3;

/** Guards remote names before they reach the disk. */
export function inSyncSet(rel: string): boolean {
  if (!rel || rel.includes("\\") || rel.includes(":")) return false;
  const segs = rel.split("/");
  if (segs.some((s) => s === "" || s === "." || s === "..")) return false;
  if (segs[0] === "curriculum.json") return segs.length === 1;
  if (segs.length === 1 || !SYNC_ROOTS.includes(segs[0])) return false;
  if (segs.some((s) => SKIP_DIRS.has(s))) return false;
  if (SKIP_FILES.has(rel)) return false;
  return !rel.endsWith(TMP);
}

async function walkLocal(root: string, rel: string, out: Map<string, LocalStat>): Promise<void> {
  let st: fs.Stats;
  try {
    st = await fsp.stat(path.join(root, rel));
  } catch {
    return;
  }
  if (st.isFile()) {
    const key = rel.split(path.sep).join("/");
    if (inSyncSet(key)) out.set(key, { size: st.size, mtimeMs: st.mtimeMs });
    return;
  }
  if (!st.isDirectory()) return;
  for (const name of await fsp.readdir(path.join(root, rel))) {
    if (!SKIP_DIRS.has(name)) await walkLocal(root, path.join(rel, name), out);
  }
}

/** Every syncable local file under dataRoot, keyed by forward-slash rel path. */
export async function listLocal(dataRoot: string): Promise<Map<string, LocalStat>> {
  const out = new Map<string, LocalStat>();
  for (const r of SYNC_ROOTS) await walkLocal(dataRoot, r, out);
  return out;
}

/** Manifest file for one provider target, under the code repo's .learner/ (never synced). */
export function manifestPath(repoRoot: string, key: string): string {
  return path.join(repoRoot, ".learner", "sync-manifests", `${key.replace(/[^A-Za-z0-9._-]/g, "_")}.json`);
}

/** Missing, invalid, for another provider/version/data folder: an empty manifest (a first sync). */
export function readManifest(file: string, key: string, dataRoot: string): Manifest {
  const root = path.resolve(dataRoot);
  try {
    const m = JSON.parse(fs.readFileSync(file, "utf-8")) as Manifest;
    if (m && m.v === 2 && m.key === key && m.dataRoot === root && m.files && typeof m.files === "object") return m;
  } catch {
    /* none yet */
  }
  return { v: 2, key, dataRoot: root, files: {} };
}

/** Atomic write (tmp + rename), falling back to a direct write. */
export function writeManifest(file: string, m: Manifest): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const tmp = `${file}.${process.pid}.tmp`;
  try {
    fs.writeFileSync(tmp, JSON.stringify(m));
    fs.renameSync(tmp, file);
  } catch {
    fs.writeFileSync(file, JSON.stringify(m));
  }
}

export interface SyncPassOptions {
  dataRoot: string;
  provider: SyncProvider;
  manifest: Manifest;
  conflictsDir: string;
  trashDir: string;
  confirmDeletions?: boolean;
  now?: () => number;
  message?: string;
}

type Action = "push" | "pull" | "drop" | "removeRemote" | "trashLocal" | "both";

async function moveFile(from: string, to: string): Promise<void> {
  await fsp.mkdir(path.dirname(to), { recursive: true });
  try {
    await fsp.rename(from, to);
  } catch {
    await fsp.copyFile(from, to);
    await fsp.unlink(from);
  }
}

/** Removes empty directories above `rel`, stopping before the top-level root (courses/, graph/, ...). */
function pruneEmptyDirs(dataRoot: string, rel: string): void {
  const segs = rel.split("/");
  for (let i = segs.length - 2; i >= 1; i--) {
    try {
      fs.rmdirSync(path.join(dataRoot, ...segs.slice(0, i + 1)));
    } catch {
      return; // not empty (or gone): stop here
    }
  }
}

/** Downloads one remote file into dataRoot, keeping the remote mtime, and records both sides in the manifest. */
async function pull(opts: SyncPassOptions, m: Manifest, rel: string, rb: RemoteStat): Promise<void> {
  const data = await opts.provider.read(rel);
  const L = path.join(opts.dataRoot, rel);
  await fsp.mkdir(path.dirname(L), { recursive: true });
  await fsp.writeFile(L + TMP, data);
  await fsp.rename(L + TMP, L);
  const sec = rb.mtimeMs / 1000;
  await fsp.utimes(L, sec, sec);
  const st = await fsp.stat(L);
  m.files[rel] = { l: { size: st.size, mtimeMs: st.mtimeMs }, r: rb };
}

async function writeConflict(file: string, data: Buffer): Promise<void> {
  await fsp.mkdir(path.dirname(file), { recursive: true });
  await fsp.writeFile(file, data);
}

async function copyTo(from: string, to: string): Promise<void> {
  await fsp.mkdir(path.dirname(to), { recursive: true });
  await fsp.copyFile(from, to);
}

/**
 * One pass against a fresh copy of the manifest. Mutates `res` as it goes so a retry keeps the counts.
 * `copied` maps rel → remote token of the remote bytes already saved as a `.remoto` copy by an earlier attempt.
 * `side` counts the local trashes of this attempt: they are already on disk, so a retry cannot see them again.
 */
async function attemptPass(
  opts: SyncPassOptions,
  res: SyncResult,
  copied: Map<string, string>,
  side: { trashed: number },
): Promise<{ result: SyncResult; manifest: Manifest }> {
  const now = opts.now ?? Date.now;
  const m = structuredClone(opts.manifest);
  const remote = new Map<string, RemoteStat>();
  for (const [rel, stat] of await opts.provider.list()) if (inSyncSet(rel)) remote.set(rel, stat);
  const local = await listLocal(opts.dataRoot);

  // Plan.
  const actions: [string, Action][] = [];
  for (const rel of new Set([...local.keys(), ...remote.keys(), ...Object.keys(m.files)])) {
    const la = local.get(rel);
    const rb = remote.get(rel);
    const e = m.files[rel];
    const lCh = !!la && !(e && la.size === e.l.size && la.mtimeMs === e.l.mtimeMs);
    const rCh = !!rb && !(e && rb.token === e.r.token);
    let a: Action | undefined;
    if (!la && !rb) a = e ? "drop" : undefined;
    else if (!la) a = e && !rCh ? "removeRemote" : "pull";
    else if (!rb) a = e && !lCh ? "trashLocal" : "push";
    else if (lCh && rCh) a = "both";
    else if (lCh) a = "push";
    else if (rCh) a = "pull";
    if (a) actions.push([rel, a]);
  }

  // Guard: a pass that would delete a large share of the known files waits for confirmation.
  const deletions = actions.filter(([, a]) => a === "removeRemote" || a === "trashLocal").length;
  const total = Object.keys(m.files).length;
  if (deletions > 0 && deletions > DELETE_GUARD * total && !opts.confirmDeletions) {
    return {
      result: { pushed: 0, pulled: 0, deleted: 0, conflicts: 0, errors: [], pendingDeletions: { count: deletions, total } },
      manifest: opts.manifest,
    };
  }

  // Apply, one file at a time: a failure is recorded and the pass goes on.
  let removed = 0;
  for (const [rel, a] of actions) {
    const la = local.get(rel);
    const rb = remote.get(rel);
    const L = path.join(opts.dataRoot, rel);
    try {
      switch (a) {
        case "push": {
          const data = await fsp.readFile(L);
          m.files[rel] = { l: la!, r: await opts.provider.write(rel, data, la!.mtimeMs) };
          res.pushed++;
          break;
        }
        case "pull":
          await pull(opts, m, rel, rb!);
          res.pulled++;
          break;
        case "drop":
          delete m.files[rel];
          break;
        case "removeRemote":
          await opts.provider.remove(rel);
          delete m.files[rel];
          removed++;
          res.deleted++;
          break;
        case "trashLocal":
          await moveFile(L, path.join(opts.trashDir, String(now()), rel));
          pruneEmptyDirs(opts.dataRoot, rel);
          delete m.files[rel];
          res.deleted++;
          side.trashed++;
          break;
        case "both": {
          const here = await fsp.readFile(L);
          const equal = opts.provider.contentEquals?.(here, rb!) ?? here.equals(await opts.provider.read(rel));
          if (equal) {
            m.files[rel] = { l: la!, r: rb! };
            break;
          }
          // Remote wins when the head commit is newer than the local edit (GitHub) or on a first meeting. The loser
          // is always kept in conflictsDir, so neither side loses bytes.
          const remoteWins = !m.files[rel] || rb!.mtimeMs >= la!.mtimeMs;
          const stamp = now();
          if (remoteWins) {
            await copyTo(L, path.join(opts.conflictsDir, `${rel}.locale-${stamp}`));
            await pull(opts, m, rel, rb!);
            res.pulled++;
            res.conflicts++;
          } else {
            // A retry re-plans the same conflict: its copy from the earlier attempt already exists, so count it once.
            if (copied.get(rel) !== rb!.token) {
              await writeConflict(path.join(opts.conflictsDir, `${rel}.remoto-${stamp}`), await opts.provider.read(rel));
              copied.set(rel, rb!.token);
              res.conflicts++;
            }
            m.files[rel] = { l: la!, r: await opts.provider.write(rel, here, la!.mtimeMs) };
            res.pushed++;
          }
          break;
        }
      }
    } catch (e) {
      // A moved remote aborts the pass so the caller retries it from a fresh listing.
      if (e instanceof RemoteMovedError) throw e;
      res.errors.push(`${rel}: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  // Commit: one remote commit for every staged write and removal.
  if (res.pushed + removed > 0) {
    await opts.provider.commit(opts.message ?? `sync: ${res.pushed} sent, ${removed} deleted (${os.hostname()})`);
  }
  return { result: res, manifest: m };
}

/** Runs one two-way pass. Retries once from a fresh manifest when the remote moved during the commit. */
export async function syncPass(opts: SyncPassOptions): Promise<{ result: SyncResult; manifest: Manifest }> {
  // Local side effects of a failed attempt are already on disk (pulled files, trashed files) and the retry re-plans
  // them as no-ops, so their counts are carried over. Remote removals were only staged: the retry counts them again.
  const carry = { conflicts: 0, errors: [] as string[], pulled: 0, trashed: 0 };
  const copied = new Map<string, string>();
  for (const attempt of [1, 2]) {
    const res: SyncResult = {
      pushed: 0,
      pulled: carry.pulled,
      deleted: carry.trashed,
      conflicts: carry.conflicts,
      errors: [...carry.errors],
    };
    const side = { trashed: 0 };
    try {
      return await attemptPass(opts, res, copied, side);
    } catch (e) {
      if (!(e instanceof RemoteMovedError)) throw e;
      if (attempt === 2) throw new Error("Il remoto è cambiato durante la sincronizzazione: riprovo alla prossima.");
      // Conflict copies from attempt 1 stay on disk, so their counts and errors must survive the retry.
      carry.conflicts = res.conflicts;
      carry.errors = res.errors;
      carry.pulled = res.pulled;
      carry.trashed += side.trashed;
    }
  }
  throw new Error("unreachable");
}
