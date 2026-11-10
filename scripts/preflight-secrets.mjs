// Publication pre-flight: export HEAD with git archive, then grep and scan the export for secrets
// and personal data. All patterns are assembled at runtime because this file is itself scanned by
// the same checks it runs; a literal token here would be a self-match.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SELF = fileURLToPath(import.meta.url);
const ENV_NAME = "." + "env";

export const ALLOWED_EMAIL = "114928369+davidef02@users.noreply.github.com";

export const PERSONAL_PATH_RE = new RegExp(
  ["C:", "\\\\", "Users", "\\\\", "|/", "Users/|/", "home/[a-zA-Z0-9_-]"].join(""),
);
export const EMAIL_RE = /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g;
const SYNTHETIC_SK_RE = new RegExp(
  [
    "(?<![A-Za-z0-9_-])",
    "sk",
    "-ant-(?:test-123456789|api03-(?:1234567890|real-secret-key-12345|super-secret-key-xyz|another-secret-token-987|spawn-secret-123)|1234|\\.\\.\\.|…7890|…1234)",
  ].join(""),
  "g",
);
const SYNTHETIC_GHO_RE = new RegExp(
  ["(?<![A-Za-z0-9_-])", "gho", "_(?:x|desktop|CLEAR_TOKEN|SECRET_TOKEN_123)(?![A-Za-z0-9_])"].join(""),
  "g",
);
// the lookbehind keeps "mask-image" and "task-list-item" out of the short-key rule
const KEY_PATTERN_RE = new RegExp(
  [
    "(?<![A-Za-z0-9_-])(?:",
    [["sk", "-"], ["ghp", "_"], ["gho", "_"], ["github_pat", "_"], ["AI", "za"], ["xo", "x"]]
      .map(([a, b]) => a + b)
      .join("|"),
    "|",
    ["-----", "BEGIN"].join(""),
    ")",
  ].join(""),
);

function listFiles(rootDir) {
  const files = [];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else files.push(path.relative(rootDir, full).split(path.sep).join("/"));
    }
  };
  walk(rootDir);
  return files.sort();
}

function looksBinary(file) {
  const fd = fs.openSync(file, "r");
  try {
    const buffer = Buffer.alloc(8000);
    const read = fs.readSync(fd, buffer, 0, buffer.length, 0);
    return buffer.subarray(0, read).includes(0);
  } finally {
    fs.closeSync(fd);
  }
}

export function exportTree(repoRoot, destDir, ref = "HEAD") {
  const treeHash = execFileSync("git", ["rev-parse", `${ref}^{tree}`], {
    cwd: repoRoot,
    encoding: "utf8",
  }).trim();
  fs.mkdirSync(destDir, { recursive: true });
  const tmpTar = path.join(os.tmpdir(), `proxi-export-${process.pid}-${Date.now()}.tar`);
  try {
    execFileSync("git", ["archive", "--format=tar", "-o", tmpTar, ref], { cwd: repoRoot, stdio: "pipe" });
    execFileSync("tar", ["-xf", tmpTar, "-C", destDir], { stdio: "pipe" });
  } finally {
    fs.rmSync(tmpTar, { force: true });
  }
  return { treeHash, destDir };
}

export function scanDirectory(rootDir, treeHash = "") {
  const hits = [];
  const counts = { envFiles: 0, personalPaths: 0, emails: 0, keyPatterns: 0 };
  const add = (file, line, category, match) => {
    hits.push({ file, line, category, match });
    if (category === "env-file") counts.envFiles += 1;
    else if (category === "personal-path") counts.personalPaths += 1;
    else if (category === "email") counts.emails += 1;
    else counts.keyPatterns += 1;
  };

  for (const relPath of listFiles(rootDir)) {
    const base = path.posix.basename(relPath);
    if (base === ENV_NAME || base.startsWith(ENV_NAME + ".")) {
      add(relPath, null, "env-file", base);
    }
    const full = path.join(rootDir, ...relPath.split("/"));
    if (looksBinary(full)) continue;

    const lines = fs.readFileSync(full, "utf8").split(/\r?\n/);
    for (let i = 0; i < lines.length; i += 1) {
      const line = lines[i];
      // .gitignore names the ignored folders on purpose; a rule is not a leaked path
      if (relPath !== ".gitignore") {
        const match = line.match(PERSONAL_PATH_RE);
        if (match) add(relPath, i + 1, "personal-path", match[0]);
      }

      for (const email of line.matchAll(EMAIL_RE)) {
        if (email[0] === ALLOWED_EMAIL || /^[^@]+@example\.com$/i.test(email[0])) continue;
        add(relPath, i + 1, "email", email[0]);
      }

      // .gitleaks.toml holds the allowlist regexes themselves, so it is expected to name key prefixes
      if (relPath !== ".gitleaks.toml") {
        const stripped = line.replace(SYNTHETIC_SK_RE, "").replace(SYNTHETIC_GHO_RE, "");
        const match = stripped.match(KEY_PATTERN_RE);
        if (match) add(relPath, i + 1, "key-pattern", match[0]);
      }
    }
  }

  return {
    ok: hits.length === 0,
    treeHash,
    exitCode: hits.length === 0 ? 0 : 1,
    findings: hits,
    hits,
    counts,
  };
}

