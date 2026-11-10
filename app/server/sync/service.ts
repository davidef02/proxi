// Sync service: owns the provider connections (GitHub device flow, Google PKCE), the single-flight sync run, the
// auto-sync cadence and the HTTP routes under /api/sync and /oauth/google/callback.
import { randomBytes } from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";
import type { Hono } from "hono";
import { z } from "zod";
import { dataRootProblem, hasData, writeDataRootPointer } from "../../../extensions/data-root.ts";
import { APP_NAME } from "../../shared/identity.mjs";
import type { SyncStatus } from "../../shared/protocol.ts";
import { HttpError } from "../sessions.ts";
import { subscribeGlobal } from "../events.ts";
import { readSettings, writeSettings } from "../settings.ts";
import { GITHUB_CLIENT_ID, githubProvider, pollDeviceFlow, setupRepo, startDeviceFlow, type DeviceCode, type GithubDeps } from "./github.ts";
import {
  GOOGLE_CLIENT_ID,
  GOOGLE_CLIENT_SECRET,
  GoogleAuth,
  authUrl,
  driveEmail,
  ensureFolder,
  exchangeCode,
  gdriveProvider,
  pkcePair,
  revokeToken,
  type GoogleDeps,
} from "./gdrive.ts";
import { manifestPath, readManifest, syncPass, writeManifest } from "./engine.ts";
import * as store from "./store.ts";
import { canEncrypt, readSecrets, writeSecrets, type SecretCipher, type Secrets } from "./secrets.ts";
import type { SyncProvider } from "./types.ts";

export interface SyncServiceOptions {
  repoRoot: string;
  dataRoot: string;
  dataRootFromEnv: boolean;
  getPort(): number;
  cipher?: SecretCipher;
  fetch?: typeof fetch;
  relaunch?: () => void | Promise<void>;
  onStatus(s: SyncStatus): void;
  onPulled(): void;
  /** Test seam: builds the provider for a connected remote (default: GitHub / Google Drive over `fetch`). */
  makeProvider?(id: "github" | "gdrive", secrets: Secrets): Promise<SyncProvider>;
  sleep?: (ms: number) => Promise<void>;
}

type Pending = NonNullable<SyncStatus["github"]["pending"]>;
type Last = Pick<SyncStatus, "state" | "at" | "pushed" | "pulled" | "deleted" | "conflicts" | "error" | "pendingDeletions">;

const GOOGLE_PENDING_MS = 10 * 60_000;

const page = (status: number, text: string) => ({
  status,
  html:
    `<!doctype html><html lang="it"><head><meta charset="utf-8"><title>${APP_NAME}</title></head><body>` +
    `<h1>${APP_NAME}</h1><p>${text.replace(/[&<>"']/g, (ch) => `&#${ch.charCodeAt(0)};`)}</p></body></html>`,
});

export class SyncService {
  private readonly opts: SyncServiceOptions;
  private readonly conflictsDir: string;
  private readonly trashDir: string;
  private last: Last = { state: "idle" };
  private runP: Promise<SyncStatus> | null = null;
  private restartRequired = false;
  private githubAbort?: AbortController;
  private githubPending?: Pending;
  private githubError?: string;
  private readonly googlePending = new Map<string, { verifier: string; redirectUri: string; expires: number }>();
  private readonly connectErrors: { github?: string; gdrive?: string } = {};

  constructor(opts: SyncServiceOptions) {
    this.opts = opts;
    this.conflictsDir = path.join(opts.repoRoot, ".learner", "drive-conflicts");
    this.trashDir = path.join(opts.repoRoot, ".learner", "trash");
  }

  private get repo() {
    return this.opts.repoRoot;
  }

  private githubDeps(): GithubDeps {
    return { fetch: this.opts.fetch, sleep: this.opts.sleep };
  }

  private googleDeps(): GoogleDeps {
    return { fetch: this.opts.fetch, now: Date.now };
  }

  private async emit(): Promise<SyncStatus> {
    const s = await this.status();
    this.opts.onStatus(s);
    return s;
  }

  /** The whole /api/sync view: settings, connections, the last run, and the counts behind the UI badges. */
  async status(): Promise<SyncStatus> {
    const s = readSettings(this.repo);
    const { secrets, error } = await readSecrets(this.repo, this.opts.cipher);
    const secretsError = (id: "github" | "gdrive") => this.connectErrors[id] ?? (error && s.syncProvider === id ? error : undefined);
    const googlePending = [...this.googlePending.values()].some((p) => p.expires > Date.now());
    return {
      ...this.last,
      provider: s.syncProvider,
      dataRoot: this.opts.dataRoot,
      dataRootFromEnv: this.opts.dataRootFromEnv,
      needsData: !hasData(this.opts.dataRoot),
      encrypted: await canEncrypt(this.opts.cipher),
      github: {
        connected: !!secrets.github,
        login: secrets.github?.login,
        repo: secrets.github?.repo,
        pending: this.githubPending,
        error: this.githubError ?? secretsError("github"),
      },
      gdrive: {
        connected: !!secrets.gdrive,
        email: secrets.gdrive?.email,
        pending: googlePending,
        error: this.connectErrors.gdrive ?? secretsError("gdrive"),
      },
      restartRequired: this.restartRequired || undefined,
      conflictCount: store.listConflicts(this.conflictsDir).length,
      trashCount: store.listTrash(this.trashDir).length,
    };
  }

