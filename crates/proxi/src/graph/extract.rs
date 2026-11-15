//! The lesson text the map extractor reads (port of `htmlToText`, `app/server/graph/extract.ts:39-54`).

use crate::js;

/// Node's `ENTITIES` table (`extract.ts:36-38`): the exact case wins, the lowercase spelling is the fallback.
const ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", " "),
    ("agrave", "à"),
    ("egrave", "è"),
    ("igrave", "ì"),
    ("ograve", "ò"),
    ("ugrave", "ù"),
    ("eacute", "é"),
    ("Agrave", "À"),
    ("Egrave", "È"),
    ("Eacute", "É"),
    ("hellip", "…"),
    ("mdash", "—"),
    ("ndash", "–"),
];

/// Tags whose whole block disappears, `head` included.
const RAW_BLOCKS: [&str; 4] = ["script", "style", "svg", "head"];

/// Closing tags that read as a paragraph break.
const BLOCK_CLOSERS: [&str; 14] = [
    "p",
    "div",
    "li",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "tr",
    "pre",
    "blockquote",
    "section",
    "article",
];

/// The lesson's visible text: raw blocks and markup gone, breaks as newlines, entities decoded.
pub fn html_to_text(html: &str) -> String {
    let text = drop_raw_blocks(html);
    let text = breaks_to_newlines(&text);
    let text = js::strip_tags(&text);
    let text = decode_entities(&text);
    let text = drop_blanks_before_newlines(&text);
    let text = collapse_newlines(&text);
    js::js_trim(&text).to_string()
}

/// `/<(script|style|svg|head)[\s\S]*?<\/\1>/gi` -> `" "`.
///
/// Node's pattern has no name boundary, so `<header>` opens a `head` block that runs to the first
/// `</head>`, and `</header>` never closes it: left as is, the coverage fingerprint must not move.
/// A block with no closing tag is left for the tag stripper.
fn drop_raw_blocks(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut kept_from = 0;
    let mut at = 0;
    while let Some(rel) = html[at..].find('<') {
        let open = at + rel;
        at = open + 1;
        let Some(name) = RAW_BLOCKS
            .iter()
            .find(|name| starts_with_ci(&html[open + 1..], name))
        else {
            continue;
        };
        let closer = format!("</{name}>");
        let Some(end) = js::find_ci(html, &closer, open + 1 + name.len()) else {
            continue;
        };
        out.push_str(&html[kept_from..open]);
        out.push(' ');
        kept_from = end + closer.len();
        at = kept_from;
    }
    out.push_str(&html[kept_from..]);
    out
}

/// `/<br\s*\/?>|<\/(p|div|li|h[1-6]|tr|pre|blockquote|section|article)>/gi` -> `"\n"`.
fn breaks_to_newlines(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut kept_from = 0;
    let mut at = 0;
    while let Some(rel) = html[at..].find('<') {
        let open = at + rel;
        match break_tag_end(&html[open..]) {
            Some(len) => {
                out.push_str(&html[kept_from..open]);
                out.push('\n');
                at = open + len;
                kept_from = at;
            }
            None => at = open + 1,
        }
    }
    out.push_str(&html[kept_from..]);
    out
}

/// Byte length of the tag at `s` when it is one Node turns into a newline; `s` starts with `<`.
fn break_tag_end(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('<')?;
    if let Some(after) = strip_prefix_ci(rest, "br") {
        let mut blanks = 0;
        for c in after.chars() {
            if !js::is_js_ws(c) {
                break;
            }
            blanks += c.len_utf8();
        }
        let after = after[blanks..]
            .strip_prefix('/')
            .unwrap_or(&after[blanks..]);
        return after.strip_prefix('>').map(|tail| s.len() - tail.len());
    }
    let after = rest.strip_prefix('/')?;
    BLOCK_CLOSERS.iter().find_map(|name| {
        let tail = strip_prefix_ci(after, name)?.strip_prefix('>')?;
        Some(s.len() - tail.len())
    })
}

/// `&(#x?[0-9a-f]+|\w+);` decoded, case-insensitively; anything else is left alone.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(rel) = text[at..].find('&') {
        let start = at + rel;
        match entity_at(&text[start..]) {
            Some((len, decoded)) => {
                out.push_str(&text[at..start]);
                out.push_str(&decoded);
                at = start + len;
            }
            None => {
                out.push_str(&text[at..=start]);
                at = start + 1;
            }
        }
    }
    out.push_str(&text[at..]);
    out
}

