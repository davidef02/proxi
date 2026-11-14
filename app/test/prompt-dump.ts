// Prints the Node system prompt and opening message for one fixture; the Rust port asserts byte equality.
import fs from "node:fs";
import { buildSystemPrompt, openingMessage } from "../server/context.ts";
import type { SessionConfig } from "../shared/protocol.ts";

interface PromptDumpInput {
  config: SessionConfig;
  repoRoot: string;
  dataRoot?: string;
  lessonPath?: string;
  today: string;
  lessonTitle?: string;
}

const file = process.argv[2];
if (!file) throw new Error("usage: prompt-dump.ts <input.json>");
const input = JSON.parse(fs.readFileSync(file, "utf-8")) as PromptDumpInput;

console.log(
  JSON.stringify({
    prompt: await buildSystemPrompt({ ...input, sessionId: "s" }),
    opening: openingMessage(input.config, input.lessonTitle),
  }),
);
