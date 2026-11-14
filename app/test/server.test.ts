import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { startFakeServers } from "./fake-servers.ts";

let mod: typeof import("../server/main.ts") | undefined;
try {
  mod = await import("../server/main.ts");
} catch {
  /* wave-1 modules not all present yet */
}
const skip = mod ? false : "waiting for wave-1 modules";

function tempRepo(): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "learner-srv-"));
  fs.writeFileSync(path.join(root, "curriculum.json"), JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Prima lezione", slug: "prima-lezione", plannedNodes: 2 }] }] }));
  fs.mkdirSync(path.join(root, "courses", "demo", "lessons"), { recursive: true });
  fs.writeFileSync(path.join(root, "courses", "demo", "MISSION.md"), "# Mission\n");
  fs.writeFileSync(path.join(root, "courses", "demo", "NOTES.md"), "# Notes\n");
  fs.mkdirSync(path.join(root, "assets"));
  fs.writeFileSync(path.join(root, "assets", "x.css"), "body{}");
  fs.writeFileSync(path.join(root, "secret.txt"), "TOPSECRET");
  return root;
}

/** Raw request: lets the test choose Host and send un-normalised paths. */
function raw(port: number, p: string, o: { method?: string; headers?: Record<string, string> } = {}): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: "127.0.0.1", port, path: p, method: o.method ?? "GET", headers: o.headers }, (res) => {
      let body = "";
      res.on("data", (d) => (body += d));
      res.on("end", () => resolve({ status: res.statusCode!, body }));
    });
    req.on("error", reject);
    req.end(o.method === "POST" ? "{}" : undefined);
  });
}

describe("server guards and read routes", { skip }, () => {
  let srv: Awaited<ReturnType<NonNullable<typeof mod>["createServer"]>>;
  let root: string;
  before(async () => {
    root = tempRepo();
    srv = await mod!.createServer({ repoRoot: root, dataRoot: root, port: 0, openBrowser: false });
  });
  after(async () => {
    await srv.close();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it("rejects a foreign Host header", async () => {
    const r = await raw(srv.port, "/api/sessions", { headers: { host: "evil.example:" + srv.port } });
    assert.ok(r.status === 403 || r.status === 421);
    assert.equal((await raw(srv.port, "/api/sessions")).status, 200);
  });

  it("requires x-learner-client on POST /api/*", async () => {
    assert.equal((await raw(srv.port, "/api/sessions/nope/interrupt", { method: "POST" })).status, 403);
    const ok = await raw(srv.port, "/api/sessions/nope/interrupt", { method: "POST", headers: { "x-learner-client": "1" } });
    assert.equal(ok.status, 404);
    assert.ok(JSON.parse(ok.body).error);
  });

  it("protects /mcp with the token", async () => {
    assert.equal((await raw(srv.port, "/mcp/abc", { method: "POST" })).status, 401);
    assert.equal((await raw(srv.port, "/mcp/abc", { method: "POST", headers: { "x-learner-token": "wrong" } })).status, 401);
    assert.equal((await raw(srv.port, "/mcp/abc", { method: "POST", headers: { "x-learner-token": srv.token } })).status, 404);
  });

  it("lists the knowledge map", async () => {
    const map = JSON.parse((await raw(srv.port, "/api/map")).body);
    assert.ok(Array.isArray(map.nodes) && Array.isArray(map.edges));
  });

  it("maps /api/anki to byCourse per course, and to no badges when AnkiConnect is down", async () => {
    const fakes = await startFakeServers();
    const stats = { new_count: 5, learn_count: 2, review_count: 4, total_in_deck: 10 };
    fakes.anki.deckNamesAndIds = { "Proxi": 1, "Proxi::Demo": 2 };
    fakes.anki.deckStats = { "1": { deck_id: 1, name: "Proxi", ...stats }, "2": { deck_id: 2, name: "Demo", ...stats } };
    const prev = process.env.LEARNER_ANKI_URL;
    try {
      process.env.LEARNER_ANKI_URL = fakes.urls.anki;
      const online = await raw(srv.port, "/api/anki");
      assert.equal(online.status, 200);
      const body = JSON.parse(online.body);
      assert.equal(body.online, true);
      assert.deepEqual(body.byCourse.demo, { due: 4, learning: 2, newCards: 5 });

      // The whole map belongs to the batch request only: one course gets its own deck, never every deck.
      const one = JSON.parse((await raw(srv.port, "/api/anki?course=demo")).body);
      assert.equal(one.online, true);
      assert.equal(one.deck, "Proxi::Demo");
      assert.equal(one.deckExists, true);
      assert.equal(one.due, 4);
      assert.equal(one.byCourse, undefined);

      process.env.LEARNER_ANKI_URL = "http://127.0.0.1:9";
      const offline = JSON.parse((await raw(srv.port, "/api/anki")).body);
      assert.equal(offline.online, false);
      assert.equal(offline.byCourse, undefined);
    } finally {
      await fakes.close();
      if (prev === undefined) delete process.env.LEARNER_ANKI_URL;
      else process.env.LEARNER_ANKI_URL = prev;
    }
  });

  it("serves opencode models and manages opencode settings", async () => {
    const modelsRes = await raw(srv.port, "/api/opencode/models");
    assert.equal(modelsRes.status, 200);
    const models = JSON.parse(modelsRes.body);
    assert.ok(Array.isArray(models) && models.length > 0);

    const postRes = await raw(srv.port, "/api/settings", {
      method: "POST",
      headers: { "x-learner-client": "1", "content-type": "application/json" },
    });
    // raw sends "{}" on POST by default, so we can test settings patch
    const getRes = await raw(srv.port, "/api/settings");
    assert.equal(getRes.status, 200);
    const s = JSON.parse(getRes.body);
    assert.ok(s.opencodeModels);
    assert.ok(s.opencodeModels.teacher);
  });

  it("answers JSON errors", async () => {
    const r = await raw(srv.port, "/api/sessions/missing");
    assert.equal(r.status, 404);
    assert.equal(typeof JSON.parse(r.body).error, "string");
    const bad = await raw(srv.port, "/api/sessions", { method: "POST", headers: { "x-learner-client": "1", "content-type": "application/json" } });
    assert.equal(bad.status, 400);
  });
});
