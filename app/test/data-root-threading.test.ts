import { describe, it, after } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { SessionManager } from "../server/sessions.ts";
import { buildSystemPrompt } from "../server/context.ts";
import { buildClaudeArgs } from "../server/claude.ts";
import { toolsFor } from "../server/tools/index.ts";
import type { EngineLaunch } from "../server/types.ts";
import type { SessionConfig } from "../shared/protocol.ts";
import { fakeContext, memoryEngine, memoryJobs } from "./helpers.ts";

const REAL = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const dirs: string[] = [];
after(() => {
  for (const d of dirs) fs.rmSync(d, { recursive: true, force: true, maxRetries: 3 });
});

/** A code repo (.learner/ and docs only) and a separate data folder (courses/, curriculum.json). */
function split() {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "dr-repo-"));
  const data = fs.mkdtempSync(path.join(os.tmpdir(), "dr-data-"));
  dirs.push(repo, data);
  fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
  fs.mkdirSync(path.join(repo, "docs", "agents"), { recursive: true });
  for (const f of ["pedagogy.md", "anki-cards.md"]) fs.copyFileSync(path.join(REAL, "docs", "agents", f), path.join(repo, "docs", "agents", f));
  const c = path.join(data, "courses", "demo");
  fs.mkdirSync(path.join(c, "learning-records"), { recursive: true });
  fs.mkdirSync(path.join(c, "lessons"), { recursive: true });
  fs.writeFileSync(path.join(c, "MISSION.md"), "# Mission\nMISSION-MARKER");
  fs.writeFileSync(
    path.join(data, "curriculum.json"),
    JSON.stringify({ chapters: [{ title: "Demo", slug: "demo", lessons: [{ title: "Lezione uno", slug: "lezione-uno", plannedNodes: 3 }] }] })
  );
  return { repo, data };
}

const planCfg = (): SessionConfig => ({ kind: "plan", course: "demo", model: "haiku", chapterTitle: "Demo" } as SessionConfig);

describe("dataRoot threading", () => {
  it("SessionManager gives the session a context whose dataRoot is the data folder", async () => {
    const { repo, data } = split();
    const m = new SessionManager({ repoRoot: repo, dataRoot: data, port: 1, token: "t", bins: { claude: { cmd: "claude" }, agy: { cmd: "agy" }, opencode: { cmd: "opencode" } }, jobs: memoryJobs() }) as any;
    const engine = memoryEngine();
    m.spawn = (_s: unknown, resume: string | undefined, cb: any) => engine.spawn(resume, cb); // no process
    const id = await m.create(planCfg());
    const ctx = m.getContext(id);
    assert.equal(ctx.dataRoot, data);
    assert.equal(ctx.repoRoot, repo);
    await m.close(id);
  });

  it("buildSystemPrompt adds the Data folder block and reads course files from the data folder", async () => {
    const { repo, data } = split();
    const prompt = await buildSystemPrompt({ config: planCfg(), repoRoot: repo, dataRoot: data, today: "2026-01-01" });
    assert.match(prompt, /# Data folder/);
    assert.ok(prompt.includes(path.resolve(data)), "data path is named in the prompt");
    assert.match(prompt, /MISSION-MARKER/);
  });

  it("buildSystemPrompt has no Data folder block when dataRoot is the repo", async () => {
    const { repo } = split();
    fs.mkdirSync(path.join(repo, "courses", "demo"), { recursive: true });
    const prompt = await buildSystemPrompt({ config: planCfg(), repoRoot: repo, dataRoot: repo, today: "2026-01-01" });
    assert.doesNotMatch(prompt, /# Data folder/);
  });

  it("buildClaudeArgs adds --add-dir <data> only when dataRoot differs from repoRoot", () => {
    const { repo, data } = split();
    const base: EngineLaunch = {
      provider: "claude", bin: { cmd: "claude" }, repoRoot: repo, scratchDir: path.join(repo, ".learner", "s"), model: "haiku",
      systemPrompt: "x", grant: ["read"], mcp: { name: "app", url: "http://127.0.0.1:1/mcp/x", token: "t" }, addDirs: [],
    };
    const withData = buildClaudeArgs({ ...base, dataRoot: data }, "uuid-1");
    const i = withData.indexOf("--add-dir");
    assert.ok(i >= 0);
    assert.equal(withData[i + 1], data);
    assert.equal(withData.filter((a) => a === "--add-dir").length, 1);

    const same = buildClaudeArgs({ ...base, dataRoot: repo }, "uuid-1");
    assert.equal(same.includes("--add-dir"), false);
  });

  it("a curriculum tool writes through ctx.dataRoot, not the repo", async () => {
    const { repo, data } = split();
    const cfg = planCfg();
    const ctx = fakeContext(repo, cfg, [{ approved: true }]);
    ctx.dataRoot = data;
    const tool = toolsFor(cfg, data).find((t) => t.name === "propose_curriculum")!;
    const out = await tool.run(
      {
        chapter: "Demo",
        slug: "demo",
        destination: "Arrivare alla fine.",
        lessons: [{ title: "Lezione uno", plannedNodes: 3 }, { title: "Lezione due", plannedNodes: 4 }],
      } as any,
      ctx
    );
    assert.equal(out.isError, undefined, out.text);
    const written = JSON.parse(fs.readFileSync(path.join(data, "curriculum.json"), "utf-8"));
    assert.ok(written.chapters[0].lessons.some((l: { title: string }) => l.title === "Lezione due"));
    assert.equal(fs.existsSync(path.join(repo, "curriculum.json")), false);
  });
});
