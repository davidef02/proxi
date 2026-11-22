export interface UpdaterPubkey {
  key: Buffer;
  keynum: Buffer;
}

export interface MinisignSignature {
  signature: Buffer;
  trustedComment: string;
  globalSignature: Buffer;
  keynum?: Buffer;
  alg?: string;
}

export interface ReleaseArtifacts {
  installerPath: string | null;
  sigPath: string | null;
}

export interface ResolveLatestJsonOptions {
  bundleDir?: string;
  latestJsonPath?: string;
}

export interface LatestJsonFromSigOptions {
  sigPath: string;
  installerName: string;
  configPath: string;
}

export interface VerifyReleaseOptions {
  bundleDir: string;
  configPath: string;
  maxBytes?: number;
  latestJsonPath?: string;
  fromSig?: boolean;
}

export interface VerifyReleaseResult {
  ok: boolean;
  errors: string[];
  installerBytes: number;
}

export const MAX_INSTALLER_BYTES: number;
export const GITHUB_RELEASE_ASSET_URL_RE: RegExp;

export function isValidPlatformUrl(url: string, expectedName: string): boolean;

export function spkiFromRawEd25519(raw32: Uint8Array | Buffer): Buffer;

export function parseUpdaterPubkey(pubkeyB64: string): UpdaterPubkey;

export function parseMinisignSignature(sigB64: string): MinisignSignature;

export function ed25519Verify(
  key32: Uint8Array | Buffer,
  data: Uint8Array | Buffer,
  sig64: Uint8Array | Buffer,
): boolean;

export function findReleaseArtifacts(bundleDir: string): ReleaseArtifacts;

export function resolveLatestJsonPath(options?: ResolveLatestJsonOptions): string;

export function latestJsonFromSig(options: LatestJsonFromSigOptions): Record<string, unknown>;

export function verifyRelease(options: VerifyReleaseOptions): VerifyReleaseResult;
