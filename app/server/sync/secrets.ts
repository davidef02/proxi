// Sync tokens: <repo>/.learner/secrets.json. Encrypted through an injected SecretCipher (Electron safeStorage in the
// desktop app); plain Node has no cipher and stores plaintext. Never written under dataRoot, never tracked by git.
import * as fs from "node:fs";
import * as path from "node:path";

export interface SecretCipher {
  available(): Promise<boolean>;
  encrypt(plain: string): Promise<string>;
  decrypt(b64: string): Promise<string>;
}

export interface GithubSecret { token: string; login: string; repo: string; branch: string }
export interface GdriveSecret { refreshToken: string; folderId: string; email?: string }
export interface Secrets { github?: GithubSecret; gdrive?: GdriveSecret }

// Encrypted by the desktop app and no cipher here: the token is still readable there, so connect must not overwrite it.
const LOCKED_ERROR = "I token di sincronizzazione sono cifrati dall'app desktop: connetti da lì.";
// Corrupt, or encrypted with a key that no longer exists: nothing can read it, so a new connect may replace it.
const UNREADABLE_ERROR = "Il file dei token non è leggibile: riconnetti.";

export const secretsFile = (repoRoot: string) => path.join(repoRoot, ".learner", "secrets.json");

/** True only when a cipher exists and reports that encryption works. Never throws. */
export async function canEncrypt(cipher?: SecretCipher): Promise<boolean> {
  if (!cipher) return false;
  try {
    return await cipher.available();
  } catch {
    return false;
  }
}

/** `locked` is set only when the file is encrypted and this process has no cipher to read it (plain Node). */
export async function readSecrets(
  repoRoot: string,
  cipher?: SecretCipher,
): Promise<{ secrets: Secrets; error?: string; locked?: boolean }> {
  const f = secretsFile(repoRoot);
  if (!fs.existsSync(f)) return { secrets: {} };
  try {
    const raw = JSON.parse(fs.readFileSync(f, "utf8")) as { v?: unknown; encrypted?: unknown; data?: unknown };
    if (typeof raw.data !== "string") throw new Error("secrets file malformed");
    let plain: string;
    if (raw.encrypted === true) {
      if (!cipher || !(await canEncrypt(cipher))) return { secrets: {}, error: LOCKED_ERROR, locked: true };
      plain = await cipher.decrypt(raw.data);
    } else {
      plain = raw.data;
    }
    return { secrets: JSON.parse(plain) as Secrets };
  } catch {
    return { secrets: {}, error: UNREADABLE_ERROR };
  }
}

export async function writeSecrets(repoRoot: string, secrets: Secrets, cipher?: SecretCipher): Promise<{ encrypted: boolean }> {
  const f = secretsFile(repoRoot);
  const json = JSON.stringify(secrets); // drops undefined keys
  if (Object.keys(JSON.parse(json) as object).length === 0) {
    fs.rmSync(f, { force: true });
    return { encrypted: false };
  }
  const usable = (await canEncrypt(cipher)) ? cipher : undefined;
  const encrypted = usable !== undefined;
  const data = usable ? await usable.encrypt(json) : json;
  fs.mkdirSync(path.dirname(f), { recursive: true });
  const body = JSON.stringify({ v: 1, encrypted, data }, null, 2) + "\n";
  const tmp = `${f}.${process.pid}.tmp`;
  try {
    fs.writeFileSync(tmp, body);
    fs.renameSync(tmp, f);
  } catch {
    // Windows: rename over a file another process holds open can fail with EPERM/EBUSY: write in place instead.
    fs.rmSync(tmp, { force: true });
    fs.writeFileSync(f, body);
  }
  if (process.platform !== "win32") {
    try {
      fs.chmodSync(f, 0o600);
    } catch {
      // best effort: the file is still inside .learner/, which is never synced
    }
  }
  return { encrypted };
}
