import fs from "node:fs";
import path from "node:path";
import type { ToolOutcome } from "../../types.ts";

const DIR = path.resolve(import.meta.dirname, "..", "..", "..", "prompts", "subagents");

export const readPrompt = async (name: string) => (await fs.promises.readFile(path.join(DIR, name), "utf-8")).trim();
export const fail = (text: string): ToolOutcome => ({ text, isError: true });
export const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));
