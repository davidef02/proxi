import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SELF = fileURLToPath(import.meta.url);
export const MAX_INSTALLER_BYTES = 15000000;

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const RFC3339_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;

export function spkiFromRawEd25519(raw32) {
  const raw = Buffer.isBuffer(raw32) ? raw32 : Buffer.from(raw32);
  if (raw.length !== 32) {
    throw new Error(`expected 32-byte Ed25519 raw key, got ${raw.length}`);
  }
  return Buffer.concat([ED25519_SPKI_PREFIX, raw]);
}

export function ed25519Verify(key32, data, sig64) {
  const spki = spkiFromRawEd25519(key32);
  const keyObj = crypto.createPublicKey({ key: spki, format: "der", type: "spki" });
  return crypto.verify(null, data, keyObj, sig64);
}

export function parseUpdaterPubkey(pubkeyB64) {
  if (typeof pubkeyB64 !== "string") {
    throw new Error("pubkey must be a string");
  }
  let text = pubkeyB64;
  try {
    const decoded = Buffer.from(pubkeyB64, "base64").toString("utf8");
    if (decoded.includes("untrusted comment:")) {
      text = decoded;
    }
  } catch {}
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  const commentIdx = lines.findIndex((l) => l.startsWith("untrusted comment:"));
  if (commentIdx === -1 || commentIdx + 1 >= lines.length) {
    throw new Error("invalid minisign pubkey: missing base64 line after untrusted comment");
  }
  const b64Line = lines[commentIdx + 1];
  const buf = Buffer.from(b64Line, "base64");
  if (buf.length < 42) {
    throw new Error(`invalid minisign pubkey length: expected 42 bytes, got ${buf.length}`);
  }
  const alg = buf.subarray(0, 2).toString("utf8");
  if (alg !== "Ed") {
    throw new Error(`unsupported algorithm: expected Ed, got ${alg}`);
  }
  const keynum = buf.subarray(2, 10);
  const key = buf.subarray(10, 42);
  return { key, keynum };
}

export function parseMinisignSignature(sigB64) {
  if (typeof sigB64 !== "string") {
    throw new Error("signature must be a string");
  }
  let text = sigB64;
  for (let i = 0; i < 3; i += 1) {
    if (text.includes("untrusted comment:") || text.includes("trusted comment:")) break;
    try {
      const decoded = Buffer.from(text, "base64").toString("utf8");
      if (decoded && decoded !== text) {
        text = decoded;
      } else {
        break;
      }
    } catch {
      break;
    }
  }
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  if (lines.length < 4) {
    throw new Error(`invalid minisign signature: expected 4 lines, got ${lines.length}`);
  }
  const line2Buf = Buffer.from(lines[1], "base64");
  if (line2Buf.length < 74) {
    throw new Error(`invalid minisign signature line 2: expected 74 bytes, got ${line2Buf.length}`);
  }
  const alg = line2Buf.subarray(0, 2).toString("utf8");
  const keynum = line2Buf.subarray(2, 10);
  const signature = line2Buf.subarray(10, 74);
  const trustedComment = lines[2];
  const line4Buf = Buffer.from(lines[3], "base64");
  if (line4Buf.length !== 64) {
    throw new Error(`invalid minisign global signature length: expected 64 bytes, got ${line4Buf.length}`);
  }
  const globalSignature = line4Buf;
  return { signature, trustedComment, globalSignature, keynum, alg };
}

function walkFiles(dir) {
  const results = [];
  if (!fs.existsSync(dir)) return results;
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      results.push(...walkFiles(full));
    } else {
      results.push(full);
    }
  }
  return results;
}

export function findReleaseArtifacts(bundleDir) {
  if (!bundleDir || !fs.existsSync(bundleDir)) {
    return { installerPath: null, sigPath: null };
  }
  const allFiles = walkFiles(bundleDir);
  const exes = allFiles.filter((f) => {
    const base = path.basename(f).toLowerCase();
    return base.endsWith(".exe") && !base.startsWith("uninstall");
  });

  let installerPath = null;
  const setupExes = exes.filter((f) => path.basename(f).toLowerCase().endsWith("-setup.exe"));
  if (setupExes.length > 0) {
    setupExes.sort((a, b) => fs.statSync(b).size - fs.statSync(a).size);
    installerPath = setupExes[0];
  } else if (exes.length > 0) {
    exes.sort((a, b) => fs.statSync(b).size - fs.statSync(a).size);
    installerPath = exes[0];
  }

  let sigPath = null;
  if (installerPath && fs.existsSync(`${installerPath}.sig`)) {
    sigPath = `${installerPath}.sig`;
  } else {
    const sigs = allFiles.filter((f) => f.toLowerCase().endsWith(".sig"));
    if (installerPath) {
      const match = sigs.find((s) => path.basename(s) === `${path.basename(installerPath)}.sig`);
      sigPath = match ?? sigs[0] ?? null;
    } else {
      sigPath = sigs[0] ?? null;
    }
  }

  return { installerPath, sigPath };
}

