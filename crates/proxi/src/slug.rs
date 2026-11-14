//! The one slug rule of the engine (port of extensions/slug.ts): plan and lesson must slug identically.

use crate::lesson::js::{is_js_space, js_trim};

/// A kebab-case, ASCII-folded identifier.
pub fn slugify(text: &str) -> String {
    let lowered = text.to_lowercase();
    let trimmed = js_trim(&lowered);
    let amped = trimmed.replace('&', "and");
    let decomposed = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(&amped);
    // The TS strips every \p{Diacritic} here: the keep-filter below already drops
    // every combining mark, so the two steps collapse into one pass.
    let mut out = String::with_capacity(decomposed.len());
    let mut in_separator_run = false;
    for c in decomposed.chars() {
        if c == '_' || c == '-' || is_js_space(c) {
            if !in_separator_run {
                out.push('-');
                in_separator_run = true;
            }
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
            in_separator_run = false;
        }
        // Everything else is dropped, like the TS `[^\w\s-]` replace.
    }
    out.trim_matches('-').to_string()
}

/// A lesson's slug comes from its TITLE, never its topic. The plan only knows the
/// title, so slugging anything else makes the taught lesson an orphan.
pub fn lesson_slug(title: &str) -> String {
    slugify(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_node_vectors() {
        assert_eq!(
            slugify("Probabilità & Statistica"),
            "probabilita-and-statistica"
        );
        assert_eq!(slugify("  Già__Fatto -- Ok  "), "gia-fatto-ok");
        assert_eq!(slugify("Ça va? “davvero” 50%!"), "ca-va-davvero-50");
        assert_eq!(slugify("İstanbul ŒUVRE ﬁne"), "istanbul-uvre-ne");
        assert_eq!(slugify("a\u{a0}b\u{feff}c\u{85}d"), "a-b-cd");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("Èèé àÀ ñ ü ß"), "eee-aa-n-u");
        assert_eq!(slugify("x\u{2028}y"), "x-y");
        assert_eq!(slugify("C++ & C#"), "c-and-c");
        assert_eq!(
            slugify("Probabilità: è “davvero” 50%? Ça va!"),
            "probabilita-e-davvero-50-ca-va"
        );
        assert_eq!(slugify("a^b`c"), "abc");
    }

    #[test]
    fn lesson_slug_is_slugify() {
        for text in ["Probabilità & Statistica", "  Già__Fatto -- Ok  ", ""] {
            assert_eq!(lesson_slug(text), slugify(text));
        }
    }
}
