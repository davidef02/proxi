import type { SettingsResponse } from "../../api.ts";
import { ROW, SWITCH, Section, Label, type Save } from "./ui.tsx";

export function MapSection({ s, save }: { s: SettingsResponse; save: Save }) {
  return (
    <Section title="Mappa">
      <label className={ROW}>
        <Label title="Aggiorna a fine lezione" hint="estrae le idee della lezione appena chiusa" />
        <input type="checkbox" checked={s.autoMap} onChange={(e) => save({ autoMap: e.target.checked })} className={SWITCH} />
      </label>
    </Section>
  );
}
