// One test server for both backends: the in-process Node server or the spawned `proxi` binary, chosen by PROXI_TEST_SERVER.
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { createServer } from "../server/main.ts";

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

/** The built `proxi` executable: `PROXI_BIN`, else the newest of target/release and target/debug. */
export function proxiBinary(root = REPO_ROOT, env: NodeJS.ProcessEnv = process.env): string {
  const exe = process.platform === "win32" ? "proxi.exe" : "proxi";
  if (env.PROXI_BIN) {
    if (fs.existsSync(env.PROXI_BIN)) return env.PROXI_BIN;
    throw new Error(`PROXI_BIN not found: ${env.PROXI_BIN}`);
  }
  const candidates = ["release", "debug"].map((profile) => path.join(root, "target", profile, exe)).filter((p) => fs.existsSync(p));
  if (candidates.length === 0) throw new Error("proxi binary not found under target/: run cargo build --release --locked");
  return candidates.reduce((newest, p) => (fs.statSync(p).mtimeMs > fs.statSync(newest).mtimeMs ? p : newest));
}

export interface StartOptions {
  repoRoot: string;
  /** Default: repoRoot. */
  dataRoot?: string;
  fakes?: FakeUrls;
  /** Default: serverMode(). */
  mode?: ServerMode;
}

/** The narrowed server behind both modes (the binary has no token, manager or jobs). */
export interface TestServer {
  mode: ServerMode;
  url: string;
  port: number;
  close(): Promise<void>;
}

export async function startServer(opts: StartOptions): Promise<TestServer> {
  const mode = opts.mode ?? serverMode();
  const repoRoot = path.resolve(opts.repoRoot);
  const dataRoot = path.resolve(opts.dataRoot ?? opts.repoRoot);
  return mode === "binary" ? startBinary(repoRoot, dataRoot, opts.fakes) : startNode(repoRoot, dataRoot, opts.fakes);
}

async function startNode(repoRoot: string, dataRoot: string, fakes?: FakeUrls): Promise<TestServer> {
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
      checkUpdateOnStart: false,
      fetchModelOnStart: false,
      fetchFn: fakes ? fakeFetch(fakes) : undefined,
    });
    let closing: Promise<void> | undefined;
    return {
      mode: "node",
      url: srv.url,
      port: srv.port,
      close: () => (closing ??= srv.close().finally(restoreAnki)),
    };
  } catch (error) {
    restoreAnki();
    throw error;
  }
}

async function startBinary(repoRoot: string, dataRoot: string, fakes?: FakeUrls): Promise<TestServer> {
  const args = ["serve", "--port", "0", "--repo-root", repoRoot, "--data-root", dataRoot, "--memory-secrets"];
  for (const service of SERVICES) {
    const url = fakes?.[service.key];
    if (url) args.push(service.flag, url);
  }

  // No shell (L12): a quoted path breaks under cmd.exe.
  const child = spawn(proxiBinary(), args, { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
  let stdout = "";
  let stderr = "";
  let exited = false;
  const exitedOnce = new Promise<void>((resolve) => child.once("exit", () => { exited = true; resolve(); }));
  const kill = () => {
    try {
      child.kill();
    } catch {
      /* already gone */
    }
  };
  process.once("exit", kill);

  const started = new Promise<{ url: string; port: number }>((resolve, reject) => {
    let settled = false;
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      fn();
    };
    const timer = setTimeout(() => {
      finish(() => {
        kill();
        reject(new Error(`proxi serve did not print its URL within 30 s: ${stderr}`));
      });
    }, 30_000);
    const reported = () => {
      const m = stdout.match(/^Proxi: (http:\/\/127\.0\.0\.1:(\d+))\s*$/m);
      return m ? { url: m[1], port: Number(m[2]) } : undefined;
    };
    child.stdout!.setEncoding("utf8");
    child.stdout!.on("data", (chunk: string) => {
      stdout += chunk;
      const found = reported();
      if (found) finish(() => resolve(found));
    });
    child.stderr!.setEncoding("utf8");
    child.stderr!.on("data", (chunk: string) => { stderr += chunk; });
    child.once("error", (error) => finish(() => reject(error)));
    child.once("exit", (code) => {
      finish(() => {
        const found = reported();
        if (found) resolve(found);
        else reject(new Error(`proxi serve exited with code ${code}: ${stderr}`));
      });
    });
  });

  let endpoint: { url: string; port: number };
  try {
    endpoint = await started;
  } catch (error) {
    process.removeListener("exit", kill);
    throw error;
  }

  let closing: Promise<void> | undefined;
  return {
    mode: "binary",
    url: endpoint.url,
    port: endpoint.port,
    close: () =>
      (closing ??= (async () => {
        process.removeListener("exit", kill);
        if (exited) return;
        kill();
        await exitedOnce;
      })()),
  };
}
