import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import { readSettings, writeSettings } from "../server/settings.ts";
import { DEFAULT_SETTINGS } from "../shared/protocol.ts";

function tempRepo(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "settings-sync-"));
}

test("sync defaults: local provider and empty OAuth client ids", () => {
  const repo = tempRepo();
  const s = readSettings(repo);
  assert.equal(s.syncProvider, "local");
  assert.equal(s.githubClientId, "");
  assert.equal(s.googleClientId, "");
  assert.equal(s.googleClientSecret, "");
  assert.equal(DEFAULT_SETTINGS.syncProvider, "local");
  fs.rmSync(repo, { recursive: true, force: true });
});

test("syncProvider patch round-trips through settings.json", () => {
  const repo = tempRepo();
  writeSettings(repo, { syncProvider: "github", githubClientId: "abc" });
  const s = readSettings(repo);
  assert.equal(s.syncProvider, "github");
  assert.equal(s.githubClientId, "abc");
  const onDisk = JSON.parse(fs.readFileSync(path.join(repo, ".learner", "settings.json"), "utf-8"));
  assert.equal(onDisk.syncProvider, "github");
  fs.rmSync(repo, { recursive: true, force: true });
});

test("an invalid syncProvider in the file is dropped leniently", () => {
  const repo = tempRepo();
  fs.mkdirSync(path.join(repo, ".learner"), { recursive: true });
  fs.writeFileSync(
    path.join(repo, ".learner", "settings.json"),
    JSON.stringify({ syncProvider: "dropbox", teacherModel: "opus" }),
  );
  const s = readSettings(repo);
  assert.equal(s.syncProvider, "local");
  assert.equal(s.teacherModel, "opus");
  fs.rmSync(repo, { recursive: true, force: true });
});

test("writeSettings rejects an unknown syncProvider", () => {
  const repo = tempRepo();
  assert.throws(() => writeSettings(repo, { syncProvider: "dropbox" }));
  fs.rmSync(repo, { recursive: true, force: true });
});
