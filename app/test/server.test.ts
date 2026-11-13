import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";

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

  it("serves only courses/ and assets/ under /files", async () => {
    assert.equal((await raw(srv.port, "/files/courses/demo/MISSION.md")).status, 200);
    assert.equal((await raw(srv.port, "/files/assets/x.css")).status, 200);
    for (const p of ["/files/secret.txt", "/files/curriculum.json", "/files/..%2fsecret.txt", "/files/courses/..%2f..%2fsecret.txt", "/files/courses/..%5csecret.txt", "/files/courses/%00"]) {
      assert.equal((await raw(srv.port, p)).status, 404, p);
    }
    for (const p of ["/files/../secret.txt", "/files/%2e%2e/secret.txt"]) {
      assert.ok(!(await raw(srv.port, p)).body.includes("TOPSECRET"), p);
    }
  });

  it("lists courses and the knowledge map", async () => {
    const courses = JSON.parse((await raw(srv.port, "/api/courses")).body);
    assert.ok(Array.isArray(courses));
    assert.equal(courses[0].slug, "demo");
    const map = JSON.parse((await raw(srv.port, "/api/map")).body);
    assert.ok(Array.isArray(map.nodes) && Array.isArray(map.edges));
  });

  it("serves a lesson's data island under /api/lesson, only for lesson artifacts", async () => {
    const island = { metadata: { title: "Prima" }, nodes: [{ nodeId: "a", title: "A", content: "x" }] };
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0001-prima.html"), `<html><script type="application/json" id="lesson-data">${JSON.stringify(island)}</script></html>`);
    fs.writeFileSync(path.join(root, "courses", "demo", "lessons", "0002-prosa.html"), "<html>prosa</html>");
    const ok = await raw(srv.port, "/api/lesson?path=courses/demo/lessons/0001-prima.html");
    assert.equal(ok.status, 200);
    assert.equal(JSON.parse(ok.body).nodes[0].nodeId, "a");
    assert.equal((await raw(srv.port, "/api/lesson?path=courses/demo/lessons/0009-no.html")).status, 404);
    assert.equal((await raw(srv.port, "/api/lesson?path=courses/demo/lessons/0002-prosa.html")).status, 422);
    for (const p of ["../secret.txt", "courses/demo/MISSION.md", "courses/demo/lessons/..%2f..%2fMISSION.md", "courses/demo/lessons/../../../secret.html", ""]) {
      assert.equal((await raw(srv.port, "/api/lesson?path=" + p)).status, 400, p);
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
