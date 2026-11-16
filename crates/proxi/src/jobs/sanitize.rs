//! SVG and demo sanitizers (port of app/server/jobs/sanitize.ts).

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::js::js_trim;
use crate::sessions::utf16_len;
use crate::vitest::truncate_utf16;

pub const MAX_SVG: usize = 200_000;
pub const MAX_HTML: usize = 500_000;

static DECODE_HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)&#x([0-9a-f]+);?").unwrap());
static DECODE_DEC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#([0-9]+);?").unwrap());
static DECODE_COLON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)&colon;").unwrap());
static DECODE_TAB_NEWLINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)&(?:tab|newline);").unwrap());
static STRIP_CONTROLS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\x00-\x1F\x7F]+").unwrap());
static CSS_ESCAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\\([0-9a-f]{1,6})\s?").unwrap());
/// JS `.` excludes every line terminator, Rust's only `\n`.
static BACKSLASH_ANY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\([^\n\r\u{2028}\u{2029}])").unwrap());

/// `TAG`, then `ATTR` over its attribute text.
static TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<(/?)([a-zA-Z][^\s/>]*)((?:"[^"]*"|'[^']*'|[^>"'])*)>"#).unwrap()
});
static ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([^\s=/>"']+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>"']+)))?"#).unwrap()
});
/// The backreference (`\1`) becomes a three-way alternation; the first present group is the value.
static CSS_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)url\s*\(\s*(?:"([^)'"]*)"|'([^)'"]*)'|([^)'"]*))\s*\)"#).unwrap()
});

static SVG_ELEMENTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    concat!(
        "svg g defs symbol use path rect circle ellipse line polyline polygon text tspan textpath title desc metadata marker ",
        "lineargradient radialgradient stop clippath mask pattern style filter fegaussianblur feoffset feblend fecolormatrix ",
        "feflood fecomposite femerge femergenode fedropshadow fecomponenttransfer fefunca fefuncr fefuncg fefuncb femorphology ",
        "feturbulence fediffuselighting fespecularlighting fedistantlight fepointlight fespotlight fetile feimage a switch"
    )
    .split_whitespace()
    .collect()
});
/// Attributes that carry a URL: only a same-document `#id` is allowed.
static URL_ATTRS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["href", "xlink:href"].into_iter().collect());
/// Attributes that never make sense in a figure and are classic carriers of external loads.
static BANNED_ATTRS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "src",
        "srcset",
        "background",
        "action",
        "formaction",
        "poster",
        "ping",
        "data",
        "style-src",
    ]
    .into_iter()
    .collect()
});

static CSS_IMPORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)@import").unwrap());
static CSS_BAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)image-set\s*\(|(?:^|[^0-9A-Za-z_-])src\s*\(|cross-fade\s*\(|expression\s*\(|behavior\s*:")
        .unwrap()
});
static SVG_JAVASCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:java|vb)script:|data:text/html").unwrap());

static XML_PROLOG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(?:<\?xml[^>]*\?>\s*)?").unwrap());
static HTML_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s:<!--.*?-->)").unwrap());
static SVG_ROOT_OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^<svg[\s>/]").unwrap());
static SVG_ROOT_CLOSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)</svg\s*>\s*$|/>\s*$").unwrap());
static DOCTYPE_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<!\s*(?:doctype|entity)").unwrap());
static CDATA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<!\[cdata\[").unwrap());
static STYLE_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<style(?-u:\b)[^>]*>(?s:(.*?))</style\s*>").unwrap());
static STYLE_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<\s*style(?-u:\b)").unwrap());
static STYLE_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)</style\s*>").unwrap());

static REMOTE_PROTOCOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(?:https?:)?//").unwrap());
static REMOTE_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(?:ftp|ws|wss|file):").unwrap());
static REFRESH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)refresh").unwrap());
static DEMO_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)@import\s+(?:url\()?\s*['"]?\s*(?:https?:)?//"#).unwrap());
static DEMO_FETCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?-u:\b)(?:fetch|WebSocket|EventSource|sendBeacon|importScripts|import)\s*\(\s*[`'"]\s*(?:https?:|wss?:)?//"#,
    )
    .unwrap()
});
static DEMO_XHR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.open\s*\(\s*[`'"][A-Za-z]+[`'"]\s*,\s*[`'"]\s*(?:https?:)?//"#).unwrap()
});
static DEMO_ASSIGN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\.(?:src|href)\s*=\s*[`'"]\s*(?:https?:)?//"#).unwrap());

/// Entities decoded (`&#106;avascript:`), control chars and CSS escapes removed, so obfuscation cannot hide a scheme.
fn decode(s: &str) -> String {
    let hex = DECODE_HEX.replace_all(s, |c: &Captures| code_point(parse_digits(&c[1], 16)));
    let dec = DECODE_DEC.replace_all(&hex, |c: &Captures| code_point(parse_digits(&c[1], 10)));
    let colon = DECODE_COLON.replace_all(&dec, ":");
    let tab_newline = DECODE_TAB_NEWLINE.replace_all(&colon, "");
    let controls = STRIP_CONTROLS.replace_all(&tab_newline, "");
    let css = CSS_ESCAPE.replace_all(&controls, |c: &Captures| {
        code_point(parse_digits(&c[1], 16))
    });
    BACKSLASH_ANY.replace_all(&css, "$1").into_owned()
}

/// `parseInt(digits, radix)` as far as `cp` cares: an overflow is past `0x10ffff` and stays empty.
fn parse_digits(digits: &str, radix: u32) -> Option<u32> {
    u32::from_str_radix(digits, radix).ok()
}

/// `String.fromCodePoint` for a parsed entity; a lone surrogate becomes U+FFFD (Rust cannot hold one).
fn code_point(n: Option<u32>) -> String {
    match n {
        Some(n) if n > 0 && n <= 0x10FFFF => char::from_u32(n)
            .map(|c| c.to_string())
            .unwrap_or_else(|| "\u{FFFD}".to_string()),
        _ => String::new(),
    }
}

struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
}