export function resolveLatestJsonPath({ bundleDir, latestJsonPath } = {}) {
  if (latestJsonPath) {
    return path.resolve(latestJsonPath);
  }
  if (bundleDir) {
    const bundleCandidate = path.join(bundleDir, "latest.json");
    if (fs.existsSync(bundleCandidate)) {
      return path.resolve(bundleCandidate);
    }
  }
  return path.resolve("./latest.json");
}

export function latestJsonFromSig({ sigPath, installerName, configPath }) {
  const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
  const version = config.version || "0.0.0";
  const rawSig = fs.readFileSync(sigPath);
  const rawStr = rawSig.toString("utf8").trim();
  const signature = rawStr.startsWith("untrusted comment:") ? rawSig.toString("base64") : rawStr;
  return {
    version,
    notes: `Release ${version}`,
    pub_date: new Date().toISOString(),
    platforms: {
      "windows-x86_64": {
        signature,
        url: installerName,
      },
    },
  };
}

export function verifyRelease({
  bundleDir,
  configPath,
  maxBytes = MAX_INSTALLER_BYTES,
  latestJsonPath,
  fromSig = false,
}) {
  const errors = [];
  let pubkey = null;

  if (!configPath || !fs.existsSync(configPath)) {
    errors.push(`config not found: ${configPath}`);
  } else {
    try {
      const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
      const pubkeyStr = config?.plugins?.updater?.pubkey;
      if (!pubkeyStr || typeof pubkeyStr !== "string") {
        errors.push(`missing plugins.updater.pubkey in ${configPath}`);
      } else {
        pubkey = parseUpdaterPubkey(pubkeyStr);
      }
    } catch (err) {
      errors.push(`failed reading config pubkey: ${err.message}`);
    }
  }

  const { installerPath, sigPath } = findReleaseArtifacts(bundleDir);
  let installerBytes = 0;
  if (!installerPath || !fs.existsSync(installerPath)) {
    errors.push(`no installer found in bundle dir: ${bundleDir}`);
  } else {
    installerBytes = fs.statSync(installerPath).size;
    if (installerBytes >= maxBytes) {
      errors.push(`installer size ${installerBytes} exceeds maximum of ${maxBytes} bytes`);
    }
  }

  let latestJson = null;
  if (fromSig) {
    if (!sigPath || !fs.existsSync(sigPath)) {
      errors.push(`missing .sig sidecar for fromSig mode in ${bundleDir}`);
    } else if (!installerPath) {
      errors.push("missing installer for fromSig mode");
    } else if (!configPath || !fs.existsSync(configPath)) {
      errors.push(`missing config for fromSig mode: ${configPath}`);
    } else {
      try {
        latestJson = latestJsonFromSig({
          sigPath,
          installerName: path.basename(installerPath),
          configPath,
        });
      } catch (err) {
        errors.push(`failed synthesizing latest.json from sig: ${err.message}`);
      }
    }
  } else {
    const resolvedPath = resolveLatestJsonPath({ bundleDir, latestJsonPath });
    if (!resolvedPath || !fs.existsSync(resolvedPath)) {
      errors.push(`missing latest.json${resolvedPath ? `: ${resolvedPath}` : ""}`);
    } else {
      try {
        latestJson = JSON.parse(fs.readFileSync(resolvedPath, "utf8"));
      } catch (err) {
        errors.push(`failed parsing latest.json: ${err.message}`);
      }
    }
  }

  if (latestJson) {
    if (typeof latestJson.version !== "string" || latestJson.version.length === 0) {
      errors.push("latest.json missing version");
    }
    if (typeof latestJson.notes !== "string" || latestJson.notes.length === 0) {
      errors.push("latest.json missing notes");
    }
    if (
      typeof latestJson.pub_date !== "string" ||
      !RFC3339_RE.test(latestJson.pub_date) ||
      Number.isNaN(Date.parse(latestJson.pub_date))
    ) {
      errors.push("latest.json invalid or missing pub_date");
    }

    const platform = latestJson.platforms?.["windows-x86_64"];
    if (!platform) {
      errors.push('latest.json missing platforms."windows-x86_64"');
    } else {
      if (typeof platform.signature !== "string" || platform.signature.length === 0) {
        errors.push('latest.json missing platforms."windows-x86_64".signature');
      }
      if (typeof platform.url !== "string" || platform.url.length === 0) {
        errors.push('latest.json missing platforms."windows-x86_64".url');
      } else if (installerPath) {
        const expectedName = path.basename(installerPath);
        if (!platform.url.endsWith(expectedName)) {
          errors.push(`url "${platform.url}" must end with installer file name "${expectedName}"`);
        }
      }

      if (pubkey && platform.signature && installerPath && fs.existsSync(installerPath)) {
        try {
          const parsed = parseMinisignSignature(platform.signature);
          const installerData = fs.readFileSync(installerPath);
          let fileOk = false;
          if (parsed.alg === "ED") {
            const hash = crypto.createHash("blake2b512").update(installerData).digest();
            fileOk = ed25519Verify(pubkey.key, hash, parsed.signature);
          } else if (parsed.alg === "Ed") {
            fileOk = ed25519Verify(pubkey.key, installerData, parsed.signature);
          } else {
            const hash = crypto.createHash("blake2b512").update(installerData).digest();
            fileOk =
              ed25519Verify(pubkey.key, installerData, parsed.signature) ||
              ed25519Verify(pubkey.key, hash, parsed.signature);
          }
          if (!fileOk) {
            errors.push("unsigned or unknown key");
          }

          const versionTag = `version:${latestJson.version}`;
          if (!parsed.trustedComment || !parsed.trustedComment.includes(versionTag)) {
            errors.push(`trusted comment does not contain ${versionTag}`);
          }

          let globalOk = false;
          const variants = [
            parsed.trustedComment,
            parsed.trustedComment.replace(/^trusted comment:\s*/, ""),
            `trusted comment: ${parsed.trustedComment.replace(/^trusted comment:\s*/, "")}`,
          ];
          for (const variant of variants) {
            const msg = Buffer.concat([parsed.signature, Buffer.from(variant, "utf8")]);
            if (ed25519Verify(pubkey.key, msg, parsed.globalSignature)) {
              globalOk = true;
              break;
            }
          }
          if (!globalOk) {
            errors.push("global signature verification failed");
          }
        } catch (err) {
          errors.push(`signature verification error: ${err.message}`);
        }
      }
    }
  }

  return {
    ok: errors.length === 0,
    errors,
    installerBytes,
  };
}

