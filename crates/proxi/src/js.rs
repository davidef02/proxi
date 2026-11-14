//! JavaScript semantics the Node port depends on.

use serde_json::{Number, Value};

/// JS `\s`.
pub fn is_js_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0B}'
            | '\u{0C}'
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

/// `String.prototype.trim` with JS whitespace.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_ws)
}

/// Every maximal JS-whitespace run becomes one space (no trim).
pub fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if is_js_ws(c) {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

/// `[A-Za-z0-9_]`, the bytes JS `\w` and `\b` agree on.
pub fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// First ASCII case-insensitive occurrence of `needle` at or after byte `from`.
pub fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let hay = hay.as_bytes();
    let needle = needle.as_bytes();
    if from > hay.len() {
        return None;
    }
    if needle.is_empty() {
        return Some(from);
    }
    let last = hay.len().checked_sub(needle.len())?;
    (from..=last).find(|&i| {
        hay[i..i + needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

/// JS truthiness.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// JS `String(v)`; `None` is JS `undefined`.
pub fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) => js_number_string(f),
            None => n.to_string(),
        },
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}

fn js_number_string(f: f64) -> String {
    if f == 0.0 {
        return "0".into();
    }
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e21 {
        return format!("{f:.0}");
    }
    format!("{f}")
}

/// `JSON.stringify(JSON.parse(text))` number handling, recursively.
pub fn normalise_numbers(v: Value) -> Value {
    match v {
        Value::Number(n) => {
            if n.as_i64().is_none() && n.as_u64().is_none() {
                if let Some(f) = n.as_f64() {
                    if f.is_finite() && f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_991.0 {
                        let i = if f == 0.0 { 0 } else { f as i64 };
                        return Value::Number(Number::from(i));
                    }
                }
            }
            Value::Number(n)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(normalise_numbers).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, normalise_numbers(v)))
                .collect(),
        ),
        other => other,
    }
}

/// `JSON.parse` with the number normalisation `JSON.stringify` would apply.
pub fn parse_json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text)
        .map(normalise_numbers)
        .map_err(|e| e.to_string())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Strict `decodeURIComponent`: bad escapes and invalid UTF-8 are `None`.
pub fn decode_uri_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex_val(*bytes.get(i + 1)?)?;
            let lo = hex_val(*bytes.get(i + 2)?)?;
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Hono `tryDecode(s, decodeURIComponent)`.
pub fn try_decode_uri_component(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    if let Some(decoded) = decode_uri_component(s) {
        return decoded;
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let ch = s[i..].chars().next().expect("char boundary");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let start = i;
        while i + 2 < bytes.len()
            && bytes[i] == b'%'
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            i += 3;
        }
        if i == start {
            out.push('%');
            i += 1;
            continue;
        }
        let run = &s[start..i];
        match decode_uri_component(run) {
            Some(decoded) => out.push_str(&decoded),
            None => out.push_str(run),
        }
    }
    out
}

/// Hono `getQueryParam` for one key: first match, `+` as space, lenient decode.
pub fn query_param(raw_query: Option<&str>, key: &str) -> Option<String> {
    let query = raw_query?;
    for part in query.split('&') {
        let (raw_name, raw_value) = match part.find('=') {
            Some(eq) => (&part[..eq], &part[eq + 1..]),
            None => (part, ""),
        };
        let name = try_decode_uri_component(&raw_name.replace('+', " "));
        if name.is_empty() {
            continue;
        }
        if name == key {
            return Some(try_decode_uri_component(&raw_value.replace('+', " ")));
        }
    }
    None
}