export function runGitleaks(
  targetDir,
  reportPath,
  treeHash = "",
  configPath = path.join(targetDir, ".gitleaks.toml"),
) {
  fs.mkdirSync(path.dirname(reportPath), { recursive: true });
  const rawReportPath = `${reportPath}.raw.json`;
  const result = spawnSync(process.env.GITLEAKS_BIN || "gitleaks", [
    "dir",
    targetDir,
    "--config",
    configPath,
    "--report-format",
    "json",
    "--report-path",
    rawReportPath,
  ], { encoding: "utf8" });

  let findings = null;
  try {
    const raw = fs.readFileSync(rawReportPath, "utf8").trim();
    findings = raw ? JSON.parse(raw) : null;
  } catch {
    // no report file: scanner missing or crashed, keep findings null so ok stays false
  }
  fs.rmSync(rawReportPath, { force: true });

  const report = {
    treeHash,
    exitCode: result.status ?? -1,
    findingsCount: Array.isArray(findings) ? findings.length : -1,
    findings,
  };
  fs.writeFileSync(reportPath, JSON.stringify(report, null, 2) + "\n");
  return {
    ok: result.status === 0 && Array.isArray(findings) && findings.length === 0,
    ...report,
  };
}

export function runTrufflehog(targetDir, reportPath, treeHash = "") {
  fs.mkdirSync(path.dirname(reportPath), { recursive: true });
  const result = spawnSync(process.env.TRUFFLEHOG_BIN || "trufflehog", [
    "filesystem",
    targetDir,
    "--json",
    "--fail",
    "--no-update",
  ], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });

  const findings = (result.stdout ?? "")
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => {
      try {
        return JSON.parse(line);
      } catch {
        return null;
      }
    })
    .filter(Boolean);

  const report = {
    treeHash,
    exitCode: result.status ?? -1,
    findingsCount: findings.length,
    findings,
  };
  fs.writeFileSync(reportPath, JSON.stringify(report, null, 2) + "\n");
  return { ok: result.status === 0 && findings.length === 0, ...report };
}

export function validateReviewReports(reportPaths, expectedTreeHash) {
  const errors = [];
  const reviews = [];
  const paths = Array.isArray(reportPaths) ? reportPaths : [];
  if (paths.length !== 2) errors.push(`expected 2 review reports, got ${paths.length}`);

  for (const reportPath of paths) {
    let review;
    try {
      review = JSON.parse(fs.readFileSync(reportPath, "utf8"));
    } catch (error) {
      errors.push(`${reportPath}: ${error.message}`);
      reviews.push(null);
      continue;
    }
    reviews.push(review);
    if (review?.treeHash !== expectedTreeHash) {
      errors.push(`${reportPath}: treeHash ${review?.treeHash ?? "missing"} != ${expectedTreeHash}`);
    }
    if (typeof review?.provider !== "string" || review.provider.length === 0) {
      errors.push(`${reportPath}: provider missing`);
    }
    if (typeof review?.model !== "string" || review.model.length === 0) {
      errors.push(`${reportPath}: model missing`);
    }
    if (review?.verdict !== "no findings") {
      errors.push(`${reportPath}: verdict ${review?.verdict ?? "missing"} != "no findings"`);
    }
    if (!Array.isArray(review?.findings) || review.findings.length !== 0) {
      errors.push(`${reportPath}: findings must be []`);
    }
  }

  if (reviews.length === 2 && reviews[0] && reviews[1] && reviews[0].provider === reviews[1].provider) {
    errors.push("reviews must come from two different providers");
  }
  return { ok: errors.length === 0, errors, reviews };
}

