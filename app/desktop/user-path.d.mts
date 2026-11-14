export type Ps = (script: string) => string;
export interface PathEdit { value: string; changed: boolean }
export declare function psQuote(s: string): string;
export declare function withPathEntry(current: string, dir: string): PathEdit;
export declare function withoutPathEntry(current: string, dir: string): PathEdit;
export declare function readUserPath(ps: Ps): string;
export declare function writeUserPath(ps: Ps, value: string): void;
