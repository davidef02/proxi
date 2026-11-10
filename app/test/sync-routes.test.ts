import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { createServer, type RunningServer } from "../server/main.ts";
import { GOOGLE_CLIENT_ID } from "../server/sync/gdrive.ts";
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

  it("GET /api/sync returns the status shape for a local provider on an existing data folder", async () => {
    const r = await fetch(`${base()}/api/sync`);
    assert.equal(r.status, 200);
    const s = (await r.json()) as Record<string, unknown>;
    assert.equal(s.provider, "local");
    assert.equal(s.dataRoot, data);
    assert.equal(s.needsData, false);
    assert.equal(s.state, "idle");
    assert.equal(s.dataRootFromEnv, false);
    assert.equal(s.conflictCount, 0);
    assert.equal(s.trashCount, 0);
    assert.equal((s.github as { connected: boolean }).connected, false);
    assert.equal((s.gdrive as { connected: boolean }).connected, false);
  });

  it("POST /api/sync/run needs the x-learner-client header", async () => {
    assert.equal((await fetch(`${base()}/api/sync/run`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" })).status, 403);
    const r = await fetch(`${base()}/api/sync/run`, { method: "POST", headers: { ...guard, "content-type": "application/json" }, body: "{}" });
    assert.equal(r.status, 200);
    assert.equal(((await r.json()) as { provider: string }).provider, "local");
  });

  it("GET /oauth/google/callback with an unknown state answers 400 html", async () => {
    const r = await fetch(`${base()}/oauth/google/callback?state=x`);
    assert.equal(r.status, 400);
    assert.match(r.headers.get("content-type") ?? "", /text\/html/);
    assert.match(await r.text(), /scaduta/);
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

  it("GET /api/sync/conflicts lists copies; POST resolve keeps the chosen side and validates its input", async () => {
    const conflicts = path.join(repo, ".learner", "drive-conflicts", "courses", "demo");
    fs.mkdirSync(conflicts, { recursive: true });
    fs.writeFileSync(path.join(data, "courses", "demo", "NOTES.md"), "local notes");
    fs.writeFileSync(path.join(conflicts, "NOTES.md.remoto-1700000000000"), "remote notes");

    const list = (await (await fetch(`${base()}/api/sync/conflicts`)).json()) as { id: string; rel: string; side: string }[];
    assert.deepEqual(list.map((c) => [c.id, c.rel, c.side]), [["courses/demo/NOTES.md.remoto-1700000000000", "courses/demo/NOTES.md", "remoto"]]);
    assert.equal(((await (await fetch(`${base()}/api/sync`)).json()) as { conflictCount: number }).conflictCount, 1);

    assert.equal((await post("/api/sync/conflicts/resolve", { id: list[0].id, choice: "remote" }, {})).status, 403);
    assert.equal((await post("/api/sync/conflicts/resolve", { id: list[0].id, choice: "mine" })).status, 400);
    assert.equal((await post("/api/sync/conflicts/resolve", { id: "../../secrets.json.locale-1", choice: "local" })).status, 400);

    const r = await post("/api/sync/conflicts/resolve", { id: list[0].id, choice: "remote" });
    assert.equal(r.status, 200);
    assert.deepEqual(await r.json(), { ok: true });
    assert.equal(fs.readFileSync(path.join(data, "courses", "demo", "NOTES.md"), "utf8"), "remote notes");
    assert.deepEqual(await (await fetch(`${base()}/api/sync/conflicts`)).json(), []);
  });

  it("GET /api/sync/trash lists trashed files; POST restore moves one back and refuses to overwrite", async () => {
    const ms = String(Date.now() - 60_000);
    const trashed = path.join(repo, ".learner", "trash", ms, "courses", "demo");
    fs.mkdirSync(trashed, { recursive: true });
    fs.writeFileSync(path.join(trashed, "gone.md"), "gone");
    fs.writeFileSync(path.join(trashed, "MISSION.md"), "old mission");

    const list = (await (await fetch(`${base()}/api/sync/trash`)).json()) as { id: string; rel: string }[];
    assert.deepEqual(list.map((e) => e.rel).sort(), ["courses/demo/MISSION.md", "courses/demo/gone.md"]);

    const gone = list.find((e) => e.rel.endsWith("gone.md"))!;
    const r = await post("/api/sync/trash/restore", { id: gone.id });
    assert.equal(r.status, 200);
    assert.equal(fs.readFileSync(path.join(data, "courses", "demo", "gone.md"), "utf8"), "gone");

    const clash = list.find((e) => e.rel.endsWith("MISSION.md"))!;
    const refused = await post("/api/sync/trash/restore", { id: clash.id });
    assert.equal(refused.status, 400);
    assert.match(((await refused.json()) as { error: string }).error, /Esiste già/);
    assert.equal(fs.readFileSync(path.join(data, "courses", "demo", "MISSION.md"), "utf8"), "# Mission\n");
  });

  it("POST /api/sync/run accepts confirmDeletions and rejects a non-boolean", async () => {
    assert.equal((await post("/api/sync/run", { confirmDeletions: true })).status, 200);
    assert.equal((await post("/api/sync/run", { confirmDeletions: "yes" })).status, 400);
  });

  it("POST /api/sync/disconnect and /api/sync/data-root validate their input", async () => {
    assert.equal((await post("/api/sync/disconnect", { provider: "dropbox" })).status, 400);
    const d = await post("/api/sync/disconnect", { provider: "github" });
    assert.equal(d.status, 200);
    assert.equal(((await d.json()) as { provider: string }).provider, "local");
    assert.equal((await post("/api/sync/data-root", { dataRoot: "relative/dir" })).status, 400);
    assert.equal((await post("/api/sync/data-root", { dataRoot: path.join(repo, "inside") })).status, 400);
    assert.equal(fs.existsSync(path.join(repo, ".learner", "config.json")), false);
  });

  it("POST /api/sync/google/start without a client id answers 400 with the Italian message", { skip: (GOOGLE_CLIENT_ID as string) !== "" }, async () => {
    const r = await post("/api/sync/google/start", {});
    assert.equal(r.status, 400);
    assert.match(((await r.json()) as { error: string }).error, /Client Google non configurato/);
  });

  it("POST /api/sync/github/start without a client id reports the Italian error in the status", { skip: (GITHUB_CLIENT_ID as string) !== "" }, async () => {
    const r = await post("/api/sync/github/start", {});
    assert.equal(r.status, 200);
    const s = (await r.json()) as { github: { error?: string; pending?: unknown } };
    assert.match(s.github.error ?? "", /Client GitHub non configurato/);
    assert.equal(s.github.pending, undefined);
  });
});

describe("server start on a fresh install: no legacy migration", () => {
  let srv: RunningServer;
  let tmp: string;
  let repo: string;
  let data: string;
  let settings: string;
  let settingsBefore: string;
  let savedEnv: string | undefined;

  before(async () => {
    savedEnv = process.env.LEARNER_DATA_ROOT;
    delete process.env.LEARNER_DATA_ROOT;
    tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sync-fresh-start-"));
    repo = path.join(tmp, "repo");
    data = path.join(tmp, "data");
    fs.mkdirSync(repo, { recursive: true });
    fs.mkdirSync(data, { recursive: true });
    fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
    const old = path.join(tmp, "old-drive");
    fs.mkdirSync(path.join(old, "courses", "demo"), { recursive: true });
    fs.writeFileSync(path.join(old, "courses", "demo", "MISSION.md"), "# Mission\n");
    settings = path.join(repo, ".learner", "settings.json");
    settingsBefore = JSON.stringify({ driveSync: true, driveFolder: old, autoMap: false });
    fs.writeFileSync(settings, settingsBefore);
    srv = await createServer({ repoRoot: repo, dataRoot: data, port: 0, openBrowser: false, checkUpdateOnStart: false });
  });

  after(async () => {
    await srv.close();
    if (savedEnv === undefined) delete process.env.LEARNER_DATA_ROOT;
    else process.env.LEARNER_DATA_ROOT = savedEnv;
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("leaves old Drive settings untouched, writes no pointer and reports an empty data folder as needsData", async () => {
    assert.equal(fs.existsSync(path.join(repo, ".learner", "config.json")), false);
    assert.equal(fs.readFileSync(settings, "utf8"), settingsBefore);
    const s = (await (await fetch(`http://127.0.0.1:${srv.port}/api/sync`)).json()) as { dataRoot: string; needsData: boolean };
    assert.equal(s.needsData, true);
    assert.equal(path.resolve(s.dataRoot), path.resolve(data));
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
