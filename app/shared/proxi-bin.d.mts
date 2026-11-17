/** The built `proxi` executable: `PROXI_BIN`, else the newest of target/release and target/debug. */
export declare function proxiBinary(root?: string, env?: NodeJS.ProcessEnv): string;

export interface RunningProxi {
  url: string;
  port: number;
  exited: Promise<void>;
  close(): Promise<void>;
}

/** Spawn `bin` with `args` and wait for the `Proxi: <url>` line it prints on startup. */
export declare function spawnProxi(args: string[], bin?: string): Promise<RunningProxi>;
