# Lessons, Reference Sheets, Anki & the Status Board

The commands run from the repo but operate in the data folder (`npm run data -- where`): relative
`--out-dir`, `--lesson` and `--node-file` paths resolve there.

## Commands

Always name the course directory: `lesson init` defaults to `./lessons/`, so pass
`--out-dir courses/<course>/lessons` (the flag is `--out-dir`, not `--lessons-dir`).

```bash
npm run lesson init -- --topic "<topic>" --title "<planned title>" \
  --dag "<mermaid>" --goal "<goal>" --out-dir courses/<course>/lessons
npm run lesson append -- --lesson courses/<course>/lessons/000N-<slug>.html --node-json '<json>'
npm run lesson compile-ref -- --lesson courses/<course>/lessons/000N-<slug>.html \
  --out-dir courses/<course>/reference

npm run anki sync -- --lesson courses/<course>/lessons/000N-<slug>.html   # only after opt-in

npm run home                                # regenerate index.html (needs Anki open)
npm run home -- --no-anki                   # regenerate without card counts
npm run home -- plan --chapter "Docker" --lessons-json '[{"title":"...","plannedNodes":4}]'
#   add --slug <course-dir> when the directory name differs from the chapter title (app-cache)
```

On Windows `npm run` cuts a multi-line argument at the first newline (a `--dag` becomes
`graph TD`, and every flag after it, `--out-dir` included, is lost). Pass multi-line values via
`node --import tsx extensions/html-lesson.ts init ...`, and node payloads via `--node-file`.

`lesson init`, `append` and `compile-ref` regenerate `index.html` themselves (without card counts
when Anki is closed). Run `npm run home` after `anki sync` so the card counts catch up.

---

## 1. HTML Lessons

Every session is distilled into `courses/<course>/lessons/NNNN-<slug>.html`, numbered per course.
`lesson init` creates it in Phase 2 from `assets/lesson-template.html` (CSS embedded, KaTeX and
Mermaid from CDN); `append` adds each node in Phase 3.

- **Prose lessons.** Lessons written before the engine carry metadata with `nodes: []` and the
  board reads them as "prosa". Leave them that way: backfilling invented nodes falsifies what was
  taught. New lessons taught through the loop get real nodes. These older lessons link their
  course's `../assets/` CSS/JS.
- **Math (KaTeX):** inline `$f(x) = x^2$`, display `$$ f(x) = \int_0^\infty e^{-t} t^{x-1} dt $$`.
- **Media:** verified inline SVG and Mermaid DAGs embedded in the document; proofs and deep dives
  in collapsible `<details>`.
- **Reference sheets (Sintesi):** `compile-ref` writes the distilled sheet (all node tiers, key snippets, final
  DAG, and learning record misconceptions with `#reference-data` data island) to `courses/<course>/reference/<slug>.html`.

---

## 2. Anki

Cards exist only after the Phase 1c opt-in ([`pedagogy.md`](pedagogy.md)). What a good card is:
[`anki-cards.md`](anki-cards.md).

- **One card per node at most:** Basic if `targetQuestion` is set, else Cloze; the quiz is never carded.
- **Math:** Anki MathJax (`\(...\)` or `\[...\]`).
- **Tags:** every card gets `learn-forme`, `<topic-slug>`, `tier::<tier>` and `node::<node-id>`,
  where `<tier>` is `unconditional-truth`, `axiom` or `derivation` (course lessons also get
  `course::<course>` and `lesson::<slug>`). Keep the `learn-forme` tag name: every card already in Anki
  carries it, and the sync's duplicate check and searches rely on it (generated lessons are titled
  `… | Proxi`; the `learn-forme-theme` localStorage key also stays).
- **TSV export:** `./decks/<topic-slug>.tsv`, tab-delimited, UTF-8.
- **AnkiConnect** (`http://localhost:8765`): `createDeck` (one deck per course:
  `Proxi::<chapter title>` for a lesson under `courses/<course>/lessons/`, the lesson being its
  topic tag; `Proxi::<Topic>` outside a course), `modelNames` + `modelFieldNames` (the profile
  is Italian: the built-in note types are `Basilare`/`Testo da completare`, not `Basic`/`Cloze`, so
  names and fields are resolved at sync time), then `addNotes`.
- **Anki closed:** the sync reports, without blocking, that the `.tsv` is saved.
