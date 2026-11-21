import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, it } from "node:test";
import {
  ed25519Verify,
  findReleaseArtifacts,
  GITHUB_RELEASE_ASSET_URL_RE,
  isValidPlatformUrl,
  latestJsonFromSig,
  parseMinisignSignature,
  parseUpdaterPubkey,
  resolveLatestJsonPath,
  spkiFromRawEd25519,
  verifyRelease,
} from "../../scripts/verify-release.mjs";

interface Fixture {
  tmp: string;
  bundleDir: string;
  configPath: string;
  installerPath: string;
  sigPath: string;
  latestJsonPath: string;
  installerName: string;
  pubkeyB64: string;
  privateKey: crypto.KeyObject;
  keynum: Buffer;
  sigB64: string;
  version: string;
}

let tmp: string;

beforeEach(() => {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "verify-release-test-"));
});

afterEach(() => {
  fs.rmSync(tmp, { recursive: true, force: true });
});

function createFixture(baseDir: string): Fixture {
  const bundleDir = path.join(baseDir, "bundle");
  fs.mkdirSync(bundleDir, { recursive: true });

  const installerName = "proxi-setup.exe";
  const installerPath = path.join(bundleDir, installerName);
  const sigPath = path.join(bundleDir, `${installerName}.sig`);
  const configPath = path.join(baseDir, "tauri.conf.json");
  const latestJsonPath = path.join(bundleDir, "latest.json");
  const version = "0.1.0";

  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const spkiDer = publicKey.export({ format: "der", type: "spki" });
  const rawPub = spkiDer.subarray(spkiDer.length - 32);
  const keynum = crypto.randomBytes(8);

  const pubBox = Buffer.concat([Buffer.from("Ed", "utf8"), keynum, rawPub]);
  const pubText = [
    `untrusted comment: minisign public key: ${keynum.toString("hex").toUpperCase()}`,
    pubBox.toString("base64"),
    "",
  ].join("\n");
  const pubkeyB64 = Buffer.from(pubText, "utf8").toString("base64");

  fs.writeFileSync(
    configPath,
    JSON.stringify(
      {
        version,
        plugins: {
          updater: {
            pubkey: pubkeyB64,
          },
        },
      },
      null,
      2,
    ),
  );

  const installerBytes = Buffer.from("sample dummy installer binary data for testing");
  fs.writeFileSync(installerPath, installerBytes);

  const fileSig = crypto.sign(null, installerBytes, privateKey);
  const line2Buf = Buffer.concat([Buffer.from("Ed", "utf8"), keynum, fileSig]);
  const commentBody = `timestamp:${Math.floor(Date.now() / 1000)}\tfile:${installerName}\tversion:${version}`;
  const trustedComment = `trusted comment: ${commentBody}`;
  const globalSig = crypto.sign(
    null,
    Buffer.concat([fileSig, Buffer.from(commentBody, "utf8")]),
    privateKey,
  );

  const sigLines = [
    "untrusted comment: signature from test key",
    line2Buf.toString("base64"),
    trustedComment,
    globalSig.toString("base64"),
    "",
  ].join("\n");
  const sigB64 = Buffer.from(sigLines, "utf8").toString("base64");
  fs.writeFileSync(sigPath, sigB64);

  const latestJson = {
    version,
    notes: "Initial test release notes",
    pub_date: new Date().toISOString(),
    platforms: {
      "windows-x86_64": {
        signature: sigB64,
        url: `https://example.com/download/${installerName}`,
      },
    },
  };
  fs.writeFileSync(latestJsonPath, JSON.stringify(latestJson, null, 2));

  return {
    tmp: baseDir,
    bundleDir,
    configPath,
    installerPath,
    sigPath,
    latestJsonPath,
    installerName,
    pubkeyB64,
    privateKey,
    keynum,
    sigB64,
    version,
  };
}

