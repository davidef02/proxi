// One test server for both backends: the in-process Node server or the spawned `proxi` binary, chosen by PROXI_TEST_SERVER.
import path from "node:path";
import fs from "node:fs";
import { createServer } from "../server/main.ts";
import { proxiBinary, spawnProxi } from "./binary.ts";

export { proxiBinary };

export const MODE_ENV = "PROXI_TEST_SERVER";
export type ServerMode = "node" | "binary";

/** The mode selected by `PROXI_TEST_SERVER`: `node` (default) or `binary`. */
export function serverMode(env: NodeJS.ProcessEnv = process.env): ServerMode {
  const raw = env[MODE_ENV];
  if (raw === undefined || raw === "" || raw === "node") return "node";
  if (raw === "binary") return "binary";
  throw new Error(`${MODE_ENV} must be "node" or "binary", got "${raw}"`);
}

export const REPO_ROOT = path.resolve(import.meta.dirname, "../..");

export const FAKE_CLAUDE = path.join(REPO_ROOT, "app", "test", "fake-claude.mjs");

/** Windows: a killed engine child can hold a fixture folder briefly during teardown. */
export async function removeTree(dir: string): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try {
      await fs.promises.rm(dir, { recursive: true, force: true, maxRetries: 1 });
      return;
    } catch (error) {
      if (attempt === 40) throw error;
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
  }
}

/** The fake engine as a claude bin pair: node running fake-claude.mjs, plus test-hook flags before Claude's args. */
export function fakeClaude(extra: string[] = []): { cmd: string; args: string[] } {
  return { cmd: process.execPath, args: [FAKE_CLAUDE, ...extra] };
}

export const FAKE_AGY = path.join(REPO_ROOT, "app", "test", "fake-agy.mjs");
export const FAKE_OPENCODE = path.join(REPO_ROOT, "app", "test", "fake-opencode.mjs");

/** The fake agy as a bin pair: node running fake-agy.mjs, plus test-hook flags before agy's own args. */
export function fakeAgy(extra: string[] = []): { cmd: string; args: string[] } {
  return { cmd: process.execPath, args: [FAKE_AGY, ...extra] };
}

/** The fake opencode as a bin pair: node running fake-opencode.mjs, plus test-hook flags before its own args. */
export function fakeOpencode(extra: string[] = []): { cmd: string; args: string[] } {
  return { cmd: process.execPath, args: [FAKE_OPENCODE, ...extra] };
}

export interface FakeUrls {
  anki?: string;
  github?: string;
  google?: string;
  huggingface?: string;
  transcription?: string;
}

type Service = { key: keyof FakeUrls; flag: string; origins: readonly string[] };

// The one canonical flag↔fake table (L11): the origins decide what fakeFetch rewrites, the flag what the binary gets.
const SERVICES: readonly Service[] = [
  { key: "anki", flag: "--anki-url", origins: [] },
  { key: "github", flag: "--github-url", origins: ["https://api.github.com", "https://github.com"] },
  { key: "google", flag: "--google-url", origins: ["https://accounts.google.com", "https://oauth2.googleapis.com", "https://www.googleapis.com"] },
  { key: "huggingface", flag: "--huggingface-url", origins: ["https://huggingface.co"] },
  { key: "transcription", flag: "--transcription-url", origins: ["https://api.groq.com", "https://api.deepgram.com"] },
];

const LOOPBACK = /^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?$/;

/**
 * `fetch` that rewrites the real service origins to the local fakes of `urls`.
 * Loopback stays untouched; an unknown origin rejects so a test never hits the network.
 */
export function fakeFetch(urls: FakeUrls, base: typeof fetch = fetch): typeof fetch {
  return async (input, init) => {
    const href = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const url = new URL(href);
    if (LOOPBACK.test(url.origin)) return base(input, init);
    const service = SERVICES.find((s) => s.origins.includes(url.origin));
    const fake = service && urls[service.key];
    if (!service || !fake) return Promise.reject(new Error(`fakeFetch: no fake for ${href}`));
    const target = fake.replace(/\/+$/, "") + url.pathname + url.search;
    return base(input instanceof Request ? new Request(target, input) : target, init);
  };
}

export interface StartOptions {
  repoRoot: string;
  /** Default: repoRoot. */
  dataRoot?: string;
  fakes?: FakeUrls;
  /** The engine to run instead of the real Claude (tests: `fakeClaude()`). */
  claude?: { cmd: string; args?: string[] };
  /** The engines to run instead of the real agy and opencode (tests: `fakeAgy()`, `fakeOpencode()`). */
  agy?: { cmd: string; args?: string[] };
  opencode?: { cmd: string; args?: string[] };
  /** Fake executables. Node mode honours `git` only; it has no build step. */
  bins?: { git?: string; cargo?: string };
  /** Default: serverMode(). */
  mode?: ServerMode;
}

