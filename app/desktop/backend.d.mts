export declare function proxiBinaryPath(root: string, env?: NodeJS.ProcessEnv): string;
export declare function serveArgs(opts: { repoRoot: string; distDir: string; port: number; env?: NodeJS.ProcessEnv }): string[];
export declare function locateEngineBin(
  provider: "claude" | "agy" | "opencode",
  env?: NodeJS.ProcessEnv,
  platform?: NodeJS.Platform,
  homeDir?: string,
): string | undefined;
export declare function probeEngine(bin: string, binArgs: string[]): Promise<boolean>;
export interface RunningBackend {
  url: string;
  port: number;
  pid: number;
  exited: Promise<void>;
  close(): Promise<void>;
}
export declare function spawnBackend(bin: string, args: string[], env?: NodeJS.ProcessEnv): Promise<RunningBackend>;
export declare function killTreeSync(pid: number): void;
