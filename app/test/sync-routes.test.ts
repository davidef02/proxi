import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { createServer, type RunningServer } from "../server/main.ts";
import { GITHUB_CLIENT_ID } from "../server/sync/github.ts";

const CURRICULUM = { chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }] };

describe("sync routes, data folder and sync-related HTTP surface", () => {
  let srv: RunningServer;
  let repo: string;
  let data: string;
  const base = () => `http://127.0.0.1:${srv.port}`;
  const guard = { "x-learner-client": "1" };

  before(async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-routes-"));
    repo = path.join(tmp, "repo");
    data = path.join(tmp, "data");
    fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
    fs.mkdirSync(path.join(repo, "assets"), { recursive: true });
    fs.writeFileSync(path.join(repo, "assets", "lesson-template.html"), "<p>template</p>");
    fs.mkdirSync(path.join(data, "courses", "demo", "lessons"), { recursive: true });
    fs.writeFileSync(path.join(data, "courses", "demo", "MISSION.md"), "# Mission\n");
    fs.writeFileSync(path.join(data, "curriculum.json"), JSON.stringify(CURRICULUM));
    srv = await createServer({ repoRoot: repo, dataRoot: data, port: 0, openBrowser: false, checkUpdateOnStart: false });
  });

  after(async () => {
    await srv.close();
    fs.rmSync(path.dirname(repo), { recursive: true, force: true });
  });

  it("serves courses/ from the data folder and assets/ from the repo", async () => {
    const mission = await fetch(`${base()}/files/courses/demo/MISSION.md`);
    assert.equal(mission.status, 200);
    assert.equal(await mission.text(), "# Mission\n");
    const template = await fetch(`${base()}/files/assets/lesson-template.html`);
    assert.equal(template.status, 200);
    assert.match(await template.text(), /<p>template<\/p>$/);
    // a repo-only course file is not served: courses/ resolves against the data folder
    fs.mkdirSync(path.join(repo, "courses", "repo-only"), { recursive: true });
    fs.writeFileSync(path.join(repo, "courses", "repo-only", "MISSION.md"), "repo");
    assert.equal((await fetch(`${base()}/files/courses/repo-only/MISSION.md`)).status, 404);
  });

  it("GET /api/courses lists a course that exists only in the data folder", async () => {
    const r = await fetch(`${base()}/api/courses`);
    assert.equal(r.status, 200);
    const body = JSON.stringify(await r.json());
    assert.match(body, /"demo"/);
  });

  it("GET /api/settings includes the data folder", async () => {
    const r = await fetch(`${base()}/api/settings`);
    assert.equal(r.status, 200);
    const s = (await r.json()) as { dataRoot: string; repoRoot: string; syncProvider: string };
    assert.equal(s.dataRoot, data);
    assert.equal(s.repoRoot, repo);
    assert.equal(s.syncProvider, "local");
  });

  const post = (p: string, body: unknown, headers: Record<string, string> = guard) =>
    fetch(`${base()}${p}`, { method: "POST", headers: { ...headers, "content-type": "application/json" }, body: JSON.stringify(body) });

  it("POST /api/sync/github/start without a client id reports the Italian error in the status", { skip: (GITHUB_CLIENT_ID as string) !== "" }, async () => {
    const r = await post("/api/sync/github/start", {});
    assert.equal(r.status, 200);
    const s = (await r.json()) as { github: { error?: string; pending?: unknown } };
    assert.match(s.github.error ?? "", /Client GitHub non configurato/);
    assert.equal(s.github.pending, undefined);
  });
});

describe("file serving on a fresh PC whose data folder is reached through a junction", () => {
  let srv: RunningServer;
  let tmp: string;
  let real: string;

  before(async () => {
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-files-junction-"));
    const repo = path.join(tmp, "repo");
    real = path.join(tmp, "real-data");
    const link = path.join(tmp, "linked-data");
    fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
    fs.mkdirSync(path.join(repo, "assets"), { recursive: true });
    fs.mkdirSync(real, { recursive: true }); // no courses/ yet: the first sync creates it
    fs.mkdirSync(path.join(tmp, "outside"), { recursive: true });
    fs.writeFileSync(path.join(tmp, "outside", "secret.md"), "secret");
    fs.symlinkSync(real, link, "junction");
    srv = await createServer({ repoRoot: repo, dataRoot: link, port: 0, openBrowser: false, checkUpdateOnStart: false });
  });

  after(async () => {
    await srv.close();
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("serves a lesson written under courses/ after the server started", async () => {
    fs.mkdirSync(path.join(real, "courses", "demo"), { recursive: true });
    fs.writeFileSync(path.join(real, "courses", "demo", "MISSION.md"), "# pulled\n");
    const r = await fetch(`http://127.0.0.1:${srv.port}/files/courses/demo/MISSION.md`);
    assert.equal(r.status, 200);
    assert.equal(await r.text(), "# pulled\n");
  });

  it("still refuses a junction inside courses/ that leads outside it", async () => {
    fs.mkdirSync(path.join(real, "courses"), { recursive: true });
    fs.symlinkSync(path.join(tmp, "outside"), path.join(real, "courses", "escape"), "junction");
    assert.equal((await fetch(`http://127.0.0.1:${srv.port}/files/courses/escape/secret.md`)).status, 404);
  });
});
