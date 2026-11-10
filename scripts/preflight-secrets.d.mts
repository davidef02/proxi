export type ScanCategory = "env-file" | "personal-path" | "email" | "key-pattern";

export interface ScanHit {
  file: string;
  line: number | null;
  category: ScanCategory;
  match: string;
}

export interface ScanCounts {
  envFiles: number;
  personalPaths: number;
  emails: number;
  keyPatterns: number;
}

export interface ScanResult {
  ok: boolean;
  treeHash: string;
  exitCode: number;
  findings: ScanHit[];
  hits: ScanHit[];
  counts: ScanCounts;
}

export interface ScannerResult {
  ok: boolean;
  treeHash: string;
  exitCode: number;
  findingsCount: number;
  findings: unknown[] | null;
}

export interface ReviewReport {
  treeHash?: string;
  provider?: string;
  model?: string;
  filesReviewed?: number;
  verdict?: string;
  findings?: unknown[];
}

export interface ReviewValidation {
  ok: boolean;
  errors: string[];
  reviews: Array<ReviewReport | null>;
}

export const ALLOWED_EMAIL: string;
export const PERSONAL_PATH_RE: RegExp;
export const EMAIL_RE: RegExp;

export function exportTree(
  repoRoot: string,
  destDir: string,
  ref?: string,
): { treeHash: string; destDir: string };

export function scanDirectory(rootDir: string, treeHash?: string): ScanResult;

export function runGitleaks(
  targetDir: string,
  reportPath: string,
  treeHash?: string,
  configPath?: string,
): ScannerResult;

export function runTrufflehog(targetDir: string, reportPath: string, treeHash?: string): ScannerResult;

export function validateReviewReports(
  reportPaths: string[],
  expectedTreeHash: string,
): ReviewValidation;

export function plantFakeSecret(scratchDir: string): string;