/// `s.replace(/<[^>]+>/g, "")`.
pub fn strip_tags(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(rel) = s[i + 1..].find('>') {
                if rel >= 1 {
                    i += rel + 2;
                    continue;
                }
            }
        }
        let ch = s[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `s.split(/(?<=[.!?])\s+/)`.
pub fn split_sentences(s: &str) -> Vec<String> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = Vec::new();
    let mut piece_start = 0;
    let mut idx = 0;
    while idx < chars.len() {
        let (run_start, ch) = chars[idx];
        if !is_js_ws(ch) {
            idx += 1;
            continue;
        }
        let mut end = idx;
        while end < chars.len() && is_js_ws(chars[end].1) {
            end += 1;
        }
        let run_end = if end < chars.len() {
            chars[end].0
        } else {
            s.len()
        };
        let after_terminal = idx > 0 && matches!(chars[idx - 1].1, '.' | '!' | '?');
        if after_terminal {
            out.push(s[piece_start..run_start].to_string());
            piece_start = run_end;
        }
        idx = end;
    }
    out.push(s[piece_start..].to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn is_js_ws_covers_the_js_whitespace_set() {
        for c in [
            '\t', '\n', '\u{0B}', '\u{0C}', '\r', ' ', '\u{A0}', '\u{1680}', '\u{2000}', '\u{200A}',
            '\u{2028}', '\u{2029}', '\u{202F}', '\u{205F}', '\u{3000}', '\u{FEFF}',
        ] {
            assert!(is_js_ws(c), "{c:?}");
        }
        assert!(!is_js_ws('a'));
        assert!(!is_js_ws('\u{200B}'));
    }

    #[test]
    fn js_trim_strips_the_js_whitespace_set() {
        assert_eq!(js_trim("\u{FEFF} a \n"), "a");
        assert_eq!(js_trim("\u{A0}\u{3000}x\u{2029}"), "x");
        assert_eq!(js_trim("  "), "");
    }

    #[test]
    fn collapse_ws_collapses_every_run() {
        assert_eq!(collapse_ws("a \n\t b"), "a b");
        assert_eq!(collapse_ws("  a  "), " a ");
        assert_eq!(collapse_ws(""), "");
    }

    #[test]
    fn is_word_byte_matches_js_word_chars() {
        for b in [b'a', b'Z', b'0', b'9', b'_'] {
            assert!(is_word_byte(b), "{b}");
        }
        for b in [b'-', b' ', b'.'] {
            assert!(!is_word_byte(b), "{b}");
        }
    }

    #[test]
    fn find_ci_is_ascii_case_insensitive() {
        assert_eq!(find_ci("xx<MAIN>", "<main", 0), Some(2));
        assert_eq!(find_ci("xx<MAIN>", "<main", 3), None);
        assert_eq!(find_ci("abc", "abc", 4), None);
        assert_eq!(find_ci("abc", "", 1), Some(1));
    }

    #[test]
    fn truthy_matches_js() {
        for (v, expected) in [
            (Value::Null, false),
            (Value::Bool(false), false),
            (Value::Bool(true), true),
            (parse_json("0").unwrap(), false),
            (parse_json("0.5").unwrap(), true),
            (parse_json("-0.0").unwrap(), false),
            (parse_json("1").unwrap(), true),
            (Value::String(String::new()), false),
            (Value::from(" "), true),
            (parse_json("[]").unwrap(), true),
            (parse_json("{}").unwrap(), true),
        ] {
            assert_eq!(truthy(&v), expected, "{v}");
        }
    }

    #[test]
    fn js_string_matches_js_string() {
        assert_eq!(js_string(None), "undefined");
        assert_eq!(js_string(Some(&Value::Null)), "null");
        assert_eq!(js_string(Some(&Value::Bool(true))), "true");
        assert_eq!(js_string(Some(&Value::from("x"))), "x");
        assert_eq!(js_string(Some(&Value::from(1.0))), "1");
        assert_eq!(js_string(Some(&Value::from(-0.0))), "0");
        assert_eq!(js_string(Some(&Value::from(0.5))), "0.5");
        assert_eq!(js_string(Some(&parse_json("[1,null,\"a\"]").unwrap())), "1,,a");
        assert_eq!(js_string(Some(&parse_json("{\"a\":1}").unwrap())), "[object Object]");
    }

    #[test]
    fn parse_json_normalises_numbers_and_keeps_key_order() {
        let v = parse_json("{\"b\":1.0,\"a\":[2e2,-0.0,0.5]}").expect("json");
        assert_eq!(
            serde_json::to_string(&v).expect("serialise"),
            "{\"b\":1,\"a\":[200,0,0.5]}"
        );
        assert!(parse_json("{nope").is_err());
    }

    #[test]
    fn decode_uri_component_is_strict() {
        assert_eq!(decode_uri_component("%2e%2e%2F").as_deref(), Some("../"));
        assert_eq!(decode_uri_component("%E0%A4%A"), None);
        assert_eq!(decode_uri_component("%"), None);
        assert_eq!(decode_uri_component("%zz"), None);
        assert_eq!(decode_uri_component("%C3%28"), None);
        assert_eq!(decode_uri_component("%00").as_deref(), Some("\0"));
        assert_eq!(decode_uri_component("a+b").as_deref(), Some("a+b"));
    }

    #[test]
    fn try_decode_uri_component_keeps_bad_escapes() {
        assert_eq!(try_decode_uri_component("ok%20x/%A4%A2"), "ok x/%A4%A2");
        assert_eq!(try_decode_uri_component("plain"), "plain");
        assert_eq!(try_decode_uri_component("%E0%A4%A"), "%E0%A4%A");
    }

    #[test]
    fn query_param_reads_the_first_decoded_value() {
        assert_eq!(
            query_param(Some("path=a+b%2Fc&path=z"), "path").as_deref(),
            Some("a b/c")
        );
        assert_eq!(query_param(Some("x=1&path"), "path").as_deref(), Some(""));
        assert_eq!(query_param(Some("x=1"), "path"), None);
        assert_eq!(query_param(None, "path"), None);
        assert_eq!(
            query_param(Some("path=%E0%A4%A"), "path").as_deref(),
            Some("%E0%A4%A")
        );
    }

    #[test]
    fn strip_tags_removes_only_closed_tags() {
        assert_eq!(strip_tags("a<b>c<>d<e"), "ac<>d<e");
        assert_eq!(strip_tags("<p class=\"x\">t</p>"), "t");
    }

    #[test]
    fn split_sentences_splits_after_terminal_marks() {
        assert_eq!(
            split_sentences("Uno. Due!  Tre? quattro"),
            strings(&["Uno.", "Due!", "Tre?", "quattro"])
        );
        assert_eq!(split_sentences("a.b c"), strings(&["a.b c"]));
        assert_eq!(split_sentences(""), strings(&[""]));
        assert_eq!(split_sentences("Uno.  \n"), strings(&["Uno.", ""]));
    }
}
