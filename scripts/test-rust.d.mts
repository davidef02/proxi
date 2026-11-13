export const RUST_PORTED: string[];

export function rustTestEnv(base?: NodeJS.ProcessEnv): NodeJS.ProcessEnv;

export function rustTestArgs(files?: string[]): string[];

export function missingFiles(root: string, files?: string[]): string[];