/// [tag name lowercased, attributes] of every tag in `s`.
fn tags(s: &str) -> Vec<Tag> {
    TAG.captures_iter(s)
        .map(|m| Tag {
            name: m[2].to_lowercase(),
            attrs: ATTR
                .captures_iter(m.get(3).map_or("", |g| g.as_str()))
                .map(|a| {
                    let value = a.get(2).or_else(|| a.get(3)).or_else(|| a.get(4));
                    (
                        a[1].to_lowercase(),
                        value.map_or_else(String::new, |v| v.as_str().to_string()),
                    )
                })
                .collect(),
        })
        .collect()
}

fn css_url_value<'a>(m: &'a Captures<'_>) -> &'a str {
    m.get(1)
        .or_else(|| m.get(2))
        .or_else(|| m.get(3))
        .map_or("", |g| g.as_str())
}

fn check_css(css: &str, errs: &mut Vec<String>) {
    let c = decode(css);
    if CSS_IMPORT.is_match(&c) {
        errs.push("@import non ammesso".to_string());
    }
    if CSS_BAD.is_match(&c) {
        errs.push("image-set()/src()/expression() non ammessi in CSS".to_string());
    }
    for m in CSS_URL.captures_iter(&c) {
        if !js_trim(css_url_value(&m)).starts_with('#') {
            errs.push(format!(
                "url() esterno non ammesso: {} (solo url(#id))",
                truncate_utf16(m.get(0).unwrap().as_str(), 60)
            ));
        }
    }
}

