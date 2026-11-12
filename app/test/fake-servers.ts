/**
 * Local fake HTTP servers for the external services, shared by the Node and binary test modes.
 */
import * as http from "node:http";
import { MockAnki } from "../../tests/mock-anki.ts";

export interface RecordedRequest {
  method: string;
  /** Pathname plus query string. */
  path: string;
  headers: http.IncomingHttpHeaders;
  body: string;
}

export interface FakeReply {
  /** Default 200. */
  status?: number;
  headers?: Record<string, string>;
  /** A string is sent as-is; anything else is JSON.stringify'd with content-type application/json. */
  body?: unknown;
}

export type FakeHandler = (req: RecordedRequest) => FakeReply | Promise<FakeReply>;

type Route = FakeReply | FakeHandler;

const key = (method: string, pathname: string) => `${method.toUpperCase()} ${pathname}`;

/** A route table on a loopback port: answers HTTP requests and records them for assertions. */
export class FakeHttpServer {
  url = "";
  requests: RecordedRequest[] = [];

  private routes = new Map<string, Route>();
  private server: http.Server | null = null;
  private closing: Promise<void> | null = null;

  constructor(readonly name: string) {}

  /** Latest `on` for the same method + pathname wins. Unknown routes answer 404. */
  on(method: string, pathname: string, reply: Route): void {
    this.routes.set(key(method, pathname), reply);
  }

  reset(): void {
    this.requests = [];
    this.routes.clear();
  }

  async start(): Promise<string> {
    const server = http.createServer((req, res) => void this.handle(req, res));
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => {
        server.off("error", reject);
        resolve();
      });
    });
    const addr = server.address();
    if (!addr || typeof addr === "string") {
      server.close();
      throw new Error(`fake ${this.name}: listen gave no port`);
    }
    this.server = server;
    this.url = `http://127.0.0.1:${addr.port}`;
    return this.url;
  }

  async close(): Promise<void> {
    if (this.closing) return this.closing;
    const server = this.server;
    this.server = null;
    if (!server) return;
    server.closeAllConnections();
    this.closing = new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
    await this.closing;
  }

  private async handle(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk as Buffer);
    const target = new URL(req.url ?? "/", "http://127.0.0.1");
    const recorded: RecordedRequest = {
      method: req.method ?? "GET",
      path: target.pathname + target.search,
      headers: req.headers,
      body: Buffer.concat(chunks).toString("utf8"),
    };
    this.requests.push(recorded);

    const route = this.routes.get(key(recorded.method, target.pathname));
    let reply: FakeReply;
    try {
      if (route === undefined) reply = { status: 404, body: { error: `fake ${this.name}: no route for ${recorded.method} ${target.pathname}` } };
      else reply = typeof route === "function" ? await route(recorded) : route;
    } catch (err) {
      reply = { status: 500, body: { error: String(err) } };
    }

    const headers = { ...reply.headers };
    let payload: string | undefined;
    if (typeof reply.body === "string") payload = reply.body;
    else if (reply.body !== undefined) {
      payload = JSON.stringify(reply.body);
      headers["content-type"] ??= "application/json";
    }
    res.writeHead(reply.status ?? 200, headers);
    res.end(payload);
  }
}

export interface FakeServers {
  anki: MockAnki;
  github: FakeHttpServer;
  google: FakeHttpServer;
  huggingface: FakeHttpServer;
  transcription: FakeHttpServer;
  urls: { anki: string; github: string; google: string; huggingface: string; transcription: string };
  reset(): void;
  close(): Promise<void>;
}

/** Starts all five fakes on free loopback ports. */
export async function startFakeServers(): Promise<FakeServers> {
  const anki = new MockAnki();
  const github = new FakeHttpServer("github");
  const google = new FakeHttpServer("google");
  const huggingface = new FakeHttpServer("huggingface");
  const transcription = new FakeHttpServer("transcription");
  const httpFakes = [github, google, huggingface, transcription];
  let ankiUrl: string;
  let githubUrl: string;
  let googleUrl: string;
  let huggingfaceUrl: string;
  let transcriptionUrl: string;
  try {
    [ankiUrl, githubUrl, googleUrl, huggingfaceUrl, transcriptionUrl] = await Promise.all([anki.start(), ...httpFakes.map((f) => f.start())]);
  } catch (err) {
    await Promise.all([anki.stop(), ...httpFakes.map((f) => f.close())]);
    throw err;
  }
  return {
    anki,
    github,
    google,
    huggingface,
    transcription,
    urls: { anki: ankiUrl, github: githubUrl, google: googleUrl, huggingface: huggingfaceUrl, transcription: transcriptionUrl },
    reset() {
      anki.reset();
      for (const f of httpFakes) f.reset();
    },
    async close() {
      await Promise.all([anki.stop(), ...httpFakes.map((f) => f.close())]);
    },
  };
}
