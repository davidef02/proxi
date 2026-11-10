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
import { DATA_REPO } from "../server/sync/github.ts";
import { DRIVE_FOLDER } from "../server/sync/gdrive.ts";
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

  it("the desktop shell sets the app name and AppUserModelId", () => {
    // Electron does not load under node:test, so probe the source text.
    const main = fs.readFileSync(path.join(repoRoot, "app", "desktop", "main.mjs"), "utf8");
    assert.ok(main.includes('from "../shared/identity.mjs"'));
    assert.ok(main.includes("app.setName(APP_ID)"));
    assert.ok(main.includes("app.setAppUserModelId(APP_USER_MODEL_ID)"));
  });

  it("the shortcut installer removes the shortcut named in the identity", () => {
    const installer = fs.readFileSync(path.join(repoRoot, "app", "desktop", "install-shortcuts.mjs"), "utf8");
    assert.ok(installer.includes('from "../shared/identity.mjs"'));
    assert.ok(installer.includes("const NAME = SHORTCUT_NAME"));
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-shortcut-"));
    try {
      fs.writeFileSync(path.join(dir, SHORTCUT_NAME), "");
      const r = spawnSync(
        process.execPath,
        [path.join(repoRoot, "app", "desktop", "install-shortcuts.mjs"), "--uninstall", "--dir", dir],
        { encoding: "utf8" },
      );
      assert.equal(r.status, 0, r.stderr);
      assert.ok(r.stdout.includes("removed"), r.stdout);
      assert.equal(fs.existsSync(path.join(dir, SHORTCUT_NAME)), false);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it("the desktop splash shows the identity name", () => {
    // The splash lives in a data: URL built by main.mjs; Electron does not load under node:test, so check its text.
    const main = fs.readFileSync(path.join(repoRoot, "app", "desktop", "main.mjs"), "utf8");
    assert.ok(main.includes("<div>${APP_NAME}<br>"), "the splash must interpolate APP_NAME");
  });

  it("the desktop shell and the shortcut installer keep no legacy rename", () => {
    // D4: fresh installs only. The old profile carry-over and the old .lnk removal must be gone.
    const oldApp = ["ia", "learner"].join("-");
    const desktop = path.join(repoRoot, "app", "desktop");
    for (const file of ["main.mjs", "install-shortcuts.mjs"]) {
      const text = fs.readFileSync(path.join(desktop, file), "utf8");
      assert.equal(text.includes(oldApp), false, `${file} still carries the old profile/shortcut name`);
    }
    assert.equal(fs.readFileSync(path.join(desktop, "install-shortcuts.mjs"), "utf8").includes("LEGACY"), false);
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
