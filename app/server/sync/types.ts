export type LocalStat = { size: number; mtimeMs: number };
/** Remote view of one file. `token` changes iff the content changes (GitHub blob sha, Drive md5Checksum). */
export type RemoteStat = { size: number; mtimeMs: number; token: string };
export interface SyncProvider {
  /** Manifest key: provider + target ("github:<owner>/<repo>", "gdrive:<folderId>"). */
  readonly key: string;
  /** Every remote file (forward-slash rel paths). Called first in each pass; resets any staged change. */
  list(): Promise<Map<string, RemoteStat>>;
  read(rel: string): Promise<Buffer>;
  /** Uploads or stages one file; returns its new remote stat. */
  write(rel: string, data: Buffer, mtimeMs: number): Promise<RemoteStat>;
  /** Deletes or stages the deletion of one remote file. */
  remove(rel: string): Promise<void>;
  /** Flushes staged changes (GitHub: one commit; Drive: no-op). Throws RemoteMovedError if the remote moved. */
  commit(message: string): Promise<void>;
  /** Optional cheap equality (hash of `data` vs `r.token`); the engine falls back to read() + byte compare. */
  contentEquals?(data: Buffer, r: RemoteStat): boolean;
}
export class RemoteMovedError extends Error {}
export type ManifestEntry = { l: LocalStat; r: RemoteStat };
/** Per file: both sides right after the last sync that touched it. An entry with a side missing = a deletion. */
/** `dataRoot` binds the manifest to the local folder it describes: another folder starts a first-meeting pass. */
export type Manifest = { v: 2; key: string; dataRoot?: string; files: Record<string, ManifestEntry> };
export interface SyncResult {
  pushed: number;
  pulled: number;
  /** Remote removals + local files moved to the trash. */
  deleted: number;
  conflicts: number;
  errors: string[];
  /** Set when the pass stopped before changing anything: deletions > 30% of the manifest entries. */
  pendingDeletions?: { count: number; total: number };
}
export type { SyncConflict, SyncTrashEntry } from "../../shared/protocol.ts";