/// `(byte length including `&` and `;`, decoded text)` for the entity at `s`, which starts with `&`.
fn entity_at(s: &str) -> Option<(usize, String)> {
    let body = &s[1..];
    let bytes = body.as_bytes();
    let numeric = bytes.first() == Some(&b'#');
    let first_digit = if numeric && matches!(bytes.get(1), Some(b'x' | b'X')) {
        2
    } else if numeric {
        1
    } else {
        0
    };
    let mut end = first_digit;
    while bytes.get(end).is_some_and(|b| {
        if numeric {
            b.is_ascii_hexdigit()
        } else {
            b.is_ascii_alphanumeric() || *b == b'_'
        }
    }) {
        end += 1;
    }
    if end == first_digit || bytes.get(end) != Some(&b';') {
        return None;
    }
    let inner = &body[..end];
    Some((end + 2, decode_entity(inner, &s[..end + 2])))
}

/// The decoded entity, or the matched text when Node keeps it as it is.
fn decode_entity(inner: &str, original: &str) -> String {
    let Some(rest) = inner.strip_prefix('#') else {
        let lower = inner.to_ascii_lowercase();
        return ENTITIES
            .iter()
            .find(|(name, _)| *name == inner)
            .or_else(|| ENTITIES.iter().find(|(name, _)| *name == lower.as_str()))
            .map_or_else(|| original.to_string(), |(_, value)| (*value).to_string());
    };
    let (digits, radix) = match rest.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (rest, 10),
    };
    // A lone surrogate is the one divergence: Node yields an unpaired code unit, which a Rust
    // `char` cannot hold, so the entity text stays.
    match parse_int(digits, radix).and_then(char::from_u32) {
        Some(c) if c != '\0' => c.to_string(),
        _ => original.to_string(),
    }
}

/// `parseInt` over the digits the entity regex already matched, saturating far past the Unicode range.
fn parse_int(digits: &str, radix: u32) -> Option<u32> {
    let mut value: u32 = 0;
    let mut any = false;
    for b in digits.bytes() {
        let Some(digit) = (b as char).to_digit(radix) else {
            break;
        };
        value = value.saturating_mul(radix).saturating_add(digit);
        any = true;
    }
    any.then_some(value)
}

/// `/[ \t]+\n/g` -> `"\n"`.
fn drop_blanks_before_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = String::new();
    for c in text.chars() {
        match c {
            '\n' => {
                blanks.clear();
                out.push('\n');
            }
            ' ' | '\t' => blanks.push(c),
            _ => {
                out.push_str(&blanks);
                blanks.clear();
                out.push(c);
            }
        }
    }
    out.push_str(&blanks);
    out
}

/// `/\n{3,}/g` -> `"\n\n"`.
fn collapse_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
            if run <= 2 {
                out.push('\n');
            }
        } else {
            run = 0;
            out.push(c);
        }
    }
    out
}

