//! Formatting helpers shared by the anki port (`anki-sync.ts:160-283`).

use serde_json::Value;

use crate::course_data::DECK_ROOT;
use crate::lesson::js::{is_js_space, js_trim, js_truthy};

/// Human topic -> safe kebab-case slug (`slugifyTopic`).
pub fn slugify_topic(topic: &str) -> String {
    let lower = topic.to_lowercase();
    let decomposed = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(&lower);
    // The TS drops every `\p{Diacritic}`, then everything outside `[\w\s-]`: one
    // filter over the decomposed text does both (combining marks are neither).
    let mut kept = String::with_capacity(decomposed.len());
    for c in decomposed.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' || is_js_space(c) {
            kept.push(c);
        }
    }
    let trimmed = js_trim(&kept);
    let mut out = String::with_capacity(trimmed.len());
    let mut in_separator_run = false;
    for c in trimmed.chars() {
        if c == '_' || c == '-' || is_js_space(c) {
            if !in_separator_run {
                out.push('-');
                in_separator_run = true;
            }
        } else {
            out.push(c);
            in_separator_run = false;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn slugify_topic_value(value: Option<&Value>) -> Result<String, String> {
    match value {
        Some(Value::String(text)) => Ok(slugify_topic(text)),
        Some(Value::Null) => {
            Err("TypeError: Cannot read properties of null (reading 'toLowerCase')".to_string())
        }
        None => Err(
            "TypeError: Cannot read properties of undefined (reading 'toLowerCase')".to_string(),
        ),
        Some(_) => Err("TypeError: topic.toLowerCase is not a function".to_string()),
    }
}

/// `formatDefaultDeckName`: backslashes and slashes become deck separators.
pub fn format_default_deck_name(topic: &str) -> String {
    let mut replaced = String::with_capacity(topic.len());
    for c in topic.chars() {
        if c == '\\' || c == '/' {
            replaced.push_str("::");
        } else {
            replaced.push(c);
        }
    }
    let clean = js_trim(&replaced);
    let prefix = format!("{DECK_ROOT}::");
    if clean.starts_with(&prefix) {
        clean.to_string()
    } else {
        format!("{prefix}{clean}")
    }
}

/// KaTeX `$`/`$$` -> Anki MathJax `\(`/`\[`, keeping already converted text.
pub fn format_latex_for_anki(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let first: Vec<char> = text.chars().collect();
    let display = latex_display(&first);
    let inline_chars: Vec<char> = display.chars().collect();
    let inline = latex_inline(&inline_chars);
    let cloze_chars: Vec<char> = inline.chars().collect();
    latex_cloze(&cloze_chars)
}

/// Pass 1: `/\$\$([\s\S]*?)\$\$/g`.
fn latex_display(chars: &[char]) -> String {
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '$' && chars[i + 1] == '$' {
            let mut close = None;
            let mut j = i + 2;
            while j + 1 < chars.len() {
                if chars[j] == '$' && chars[j + 1] == '$' {
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            match close {
                Some(j) => {
                    let inner: String = chars[i + 2..j].iter().collect();
                    out.push_str("\\[");
                    out.push_str(js_trim(&inner));
                    out.push_str("\\]");
                    i = j + 2;
                }
                None => {
                    // No closer anywhere after this point, so no later start can match either.
                    out.extend(&chars[i..]);
                    break;
                }
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Pass 2: `/(?<!\\|\$)\$(?!\$)(.+?)(?<!\\|\$)\$/g` (`.` is no line terminator in JS).
fn latex_inline(chars: &[char]) -> String {
    let len = chars.len();
    let mut out = String::with_capacity(len);
    let mut s = 0;
    while s < len {
        let prev_ok = s == 0 || (chars[s - 1] != '\\' && chars[s - 1] != '$');
        let opens = chars[s] == '$' && prev_ok && s + 1 < len && chars[s + 1] != '$';
        if !opens {
            out.push(chars[s]);
            s += 1;
            continue;
        }
        // The lookbehind reads this pass's input, so `$a$$b$` keeps its third `$` as text.
        let mut close = None;
        let mut e = s + 2;
        while e < len {
            let prev = chars[e - 1];
            if matches!(prev, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                break;
            }
            if chars[e] == '$' && prev != '\\' && prev != '$' {
                close = Some(e);
                break;
            }
            e += 1;
        }
        match close {
            Some(e) => {
                let inner: String = chars[s + 1..e].iter().collect();
                out.push_str("\\(");
                out.push_str(js_trim(&inner));
                out.push_str("\\)");
                s = e + 1;
            }
            None => {
                out.push(chars[s]);
                s += 1;
            }
        }
    }
    out
}

/// Pass 3: `/\{\{c(\d+)::?([\s\S]*?)\}\}/g`, spacing a trailing `}` run.
fn latex_cloze(chars: &[char]) -> String {
    let mut out = String::with_capacity(chars.len());
    let mut s = 0;
    while s < chars.len() {
        match cloze_at(chars, s) {
            Some((digits, inner_start, close)) => {
                let inner: String = chars[inner_start..close].iter().collect();
                if inner.ends_with('}') || inner.ends_with("}\\)") {
                    let tail = inner.len() - inner.trim_end_matches('}').len();
                    let mut spaced = String::with_capacity(inner.len() + tail);
                    spaced.push_str(&inner[..inner.len() - tail]);
                    for i in 0..tail {
                        if i > 0 {
                            spaced.push(' ');
                        }
                        spaced.push('}');
                    }
                    out.push_str("{{c");
                    out.push_str(&digits);
                    out.push_str("::");
                    out.push_str(&spaced);
                    out.push_str(" }}");
                } else {
                    out.extend(&chars[s..close + 2]);
                }
                s = close + 2;
            }
            None => {
                out.push(chars[s]);
                s += 1;
            }
        }
    }
    out
}

/// A `{{c<digits>::<inner>}}` match at `s`: digits, inner start, index of the closing `}`.
fn cloze_at(chars: &[char], s: usize) -> Option<(String, usize, usize)> {
    if s + 3 > chars.len() || chars[s] != '{' || chars[s + 1] != '{' || chars[s + 2] != 'c' {
        return None;
    }
    let mut p = s + 3;
    let digits_start = p;
    while p < chars.len() && chars[p].is_ascii_digit() {
        p += 1;
    }
    if p == digits_start || chars.get(p) != Some(&':') {
        return None;
    }
    let digits: String = chars[digits_start..p].iter().collect();
    p += 1;
    if chars.get(p) == Some(&':') {
        p += 1;
    }
    let inner_start = p;
    let mut close = p;
    while close + 1 < chars.len() {
        if chars[close] == '}' && chars[close + 1] == '}' {
            return Some((digits, inner_start, close));
        }
        close += 1;
    }
    None
}

pub fn format_latex_value(value: Option<&Value>) -> Result<String, String> {
    if !js_truthy(value) {
        return Ok(String::new());
    }
    match value {
        Some(Value::String(text)) => Ok(format_latex_for_anki(text)),
        _ => Err("TypeError: result.replace is not a function".to_string()),
    }
}

/// Tabs to spaces, newlines to `<br>`, then JS trim.
pub fn escape_tsv_field(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("<br>");
            }
            '\n' => out.push_str("<br>"),
            '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    js_trim(&out).to_string()
}

pub fn escape_tsv_value(value: Option<&Value>) -> Result<String, String> {
    if !js_truthy(value) {
        return Ok(String::new());
    }
    match value {
        Some(Value::String(text)) => Ok(escape_tsv_field(text)),
        _ => Err("TypeError: text.replace is not a function".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn latex_converts_katex_delimiters() {
        let input = "Given $f(x) = x^2$, the derivative is $$\\frac{df}{dx} = 2x$$";
        assert_eq!(
            format_latex_for_anki(input),
            "Given \\(f(x) = x^2\\), the derivative is \\[\\frac{df}{dx} = 2x\\]"
        );
    }

    #[test]
    fn latex_preserves_mathjax() {
        let input = "Already formatted \\(a^2 + b^2 = c^2\\) and \\[e^{i\\pi} + 1 = 0\\]";
        assert_eq!(format_latex_for_anki(input), input);
    }

    #[test]
    fn latex_cloze_spacing() {
        // The lazy inner stops at the first `}}`, so this one is left alone.
        assert_eq!(
            format_latex_for_anki("The formula is {{c1:\\frac{a}{b}}}."),
            "The formula is {{c1:\\frac{a}{b}}}."
        );
        assert_eq!(
            format_latex_for_anki("{{c1::\\(\\frac{a}{b}\\)}}"),
            "{{c1::\\(\\frac{a}{b}\\) }}"
        );
        assert_eq!(format_latex_for_anki("{{c12:x}}"), "{{c12:x}}");
        assert_eq!(format_latex_for_anki("{{c1::a}}}"), "{{c1::a}}}");
    }

    #[test]
    fn latex_inline_edges() {
        assert_eq!(format_latex_for_anki("$a$$b$"), "\\(a\\)$b$");
        assert_eq!(format_latex_for_anki("\\$5 e $y$"), "\\$5 e \\(y\\)");
        assert_eq!(format_latex_for_anki("$a\nb$"), "$a\nb$");
        assert_eq!(format_latex_for_anki("$ x $"), "\\(x\\)");
        assert_eq!(format_latex_for_anki("$$ 2x $$ ok"), "\\[2x\\] ok");
        assert_eq!(format_latex_for_anki("$$a"), "$$a");
        assert_eq!(format_latex_for_anki("$"), "$");
        assert_eq!(format_latex_for_anki("é $x$ é"), "é \\(x\\) é");
    }

    #[test]
    fn escape_tsv_field_matches_node() {
        assert_eq!(
            escape_tsv_field("Line 1\twith tab\nLine 2\r\nLine 3"),
            "Line 1 with tab<br>Line 2<br>Line 3"
        );
        assert_eq!(escape_tsv_field("a\rb"), "a<br>b");
        assert_eq!(escape_tsv_field("\u{feff} x \u{a0}"), "x");
        assert_eq!(escape_tsv_field(""), "");
    }

    #[test]
    fn slugify_topic_drops_ampersand() {
        // `&` disappears instead of becoming "and"; '.' goes too, so "Vue.js" is "vuejs".
        assert_eq!(slugify_topic("Vue.js & Vetrine"), "vuejs-vetrine");
        assert_eq!(slugify_topic("Probabilità"), "probabilita");
        assert_eq!(
            slugify_topic("Calculus: Derivatives"),
            "calculus-derivatives"
        );
        assert_eq!(slugify_topic("  Già__Fatto -- Ok  "), "gia-fatto-ok");
        assert_eq!(slugify_topic("Reattività & Co"), "reattivita-co");
        assert_eq!(slugify_topic("---"), "");
    }

    #[test]
    fn default_deck_name() {
        assert_eq!(
            format_default_deck_name("Fisica/Meccanica"),
            "Proxi::Fisica::Meccanica"
        );
        assert_eq!(format_default_deck_name(" Proxi::Già "), "Proxi::Già");
        assert_eq!(format_default_deck_name("Calculus"), "Proxi::Calculus");
    }

    #[test]
    fn value_wrappers_fail_like_js() {
        assert!(
            slugify_topic_value(None)
                .unwrap_err()
                .contains("reading 'toLowerCase'")
        );
        assert!(
            slugify_topic_value(Some(&json!(null)))
                .unwrap_err()
                .contains("reading 'toLowerCase'")
        );
        assert_eq!(
            slugify_topic_value(Some(&json!(5))).unwrap_err(),
            "TypeError: topic.toLowerCase is not a function"
        );

        assert_eq!(format_latex_value(Some(&json!(0))).unwrap(), "");
        assert!(
            format_latex_value(Some(&json!(5)))
                .unwrap_err()
                .starts_with("TypeError: ")
        );
        assert_eq!(escape_tsv_value(Some(&json!(0))).unwrap(), "");
        assert!(
            escape_tsv_value(Some(&json!(5)))
                .unwrap_err()
                .starts_with("TypeError: ")
        );
    }
}
