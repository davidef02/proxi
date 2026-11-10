import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  canEncrypt,
  readSecrets,
  secretsFile,
  writeSecrets,
  type SecretCipher,
  type Secrets,
} from "../server/sync/secrets.ts";

// Fake cipher: "encryption" = base64 of the reversed string. Enough to prove the token is not stored as plaintext.
const fakeCipher = (ok = true): SecretCipher => ({
  available: async () => ok,
  encrypt: async (plain) => Buffer.from(plain.split("").reverse().join(""), "utf8").toString("base64"),
  decrypt: async (b64) => Buffer.from(b64, "base64").toString("utf8").split("").reverse().join(""),
});

const sample: Secrets = {
  github: { token: "gho_SECRET_TOKEN_123", login: "davide", repo: "davide/proxi-data", branch: "main" },
};

function tempRepo(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "ia-secrets-"));
}

test("round trip with a cipher: encrypted flag set, token not on disk in clear", async () => {
  const repo = tempRepo();
  try {
    const w = await writeSecrets(repo, sample, fakeCipher());
    assert.equal(w.encrypted, true);
    const onDisk = fs.readFileSync(secretsFile(repo), "utf8");
    assert.equal(onDisk.includes("gho_SECRET_TOKEN_123"), false);
    assert.equal(JSON.parse(onDisk).encrypted, true);
    assert.equal(JSON.parse(onDisk).v, 1);
    const r = await readSecrets(repo, fakeCipher());
    assert.equal(r.error, undefined);
    assert.deepEqual(r.secrets, sample);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("no cipher: plaintext with encrypted false, and it reads back", async () => {
  const repo = tempRepo();
  try {
    const w = await writeSecrets(repo, sample);
    assert.equal(w.encrypted, false);
    const onDisk = fs.readFileSync(secretsFile(repo), "utf8");
    assert.equal(onDisk.includes("gho_SECRET_TOKEN_123"), true);
    const r = await readSecrets(repo);
    assert.deepEqual(r.secrets, sample);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("cipher present but unavailable: falls back to plaintext", async () => {
  const repo = tempRepo();
  try {
    const w = await writeSecrets(repo, sample, fakeCipher(false));
    assert.equal(w.encrypted, false);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("encrypted file read without a cipher: error string and empty secrets", async () => {
  const repo = tempRepo();
  try {
    await writeSecrets(repo, sample, fakeCipher());
    const r = await readSecrets(repo);
    assert.deepEqual(r.secrets, {});
    assert.equal(r.error, "I token di sincronizzazione sono cifrati dall'app desktop: connetti da lì.");
    assert.equal(r.locked, true);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("encrypted file whose key no longer decrypts: unreadable, not locked (a connect may replace it)", async () => {
  const repo = tempRepo();
  try {
    await writeSecrets(repo, sample, fakeCipher());
    const brokenKey: SecretCipher = {
      ...fakeCipher(),
      decrypt: async () => {
        throw new Error("decryptString failed");
      },
    };
    const r = await readSecrets(repo, brokenKey);
    assert.deepEqual(r.secrets, {});
    assert.equal(r.error, "Il file dei token non è leggibile: riconnetti.");
    assert.equal(r.locked, undefined);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("missing file reads as empty secrets with no error", async () => {
  const repo = tempRepo();
  try {
    assert.deepEqual(await readSecrets(repo), { secrets: {} });
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});

test("canEncrypt: false without a cipher, false when available() is false, never throws", async () => {
  assert.equal(await canEncrypt(undefined), false);
  assert.equal(await canEncrypt(fakeCipher(false)), false);
  assert.equal(await canEncrypt(fakeCipher(true)), true);
  const broken: SecretCipher = {
    available: async () => {
      throw new Error("boom");
    },
    encrypt: async () => "",
    decrypt: async () => "",
  };
  assert.equal(await canEncrypt(broken), false);
});

test("writing an empty Secrets deletes the file", async () => {
  const repo = tempRepo();
  try {
    await writeSecrets(repo, sample);
    assert.equal(fs.existsSync(secretsFile(repo)), true);
    const w = await writeSecrets(repo, {});
    assert.equal(w.encrypted, false);
    assert.equal(fs.existsSync(secretsFile(repo)), false);
  } finally {
    fs.rmSync(repo, { recursive: true, force: true });
  }
});
