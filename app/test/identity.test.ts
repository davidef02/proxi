import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import {
  APP_ID,
  APP_NAME,
  APP_USER_MODEL_ID,
  DATA_DIR_NAME,
  DECK_ROOT as IDENTITY_DECK_ROOT,
  DRIVE_APP_PROPERTY,
  SHORTCUT_NAME,
  SKIP_MODEL_ENV,
} from "../shared/identity.mjs";
import { defaultDataRoot } from "../../extensions/data-root.ts";
import { DATA_REPO, DRIVE_FOLDER } from "../shared/protocol.ts";
import { DECK_ROOT, cardsToTsv, formatDefaultDeckName } from "../../extensions/anki-sync.ts";

const repoRoot = path.resolve(import.meta.dirname, "../..");

describe("identity", () => {
  it("exports the app's names", () => {
    assert.equal(APP_NAME, "Proxi");
    assert.equal(APP_ID, "proxi");
    assert.equal(APP_USER_MODEL_ID, "DavideF02.Proxi");
    assert.equal(IDENTITY_DECK_ROOT, "Proxi");
    assert.equal(DATA_DIR_NAME, "proxi-data");
    assert.equal(SHORTCUT_NAME, "Proxi.lnk");
    assert.equal(SKIP_MODEL_ENV, "PROXI_SKIP_MODEL");
    assert.equal(DRIVE_APP_PROPERTY, "proxi");
  });

  it("defaults the data folder to <home>/Documents/proxi-data", () => {
    const home = path.join(os.tmpdir(), "identity-home");
    assert.equal(defaultDataRoot(home), path.join(home, "Documents", DATA_DIR_NAME));
  });

  it("names the synced repo and the Drive folder after the data folder", () => {
    assert.equal(DATA_REPO, DATA_DIR_NAME);
    assert.equal(DRIVE_FOLDER, DATA_DIR_NAME);
  });

  it("derives the Anki deck root from the identity", () => {
    assert.equal(DECK_ROOT, IDENTITY_DECK_ROOT);
    assert.equal(formatDefaultDeckName("Demo"), `${DECK_ROOT}::Demo`);
    assert.ok(cardsToTsv([]).includes(`#deck:${DECK_ROOT}::General`));
  });

  it("package.json and package-lock.json carry the identity names", () => {
    const pkg = JSON.parse(fs.readFileSync(path.join(repoRoot, "package.json"), "utf8"));
    const lock = JSON.parse(fs.readFileSync(path.join(repoRoot, "package-lock.json"), "utf8"));
    assert.equal(pkg.name, APP_ID);
    assert.equal(pkg.productName, APP_NAME);
    assert.equal(lock.name, APP_ID);
    assert.equal(lock.packages[""].name, APP_ID);
  });

  it("the postinstall model download can be skipped with SKIP_MODEL_ENV", () => {
    const r = spawnSync(process.execPath, [path.join(repoRoot, "scripts", "fetch-embedding-model.mjs")], {
      env: { ...process.env, [SKIP_MODEL_ENV]: "1" },
      encoding: "utf8",
    });
    assert.equal(r.status, 0, r.stderr);
    assert.ok(r.stdout.includes(`${SKIP_MODEL_ENV}=1`), r.stdout);
  });
});