describe("verifyRelease", () => {
  it("(1) a valid bundle verifies ok", () => {
    const fix = createFixture(tmp);
    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, true);
    assert.deepEqual(res.errors, []);
    assert.ok(res.installerBytes > 0);
  });

  it("(2) a flipped installer byte fails with a signature error", () => {
    const fix = createFixture(tmp);
    const bytes = fs.readFileSync(fix.installerPath);
    bytes[0] ^= 0xff;
    fs.writeFileSync(fix.installerPath, bytes);

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, false);
    assert.ok(res.errors.some((e) => e.includes("unsigned or unknown key")));
  });

  it("(3) a different pubkey in the config fails", () => {
    const fix = createFixture(tmp);
    const { publicKey: otherPub } = crypto.generateKeyPairSync("ed25519");
    const otherDer = otherPub.export({ format: "der", type: "spki" });
    const otherRaw = otherDer.subarray(otherDer.length - 32);
    const otherKeynum = crypto.randomBytes(8);
    const otherBox = Buffer.concat([Buffer.from("Ed", "utf8"), otherKeynum, otherRaw]);
    const otherText = [
      `untrusted comment: minisign public key: ${otherKeynum.toString("hex").toUpperCase()}`,
      otherBox.toString("base64"),
      "",
    ].join("\n");
    const otherPubkeyB64 = Buffer.from(otherText, "utf8").toString("base64");

    fs.writeFileSync(
      fix.configPath,
      JSON.stringify(
        {
          version: fix.version,
          plugins: { updater: { pubkey: otherPubkeyB64 } },
        },
        null,
        2,
      ),
    );

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, false);
    assert.ok(res.errors.some((e) => e.includes("unsigned or unknown key")));
  });

  it("(4) trusted comment version: != latest.json version fails", () => {
    const fix = createFixture(tmp);
    const latest = JSON.parse(fs.readFileSync(fix.latestJsonPath, "utf8"));
    latest.version = "0.9.9";
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(latest, null, 2));

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, false);
    assert.ok(res.errors.some((e) => e.includes("trusted comment does not contain version:0.9.9")));
  });

  it("(5) a tampered global signature fails", () => {
    const fix = createFixture(tmp);
    const parsed = parseMinisignSignature(fix.sigB64);
    const tamperedGlobal = Buffer.from(parsed.globalSignature);
    tamperedGlobal[0] ^= 0xff;

    const lines = [
      "untrusted comment: test signature",
      Buffer.concat([Buffer.from("Ed", "utf8"), fix.keynum, parsed.signature]).toString("base64"),
      parsed.trustedComment,
      tamperedGlobal.toString("base64"),
      "",
    ].join("\n");
    const tamperedSigB64 = Buffer.from(lines, "utf8").toString("base64");

    const latest = JSON.parse(fs.readFileSync(fix.latestJsonPath, "utf8"));
    latest.platforms["windows-x86_64"].signature = tamperedSigB64;
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(latest, null, 2));

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, false);
    assert.ok(res.errors.some((e) => e.includes("global signature verification failed")));
  });

  it("(6) installer >= maxBytes fails the size gate", () => {
    const fix = createFixture(tmp);
    const installerSize = fs.statSync(fix.installerPath).size;
    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
      maxBytes: installerSize,
    });
    assert.equal(res.ok, false);
    assert.ok(
      res.errors.some((e) => e.includes(`exceeds maximum of ${installerSize} bytes`)),
    );
  });

  it("(7) missing pub_date/signature/url fields each fail", () => {
    const fix = createFixture(tmp);
    const orig = JSON.parse(fs.readFileSync(fix.latestJsonPath, "utf8"));

    // Missing pub_date
    const noPubDate = { ...orig, pub_date: "" };
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(noPubDate, null, 2));
    assert.equal(
      verifyRelease({
        bundleDir: fix.bundleDir,
        configPath: fix.configPath,
        latestJsonPath: fix.latestJsonPath,
      }).ok,
      false,
    );

    // Missing signature
    const noSig = {
      ...orig,
      platforms: {
        "windows-x86_64": { ...orig.platforms["windows-x86_64"], signature: "" },
      },
    };
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(noSig, null, 2));
    assert.equal(
      verifyRelease({
        bundleDir: fix.bundleDir,
        configPath: fix.configPath,
        latestJsonPath: fix.latestJsonPath,
      }).ok,
      false,
    );

    // Missing url
    const noUrl = {
      ...orig,
      platforms: {
        "windows-x86_64": { ...orig.platforms["windows-x86_64"], url: "" },
      },
    };
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(noUrl, null, 2));
    assert.equal(
      verifyRelease({
        bundleDir: fix.bundleDir,
        configPath: fix.configPath,
        latestJsonPath: fix.latestJsonPath,
      }).ok,
      false,
    );
  });

  it("(8) missing latest.json or installer fails", () => {
    const fix = createFixture(tmp);

    // Missing latest.json
    const resNoLatest = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: path.join(fix.tmp, "nonexistent-latest.json"),
    });
    assert.equal(resNoLatest.ok, false);
    assert.ok(resNoLatest.errors.some((e) => e.includes("missing latest.json")));

    // Missing installer
    fs.rmSync(fix.installerPath);
    const resNoInstaller = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(resNoInstaller.ok, false);
    assert.ok(resNoInstaller.errors.some((e) => e.includes("no installer found")));
  });

  it("(9) parseUpdaterPubkey rejects a non-Ed algorithm", () => {
    const keynum = crypto.randomBytes(8);
    const rawKey = crypto.randomBytes(32);
    const badBox = Buffer.concat([Buffer.from("XX", "utf8"), keynum, rawKey]);
    const badPubText = [
      `untrusted comment: minisign public key: ${keynum.toString("hex").toUpperCase()}`,
      badBox.toString("base64"),
      "",
    ].join("\n");
    const badPubB64 = Buffer.from(badPubText, "utf8").toString("base64");

    assert.throws(
      () => parseUpdaterPubkey(badPubB64),
      /unsupported algorithm: expected Ed, got XX/,
    );
  });

  it("(10) the url must name the installer file", () => {
    const fix = createFixture(tmp);
    const latest = JSON.parse(fs.readFileSync(fix.latestJsonPath, "utf8"));
    latest.platforms["windows-x86_64"].url = "https://example.com/download/other-name.exe";
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(latest, null, 2));

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, false);
    assert.ok(
      res.errors.some((e) => e.includes(`must end with installer file name "${fix.installerName}"`)),
    );
  });

  it("(11) latest.json resolution: --latest-json wins, then <bundleDir>/latest.json, then ./latest.json, and missing in all three fails", () => {
    const fix = createFixture(tmp);
    const explicitPath = path.join(tmp, "custom-latest.json");
    fs.copyFileSync(fix.latestJsonPath, explicitPath);

    // 1. explicit path wins
    const winExplicit = resolveLatestJsonPath({
      bundleDir: fix.bundleDir,
      latestJsonPath: explicitPath,
    });
    assert.equal(winExplicit, path.resolve(explicitPath));

    // 2. bundleDir/latest.json wins over root fallback when latestJsonPath not given
    const winBundle = resolveLatestJsonPath({ bundleDir: fix.bundleDir });
    assert.equal(winBundle, path.resolve(fix.latestJsonPath));

    // 3. fallback to ./latest.json when bundleDir has no latest.json
    fs.rmSync(fix.latestJsonPath);
    const emptyBundle = path.join(tmp, "empty-bundle");
    fs.mkdirSync(emptyBundle, { recursive: true });
    const winRoot = resolveLatestJsonPath({ bundleDir: emptyBundle });
    assert.equal(winRoot, path.resolve("./latest.json"));

    // 4. missing in all three fails verifyRelease
    const res = verifyRelease({
      bundleDir: emptyBundle,
      configPath: fix.configPath,
      latestJsonPath: path.join(tmp, "missing-everywhere.json"),
    });
    assert.equal(res.ok, false);
    assert.ok(res.errors.some((e) => e.includes("missing latest.json")));
  });

  it("(12) --from-sig builds a latest.json from the .sig sidecar that verifies ok, while the same bundle without --from-sig (no latest.json anywhere) fails", () => {
    const fix = createFixture(tmp);
    fs.rmSync(fix.latestJsonPath);

    const nonExistentPath = path.join(tmp, "does-not-exist-latest.json");

    // without fromSig -> fails because latest.json is missing
    const resWithout = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: nonExistentPath,
      fromSig: false,
    });
    assert.equal(resWithout.ok, false);
    assert.ok(resWithout.errors.some((e) => e.includes("missing latest.json")));

    // with fromSig -> synthesizes latest.json and verifies ok
    const resWith = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      fromSig: true,
    });
    assert.equal(resWith.ok, true);
    assert.deepEqual(resWith.errors, []);
    assert.ok(resWith.installerBytes > 0);
  });

  it("(13) accepts empty notes string in latest.json", () => {
    const fix = createFixture(tmp);
    const latest = JSON.parse(fs.readFileSync(fix.latestJsonPath, "utf8"));
    latest.notes = "";
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(latest, null, 2));

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, true);
    assert.deepEqual(res.errors, []);
  });

  it("(14) missing notes in latest.json fails", () => {
    const fix = createFixture(tmp);
    const latest = JSON.parse(fs.readFileSync(fix.latestJsonPath, "utf8"));
    delete latest.notes;
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(latest, null, 2));

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, false);
    assert.ok(res.errors.some((e) => e.includes("latest.json missing notes")));
  });

  it("(15) accepts api.github.com release asset URL", () => {
    const fix = createFixture(tmp);
    const latest = JSON.parse(fs.readFileSync(fix.latestJsonPath, "utf8"));
    latest.platforms["windows-x86_64"].url =
      "https://api.github.com/repos/davidef02/proxi/releases/assets/775799807";
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(latest, null, 2));

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, true);
    assert.deepEqual(res.errors, []);
  });

  it("(16) accepts real tauri-action latest.json format with empty notes, api.github.com asset url and both windows-x86_64 and windows-x86_64-nsis", () => {
    const fix = createFixture(tmp);
    const realLatest = {
      version: fix.version,
      notes: "",
      pub_date: new Date().toISOString(),
      platforms: {
        "windows-x86_64": {
          signature: fix.sigB64,
          url: "https://api.github.com/repos/davidef02/proxi/releases/assets/775799807",
        },
        "windows-x86_64-nsis": {
          signature: fix.sigB64,
          url: "https://api.github.com/repos/davidef02/proxi/releases/assets/775799807",
        },
      },
    };
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(realLatest, null, 2));

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, true);
    assert.deepEqual(res.errors, []);
  });

  it("(17) multi-platform latest.json fails if windows-x86_64-nsis has invalid url", () => {
    const fix = createFixture(tmp);
    const realLatest = {
      version: fix.version,
      notes: "",
      pub_date: new Date().toISOString(),
      platforms: {
        "windows-x86_64": {
          signature: fix.sigB64,
          url: "https://api.github.com/repos/davidef02/proxi/releases/assets/775799807",
        },
        "windows-x86_64-nsis": {
          signature: fix.sigB64,
          url: "https://example.com/download/other-name.exe",
        },
      },
    };
    fs.writeFileSync(fix.latestJsonPath, JSON.stringify(realLatest, null, 2));

    const res = verifyRelease({
      bundleDir: fix.bundleDir,
      configPath: fix.configPath,
      latestJsonPath: fix.latestJsonPath,
    });
    assert.equal(res.ok, false);
    assert.ok(
      res.errors.some((e) => e.includes(`must end with installer file name "${fix.installerName}"`)),
    );
  });

  it("(18) isValidPlatformUrl validates installer filenames and GitHub asset URLs correctly", () => {
    assert.equal(isValidPlatformUrl("proxi-setup.exe", "proxi-setup.exe"), true);
    assert.equal(
      isValidPlatformUrl("https://example.com/download/proxi-setup.exe", "proxi-setup.exe"),
      true,
    );
    assert.equal(
      isValidPlatformUrl(
        "https://api.github.com/repos/davidef02/proxi/releases/assets/775799807",
        "proxi-setup.exe",
      ),
      true,
    );
    assert.equal(
      isValidPlatformUrl(
        "https://api.github.com/repos/user-name/repo.name/releases/assets/12345",
        "proxi-setup.exe",
      ),
      true,
    );
    assert.equal(
      isValidPlatformUrl("https://example.com/download/other.exe", "proxi-setup.exe"),
      false,
    );
    assert.equal(
      isValidPlatformUrl(
        "https://api.github.com/repos/user/repo/releases/assets/notanumber",
        "proxi-setup.exe",
      ),
      false,
    );
    assert.equal(isValidPlatformUrl("", "proxi-setup.exe"), false);
  });
});
