import { generateHomepage } from "../../../extensions/homepage.ts";

/** Regenerate index.html: with card counts when Anki answers, without otherwise. Fire-and-forget. */
export function refreshBoard(dataRoot: string): Promise<void> {
  return generateHomepage({ baseDir: dataRoot, ankiUrl: process.env.LEARNER_ANKI_URL || undefined })
    .catch(() => generateHomepage({ baseDir: dataRoot, noAnki: true }))
    .then(() => undefined)
    .catch((e) => {
      process.stderr.write(`[board] refresh failed: ${e instanceof Error ? e.message : e}\n`);
    });
}
