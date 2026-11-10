---
name: plan
description: Plan a chapter — the ordered list of lessons that takes him from where he is to a goal on a macro-subject. Use when he names a subject bigger than one session ("voglio imparare l'algebra lineare"), asks what to study next, or when /teach is invoked on a subject that has no course yet. Produces a plan, teaches nothing.
---

# Planning a chapter

A **chapter** is a course: `courses/<slug>/`, holding its own lessons, reference sheets and learning
records. `courses/...` paths are relative to the data folder (`npm run data -- where`). A **lesson** is one session and the nodes it produces. This skill decides the chapter's
lessons and their order — nothing else. It never teaches: the pull to start explaining is the signal
that planning is done.

The plan is yours to write, never his. He should not have to author a curriculum by hand.

## Why this exists

A lesson's own DAG (`metadata.initialDag`) plans the nodes *inside* one session. Nothing above it
plans the sessions themselves, so a lesson not yet taught has nowhere to be named. `curriculum.json`
is that place, and this skill is the only thing that writes it.

## Loop

### 0. See what is already planned

```bash
npm run home -- chapters
```

Re-planning an existing chapter replaces it wholesale, so know whether one already covers the
subject before you write. If a course directory already exists, read its `MISSION.md` first: the
destination may already be settled, and your job is then only the lesson breakdown.

### 1. Probe the edge, coarsely

Planning needs a different probe than teaching: not "does he hold this specific node" but "where
does his footing on this subject end". Two or three bare claims spanning the subject, from clearly
known to clearly beyond, and ask which he can derive rather than recall. Follow the probe rules in
[`docs/agents/pedagogy.md`](../../docs/agents/pedagogy.md) — symmetric claims, explanation after
every answer, no leading.

Stop as soon as you can name the first lesson he'd actually benefit from. Precision beyond that is
wasted: the plan is mutable, and lesson 5 will be re-planned by the time you reach it.

Before probing, consult his knowledge graph when the runtime has the concept tools (`search_concepts`
on the subject's key terms, `browse_concepts`, `open_concept`): what he holds in other courses moves
the starting point. Status rule: `solido` = skip it, no question; `insegnato` = one confirmation question per group of related concepts (a group = concepts born in the same lesson in `browse_concepts`, or linked by a chain in `open_concept`), and if he answers it right move on; `fragile` or `mancato` = revisit it in the probe. If the graph has nothing relevant (no match, or `browse_concepts` reports an unknown course), probe the lesson's strands as usual: an empty result is not an error.

### 2. Name the destination

One or two sentences: what he can do or derive once the chapter is finished. Put it to him and let
him correct it. A chapter without a destination sprawls.

For a new course, write it into `courses/<slug>/MISSION.md` — **Why**, **Success looks like**,
**Constraints**, **Out of scope**. Every bullet in `## Success looks like` must be **concrete and
falsifiable**: describe an observable task or derivation he can perform (write, measure, trace, name
the exact file/function for a given case), never a bare state verb ("capire", "conoscere", "sapere")
without a verifiable check. Follow the shape of `courses/docker/MISSION.md`, including its
habit of appending a dated *"Correzione di rotta"* (and updating `## Success looks like` to match)
rather than silently rewriting history when the mission changes.

### 3. Break it into lessons

Each lesson is **one session's worth**: a handful of nodes around a single centre of gravity, with a
title that names the *insight*, not the syllabus entry ("Il determinante come area", not
"Determinanti"). Order them so every lesson only depends on lessons above it — same dependency
discipline as a node DAG, one level up.

Estimate `plannedNodes` per lesson (typically 3–6). It drives the hollow dots on the board and is a
guess, not a contract.

Plan from the same status rule: propose no lesson whose subject is a `solido` concept; fold the `insegnato` concepts the chapter rests on into a brief recap inside the first lesson (count them in its plannedNodes) instead of giving them a lesson of their own; give `fragile` and `mancato` concepts a place in the plan.

Keep the chapter to what he can see the point of. Six lessons he'll do beats twenty he won't.

### 4. Put the plan to him before writing it

Show the ordered list with the destination. This is the one moment he should push back on scope and
ordering — after this the plan exists and re-planning replaces it wholesale.

### 5. Write it

```bash
npm run home -- plan --chapter "<chapter title>" \
  --lessons-json '[{"title":"<lesson title>","plannedNodes":4}, ...]'
```

The course directory defaults to the slug of the chapter title. Pass `--slug <course-dir>` when it
differs — `App State & Caching` lives in `courses/app-cache`. The directory in the plan **must**
match the real directory, or every lesson of that course reads as outside the plan.

Then regenerate the board:

```bash
npm run home                 # requires Anki open (AnkiConnect)
npm run home -- --no-anki    # without card counts
```

Lesson slugs derive from the titles, and a lesson counts as taught when
`courses/<course>/lessons/NNNN-<slug>.html` exists. So **`/teach` must keep the planned title
verbatim** in `lesson init --title`, or the taught lesson will not match its plan entry and will
land under "Fuori dal piano".

### 6. Hand off

Name the first lesson and offer `/teach` on it. Do not start teaching in this session: planning a
chapter and teaching a lesson are different lengths of work, and he chose to separate them.

## Re-planning

Re-planning a chapter replaces its lesson list wholesale; git keeps the history. Lessons already
taught keep their files and stay matched as long as their titles survive in the new plan — drop a
taught lesson's title and its artifact still exists, it just shows under "Fuori dal piano".

For a lesson whose file predates the title-derived slug rule, a plan entry may carry an explicit
`"slug"` so it still matches: `{"title":"<full title>","slug":"<existing-file-slug>"}`. Use it only
to describe a file that already exists, never to rename a future lesson.
