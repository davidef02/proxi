# Writing Anki Cards That Survive Review

Mechanics (opt-in, tags `learn-forme` / `tier::` / `node::` / `course::` / `lesson::`, deck root `Proxi`, TSV export, AnkiConnect) live in [`lessons-and-anki.md`](lessons-and-anki.md); the `ankiParams` payload is in [`pedagogy.md`](pedagogy.md). This file is only about what makes a card worth reviewing. Sources: Wozniak, *Twenty rules of formulating knowledge* (https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge); Matuschak, *How to write good prompts* (https://andymatuschak.org/prompts/); Nielsen, *Augmenting Long-term Memory* (https://augmentingcognition.com/ltm.html); Anki manual, cloze (https://docs.ankiweb.net/editing.html#cloze-deletion).

**What the pipeline turns a node into** (`extensions/anki-sync.ts`): **at most one note.** A Basic note with `targetQuestion` on the front and `targetFact` on the back, or, when there is no question, a Cloze note from `suggestedCloze` (back: `clozeExtra`, one line, or nothing). If both are authored, only the Basic is emitted. The quiz never becomes a card: it checks understanding once, in the lesson. Nothing else lands on a card: not the title, not the motivation, not the content.

**Fewer cards, by his explicit request** (2026-10-01: «ne voglio di meno... le più essenziali senza tutti sti concetti ripetuti»). Three cards per node (cloze, fact, quiz) restated each idea three ways, and 27 Vue notes after two lessons were pruned to 8. The rules that follow:
- **One card per node at most, and not every node gets one.** Card only the lesson's load-bearing truths, typically two or three per lesson. A node whose fact he can rederive from another card, or that only bridges two nodes, gets no `ankiParams`.
- **No concept twice across the course.** Before writing a card, read the `ankiParams` of the course's earlier lessons (`grep -o '"targetQuestion":"[^"]*"' <dataRoot>/courses/<course>/lessons/*.html`). If an earlier card already tests the idea («`<script setup>` gira una volta per istanza»), skip it, even when the new node reaches it from another angle.
- **Pick the form, don't fill both.** Question card for a *why* or a contrast with React; cloze when one identifier he will type is the whole point (`modelValue` / `update:modelValue`).
- **Fields are HTML.** Escape `<` and `>` as `&lt;` `&gt;`: a raw `<script setup>` or `<X @select>` is swallowed as a tag and the card shows a hole. Avoid `$` outside math: `formatLatexForAnki` turns `$emit … $event` into a MathJax span.

---

