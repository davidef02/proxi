# Context

Glossary of the Proxi domain. Vocabulary only — no implementation detail.

## Chapter

A named grouping of **lessons** covering one macro-subject, and the only place a lesson that does
not yet exist can be named. A chapter is realised as a **course**: one directory holding everything
that belongs to it. Its destination is declared in its mission, not inferred from its lessons. A
chapter is planned by an agent, never hand-authored by the learner.

## Course

The concrete form of a chapter: `<dataRoot>/courses/<slug>/`. "Course" names the directory and its contents;
"chapter" names the role it plays in the plan. They are the same thing seen from two sides.

## Lesson

A single teaching session on one **topic**, and the grouping of the **cognitive nodes** produced by
it. A lesson is *taught*, not *planned*: it exists once it has been taught, and its permanent form
is an HTML artifact inside its course. A planned-but-not-yet-taught lesson exists only as a name in
its chapter. Lesson numbering restarts inside each course, so a lesson's identity is the pair
(course, slug) — never the number alone.

## Cognitive Node

The atomic unit of teaching: one concept carried through the full pedagogical loop (Motivate →
Establish → Connect → Quiz-check → Capture) and verified before capture. Nodes are the vertices of
a lesson's dependency DAG; the DAG is the teaching order.

A lesson written as prose, before nodes were captured separately, has none. That is a real state,
not missing data: it was taught, and its nodes were never recorded.

## Pedagogical Tier

What kind of truth a node is: an **unconditional truth** (accepted at face value, no caveats), an
**axiom** (follows from nothing else), or a **derivation** (built from nodes it depends on).
Epistemic, not about difficulty or practice.

## Learning Record

A dated account of one session: what he actually did, what transferred, what didn't. Written for
the teacher, not the learner — it is how the next session avoids repeating the last one's mistake.

## Deck

The Anki counterpart of a lesson's captured knowledge: a named collection of **cards** generated
from the nodes of one course: `Proxi::<chapter title>`, every lesson of the chapter in it (the
lesson is a tag, not a deck). Decks exist only for courses he has opted into.

## Card

One atomic recall prompt derived from a node, owned by Anki once synced. Its study state (due, new,
learning) lives in Anki and nowhere else in this repo.

## Data folder (`dataRoot`)

The directory that holds learner data: `courses/`, `graph/`, `decks/`, `curriculum.json` and the generated
`index.html`. It sits outside the code repo and is the unit of sync. Resolved in this order: env
`LEARNER_DATA_ROOT`, then the pointer `.learner/config.json` `{dataRoot}`, then `~/Documents/proxi-data`.
`.learner/` (settings, sessions, jobs, manifests, secrets) stays in the code repo and is never synced.

## Sync provider

How the data folder is kept in step across PCs, chosen once in Settings → Sincronizzazione: **local** (no remote
sync), **github** (private repo `proxi-data`, one commit per sync) or **gdrive** (Google Drive, `drive.file`).