/** The narrowed server behind both modes (the binary has no token, manager or jobs). */
export interface TestServer {
  mode: ServerMode;
  url: string;
  port: number;
  /** Node mode only: the MCP secret. */
  token?: string;
  /** Binary mode: resolves when the process exits; apply's relaunch exits it. */
  exited?: Promise<void>;
  close(): Promise<void>;
}

export async function startServer(opts: StartOptions): Promise<TestServer> {
  const mode = opts.mode ?? serverMode();
  const repoRoot = path.resolve(opts.repoRoot);
  const dataRoot = path.resolve(opts.dataRoot ?? opts.repoRoot);
  return mode === "binary"
    ? startBinary(repoRoot, dataRoot, opts.fakes, opts.bins, opts.claude, opts.agy, opts.opencode)
    : startNode(repoRoot, dataRoot, opts.fakes, opts.claude, opts.bins, opts.agy, opts.opencode);
}

/** The `serve` arguments both-mode HTTP tests use: no startup update check, secrets in memory, fakes and bins. */
export function binaryArgs(
  repoRoot: string,
  dataRoot: string,
  fakes?: FakeUrls,
  bins?: StartOptions["bins"],
): string[] {
  const args = ["serve", "--port", "0", "--repo-root", repoRoot, "--data-root", dataRoot, "--memory-secrets", "--no-update-check"];
  for (const service of SERVICES) {
    const url = fakes?.[service.key];
    if (url) args.push(service.flag, url);
  }
  if (bins?.git) args.push("--git-bin", bins.git);
  if (bins?.cargo) args.push("--cargo-bin", bins.cargo);
  return args;
}

async function startNode(
  repoRoot: string,
  dataRoot: string,
  fakes?: FakeUrls,
  claude?: { cmd: string; args?: string[] },
  bins?: StartOptions["bins"],
  agy?: { cmd: string; args?: string[] },
  opencode?: { cmd: string; args?: string[] },
): Promise<TestServer> {
  // The Anki URL is the Node seam the fake server can reach without a new URL option.
  const prevAnki = process.env.LEARNER_ANKI_URL;
  if (fakes?.anki) process.env.LEARNER_ANKI_URL = fakes.anki;
  const restoreAnki = () => {
    if (prevAnki === undefined) delete process.env.LEARNER_ANKI_URL;
    else process.env.LEARNER_ANKI_URL = prevAnki;
  };
  try {
    const srv = await createServer({
      repoRoot,
      dataRoot,
      port: 0,
      openBrowser: false,
      claudeBin: claude?.cmd,
      claudeBinArgs: claude?.args,
      agyBin: agy?.cmd,
      agyBinArgs: agy?.args,
      opencodeBin: opencode?.cmd,
      opencodeBinArgs: opencode?.args,
      checkUpdateOnStart: false,
      fetchModelOnStart: false,
      fetchFn: fakes ? fakeFetch(fakes) : undefined,
      gitBin: bins?.git,
      // Node's applyUpdate exits the process after 500 ms without it: keep the test server alive.
      relaunch: () => {},
    });
    let closing: Promise<void> | undefined;
    return {
      mode: "node",
      url: srv.url,
      port: srv.port,
      token: srv.token,
      close: () => (closing ??= srv.close().finally(restoreAnki)),
    };
  } catch (error) {
    restoreAnki();
    throw error;
  }
}

async function startBinary(
  repoRoot: string,
  dataRoot: string,
  fakes?: FakeUrls,
  bins?: StartOptions["bins"],
  claude?: { cmd: string; args?: string[] },
  agy?: { cmd: string; args?: string[] },
  opencode?: { cmd: string; args?: string[] },
): Promise<TestServer> {
  const args = binaryArgs(repoRoot, dataRoot, fakes, bins);
  if (claude) {
    args.push("--claude-bin", claude.cmd);
    for (const arg of claude.args ?? []) args.push("--claude-bin-arg", arg);
  }
  if (agy) {
    args.push("--agy-bin", agy.cmd);
    for (const arg of agy.args ?? []) args.push("--agy-bin-arg", arg);
  }
  if (opencode) {
    args.push("--opencode-bin", opencode.cmd);
    for (const arg of opencode.args ?? []) args.push("--opencode-bin-arg", arg);
  }

  const p = await spawnProxi(args);
  return { mode: "binary", url: p.url, port: p.port, exited: p.exited, close: p.close };
}