## 1. One distinction per card (minimum information principle)
- Wozniak's Dead Sea example: "What are the characteristics of the Dead Sea?" becomes eight one-fact items. Simple items are processed "always in the same way" and each gets its own repetition schedule (https://www.supermemo.com/en/blog/twenty-rules-of-formulating-knowledge, rule 4).
- Nielsen split "how to create a soft link" into syntax and argument order because with the combined card "I was often a little fuzzy about where exactly my mistake was" (https://augmentingcognition.com/ltm.html).
- Matuschak's five properties: focused, precise, consistent, tractable, effortful (https://andymatuschak.org/prompts/).
- Check: if the back contains an "e", a comma or a second sentence joining two facts, split it into two cards.

## 2. No yes/no cards, no list cards
- Nielsen calls a yes/no answer a "question smell": ask *which* or *why* instead. Matuschak: binary prompts "require little effort and produce shallow understanding" (https://augmentingcognition.com/ltm.html, https://andymatuschak.org/prompts/).
- Wozniak rules 9 and 10: sets are the hardest items, enumerations next. Convert a set into an ordered sequence, and an enumeration into cloze items, overlapping if the order matters (`A…E → B C D`, `B…F → C D E`). Extra items do not violate rule 4 because "the extra information is added in extra items" (rule 17).
- Matuschak, closed lists: one cloze per item, list kept in the same order every time; open lists ("name two ways…") get separate fuzzy prompts, never a full recitation.
- Rewrite recipe: "X fa Y?" → "Cosa fa X quando Y?"; a list of N → N cloze cards, or a single note with `{{c1}}…{{cN}}`.

## 3. Cloze deletion
- Use it for sentences where one identifier, keyword or value is the whole point (Wozniak rule 5: "easy and effective"). Trim the sentence to the cue first (rule 12: "PageMaker lost ground to…" beats the paragraph it came from).
- Anki syntax: `{{c1::text}}`; different numbers → separate cards; the same number twice → both hidden on one card; `{{c1::text::hint}}` shows a hint; `{{c1,2::x}}` puts a deletion on several cards (https://docs.ankiweb.net/editing.html#cloze-deletion).
- Keep the blank one token or identifier. The visible text must force exactly that token; if it does not, add context, do not widen the blank.
- One blank per card. Multiple `cN` numbers on one `suggestedCloze` are fine (each yields its own card); the same number on two blanks is only for deliberate overlapping.
- Cloze made by pasting a passage teaches the *shape* of the sentence, not the fact (Matuschak). Rewrite in the learner's words.

## 4. Cueing: the front admits one answer
- Matuschak's omelette: "What's the first step?" has several defensible answers, so you end up memorising "what the prompt is asking". Add context until only one answer fits, keeping the blank small (https://andymatuschak.org/prompts/).
- Wozniak rule 16: a label prefix (`bioch: GRE`) puts the brain in the right context with fewer words; rule 11: interference between similar items "is probably the single greatest cause of forgetting".
- For this learner every colliding term gets a `Vue 3:` prefix: `ref`, `props`, `emit`, `watch`, `key`, `slot`, `ref` on a DOM element vs `useRef`. The node `title` is the Basic-card cue, so title the node as the specific mechanism, never as a topic label ("Reattività di `ref`" is a cue, "Reattività" is not).

## 5. Why orphan cards fail
- Wozniak rule 1: "Do not learn if you do not understand"; rule 2: build the overall picture first, then memorise; rule 13: refer to memories you already hold.
- Nielsen: isolated facts turn "stale" and get missed after months; add two or three cards per new idea and Ankify in service of a real project, not speculatively (https://augmentingcognition.com/ltm.html).
- Matuschak: if material "doesn't connect to something you understand", rethink before writing the prompt (https://andymatuschak.org/prompts/).
- Here the connection is React: name the React counterpart on the card whenever one exists (`useMemo`, `useState`, `useEffect`, `props.children`, Context). Cards for a node the learner failed the quiz-check on are not emitted; reteach first.

## 6. Prompt types that fit programming (Matuschak's taxonomy)
- **Factual**: one API name, one default, one argument order. Use for identifiers he will type (`modelValue`, `update:modelValue`, `.value`).
- **Explanation**: "perché…?" cards. Best fit for framework semantics (why `computed` needs no dependency array). Use for every `derivation`-tier node.
- **Procedural**: extract the *verb* and the *condition*, not the full step list ("quando devi chiamare X?"). Use for lifecycle and cleanup order.
- **Conceptual (similarities/differences)**: the natural card for a React developer: what is the same, what differs, in one pair at a time.
- **Salience**: a behavioural cue he should notice while coding ("Vue 3: quando destrutturi un oggetto reattivo devi ricordarti di…"). Use sparingly, one or two per lesson.
- **Creative / open-list**: skip for a course deck; the generation effect is better served by the quiz-check. (https://andymatuschak.org/prompts/)

## 7. When not to make a card
- He can derive it in seconds from something he already holds (Wozniak rule 3: build upon basics; Matuschak: "no deep virtue in writing a prompt about every detail").
- He will look it up anyway: option-object shapes, rarely used flags, signatures the IDE completes. Nielsen's threshold: worth about ten minutes of future time, or genuinely striking (https://augmentingcognition.com/ltm.html).
- He did not understand it yet (Wozniak rule 1), or it is version-volatile: if you keep it, date-stamp it ("Vue 3.4+") (rule 19).
- Nielsen on APIs: do not Ankify a tutorial wholesale; card only what recurs once he is using it.

---

## Before / after (Vue 3, Composition API, learner coming from React)

| Bad | Rewrite | Why |
| :--- | :--- | :--- |
| Quali sono le differenze tra `ref` e `reactive`? | Vue 3: un valore primitivo (stringa, numero) si rende reattivo con {{c1::`ref()`}}, perché `reactive()` accetta solo oggetti. | A set with an open-ended answer; the rewrite tests the one distinction `useState` never made him face. |
| `computed` in Vue 3 ha un array di dipendenze come `useMemo`? → No | Vue 3: come fa `computed` a sapere quando ricalcolarsi, se non riceve l'array di dipendenze di `useMemo`? → Traccia a runtime i valori reattivi letti durante l'esecuzione del getter. | Yes/no became an explanation prompt anchored to a memory he already has. |
| Descrivi come funziona `v-model` su un componente custom. | Vue 3, `v-model` su un componente custom: il figlio riceve la prop {{c1::`modelValue`}} ed emette l'evento {{c2::`update:modelValue`}}. | Vague and multi-fact; two blanks with different numbers give each half of the contract its own schedule. |
| Il corpo di `<script setup>` viene rieseguito a ogni render? → No | Vue 3: quante volte viene eseguito il corpo di `<script setup>` nella vita di un'istanza, rispetto al corpo di una function component React? → Una sola volta, al setup; React riesegue il corpo a ogni render. | Yes/no became a similarities/differences prompt; the React contrast is the reason the fact matters to him. |
| Cosa fanno `provide` e `inject`? | Vue 3, `provide`/`inject`: cosa devi passare a `provide` perché il discendente veda gli aggiornamenti e non una copia congelata? → Il `ref` stesso (o l'oggetto `reactive`), non il suo `.value`. | Two concepts and no single answer; the rewrite isolates the one failure mode he will actually hit. |
| Elenca i lifecycle hook della Composition API. | Vue 3: l'hook della Composition API che corrisponde al `useEffect` con array vuoto (dopo il primo mount) si chiama {{c1::`onMounted`}}. | Enumeration; one cloze per hook, each tied to the React counterpart he already knows. |