pub fn check_svg(svg: &str) -> Vec<String> {
    let mut errs = Vec::new();
    let len = utf16_len(svg);
    if len > MAX_SVG {
        return vec![format!(
            "SVG troppo grande ({len} caratteri, massimo {MAX_SVG})"
        )];
    }
    let without_prolog = XML_PROLOG.replace(svg, "");
    let body = HTML_COMMENT.replace_all(without_prolog.as_ref(), "");
    let body = js_trim(&body);
    if !SVG_ROOT_OPEN.is_match(body) || !SVG_ROOT_CLOSE.is_match(body) {
        errs.push(
            "la radice deve essere un elemento <svg> e il documento deve chiudersi con </svg>"
                .to_string(),
        );
    }
    if DOCTYPE_ENTITY.is_match(svg) {
        errs.push("DOCTYPE/ENTITY non ammessi".to_string());
    }
    if CDATA.is_match(body) || has_processing_instruction(body) {
        errs.push("CDATA e istruzioni di elaborazione non ammessi".to_string());
    }
    for t in tags(body) {
        if !SVG_ELEMENTS.contains(t.name.as_str()) {
            errs.push(format!("elemento <{}> non ammesso", t.name));
            continue;
        }
        for (name, raw) in &t.attrs {
            let v = js_trim(&decode(raw)).to_string();
            if name.starts_with("on") {
                errs.push("attributi di evento on*= non ammessi".to_string());
            } else if BANNED_ATTRS.contains(name.as_str()) {
                errs.push(format!("attributo {name}= non ammesso"));
            } else if URL_ATTRS.contains(name.as_str()) && !v.starts_with('#') {
                errs.push(format!(
                    "riferimento esterno non ammesso: {name}=\"{}\" (solo href=\"#id\")",
                    truncate_utf16(raw, 50)
                ));
            } else if name == "style" {
                check_css(raw, &mut errs);
            }
            if SVG_JAVASCRIPT.is_match(&v) {
                errs.push("URL javascript:/data:text/html non ammessi".to_string());
            }
            if name != "style" {
                for m in CSS_URL.captures_iter(&v) {
                    if !js_trim(css_url_value(&m)).starts_with('#') {
                        errs.push(format!(
                            "url() esterno non ammesso: {} (solo url(#id))",
                            truncate_utf16(m.get(0).unwrap().as_str(), 60)
                        ));
                    }
                }
            }
        }
    }
    for m in STYLE_BLOCK.captures_iter(body) {
        check_css(&m[1], &mut errs);
    }
    if STYLE_OPEN.is_match(body) && !STYLE_CLOSE.is_match(body) {
        errs.push("<style> non chiuso".to_string());
    }
    dedupe(errs)
}

/// `<\?(?!xml)` with JS's case-insensitive lookahead: the regex crate has none.
fn has_processing_instruction(body: &str) -> bool {
    body.as_bytes().windows(2).enumerate().any(|(i, w)| {
        w == b"<?"
            && !body[i + 2..]
                .get(..3)
                .is_some_and(|s| s.eq_ignore_ascii_case("xml"))
    })
}

/// Early feedback only (cheap, bypassable by design): the real boundary is the CSP the page runs under.
pub fn check_demo(html: &str) -> Vec<String> {
    let mut errs = Vec::new();
    if js_trim(html).is_empty() {
        return vec!["HTML vuoto".to_string()];
    }
    let len = utf16_len(html);
    if len > MAX_HTML {
        return vec![format!(
            "HTML troppo grande ({len} caratteri, massimo {MAX_HTML})"
        )];
    }
    for t in tags(html) {
        if matches!(t.name.as_str(), "iframe" | "object" | "embed" | "base") {
            errs.push("<iframe>/<object>/<embed>/<base> non ammessi".to_string());
        }
        for (name, raw) in &t.attrs {
            let v = decode(raw);
            if is_demo_url_attr(name) && remote(&v) {
                errs.push(format!(
                    "risorsa di rete esterna non ammessa: {name}=\"{}\"",
                    truncate_utf16(raw, 60)
                ));
            }
            if name == "http-equiv" && REFRESH.is_match(&v) {
                errs.push("<meta refresh> non ammesso".to_string());
            }
        }
    }
    let css = decode(html);
    for m in CSS_URL.captures_iter(&css) {
        if remote(css_url_value(&m)) {
            errs.push(format!(
                "url() esterno non ammesso: {}",
                truncate_utf16(m.get(0).unwrap().as_str(), 80)
            ));
        }
    }
    if DEMO_IMPORT.is_match(&css) {
        errs.push("@import esterno non ammesso".to_string());
    }
    if DEMO_FETCH.is_match(html) || DEMO_XHR.is_match(html) {
        errs.push(
            "chiamate di rete verso URL remoti non ammesse (fetch/XMLHttpRequest/WebSocket/import)"
                .to_string(),
        );
    }
    if DEMO_ASSIGN.is_match(html) {
        errs.push("assegnazione di src/href remoti non ammessa".to_string());
    }
    dedupe(errs)
}

fn is_demo_url_attr(name: &str) -> bool {
    matches!(
        name,
        "href"
            | "xlink:href"
            | "src"
            | "srcset"
            | "action"
            | "formaction"
            | "poster"
            | "background"
            | "ping"
            | "data"
    )
}

fn remote(v: &str) -> bool {
    REMOTE_PROTOCOL.is_match(v) || REMOTE_SCHEME.is_match(v)
}

