import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { createServer } from "../server/main.ts";
import type { AttachmentMeta, SessionState } from "../shared/protocol.ts";

const FAKE = path.resolve(import.meta.dirname, "fake-claude.mjs");

async function waitFor<T>(fn: () => T | undefined | false, ms = 15000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("waitFor timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}

function tempRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-attach-"));
  fs.writeFileSync(
    path.join(root, "curriculum.json"),
    JSON.stringify({
      chapters: [
        {
          title: "Demo",
          slug: "demo",
          lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }],
        },
      ],
    }),
  );
  fs.mkdirSync(path.join(root, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(root, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(root, "courses", "demo", "NOTES.md"), "# Notes\n");
  fs.mkdirSync(path.join(root, "assets"));
  return root;
}

describe("Composer attachments (Images and PDFs)", () => {
  let srv: Awaited<ReturnType<typeof createServer>>;
  let root: string;
  let sessionId: string;

  before(async () => {
    root = tempRepo();
    srv = await createServer({
      repoRoot: root,
      dataRoot: root,
      port: 0,
      openBrowser: false,
      claudeBin: process.execPath,
      claudeBinArgs: [FAKE],
    });
    // Create a session for testing attachments
    sessionId = await srv.manager.create({
      kind: "teach",
      course: "demo",
      target: { mode: "planned", title: "Prima lezione" },
      ankiOptIn: false,
    });
    await waitFor(() => srv.manager.get(sessionId)?.status === "idle");
  });

  after(async () => {
    await srv.close();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it("uploads an image and a PDF via multipart form data", async () => {
    const pngBytes = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00]);
    const pdfBytes = Buffer.from("%PDF-1.4 sample content\n%%EOF");

    const fd = new FormData();
    fd.append("files", new Blob([pngBytes], { type: "image/png" }), "diagram.png");
    fd.append("files", new Blob([pdfBytes], { type: "application/pdf" }), "notes.pdf");

    const res = await fetch(`http://127.0.0.1:${srv.port}/api/sessions/${sessionId}/attachments`, {
      method: "POST",
      headers: { "x-learner-client": "1" },
      body: fd,
    });

    assert.equal(res.status, 200);
    const metas = (await res.json()) as AttachmentMeta[];
    assert.equal(metas.length, 2);

    const [pngMeta, pdfMeta] = metas;
    assert.equal(pngMeta.name, "diagram.png");
    assert.equal(pngMeta.mimeType, "image/png");
    assert.equal(pngMeta.size, pngBytes.length);
    assert.ok(pngMeta.path.startsWith(`.learner/sessions/${sessionId}/attachments/`));
    assert.ok(pngMeta.path.endsWith("_diagram.png"));

    assert.equal(pdfMeta.name, "notes.pdf");
    assert.equal(pdfMeta.mimeType, "application/pdf");
    assert.equal(pdfMeta.size, pdfBytes.length);
    assert.ok(pdfMeta.path.startsWith(`.learner/sessions/${sessionId}/attachments/`));
    assert.ok(pdfMeta.path.endsWith("_notes.pdf"));

    // Verify stored files on disk
    const storedPng = fs.readFileSync(path.join(root, pngMeta.path));
    assert.deepEqual(storedPng, pngBytes);

    const storedPdf = fs.readFileSync(path.join(root, pdfMeta.path));
    assert.deepEqual(storedPdf, pdfBytes);
  });

  it("sanitizes dangerous filenames and path traversal", async () => {
    const fileBytes = Buffer.from("test content");
    const fd = new FormData();
    fd.append("files", new Blob([fileBytes], { type: "image/png" }), "../../../etc/passwd.png");

    const res = await fetch(`http://127.0.0.1:${srv.port}/api/sessions/${sessionId}/attachments`, {
      method: "POST",
      headers: { "x-learner-client": "1" },
      body: fd,
    });

    assert.equal(res.status, 200);
    const metas = (await res.json()) as AttachmentMeta[];
    assert.equal(metas.length, 1);
    assert.ok(!metas[0].path.includes(".."));
    assert.ok(fs.existsSync(path.join(root, metas[0].path)));
  });

  it("rejects unsupported file types with 400", async () => {
    const fd = new FormData();
    fd.append("files", new Blob([Buffer.from("MZ binary")], { type: "application/x-msdownload" }), "malware.exe");

    const res = await fetch(`http://127.0.0.1:${srv.port}/api/sessions/${sessionId}/attachments`, {
      method: "POST",
      headers: { "x-learner-client": "1" },
      body: fd,
    });

    assert.equal(res.status, 400);
  });

  it("rejects empty uploads with 400", async () => {
    const fd = new FormData();
    const res = await fetch(`http://127.0.0.1:${srv.port}/api/sessions/${sessionId}/attachments`, {
      method: "POST",
      headers: { "x-learner-client": "1" },
      body: fd,
    });

    assert.equal(res.status, 400);
  });

  it("attaches metadata to learner message item", async () => {
    const sampleAttachment: AttachmentMeta = {
      id: "att-123",
      name: "slide.pdf",
      path: `.learner/sessions/${sessionId}/attachments/att-123_slide.pdf`,
      mimeType: "application/pdf",
      size: 1024,
    };

    const res = await fetch(`http://127.0.0.1:${srv.port}/api/sessions/${sessionId}/message`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-learner-client": "1" },
      body: JSON.stringify({
        text: "Ecco le slide della lezione",
        attachments: [sampleAttachment],
      }),
    });

    assert.equal(res.status, 200);

    const sRes = await fetch(`http://127.0.0.1:${srv.port}/api/sessions/${sessionId}`, {
      headers: { "x-learner-client": "1" },
    });
    assert.equal(sRes.status, 200);
    const state = (await sRes.json()) as SessionState;

    const learnerItem = state.items.find((it) => it.kind === "learner" && it.text === "Ecco le slide della lezione");
    assert.ok(learnerItem && learnerItem.kind === "learner");
    assert.ok(learnerItem.attachments && learnerItem.attachments.length === 1);
    assert.equal(learnerItem.attachments[0].name, "slide.pdf");
    assert.equal(learnerItem.attachments[0].path, sampleAttachment.path);
  });

  it("deleting session cleans up attachments folder", async () => {
    const attachDir = path.join(root, ".learner", "sessions", sessionId, "attachments");
    assert.ok(fs.existsSync(attachDir));

    await srv.manager.delete(sessionId);
    assert.ok(!fs.existsSync(attachDir));
  });
});