export function plantFakeSecret(scratchDir) {
  fs.mkdirSync(scratchDir, { recursive: true });
  const file = path.join(scratchDir, "planted-leak.txt");
  const lines = [
    ["ghp", "_", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"].join(""),
    ["-----", "BEGIN RSA PRIVATE KEY", "-----"].join(""),
    ["C:", "\\", "Users", "\\", "leak", "\\", "id_rsa"].join(""),
    ["leak", "@", "private-domain.invalid"].join(""),
  ];
  fs.writeFileSync(file, lines.join("\n") + "\n");
  return file;
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(value, null, 2) + "\n");
}

function headTree() {
  return execFileSync("git", ["rev-parse", "HEAD^{tree}"], { cwd: ROOT, encoding: "utf8" }).trim();
}

function exportOnly(args) {
  let exportDir = args.exportDir;
  if (exportDir) {
    // never wipe an existing dir: a fat-fingered path could point at the repo or home
    if (fs.existsSync(exportDir) && fs.readdirSync(exportDir).length > 0) {
      throw new Error(`--export-dir ${exportDir} must be empty or missing`);
    }
  } else {
    exportDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-export-"));
  }

  const { treeHash } = exportTree(ROOT, exportDir);
  fs.writeFileSync(path.join(args.evidenceDir, "tree-hash.txt"), `${treeHash}\n`);

  const gitleaks = runGitleaks(exportDir, path.join(args.evidenceDir, "gitleaks.json"), treeHash);
  const trufflehog = runTrufflehog(exportDir, path.join(args.evidenceDir, "trufflehog.json"), treeHash);
  const grep = scanDirectory(exportDir, treeHash);
  writeJson(path.join(args.evidenceDir, "grep.json"), {
    ...grep,
    findingsCount: grep.findings.length,
  });

  const ok = gitleaks.ok && trufflehog.ok && grep.ok;
  console.log(`${ok ? "preflight export clean" : "preflight export FAILED"}: tree ${treeHash}`);
  console.log(`export: ${exportDir}`);
  for (const [name, report] of [["gitleaks", gitleaks], ["trufflehog", trufflehog], ["grep", grep]]) {
    const count = typeof report.findingsCount === "number" ? report.findingsCount : report.findings.length;
    console.log(`  ${name}: exit ${report.exitCode}, findings ${count}`);
  }
  process.exitCode = ok ? 0 : 1;
}

function negativeTest(args) {
  const scratchDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-negative-"));
  const reportDir = fs.mkdtempSync(path.join(os.tmpdir(), "proxi-negative-reports-"));
  try {
    const { treeHash } = exportTree(ROOT, scratchDir);
    plantFakeSecret(scratchDir);

    const grep = scanDirectory(scratchDir, treeHash);
    const gitleaks = runGitleaks(scratchDir, path.join(reportDir, "gitleaks.json"), treeHash);
    const trufflehog = runTrufflehog(scratchDir, path.join(reportDir, "trufflehog.json"), treeHash);

    // a missing scanner also fails ok=false, so demand actual findings: only those prove rejection
    const rejected = {
      grep: !grep.ok && grep.findings.length > 0,
      gitleaks: !gitleaks.ok && gitleaks.findingsCount > 0,
      trufflehog: !trufflehog.ok && trufflehog.findingsCount > 0,
    };
    const rejectedEverywhere = rejected.grep && rejected.gitleaks && rejected.trufflehog;

    writeJson(path.join(args.evidenceDir, "negative-run.json"), {
      treeHash,
      gatePassed: false,
      rejected,
      checks: {
        grep: { ok: grep.ok, exitCode: grep.exitCode, findingsCount: grep.findings.length },
        gitleaks: { ok: gitleaks.ok, exitCode: gitleaks.exitCode, findingsCount: gitleaks.findingsCount },
        trufflehog: { ok: trufflehog.ok, exitCode: trufflehog.exitCode, findingsCount: trufflehog.findingsCount },
      },
      hits: grep.hits,
    });

    console.log(
      `${rejectedEverywhere ? "negative run rejected the planted secret" : "negative run FAILED to reject the planted secret"}: tree ${treeHash}`,
    );
    for (const [name, didReject] of Object.entries(rejected)) {
      console.log(`  ${name}: ${didReject ? "rejected" : "did not reject"}`);
    }
    process.exitCode = rejectedEverywhere ? 0 : 1;
  } finally {
    fs.rmSync(scratchDir, { recursive: true, force: true });
    fs.rmSync(reportDir, { recursive: true, force: true });
  }
}