/// `[...new Set(errs)]`: first occurrence order.
fn dedupe(errs: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(errs.len());
    for err in errs {
        if seen.insert(err.clone()) {
            out.push(err);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected lists below are Node's own `checkSvg`/`checkDemo` output, verbatim.
    fn expect(errs: &[&str]) -> Vec<String> {
        errs.iter().map(|e| e.to_string()).collect()
    }

    const OK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><defs><marker id="a"><path d="M0 0"/></marker></defs><path marker-end="url(#a)" d="M0 0L5 5" stroke="#eee"/><text x="1" y="1">x &#8594; y</text></svg>"##;
    const ROOT: &str =
        "la radice deve essere un elemento <svg> e il documento deve chiudersi con </svg>";
    const JS_URL: &str = "URL javascript:/data:text/html non ammessi";

    #[test]
    fn accepts_a_plain_svg_and_local_urls() {
        assert_eq!(check_svg(OK), expect(&[]));
        assert_eq!(
            check_svg(&format!("<?xml version=\"1.0\"?>\n{OK}")),
            expect(&[])
        );
        assert_eq!(
            check_svg(r##"<svg><use href="#a"/><rect fill="url(#g)"/></svg>"##),
            expect(&[])
        );
        assert_eq!(
            check_svg(r#"<svg><rect fill="url(#g)" style="fill:url(#g)"/></svg>"#),
            expect(&[])
        );
        assert_eq!(
            check_svg(r#"<svg><style>.a{fill:url(#g)}</style></svg>"#),
            expect(&[])
        );
        assert_eq!(
            check_svg(r##"<svg><rect style='fill:url("#g")'/></svg>"##),
            expect(&[])
        );
        assert_eq!(
            check_svg(
                r##"<svg viewBox="0 0 10 10"><text x="1" y="1">data = 5 e src = 3</text><rect fill="#fff" style="fill:url(#g)"/></svg>"##
            ),
            expect(&[])
        );
    }

    #[test]
    fn rejects_every_jobs_svg_fixture() {
        let cases: &[(&str, &str, &[&str])] = &[
            (
                "script",
                "<svg><script>alert(1)</script></svg>",
                &["elemento <script> non ammesso"],
            ),
            (
                "SCRIPT upper",
                "<svg><SCRIPT >x</SCRIPT></svg>",
                &["elemento <script> non ammesso"],
            ),
            (
                "foreignObject",
                "<svg><foreignObject><div/></foreignObject></svg>",
                &[
                    "elemento <foreignobject> non ammesso",
                    "elemento <div> non ammesso",
                ],
            ),
            (
                "onload",
                r#"<svg onload="x()"></svg>"#,
                &["attributi di evento on*= non ammessi"],
            ),
            (
                "onclick after quote",
                "<svg><rect fill=\"red\"onclick=x()/></svg>",
                &["attributi di evento on*= non ammessi"],
            ),
            (
                "javascript href",
                r#"<svg><a href="javascript:alert(1)"><text>x</text></a></svg>"#,
                &[
                    r##"riferimento esterno non ammesso: href="javascript:alert(1)" (solo href="#id")"##,
                    JS_URL,
                ],
            ),
            (
                "entity-obfuscated javascript",
                r#"<svg><a href="&#106;avascript:alert(1)"><text>x</text></a></svg>"#,
                &[
                    r##"riferimento esterno non ammesso: href="&#106;avascript:alert(1)" (solo href="#id")"##,
                    JS_URL,
                ],
            ),
            (
                "whitespace javascript",
                "<svg><a href=\"java\tscript:alert(1)\"><text>x</text></a></svg>",
                &[
                    "riferimento esterno non ammesso: href=\"java\tscript:alert(1)\" (solo href=\"#id\")",
                    JS_URL,
                ],
            ),
            (
                "external href",
                r#"<svg><image href="https://evil.example/x.png"/></svg>"#,
                &["elemento <image> non ammesso"],
            ),
            (
                "xlink external",
                r#"<svg><image xlink:href="http://evil.example/x.png"/></svg>"#,
                &["elemento <image> non ammesso"],
            ),
            (
                "protocol-relative",
                r#"<svg><image href="//evil.example/x.png"/></svg>"#,
                &["elemento <image> non ammesso"],
            ),
            (
                "external url()",
                r#"<svg><rect style="fill:url(https://evil.example/x)"/></svg>"#,
                &["url() esterno non ammesso: url(https://evil.example/x) (solo url(#id))"],
            ),
            (
                "@import",
                r#"<svg><style>@import "x.css";</style></svg>"#,
                &["@import non ammesso"],
            ),
            (
                "not svg root",
                "<div><svg></svg></div>",
                &[ROOT, "elemento <div> non ammesso"],
            ),
            (
                "html root",
                "<html><svg></svg></html>",
                &[ROOT, "elemento <html> non ammesso"],
            ),
            (
                "trailing junk",
                "<svg></svg><script>1</script>",
                &[ROOT, "elemento <script> non ammesso"],
            ),
            (
                "doctype",
                "<!DOCTYPE svg [<!ENTITY x \"y\">]><svg>&x;</svg>",
                &[ROOT, "DOCTYPE/ENTITY non ammessi"],
            ),
            (
                "iframe",
                r#"<svg><iframe src="x"/></svg>"#,
                &["elemento <iframe> non ammesso"],
            ),
            (
                "animate href",
                r#"<svg><a><animate attributeName="href" values="javascript:alert(1)"/></a></svg>"#,
                &["elemento <animate> non ammesso"],
            ),
            ("empty", "", &[ROOT]),
        ];
        for (name, svg, expected) in cases {
            assert_eq!(check_svg(svg), expect(expected), "{name}");
        }
    }

    #[test]
    fn rejects_every_hardening_svg_fixture() {
        let cases: &[(&str, &str, &[&str])] = &[
            (
                "tab-split javascript (entity)",
                r#"<svg><a href="java&#9;script:alert(1)"><text>x</text></a></svg>"#,
                &[
                    "riferimento esterno non ammesso: href=\"java&#9;script:alert(1)\" (solo href=\"#id\")",
                    JS_URL,
                ],
            ),
            (
                "newline-split javascript (entity)",
                r#"<svg><a xlink:href="java&#10;script:alert(1)"><text>x</text></a></svg>"#,
                &[
                    "riferimento esterno non ammesso: xlink:href=\"java&#10;script:alert(1)\" (solo href=\"#id\")",
                    JS_URL,
                ],
            ),
            (
                "background attr",
                r#"<svg><rect background="http://evil.example/x"/></svg>"#,
                &["attributo background= non ammesso"],
            ),
            (
                "svg root then table background",
                r#"<svg/><table background="http://evil.example/x"><tr><td>x</td></tr></table>"#,
                &[
                    ROOT,
                    "elemento <table> non ammesso",
                    "elemento <tr> non ammesso",
                    "elemento <td> non ammesso",
                ],
            ),
            (
                "image element",
                r#"<svg><image href="data:image/png;base64,AAAA"/></svg>"#,
                &["elemento <image> non ammesso"],
            ),
            (
                "css escape url",
                "<svg><rect style=\"fill:u\\72l(http://evil.example/x)\"/></svg>",
                &["url() esterno non ammesso: url(http://evil.example/x) (solo url(#id))"],
            ),
            (
                "style element escape",
                "<svg><style>rect{fill:\\75rl(http://evil.example/x)}</style></svg>",
                &["url() esterno non ammesso: url(http://evil.example/x) (solo url(#id))"],
            ),
            (
                "image-set",
                "<svg><rect style=\"fill:image-set('http://evil.example/x' 1x)\"/></svg>",
                &["image-set()/src()/expression() non ammessi in CSS"],
            ),
            (
                "srcset",
                r#"<svg><rect srcset="http://evil.example/x"/></svg>"#,
                &["attributo srcset= non ammesso"],
            ),
        ];
        for (name, svg, expected) in cases {
            assert_eq!(check_svg(svg), expect(expected), "{name}");
        }
    }

    #[test]
    fn size_caps_count_utf16_units() {
        assert_eq!(
            check_svg(&"a".repeat(MAX_SVG + 1)),
            expect(&["SVG troppo grande (200001 caratteri, massimo 200000)"])
        );
        assert_eq!(
            check_svg(&"😀".repeat(MAX_SVG / 2 + 1)),
            expect(&["SVG troppo grande (200002 caratteri, massimo 200000)"])
        );
        // exactly at the cap: not the size error, the body is just invalid svg
        assert_eq!(check_svg(&"😀".repeat(MAX_SVG / 2)), expect(&[ROOT]));

        assert_eq!(check_demo(&"a".repeat(MAX_HTML)), expect(&[]));
        assert_eq!(
            check_demo(&"a".repeat(MAX_HTML + 1)),
            expect(&["HTML troppo grande (500001 caratteri, massimo 500000)"])
        );
        assert_eq!(
            check_demo(&"😀".repeat(MAX_HTML / 2 + 1)),
            expect(&["HTML troppo grande (500002 caratteri, massimo 500000)"])
        );
    }

    #[test]
    fn obfuscated_schemes_are_decoded() {
        for href in ["&#106;avascript:alert(1)", "&#x6A;avascript:alert(1)"] {
            let svg = format!(r#"<svg><a href="{href}"><text>x</text></a></svg>"#);
            assert_eq!(
                check_svg(&svg),
                expect(&[
                    &format!(
                        "riferimento esterno non ammesso: href=\"{href}\" (solo href=\"#id\")"
                    ),
                    JS_URL
                ])
            );
        }
        let escaped = check_svg("<svg><rect style=\"fill:u\\72l(http://evil.example/x)\"/></svg>");
        assert_eq!(
            escaped,
            expect(&["url() esterno non ammesso: url(http://evil.example/x) (solo url(#id))"])
        );
    }

    #[test]
    fn quoted_remote_urls_are_reported_in_styles() {
        assert_eq!(
            check_svg(r#"<svg><rect style='fill:url("http://x/a")'/></svg>"#),
            expect(&["url() esterno non ammesso: url(\"http://x/a\") (solo url(#id))"])
        );
        assert_eq!(
            check_svg(r#"<svg><rect style="fill:url('http://x/a')"/></svg>"#),
            expect(&["url() esterno non ammesso: url('http://x/a') (solo url(#id))"])
        );
        assert_eq!(
            check_svg(r#"<svg><style>.a{fill:url("http://x/a")}</style></svg>"#),
            expect(&["url() esterno non ammesso: url(\"http://x/a\") (solo url(#id))"])
        );
        assert_eq!(
            check_svg(r#"<svg><style>.a{fill:url('http://x/a')}</style></svg>"#),
            expect(&["url() esterno non ammesso: url('http://x/a') (solo url(#id))"])
        );
        assert_eq!(
            check_svg(r#"<svg><rect style="fill:URL(http://evil.example/x)"/></svg>"#),
            expect(&["url() esterno non ammesso: URL(http://evil.example/x) (solo url(#id))"])
        );
        // a mismatched quote is not a url() at all
        assert_eq!(
            check_svg(r#"<svg><rect style="fill:url('x&quot;)"/></svg>"#),
            expect(&[])
        );
        assert_eq!(
            check_svg(r#"<svg><style>.a{fill:url('x")}</style></svg>"#),
            expect(&[])
        );
    }

    #[test]
    fn non_style_attributes_are_scanned_for_urls() {
        assert_eq!(
            check_svg(r#"<svg><text data-x="url(http://x)">t</text></svg>"#),
            expect(&["url() esterno non ammesso: url(http://x) (solo url(#id))"])
        );
        assert_eq!(
            check_svg(r#"<svg><text aria-label="url(http://x)">t</text></svg>"#),
            expect(&["url() esterno non ammesso: url(http://x) (solo url(#id))"])
        );
        // text content is not an attribute
        assert_eq!(
            check_svg("<svg><text>url(http://x)</text></svg>"),
            expect(&[])
        );
    }

    #[test]
    fn processing_instructions_are_rejected_but_the_xml_prolog_is_not() {
        assert_eq!(
            check_svg("<svg><?php echo 1; ?></svg>"),
            expect(&["CDATA e istruzioni di elaborazione non ammessi"])
        );
        assert_eq!(
            check_svg("<svg><![CDATA[x]]></svg>"),
            expect(&["CDATA e istruzioni di elaborazione non ammessi"])
        );
        assert_eq!(
            check_svg("<svg><![cdata[x]]></svg>"),
            expect(&["CDATA e istruzioni di elaborazione non ammessi"])
        );
        assert_eq!(
            check_svg(&format!("<?xml version=\"1.0\"?>\n{OK}")),
            expect(&[])
        );
    }

    #[test]
    fn reports_an_unclosed_style_and_ascii_word_boundaries() {
        assert_eq!(
            check_svg("<svg><style>.a{fill:red}</style"),
            expect(&[ROOT, "<style> non chiuso"])
        );
        assert_eq!(
            check_svg("<svg><STYLE>.a{fill:red}"),
            expect(&[ROOT, "<style> non chiuso"])
        );
        assert_eq!(
            check_svg("<svg><styleé>.a{fill:red}</styleé></svg>"),
            expect(&["elemento <styleé> non ammesso", "<style> non chiuso"])
        );
        assert_eq!(
            check_svg("<svg><STYLE>.a{fill:red}</STYLE></svg>"),
            expect(&[])
        );
    }

    #[test]
    fn duplicate_errors_keep_the_first_occurrence() {
        assert_eq!(
            check_svg(r#"<svg><rect src="x"/><rect src="y"/></svg>"#),
            expect(&["attributo src= non ammesso"])
        );
    }

    #[test]
    fn demo_accepts_self_contained_and_legit_text() {
        let page = r#"<!doctype html><html><body><input type="range" id="r"><output id="o"></output><script>document.getElementById("r").oninput=e=>{o.textContent=e.target.value}; const u = "see https://x"</script><style>body{background:url(data:image/png;base64,AAAA)}</style></body></html>"#;
        assert_eq!(check_demo(page), expect(&[]));
        assert_eq!(
            check_demo(
                r##"<!doctype html><p>data = "x" e src = 3</p><a href="#a">ok</a><script>const s = "https://x"</script>"##
            ),
            expect(&[])
        );
        assert_eq!(check_demo("   \n\t "), expect(&["HTML vuoto"]));
    }

    #[test]
    fn demo_rejects_external_network_resources() {
        let cases: &[(&str, &str, &[&str])] = &[
            (
                "script src",
                r#"<script src="https://cdn.example/x.js"></script>"#,
                &["risorsa di rete esterna non ammessa: src=\"https://cdn.example/x.js\""],
            ),
            (
                "script src protocol relative",
                "<script src=//cdn.example/x.js></script>",
                &["risorsa di rete esterna non ammessa: src=\"//cdn.example/x.js\""],
            ),
            (
                "link href",
                r#"<link rel="stylesheet" href="http://x/y.css">"#,
                &["risorsa di rete esterna non ammessa: href=\"http://x/y.css\""],
            ),
            (
                "img src",
                r#"<img src="https://x/y.png">"#,
                &["risorsa di rete esterna non ammessa: src=\"https://x/y.png\""],
            ),
            (
                "import",
                r#"<style>@import url("https://x/y.css");</style>"#,
                &[
                    "url() esterno non ammesso: url(\"https://x/y.css\")",
                    "@import esterno non ammesso",
                ],
            ),
            (
                "style attr background",
                r#"<div style="background:url(https://x/y.png)"></div>"#,
                &["url() esterno non ammesso: url(https://x/y.png)"],
            ),
            (
                "fetch",
                r#"<script>fetch("https://api.example/x")</script>"#,
                &[
                    "chiamate di rete verso URL remoti non ammesse (fetch/XMLHttpRequest/WebSocket/import)",
                ],
            ),
            (
                "fetch backtick",
                "<script>fetch(`//api.example/x`)</script>",
                &[
                    "chiamate di rete verso URL remoti non ammesse (fetch/XMLHttpRequest/WebSocket/import)",
                ],
            ),
            (
                "xhr",
                r#"<script>const x=new XMLHttpRequest();x.open("GET","https://x/y")</script>"#,
                &[
                    "chiamate di rete verso URL remoti non ammesse (fetch/XMLHttpRequest/WebSocket/import)",
                ],
            ),
            (
                "websocket",
                r#"<script>new WebSocket("wss://x/y")</script>"#,
                &[
                    "chiamate di rete verso URL remoti non ammesse (fetch/XMLHttpRequest/WebSocket/import)",
                ],
            ),
            (
                "img.src assignment",
                r#"<script>img.src="http://x/y.png"</script>"#,
                &["assegnazione di src/href remoti non ammessa"],
            ),
            (
                "iframe",
                r#"<iframe src="about:blank"></iframe>"#,
                &["<iframe>/<object>/<embed>/<base> non ammessi"],
            ),
            (
                "meta refresh",
                r#"<meta http-equiv="refresh" content="0;url=http://x">"#,
                &["<meta refresh> non ammesso"],
            ),
            ("empty", "", &["HTML vuoto"]),
        ];
        for (name, html, expected) in cases {
            assert_eq!(check_demo(html), expect(expected), "{name}");
        }
    }
}