/// ASCII case-insensitive `str::strip_prefix`.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    strip_prefix_ci(s, prefix).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_blocks_go_with_their_content_in_any_case() {
        assert_eq!(
            html_to_text("<SCRIPT>a</SCRIPT><Style>b</STYLE><svg>c</svg><HEAD>d</HEAD>e"),
            "e"
        );
        assert_eq!(
            html_to_text("<head><title>t</title></head><p>body</p>"),
            "body"
        );
        assert_eq!(html_to_text("<script>if (a<b) {}</script>c"), "c");
        assert_eq!(html_to_text("<head>a</head>x"), "x");
        assert_eq!(html_to_text("<head</head>x"), "x");
    }

    #[test]
    fn a_dropped_block_leaves_a_space_behind() {
        assert_eq!(
            html_to_text("<p>a</p><script>var x = 1;</script><p>b</p>"),
            "a\n b"
        );
    }

    #[test]
    fn a_block_without_its_closing_tag_is_left_for_the_tag_stripper() {
        assert_eq!(html_to_text("<script>never closed"), "never closed");
        assert_eq!(html_to_text("<head>a</header>b"), "ab");
    }

    #[test]
    fn every_br_spelling_is_one_newline() {
        assert_eq!(
            html_to_text("<p>a<br>b<BR/>c<br />d<br\n/>e<br\t />f</p>"),
            "a\nb\nc\nd\ne\nf"
        );
        assert_eq!(html_to_text("a<br/>b"), "a\nb");
        assert_eq!(html_to_text("<br>"), "");
    }

    #[test]
    fn block_closing_tags_are_newlines() {
        assert_eq!(
            html_to_text(
                "a</div>b</li>c</h1>d</h6>e</tr>f</pre>g</blockquote>h</section>i</article>j</p>"
            ),
            "a\nb\nc\nd\ne\nf\ng\nh\ni\nj"
        );
        assert_eq!(html_to_text("<h1>T</h1><p>x</p>"), "T\nx");
        assert_eq!(html_to_text("<p>a</p><p>b</p>"), "a\nb");
    }

    #[test]
    fn a_spaced_closing_tag_is_no_break() {
        assert_eq!(html_to_text("a</p >b"), "ab");
    }

    #[test]
    fn tags_are_stripped_and_their_text_kept() {
        assert_eq!(html_to_text("<span class=\"x\">t</span>"), "t");
        assert_eq!(html_to_text("x<b>y</b>z"), "xyz");
        assert_eq!(html_to_text("a<!-- comment -->b"), "ab");
    }

    #[test]
    fn numeric_entities_decode_decimal_and_hex() {
        assert_eq!(html_to_text("&#65;&#x41;&#X41;"), "AAA");
        assert_eq!(html_to_text("&#1114111;"), "\u{10FFFF}");
        assert_eq!(html_to_text("&#x10FFFF;"), "\u{10FFFF}");
    }

    #[test]
    fn an_out_of_range_or_zero_code_point_keeps_the_entity() {
        assert_eq!(html_to_text("&#0;"), "&#0;");
        assert_eq!(html_to_text("&#x0;"), "&#x0;");
        assert_eq!(html_to_text("&#1114112;"), "&#1114112;");
        assert_eq!(html_to_text("&#x110000;"), "&#x110000;");
        assert_eq!(
            html_to_text("&#999999999999999999999;"),
            "&#999999999999999999999;"
        );
    }

    #[test]
    fn a_decimal_entity_stops_at_the_first_non_digit() {
        assert_eq!(html_to_text("&#1a;"), "\u{1}");
        assert_eq!(html_to_text("&#a1;"), "&#a1;");
    }

    #[test]
    fn a_lone_surrogate_keeps_the_entity_text() {
        // Node gives "\ud800" here; a Rust `char` cannot hold it (the module's one divergence).
        assert_eq!(html_to_text("&#xD800;"), "&#xD800;");
    }

    #[test]
    fn named_entities_use_the_exact_case_then_the_lowercase_fallback() {
        assert_eq!(html_to_text("&Egrave;&egrave;&EGRAVE;"), "Èèè");
        assert_eq!(html_to_text("&Agrave;&Eacute;&eacute;"), "ÀÉé");
        assert_eq!(html_to_text("&AMP;&Amp;&amp;"), "&&&");
        assert_eq!(html_to_text("&hellip;&mdash;&ndash;"), "…—–");
        assert_eq!(html_to_text("&amp;lt;"), "&lt;");
    }

    #[test]
    fn an_unknown_entity_stays_as_text() {
        assert_eq!(html_to_text("&foo;"), "&foo;");
        assert_eq!(html_to_text("A & B"), "A & B");
    }

    #[test]
    fn whitespace_before_a_newline_goes_and_three_newlines_become_two() {
        assert_eq!(html_to_text("a  \n  b\t\nc"), "a\n  b\nc");
        assert_eq!(html_to_text("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(html_to_text("<p>  a  </p>\n\n\n<p>b</p>"), "a\n\nb");
        assert_eq!(html_to_text("  a  "), "a");
    }

    #[test]
    fn a_carriage_return_is_not_a_newline() {
        assert_eq!(html_to_text("<p>a</p>\r\n<p>b</p>"), "a\n\r\nb");
    }

    #[test]
    fn malformed_html_is_text() {
        assert_eq!(html_to_text("a < b"), "a < b");
        assert_eq!(html_to_text("<p>unclosed"), "unclosed");
        assert_eq!(html_to_text("&#xZZ;"), "&#xZZ;");
        assert_eq!(html_to_text("<<>>"), ">");
        assert_eq!(html_to_text(""), "");
    }

    #[test]
    fn a_tag_name_is_not_a_word_boundary() {
        assert_eq!(html_to_text("<header>x</header>"), "x");
        assert_eq!(html_to_text("<header>x</header><head>y</head>z"), "z");
        assert_eq!(html_to_text("<header>x</head>er>"), "er>");
        assert_eq!(html_to_text("<headline>x</head>y"), "y");
        assert_eq!(html_to_text("<scripts>a</script>b"), "b");
        assert_eq!(html_to_text("<svgfoo>a</svg>b"), "b");
        assert_eq!(html_to_text("a<head>b</header>c</head>d"), "a d");
        assert_eq!(html_to_text("<p>a</p><header>T</header><p>b</p>"), "a\nTb");
    }
}
