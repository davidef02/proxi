/**
 * slug.ts — The one slug rule of the engine.
 *
 * Part of the Proxi teaching engine.
 * A slug is the stable identity a lesson is joined on: the plan names a lesson
 * (curriculum.json), the taught artifact names itself (lessons/*.html), and the
 * homepage joins the two. That join only holds while both sides slug the same
 * string from the same rule — hence one function, one derivation.
 */

/** A kebab-case, ASCII-folded identifier. */
export type Slug = string;

export function slugify(text: string): Slug {
  return text
    .toLowerCase()
    .trim()
    .replace(/&/g, "and")
    // fold accents to ASCII first, so "Probabilità" slugs to "probabilita", not "probabilit"
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .replace(/[^\w\s-]/g, "")
    .replace(/[\s_-]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/**
 * A lesson's slug comes from its TITLE, never its topic. The plan only knows the
 * title, so slugging anything else makes the taught lesson an orphan.
 */
export function lessonSlug(title: string): Slug {
  return slugify(title);
}
