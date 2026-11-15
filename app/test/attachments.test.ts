// Composer attachments over HTTP only (no server internals), so it runs against the Node server and the proxi binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import type { AttachmentMeta, ChatItem, SessionState } from "../shared/protocol.ts";
import { fakeClaude, startServer, type TestServer } from "./harness.ts";

process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9"; // closed port: never reach the real Anki

const SEED = {
  chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }],
};

function tempDir(prefix: string): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

/** A data folder with one course and one planned lesson. */
function seedData(): string {
  const data = tempDir("learner-attach-data-");
  fs.writeFileSync(path.join(data, "curriculum.json"), JSON.stringify(SEED));
  fs.mkdirSync(path.join(data, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(data, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(data, "courses", "demo", "NOTES.md"), "# Notes\n");
  return data;
}

async function waitFor<T>(fn: () => T | undefined | false | Promise<T | undefined | false>, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** A file part (a field carrying a filename): the only kind the server counts. */
function addFile(fd: FormData, name: string, type: string, bytes: Buffer, field = "files"): void {
  fd.append(field, new Blob([new Uint8Array(bytes)], { type }), name);
}

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

describe("Composer attachments (Images and PDFs)", () => {
  let repo: string;
  let data: string;
  let server: TestServer;
  let inputFile: string;
  let id: string;
  let pngMeta: AttachmentMeta | undefined;

  const start = () => startServer({ repoRoot: repo, dataRoot: data, claude: fakeClaude(["--input-out", inputFile]) });
  const post = (p: string, body: unknown = {}) =>
    fetch(server.url + p, { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" }, body: JSON.stringify(body) });
  const upload = (sid: string, fd: FormData) =>
    fetch(`${server.url}/api/sessions/${sid}/attachments`, { method: "POST", headers: { "x-learner-client": "1" }, body: fd });
  const getState = async (sid: string): Promise<SessionState> => {
    const r = await fetch(`${server.url}/api/sessions/${sid}`);
    assert.equal(r.status, 200);
    return (await r.json()) as SessionState;
  };
  const idle = (sid: string) => waitFor(async () => (await getState(sid)).status === "idle");
  /** Every raw stdin line the fake engine saw. */
  const rawInputs = (): string[] => {
    try {
      return fs.readFileSync(inputFile, "utf-8").split("\n").filter(Boolean);
    } catch {
      return [];
    }
  };

  before(async () => {
    repo = tempDir("learner-attach-repo-");
    data = seedData();
    inputFile = path.join(repo, "input.jsonl");
    server = await start();
    const created = await post("/api/sessions", {
      kind: "teach",
      course: "demo",
      model: "haiku",
      target: { mode: "planned", title: "Prima lezione" },
      ankiOptIn: false,
    });
    assert.equal(created.status, 200);
    id = ((await created.json()) as { id: string }).id;
    await idle(id);
  });

  after(async () => {
    await server.close();
    fs.rmSync(repo, { recursive: true, force: true, maxRetries: 3 });
    fs.rmSync(data, { recursive: true, force: true, maxRetries: 3 });
  });

  it("uploads an image and a PDF with Node's metadata and on-disk layout", async () => {
    const pngBytes = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00]);
    const pdfBytes = Buffer.from("%PDF-1.4 sample content\n%%EOF");

    const fd = new FormData();
    fd.append("files", new Blob([pngBytes], { type: "image/png" }), "diagram.png");
    fd.append("files", new Blob([pdfBytes], { type: "application/pdf" }), "notes.pdf");
    const res = await upload(id, fd);
    assert.equal(res.status, 200);
    const metas = (await res.json()) as AttachmentMeta[];
    assert.equal(metas.length, 2);

    const [png, pdf] = metas;
    pngMeta = png;
    assert.deepEqual(Object.keys(png), ["id", "name", "path", "mimeType", "size"]);
    assert.match(png.id, UUID_V4);
    assert.equal(png.name, "diagram.png");
    assert.equal(png.mimeType, "image/png");
    assert.equal(png.size, pngBytes.length);
    assert.ok(png.path.startsWith(`.learner/sessions/${id}/attachments/`));
    assert.ok(png.path.endsWith("_diagram.png"));
    assert.ok(path.posix.basename(png.path).startsWith(png.id.slice(0, 8) + "_"), "stored name keeps the id prefix");

    assert.equal(pdf.name, "notes.pdf");
    assert.equal(pdf.mimeType, "application/pdf");
    assert.equal(pdf.size, pdfBytes.length);
    assert.ok(pdf.path.startsWith(`.learner/sessions/${id}/attachments/`));
    assert.ok(pdf.path.endsWith("_notes.pdf"));

    assert.deepEqual(fs.readFileSync(path.join(repo, png.path)), pngBytes);
    assert.deepEqual(fs.readFileSync(path.join(repo, pdf.path)), pdfBytes);
  });

  it("cuts a traversal filename down to its basename", async () => {
    const fd = new FormData();
    addFile(fd, "../../../etc/passwd.png", "image/png", Buffer.from("test content"));

    const res = await upload(id, fd);
    assert.equal(res.status, 200);
    const metas = (await res.json()) as AttachmentMeta[];
    assert.equal(metas.length, 1);
    assert.equal(metas[0].name, "passwd.png");
    assert.ok(!metas[0].path.includes(".."));
    assert.ok(fs.existsSync(path.join(repo, metas[0].path)));
  });

  it("replaces spaces and accents with underscores, keeping the extension's case", async () => {
    const fd = new FormData();
    addFile(fd, "mia foto è.PNG", "image/png", Buffer.from("foto"));

    const res = await upload(id, fd);
    assert.equal(res.status, 200);
    const metas = (await res.json()) as AttachmentMeta[];
    assert.equal(metas[0].name, "mia foto è.PNG");
    assert.ok(metas[0].path.endsWith("_mia_foto__.PNG"), metas[0].path);
  });

  it("answers the `files` fields before the `file` ones, whatever the body order", async () => {
    const fd = new FormData();
    fd.append("file", new Blob(["uno"], { type: "text/plain" }), "uno.txt");
    fd.append("files", new Blob(["due"], { type: "text/markdown" }), "due.md");

    const res = await upload(id, fd);
    assert.equal(res.status, 200);
    const metas = (await res.json()) as AttachmentMeta[];
    assert.deepEqual(metas.map((m) => m.name), ["due.md", "uno.txt"]);
  });

  it("follows Node's File.type when a part has no or an empty Content-Type", async () => {
    // WHATWG's parser (undici, and the binary) gives `text/plain` to a part with no Content-Type and
    // `""` to an empty one, which the server stores as application/octet-stream.
    const res = await fetch(`${server.url}/api/sessions/${id}/attachments`, {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "multipart/form-data; boundary=x" },
      body: [
        "--x",
        'Content-Disposition: form-data; name="files"; filename="no-type.png"',
        "",
        "niente",
        "--x",
        'Content-Disposition: form-data; name="files"; filename="empty-type.png"',
        "Content-Type: ",
        "",
        "vuoto",
        "--x--",
        "",
      ].join("\r\n"),
    });
    assert.equal(res.status, 200);
    const metas = (await res.json()) as AttachmentMeta[];
    assert.deepEqual(metas.map((m) => [m.name, m.mimeType]), [
      ["no-type.png", "text/plain"],
      ["empty-type.png", "application/octet-stream"],
    ]);
  });

  it("rejects unsupported file types with 400", async () => {
    const exe = new FormData();
    exe.append("files", new Blob([Buffer.from("MZ binary")], { type: "application/x-msdownload" }), "malware.exe");
    const bad = await upload(id, exe);
    assert.equal(bad.status, 400);
    assert.deepEqual(await bad.json(), { error: "Tipo di file non supportato: .exe" });

    const noExt = new FormData();
    noExt.append("files", new Blob([Buffer.from("leggimi")], { type: "text/plain" }), "LEGGIMI");
    const unknown = await upload(id, noExt);
    assert.equal(unknown.status, 400);
    assert.deepEqual(await unknown.json(), { error: "Tipo di file non supportato: sconosciuto" });
  });

  it("rejects empty uploads with 400", async () => {
    const res = await upload(id, new FormData());
    assert.equal(res.status, 400);
    assert.deepEqual(await res.json(), { error: "Nessun file caricato" });
  });

  it("answers 404 for an unknown session", async () => {
    const fd = new FormData();
    addFile(fd, "diagram.png", "image/png", Buffer.from([0x89, 0x50]));
    const res = await upload("nope", fd);
    assert.equal(res.status, 404);
    assert.deepEqual(await res.json(), { error: "Session not found" });
  });

  it("answers 500 with a message for a malformed multipart body", async () => {
    const res = await fetch(`${server.url}/api/sessions/${id}/attachments`, {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "multipart/form-data; boundary=x" },
      body: "garbage",
    });
    assert.equal(res.status, 500);
    const body = (await res.json()) as { error?: string };
    assert.equal(typeof body.error, "string");
    assert.ok(body.error && body.error.length > 0);
  });

  it("attaches the uploaded metadata to the learner item and the engine prompt", async () => {
    assert.ok(pngMeta, "the first test uploaded diagram.png");
    const meta = pngMeta;
    const text = "Ecco le slide della lezione";
    assert.equal((await post(`/api/sessions/${id}/message`, { text, attachments: [meta] })).status, 200);
    await idle(id);

    const items = (await getState(id)).items;
    const item = items.find((i): i is Extract<ChatItem, { kind: "learner" }> => i.kind === "learner" && i.text === text);
    assert.ok(item, "the learner item carries the text");
    assert.deepEqual(item.attachments, [meta]);

    const users = rawInputs()
      .map((line) => JSON.parse(line))
      .filter((line) => line.type === "user");
    const prompt = `[Allegati forniti dallo studente:\n- diagram.png (percorso: ${meta.path})]\n\n${text}`;
    assert.ok(users.some((line) => line.message?.content === prompt), `no user line carried ${JSON.stringify(prompt)}`);
  });

  it("rejects malformed attachment metadata with the schema's issues", async () => {
    const res = await post(`/api/sessions/${id}/message`, { text: "x", attachments: [{ name: "a" }] });
    assert.equal(res.status, 400);
    const body = (await res.json()) as { error: string };
    assert.ok(body.error.startsWith("attachments.0.id: "), body.error);
  });

  it("deleting the session removes the attachments folder", async () => {
    const dir = path.join(repo, ".learner", "sessions", id);
    assert.ok(fs.existsSync(path.join(dir, "attachments")));

    const res = await fetch(`${server.url}/api/sessions/${id}`, { method: "DELETE", headers: { "x-learner-client": "1" } });
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), { ok: true });
    assert.ok(!fs.existsSync(dir));
    assert.equal((await fetch(`${server.url}/api/sessions/${id}`)).status, 404);
  });
});
