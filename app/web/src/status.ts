import type { SessionStatus } from "../../shared/protocol.ts";

/** One label per status, used by Home (short) and the session header (long). */
export const STATUS_LABEL: Record<SessionStatus, { short: string; long: string }> = {
  starting: { short: "Avvio…", long: "Avvio…" },
  running: { short: "Il docente scrive…", long: "Il docente scrive…" },
  waiting: { short: "Tocca a te", long: "Tocca a te: rispondi alla domanda" },
  idle: { short: "Tocca a te", long: "Tocca a te" },
  error: { short: "Errore", long: "Errore" },
  closed: { short: "Chiusa", long: "Chiusa" },
};
