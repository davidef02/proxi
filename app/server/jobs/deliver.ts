// The learner-side turn that hands a finished (or failed) job to the teacher.
import type { JobRecord } from "../../shared/protocol.ts";

export function jobTurnText(job: JobRecord): string {
  const head = `[Risultato del subagente ${job.kind} · job ${job.id}]`;
  const r = job.result;
  if (job.status !== "done" || !r) return `${head}\n\nIl subagente non ha consegnato: ${job.error ?? "errore sconosciuto"}.`;
  switch (r.kind) {
    case "research":
      return `${head}\n\n${r.brief}\n\nFonti:\n${r.sources.map((s) => `- ${s.title}: ${s.url}`).join("\n")}`;
    case "visual":
      return `${head}\n\nLa figura è già visibile allo studente nella chat.` + (r.caption ? `\nDidascalia: ${r.caption}` : "");
    case "demo":
      return `${head}\n\nLa demo "${r.title}" è già visibile allo studente nella chat.` + (r.notes ? `\nNote: ${r.notes}` : "");
    default:
      return head; // map jobs are never delivered
  }
}