  /** Runs one pass now. Calls while one runs share it. Never throws: errors land in the status. */
  run(o: { confirmDeletions?: boolean } = {}): Promise<SyncStatus> {
    if (this.runP) return this.runP;
    this.runP = this.doRun(o).finally(() => {
      this.runP = null;
    });
    return this.runP;
  }

  private async doRun(o: { confirmDeletions?: boolean }): Promise<SyncStatus> {
    store.purgeTrash(this.trashDir);
    const id = readSettings(this.repo).syncProvider;
    if (id === "local") {
      this.last = { state: "idle" };
      return this.status();
    }
    const { secrets } = await readSecrets(this.repo, this.opts.cipher);
    if (!secrets[id]) {
      this.last = { state: "error", error: "Non connesso: premi Accedi." };
      return this.emit();
    }
    try {
      const provider = await this.providerFor(id, secrets);
      const manifestFile = manifestPath(this.repo, provider.key);
      const manifest = readManifest(manifestFile, provider.key, this.opts.dataRoot);
      this.last = { state: "syncing" };
      this.opts.onStatus(await this.status());
      const { result, manifest: next } = await syncPass({
        dataRoot: this.opts.dataRoot,
        provider,
        manifest,
        conflictsDir: this.conflictsDir,
        trashDir: this.trashDir,
        confirmDeletions: o.confirmDeletions,
      });
      if (result.pendingDeletions) {
        this.last = { state: "confirm", pendingDeletions: result.pendingDeletions };
      } else {
        writeManifest(manifestFile, next);
        fs.mkdirSync(path.join(this.opts.dataRoot, "courses"), { recursive: true });
        const failed = result.errors.length;
        this.last = {
          state: failed ? "error" : "idle",
          at: new Date().toISOString(),
          pushed: result.pushed,
          pulled: result.pulled,
          deleted: result.deleted,
          conflicts: result.conflicts,
          error: failed ? `${failed} file non sincronizzati (riprovo alla prossima): ${result.errors[0]}` : undefined,
        };
        if (result.pulled > 0 || result.deleted > 0) this.opts.onPulled();
      }
    } catch (e) {
      this.last = { state: "error", at: new Date().toISOString(), error: e instanceof Error ? e.message : String(e) };
    }
    return this.emit();
  }

  private async providerFor(id: "github" | "gdrive", secrets: Secrets): Promise<SyncProvider> {
    if (this.opts.makeProvider) return this.opts.makeProvider(id, secrets);
    if (id === "github") {
      const g = secrets.github!;
      return githubProvider(this.githubDeps(), { token: g.token, repo: g.repo, branch: g.branch });
    }
    const d = secrets.gdrive!;
    const s = readSettings(this.repo);
    const deps = this.googleDeps();
    const auth = new GoogleAuth(deps, {
      clientId: s.googleClientId || GOOGLE_CLIENT_ID,
      clientSecret: s.googleClientSecret || GOOGLE_CLIENT_SECRET,
      refreshToken: d.refreshToken,
    });
    return gdriveProvider(deps, auth, d.folderId);
  }

  /** Auto sync: at start, and shortly after the courses or the map change, when a remote is connected. The returned
   *  stop function unsubscribes and runs a last pass (bounded) so the session's work leaves the machine. */
  startAuto(): () => Promise<void> {
    let timer: NodeJS.Timeout | undefined;
    const enabled = async (): Promise<boolean> => {
      const p = readSettings(this.repo).syncProvider;
      if (p === "local") return false;
      return !!(await readSecrets(this.repo, this.opts.cipher)).secrets[p];
    };
    const kick = (delay: number) => {
      clearTimeout(timer);
      timer = setTimeout(() => void enabled().then((ok) => { if (ok) void this.run(); }), delay);
      timer.unref();
    };
    kick(1000);
    const off = subscribeGlobal((e) => {
      if (e.type === "courses" || e.type === "graph") kick(5000);
    });
    return async () => {
      clearTimeout(timer);
      off();
      if (await enabled()) await Promise.race([this.run(), new Promise((r) => setTimeout(r, 15_000).unref())]);
    };
  }

  // ───────────── GitHub: device flow ─────────────

