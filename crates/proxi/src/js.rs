//! JavaScript semantics the Node port depends on: JSON parsing, number shapes, string and regex helpers.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Number, Value};

/// 2^53, the largest integer a JS number holds exactly.
const MAX_SAFE: f64 = 9007199254740992.0;

/// `JSON.parse` plus JS number shapes: an integral float comes back as an integer (`1.0` -> `1`).
pub fn parse(text: &str) -> Result<Value, serde_json::Error> {
    let value: Value = serde_json::from_str(text)?;
    Ok(normalise(value))
}

fn normalise(value: Value) -> Value {
    match value {
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                return Value::Number(n);
            }
            match n.as_f64() {
                Some(x) if is_safe_integer(x) => Value::from(x as i64),
                _ => Value::Number(n),
            }
        }
        Value::Array(items) => Value::Array(items.into_iter().map(normalise).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, normalise(item)))
                .collect(),
        ),
        other => other,
    }
}

fn is_safe_integer(x: f64) -> bool {
    x.is_finite() && x.fract() == 0.0 && x.abs() <= MAX_SAFE
}

/// A JS number as JSON: `5`, never `5.0`; `-0` prints as `0`; NaN and infinities become `null`.
pub fn number(x: f64) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    if is_safe_integer(x) {
        return Value::from(x as i64);
    }
    Value::Number(Number::from_f64(x).unwrap())
}

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

/// `String.prototype.localeCompare` with the default locale: an ASCII approximation of ICU root
/// collation. Punctuation sorts before digits, digits before letters; case breaks ties, lowercase
/// first. Non-ASCII characters sort after `z` by their lowercase code point; full ICU is out of scope.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let mut left = a.chars();
    let mut right = b.chars();
    loop {
        match (left.next(), right.next()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match primary_key(x).cmp(&primary_key(y)) {
                Ordering::Equal => {}
                other => return other,
            },
        }
    }
    for (x, y) in a.chars().zip(b.chars()) {
        match case_rank(x).cmp(&case_rank(y)) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    a.chars().cmp(b.chars())
}

/// ICU order for ASCII punctuation/symbols (`localeCompare` puts `_` first, `$` last).
const PUNCTUATION_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary_key(c: char) -> (i8, u32) {
    if c.is_whitespace() {
        return (-1, 0);
    }
    if c.is_ascii_punctuation() {
        let index = PUNCTUATION_ORDER.find(c).expect("every ASCII punctuation is listed");
        return (0, index as u32);
    }
    if c.is_ascii_digit() {
        return (1, c as u32 - '0' as u32);
    }
    let lower = c.to_lowercase().next().unwrap_or(c);
    (2, lower as u32)
}

fn case_rank(c: char) -> u8 {
    if c.is_uppercase() {
        1
    } else {
        0
    }
}

/// JS truthiness: only null, false, 0, NaN and "" are falsy (empty arrays and objects are true).
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0 && !x.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
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

/// JS `String(v)` / template interpolation.
pub fn display(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => {
            let shape = number(n.as_f64().unwrap_or(f64::NAN));
            if shape.is_null() {
                "null".to_string()
            } else {
                serde_json::to_string(&shape).unwrap_or_default()
            }
        }
        Some(Value::String(s)) => s.clone(),
        // Array.join: null and undefined elements print as nothing.
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    String::new()
                } else {
                    display(Some(item))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// JS `Number(v)`, coercion included.
pub fn to_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// `Number.isFinite(v)`: no coercion, strings and booleans are not numbers.
pub fn finite(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
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

/// tmp + rename; Windows can refuse the rename (EPERM, AV/indexer), so fall back to a direct write.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    let tmp = PathBuf::from(name);
    fs::write(&tmp, data)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::write(path, data)?;
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-js-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn string_of(value: &Value) -> String {
        serde_json::to_string(value).unwrap()
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
    fn locale_compare_approximates_icu_root_order() {
        let mut names = ["Esercizio.vue", "app.js"];
        names.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(names, ["app.js", "Esercizio.vue"]);

        assert_eq!(locale_compare("a", "B"), std::cmp::Ordering::Less);
        assert_eq!(locale_compare("a", "A"), std::cmp::Ordering::Less);
        assert_eq!(locale_compare("_x", "1"), std::cmp::Ordering::Less);
        assert_eq!(locale_compare("1", "a"), std::cmp::Ordering::Less);

        let mut names = ["zz-v", "02-a", "01-b", "01-a"];
        names.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(names, ["01-a", "01-b", "02-a", "zz-v"]);

        assert_eq!(locale_compare("a-b", "a.b"), std::cmp::Ordering::Less);
    }

    #[test]
    fn parse_reads_integral_floats_as_integers() {
        let parsed =
            parse("{\"a\":1.0,\"b\":2.5,\"c\":[3.0],\"d\":-0.0,\"e\":1e300,\"f\":7}").unwrap();
        assert_eq!(
            string_of(&parsed),
            "{\"a\":1,\"b\":2.5,\"c\":[3],\"d\":0,\"e\":1e+300,\"f\":7}"
        );
    }

    #[test]
    fn number_keeps_the_js_shape() {
        assert_eq!(string_of(&number(64.0)), "64");
        assert_eq!(string_of(&number(-0.0)), "0");
        assert_eq!(string_of(&number(42.5)), "42.5");
        assert_eq!(string_of(&number(f64::NAN)), "null");
        assert_eq!(string_of(&number(f64::INFINITY)), "null");
    }

    #[test]
    fn truthy_matches_js() {
        assert!(!truthy(None));
        for (v, expected) in [
            (Value::Null, false),
            (Value::Bool(false), false),
            (Value::Bool(true), true),
            (parse_json("0").unwrap(), false),
            (parse_json("0.5").unwrap(), true),
            (parse_json("-0.0").unwrap(), false),
            (parse_json("1").unwrap(), true),
            (Value::String(String::new()), false),
            (Value::from("0"), true),
            (Value::from(" "), true),
            (parse_json("[]").unwrap(), true),
            (parse_json("{}").unwrap(), true),
        ] {
            assert_eq!(truthy(Some(&v)), expected, "{v}");
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
    fn display_follows_string() {
        assert_eq!(display(None), "undefined");
        assert_eq!(display(Some(&Value::Null)), "null");
        assert_eq!(display(Some(&json!(true))), "true");
        assert_eq!(display(Some(&json!(1))), "1");
        assert_eq!(display(Some(&json!(1.5))), "1.5");
        assert_eq!(display(Some(&json!("x"))), "x");
        assert_eq!(display(Some(&json!([1, null, "a"]))), "1,,a");
        assert_eq!(display(Some(&json!({}))), "[object Object]");
    }

    #[test]
    fn to_number_follows_number() {
        assert_eq!(to_number(Some(&json!("  2 "))), 2.0);
        assert_eq!(to_number(Some(&json!(""))), 0.0);
        assert!(to_number(Some(&json!("x"))).is_nan());
        assert_eq!(to_number(Some(&json!(true))), 1.0);
        assert_eq!(to_number(Some(&json!(null))), 0.0);
        assert!(to_number(None).is_nan());
    }

    #[test]
    fn finite_does_not_coerce() {
        assert_eq!(finite(Some(&json!("5"))), None);
        assert_eq!(finite(Some(&json!(5))), Some(5.0));
        assert_eq!(finite(Some(&Value::Null)), None);
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

    #[test]
    fn write_atomic_leaves_the_bytes_and_no_tmp() {
        let dir = temp_dir("atomic");
        let file = dir.join("layout.json");
        write_atomic(&file, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"{\"a\":1}");
        assert!(!dir.join("layout.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
