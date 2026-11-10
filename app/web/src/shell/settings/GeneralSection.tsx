import { Monitor, Moon, Sun, type LucideIcon } from "lucide-react";
import { useState } from "react";
import { modePref, palette, setModePref, setPalette, type ModePref, type Palette } from "../../theme.ts";
import { setZoomPref, useZoom, ZOOM_OPTIONS } from "../../zoom.ts";
import { ChoiceCards, PaletteCards, ROW, STACK, Section, Label, Segmented } from "./ui.tsx";

const MODES: [ModePref, string, LucideIcon][] = [["system", "Sistema", Monitor], ["light", "Chiaro", Sun], ["dark", "Scuro", Moon]];

export function GeneralSection() {
  const [mode, setMode] = useState<ModePref>(modePref);
  const [pal, setPal] = useState<Palette>(palette);
  const zoom = useZoom();
  return (
    <Section title="Generali">
      <div className={STACK}>
        <Label title="Schema colori" hint="vale per questo computer" />
        <ChoiceCards label="Schema colori del tema" value={mode} options={MODES} onChange={(v) => { setMode(v); setModePref(v); }} />
      </div>
      <div className={STACK}>
        <Label title="Temi" hint="colori di sfondo, testo e accento" />
        <PaletteCards value={pal} onChange={(p) => { setPal(p); setPalette(p); }} />
      </div>
      <div className={ROW}>
        <Label title="Dimensione testo" hint="scala l'intera interfaccia" />
        <Segmented label="Dimensione testo" value={zoom} options={ZOOM_OPTIONS} onChange={(v) => setZoomPref(v)} />
      </div>
    </Section>
  );
}
