//! The one slug rule of the engine: the (course, slug) join identity; mirrors `extensions/slug.ts`.

use icu_normalizer::DecomposingNormalizerBorrowed;

/// JS `\s`: WhiteSpace + LineTerminator, no `u` flag surprises.
pub fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{B}'
            | '\u{C}'
            | '\r'
            | ' '
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// JS `String.prototype.trim` (its whitespace set, not Rust's).
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

pub fn slugify(text: &str) -> String {
    let lowered = text.to_lowercase();
    let trimmed = js_trim(&lowered);
    let ampersands = trimmed.replace('&', "and");
    // fold accents to ASCII first, so "Probabilità" slugs to "probabilita", not "probabilit"
    let nfd = DecomposingNormalizerBorrowed::new_nfd().normalize(&ampersands);
    let kept: String = nfd
        .chars()
        .filter(|&c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || is_js_space(c))
        .collect();
    let mut out = String::with_capacity(kept.len());
    let mut in_separator = false;
    for c in kept.chars() {
        if c == '_' || c == '-' || is_js_space(c) {
            if !in_separator {
                out.push('-');
                in_separator = true;
            }
        } else {
            out.push(c);
            in_separator = false;
        }
    }
    out.trim_matches('-').to_string()
}

/// A lesson's slug comes from its TITLE, never its topic: the plan only knows the title.
pub fn lesson_slug(title: &str) -> String {
    slugify(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_node_cases() {
        assert_eq!(slugify("Probabilità condizionata"), "probabilita-condizionata");
        assert_eq!(slugify("App State & Caching"), "app-state-and-caching");
        assert_eq!(slugify("Image, e container!"), "image-e-container");
        assert_eq!(slugify("  --Rust__ WebAssembly--  "), "rust-webassembly");
        assert_eq!(slugify("Ünïcödé çà"), "unicode-ca");
        assert_eq!(slugify("日本語 test"), "test");
        assert_eq!(slugify("a\u{A0}b\u{FEFF}c"), "a-b-c");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn lesson_slug_is_slugify() {
        assert_eq!(lesson_slug("Probabilità condizionata"), slugify("Probabilità condizionata"));
        assert_eq!(lesson_slug("App State & Caching"), "app-state-and-caching");
    }
}