function fullGate(args) {
  const expectedTreeHash = headTree();
  const errors = [];
  const reports = {};

  for (const name of ["gitleaks", "trufflehog", "grep"]) {
    const file = path.join(args.evidenceDir, `${name}.json`);
    let report;
    try {
      report = JSON.parse(fs.readFileSync(file, "utf8"));
    } catch (error) {
      errors.push(`${name}.json unreadable: ${error.message}`);
      continue;
    }
    reports[name] = report;
    if (report.treeHash !== expectedTreeHash) {
      errors.push(`${name}.json treeHash ${report.treeHash} != ${expectedTreeHash}`);
    }
    if (report.exitCode !== 0) errors.push(`${name}.json exitCode ${report.exitCode}`);
    if (!Array.isArray(report.findings) || report.findings.length !== 0) {
      errors.push(`${name}.json has findings`);
    }
  }

  const treeFile = path.join(args.evidenceDir, "tree-hash.txt");
  let recordedTreeHash = "";
  try {
    recordedTreeHash = fs.readFileSync(treeFile, "utf8").trim();
  } catch (error) {
    errors.push(`tree-hash.txt unreadable: ${error.message}`);
  }
  if (recordedTreeHash !== expectedTreeHash) {
    errors.push(`tree-hash.txt ${recordedTreeHash} != ${expectedTreeHash}`);
  }

  const reviewPaths = ["review-1.json", "review-2.json"].map((name) => path.join(args.evidenceDir, name));
  const reviews = validateReviewReports(reviewPaths, expectedTreeHash);
  errors.push(...reviews.errors);

  const summary = {
    ok: errors.length === 0,
    treeHash: expectedTreeHash,
    reports: Object.fromEntries(
      Object.entries(reports).map(([name, report]) => [
        name,
        {
          treeHash: report.treeHash,
          exitCode: report.exitCode,
          findingsCount:
            typeof report.findingsCount === "number"
              ? report.findingsCount
              : Array.isArray(report.findings)
                ? report.findings.length
                : -1,
        },
      ]),
    ),
    reviews: reviews.reviews.map((review) =>
      review
        ? {
            treeHash: review.treeHash,
            provider: review.provider,
            model: review.model,
            verdict: review.verdict,
            findingsCount: Array.isArray(review.findings) ? review.findings.length : -1,
          }
        : null,
    ),
    errors,
  };
  writeJson(path.join(args.evidenceDir, "summary.json"), summary);

  if (summary.ok) {
    console.log(`preflight gate passed: tree ${expectedTreeHash}`);
  } else {
    console.error(`preflight gate FAILED: tree ${expectedTreeHash}`);
    for (const error of errors) console.error(`  ${error}`);
  }
  process.exitCode = summary.ok ? 0 : 1;
}

function main() {
  const args = {
    evidenceDir: path.join(ROOT, "docs", "issues", "72", "evidence"),
    exportDir: "",
    exportOnly: false,
    negativeTest: false,
  };
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--export-only") {
      args.exportOnly = true;
    } else if (flag === "--negative-test") {
      args.negativeTest = true;
    } else if (flag === "--evidence-dir" || flag === "--export-dir") {
      const value = argv[i + 1];
      if (!value || value.startsWith("--")) {
        console.error(`${flag} needs a path`);
        process.exit(1);
      }
      i += 1;
      if (flag === "--evidence-dir") args.evidenceDir = path.resolve(ROOT, value);
      else args.exportDir = path.resolve(ROOT, value);
    }
  }
  fs.mkdirSync(args.evidenceDir, { recursive: true });

  if (args.exportOnly) exportOnly(args);
  else if (args.negativeTest) negativeTest(args);
  else fullGate(args);
}

const entry = process.argv[1] ? path.resolve(process.argv[1]) : "";
const isMain = entry !== "" && (process.platform === "win32" ? entry.toLowerCase() === SELF.toLowerCase() : entry === SELF);
if (isMain) main();
