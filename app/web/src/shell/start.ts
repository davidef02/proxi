import { api } from "../api.ts";
import { navigate } from "../router.ts";
import type { SessionConfig } from "../../../shared/protocol.ts";

/** Create the session at once (no dialog; the model comes from settings, the Anki question is asked inside) and open it. */
export async function startSession(config: SessionConfig): Promise<void> {
  const { id } = await api.createSession(config);
  navigate("/s/" + id);
}

export const slugify = (s: string) =>
  s.normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");

export function relTime(iso: string, now = Date.now()): string {
  const s = Math.max(0, (now - new Date(iso).getTime()) / 1000);
  if (s < 60) return "ora";
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86400) return `${Math.floor(s / 3600)} h`;
  if (s < 86400 * 7) return `${Math.floor(s / 86400)} g`;
  return new Date(iso).toLocaleDateString("it-IT", { day: "numeric", month: "short" });
}
