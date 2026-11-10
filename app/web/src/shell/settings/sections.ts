// The settings dialog's sections, in tab order. Ids are the programmatic open targets (openSettings).
import { Cpu, Download, FolderOpen, Mic, Network, RefreshCw, SlidersHorizontal, type LucideIcon } from "lucide-react";

export type SettingsSection = "generali" | "motore" | "dettatura" | "mappa" | "sync" | "aggiornamenti" | "dati";
export const SECTIONS: { id: SettingsSection; label: string; icon: LucideIcon }[] = [
  { id: "generali", label: "Generali", icon: SlidersHorizontal },
  { id: "motore", label: "Motore", icon: Cpu },
  { id: "dettatura", label: "Dettatura vocale", icon: Mic },
  { id: "mappa", label: "Mappa", icon: Network },
  { id: "sync", label: "Sincronizzazione", icon: RefreshCw },
  { id: "aggiornamenti", label: "Aggiornamenti", icon: Download },
  { id: "dati", label: "Dati", icon: FolderOpen },
];