  async githubStart(): Promise<SyncStatus> {
    this.githubAbort?.abort();
    const ac = new AbortController();
    this.githubAbort = ac;
    this.githubError = undefined;
    this.githubPending = undefined;
    const clientId = readSettings(this.repo).githubClientId || GITHUB_CLIENT_ID;
    let code: DeviceCode;
    try {
      code = await startDeviceFlow(this.githubDeps(), clientId);
    } catch (e) {
      this.githubError = e instanceof Error ? e.message : String(e);
      return this.emit();
    }
    this.githubPending = {
      userCode: code.userCode,
      verificationUri: code.verificationUri,
      expiresAt: new Date(Date.now() + code.expiresIn * 1000).toISOString(),
    };
    void this.finishGithub(code, clientId, ac);
    return this.emit();
  }

  private async finishGithub(code: DeviceCode, clientId: string, ac: AbortController): Promise<void> {
    let ok = false;
    try {
      const token = await pollDeviceFlow(this.githubDeps(), clientId, code, ac.signal);
      const conn = await setupRepo(this.githubDeps(), token);
      // A file this process cannot decrypt holds the other provider's token: writing over it would lose that token.
      const { secrets, error: secretsError, locked } = await readSecrets(this.repo, this.opts.cipher);
      if (locked) throw new Error(secretsError);
      await writeSecrets(this.repo, { ...secrets, github: { token, login: conn.login, repo: conn.repo, branch: conn.branch } }, this.opts.cipher);
      writeSettings(this.repo, { syncProvider: "github" });
      ok = true;
    } catch (e) {
      if (!ac.signal.aborted) this.githubError = e instanceof Error ? e.message : String(e);
    }
    if (ac.signal.aborted) return; // cancelled or replaced: githubCancel / githubStart already updated the state
    this.githubPending = undefined;
    if (ok) void this.run();
    await this.emit();
  }

  async githubCancel(): Promise<SyncStatus> {
    this.githubAbort?.abort();
    this.githubAbort = undefined;
    this.githubPending = undefined;
    return this.emit();
  }

  // ───────────── Google Drive: PKCE over loopback ─────────────

  async googleStart(): Promise<{ url: string }> {
    const s = readSettings(this.repo);
    const redirectUri = `http://127.0.0.1:${this.opts.getPort()}/oauth/google/callback`;
    const state = randomBytes(16).toString("hex");
    const { verifier, challenge } = pkcePair();
    let url: string;
    try {
      url = authUrl({ clientId: s.googleClientId || GOOGLE_CLIENT_ID, redirectUri, challenge, state });
    } catch (e) {
      throw new HttpError(400, e instanceof Error ? e.message : String(e));
    }
    const now = Date.now();
    for (const [k, v] of this.googlePending) if (v.expires < now) this.googlePending.delete(k);
    this.googlePending.set(state, { verifier, redirectUri, expires: now + GOOGLE_PENDING_MS });
    this.connectErrors.gdrive = undefined;
    void this.emit();
    return { url };
  }

  async googleCallback(q: { code?: string; state?: string; error?: string }): Promise<{ status: number; html: string }> {
    const entry = q.state ? this.googlePending.get(q.state) : undefined;
    if (q.state) this.googlePending.delete(q.state); // one-time
    if (!entry || entry.expires < Date.now()) return page(400, `Richiesta di accesso non valida o scaduta: riprova da ${APP_NAME}.`);
    if (q.error || !q.code) return page(400, "Accesso a Google annullato.");
    try {
      const s = readSettings(this.repo);
      const clientId = s.googleClientId || GOOGLE_CLIENT_ID;
      const clientSecret = s.googleClientSecret || GOOGLE_CLIENT_SECRET;
      const deps = this.googleDeps();
      const tokens = await exchangeCode(deps, { clientId, clientSecret, code: q.code, verifier: entry.verifier, redirectUri: entry.redirectUri });
      const auth = new GoogleAuth(deps, { clientId, clientSecret, refreshToken: tokens.refreshToken });
      const folderId = await ensureFolder(deps, auth);
      const email = await driveEmail(deps, auth);
      // A file this process cannot decrypt holds the other provider's token: writing over it would lose that token.
      const { secrets, error: secretsError, locked } = await readSecrets(this.repo, this.opts.cipher);
      if (locked) throw new Error(secretsError);
      await writeSecrets(this.repo, { ...secrets, gdrive: { refreshToken: tokens.refreshToken, folderId, email } }, this.opts.cipher);
      writeSettings(this.repo, { syncProvider: "gdrive" });
      this.connectErrors.gdrive = undefined;
      void this.run();
      await this.emit();
      return page(200, `Accesso a Google completato. Puoi chiudere questa scheda e tornare a ${APP_NAME}.`);
    } catch (e) {
      this.connectErrors.gdrive = e instanceof Error ? e.message : String(e);
      await this.emit();
      return page(400, `Accesso non riuscito: ${this.connectErrors.gdrive}`);
    }
  }