function printUsage() {
  console.log(`Usage: node scripts/verify-release.mjs --bundle-dir <dir> --config <path> [options]

Options:
  --bundle-dir <dir>       Path to bundle directory containing installer
  --config <path>           Path to tauri.conf.json
  --max-bytes <number>      Maximum installer size in bytes (default: ${MAX_INSTALLER_BYTES})
  --latest-json <path>      Path to latest.json
  --from-sig                Synthesize latest.json from .sig sidecar
`);
}

function main() {
  const args = {
    bundleDir: "",
    configPath: "",
    maxBytes: MAX_INSTALLER_BYTES,
    latestJsonPath: "",
    fromSig: false,
  };

  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--from-sig") {
      args.fromSig = true;
    } else if (flag === "--bundle-dir") {
      args.bundleDir = argv[i + 1] ?? "";
      i += 1;
    } else if (flag === "--config") {
      args.configPath = argv[i + 1] ?? "";
      i += 1;
    } else if (flag === "--max-bytes") {
      args.maxBytes = Number(argv[i + 1]) || MAX_INSTALLER_BYTES;
      i += 1;
    } else if (flag === "--latest-json") {
      args.latestJsonPath = argv[i + 1] ?? "";
      i += 1;
    } else if (flag === "--help" || flag === "-h") {
      printUsage();
      process.exitCode = 0;
      return;
    }
  }

  if (!args.bundleDir || !args.configPath) {
    console.error("error: --bundle-dir and --config are required");
    printUsage();
    process.exitCode = 1;
    return;
  }

  const result = verifyRelease(args);
  if (!result.ok) {
    console.error("Release verification failed:");
    for (const err of result.errors) {
      console.error(`  - ${err}`);
    }
    process.exitCode = 1;
  } else {
    console.log(`Release verification passed (${result.installerBytes} bytes)`);
    process.exitCode = 0;
  }
}

const entry = process.argv[1] ? path.resolve(process.argv[1]) : "";
const isMain =
  entry !== "" &&
  (process.platform === "win32"
    ? entry.toLowerCase() === SELF.toLowerCase()
    : entry === SELF);
if (isMain) main();
