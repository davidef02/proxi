# Roadmap
<!-- Format owned by the init-project skill. Edit only with these moves:
- New milestone: append `- [ ] Mxx <name>` to Milestones (next two-digit number) and a `## Mxx <name>` block (`Goal:` one sentence; `Exit:` observable bullets) just before `## Fog`. `Mxx <name>` is also the GitHub milestone title, character for character.
- Milestone reshaped: rewrite its `Goal:` / `Exit:` lines; a new name changes its `- [ ]` line, its `## ` heading and its GitHub title identically.
- Milestones reordered: the open milestones take consecutive numbers after the highest done one, in the new work order; rewrite each `- [ ]` line and `## ` heading, keep both lists sorted by number, and retitle them on GitHub.
- Milestone done: change its line to `- [x] Mxx <name> (YYYY-MM-DD): <outcome, at most 12 words>` and delete its `## Mxx` block.
- Milestone dropped: delete its `- [ ]` line and its block; its open issues move first, then its GitHub milestone is deleted.
- Vision changed: replace the `Vision:` line.
- New fog: append `- <idea>: <what is unclear, or which decision unlocks it>` under Fog.
- Fog graduated (now issues or a milestone): delete its line.
- Fog dropped (no longer wanted): delete its line.
Issues, counts and progress live on GitHub, never here. -->

Vision: Ottimizzare il processo di apprendimento riducendo al minimo l'attrito nello studio e applicando principi di science-based learning e science-based teaching: proximity learning (individuare sempre il prossimo nodo cognitivo ottimale nella zona di sviluppo prossimale), un tutor personalizzato sulla persona (didattica per scoperta motivata ispirata a 3Blue1Brown, verità incondizionate come radici del grafo di conoscenza, active recall e diagnosi immediata dei misconconcetti su lezioni HTML standalone costruite per nodi) e spaced repetition su Anki, rendendo immediata in ogni schermata la prossima azione utile (Continua o Ripasso).

## Milestones
- [x] M01 Riduzione attrito UI (2026-11-04): Mappa, top bar, pagina corso e sidebar snellite per ridurre l'attrito.
- [x] M02 Spaced repetition e Anki (2026-11-06): Collegamento bidirezionale Anki-app: ripassi importati, concetti deboli su mappa e probe mirato.
- [x] M03 Motore ZPD globale (2026-11-06): Conoscenza globale per significato, ancoraggio idee e confine ZPD senza sovraccarico contesto.
- [x] M04 Nuova identità (2026-11-11): App rinominata Proxi, repo pubblico pulito, ia-teacher archiviato.
- [x] M05 Backend Rust (2026-11-18): Backend e comandi in un unico binario Rust
- [ ] M06 App nativa Tauri

## M06 App nativa Tauri
Goal: Proxi si installa e si aggiorna come app Tauri su Windows, senza Electron, consumando meno risorse.
Exit:
- L'installer Windows è su GitHub Releases e l'app si aggiorna da Impostazioni con aggiornamenti firmati.
- RAM a riposo almeno −15% rispetto alla misura di M05 e installer Windows sotto i 15 MB; avvio a freddo non più lento.
- Finestra, tema, zoom, appunti, microfono, link esterni, istanza singola e segreti nel keychain funzionano come in Electron.
- Electron e l'aggiornamento via `git pull` sono rimossi.
- La CI compila Windows, macOS e Linux, e il README spiega come compilare per tutti e tre.
Settled:
- Shell: Tauri v2 su WebView2; wry custom, Electron, Avalonia e Wails (Go) scartati (ricerca 2026-11-18).
- Aggiornamenti firmati = firme Ed25519 (minisign) su latest.json di GitHub Releases; installer non firmato Authenticode (SmartScreen solo al primo avvio).
- UI: React mantenuto; Svelte 5 solo per eventuali rebuild futuri a pezzi.

## Fog