  async disconnect(provider: "github" | "gdrive"): Promise<SyncStatus> {
    const { secrets } = await readSecrets(this.repo, this.opts.cipher);
    if (provider === "gdrive" && secrets.gdrive) await revokeToken(this.googleDeps(), secrets.gdrive.refreshToken);
    const next: Secrets = { ...secrets };
    delete next[provider];
    await writeSecrets(this.repo, next, this.opts.cipher);
    if (provider === "github") {
      this.githubAbort?.abort();
      this.githubAbort = undefined;
      this.githubPending = undefined;
      this.githubError = undefined;
    } else {
      this.connectErrors.gdrive = undefined;
    }
    if (readSettings(this.repo).syncProvider === provider) writeSettings(this.repo, { syncProvider: "local" });
    return this.emit();
  }

  // ───────────── data folder ─────────────

  /** The data folder is not hot-swapped: the pointer is written and the server restarts (when `relaunch` exists). */
  async setDataRoot(dir: string): Promise<SyncStatus> {
    if (this.opts.dataRootFromEnv) throw new HttpError(409, "La cartella dei dati è impostata da LEARNER_DATA_ROOT.");
    const problem = dataRootProblem(this.repo, dir);
    if (problem) throw new HttpError(400, problem);
    const target = path.resolve(dir);
    fs.mkdirSync(path.join(target, "courses"), { recursive: true });
    if (target === path.resolve(this.opts.dataRoot)) return this.emit();
    writeDataRootPointer(this.repo, target);
    this.restartRequired = true;
    const s = await this.emit();
    if (this.opts.relaunch) setTimeout(() => void this.opts.relaunch!(), 500);
    return s;
  }

  // ───────────── conflicts and trash ─────────────

  conflicts() {
    return store.listConflicts(this.conflictsDir);
  }

  async resolveConflict(id: string, choice: "local" | "remote" | "both"): Promise<{ ok: true }> {
    try {
      await store.resolveConflict(this.conflictsDir, this.opts.dataRoot, id, choice);
    } catch (e) {
      throw new HttpError(400, e instanceof Error ? e.message : String(e));
    }
    this.opts.onPulled();
    await this.emit();
    return { ok: true };
  }

  trash() {
    return store.listTrash(this.trashDir);
  }

  async restore(id: string): Promise<{ ok: true }> {
    try {
      await store.restoreTrash(this.trashDir, this.opts.dataRoot, id);
    } catch (e) {
      throw new HttpError(400, e instanceof Error ? e.message : String(e));
    }
    this.opts.onPulled();
    await this.emit();
    return { ok: true };
  }
}

/** /api/sync routes. Registered before the SPA fallback. */
export function mountSync(app: Hono, svc: SyncService): void {
  const body = (c: { req: { json(): Promise<unknown> } }) => c.req.json().catch(() => ({}));
  app.get("/api/sync", async (c) => c.json(await svc.status()));
  app.post("/api/sync/run", async (c) => {
    const { confirmDeletions } = z.object({ confirmDeletions: z.boolean().optional() }).parse(await body(c));
    return c.json(await svc.run({ confirmDeletions }));
  });
  app.post("/api/sync/github/start", async (c) => c.json(await svc.githubStart()));
  app.post("/api/sync/github/cancel", async (c) => c.json(await svc.githubCancel()));
  app.post("/api/sync/google/start", async (c) => c.json(await svc.googleStart()));
  app.get("/oauth/google/callback", async (c) => {
    const r = await svc.googleCallback({ code: c.req.query("code"), state: c.req.query("state"), error: c.req.query("error") });
    return c.html(r.html, r.status === 200 ? 200 : 400);
  });
  app.post("/api/sync/disconnect", async (c) => {
    const { provider } = z.object({ provider: z.enum(["github", "gdrive"]) }).parse(await body(c));
    return c.json(await svc.disconnect(provider));
  });
  app.post("/api/sync/data-root", async (c) => {
    const { dataRoot } = z.object({ dataRoot: z.string().min(1).max(500) }).parse(await body(c));
    return c.json(await svc.setDataRoot(dataRoot));
  });
  app.get("/api/sync/conflicts", (c) => c.json(svc.conflicts()));
  app.post("/api/sync/conflicts/resolve", async (c) => {
    const { id, choice } = z.object({ id: z.string().min(1).max(1000), choice: z.enum(["local", "remote", "both"]) }).parse(await body(c));
    return c.json(await svc.resolveConflict(id, choice));
  });
  app.get("/api/sync/trash", (c) => c.json(svc.trash()));
  app.post("/api/sync/trash/restore", async (c) => {
    const { id } = z.object({ id: z.string().min(1).max(1000) }).parse(await body(c));
    return c.json(await svc.restore(id));
  });
}
