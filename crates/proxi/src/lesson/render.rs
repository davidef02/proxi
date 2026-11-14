//! Node article renderer (renderNodeHtml and helpers).

use serde_json::Value;

use crate::lesson::DECK_ROOT;
use crate::lesson::js::{is_js_space, js_concat, js_to_string, js_trim, js_truthy, text};

pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// One global left-to-right pass of `delim X delim`, with lazy `.+?` X: no line break inside.
/// Emits the skipped char and retries one past it when no closing delimiter follows.
fn replace_pairs(s: &str, delim: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(delim) {
            let start = i + delim.len();
            let mut k = start;
            let mut closing = None;
            while k < s.len() {
                let ch = s[k..].chars().next().unwrap();
                if is_line_terminator(ch) {
                    break;
                }
                k += ch.len_utf8();
                if s[k..].starts_with(delim) {
                    closing = Some(k);
                    break;
                }
            }
            if let Some(end) = closing {
                out.push_str(open);
                out.push_str(&s[start..end]);
                out.push_str(close);
                i = end + delim.len();
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The `` `X` `` pass: X is one or more non-backtick chars, newlines included.
fn replace_code_spans(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with('`')
            && let Some(len) = s[i + 1..].find('`')
            && len > 0
        {
            out.push_str("<code>");
            out.push_str(&s[i + 1..i + 1 + len]);
            out.push_str("</code>");
            i = i + 1 + len + 1;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn format_inline_markdown(t: &str) -> String {
    let strong = replace_pairs(t, "**", "<strong>", "</strong>");
    let em = replace_pairs(&strong, "*", "<em>", "</em>");
    replace_code_spans(&em)
}

/// Counts matches of an ASCII needle, case-insensitively, non-overlapping left to right.
fn count_ci(s: &str, needle: &[u8]) -> usize {
    let bytes = s.as_bytes();
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if bytes[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

/// `/<pre[\s>]/gi`: "<pre" followed by a JS space or ">".
fn count_pre_openers(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut n = 0;
    let mut i = 0;
    while i + 4 <= bytes.len() {
        // bytes[i] == b'<' keeps i on a char boundary, so i + 4 is one too.
        if bytes[i] == b'<'
            && bytes[i + 1..i + 4].eq_ignore_ascii_case(b"pre")
            && let Some(ch) = s[i + 4..].chars().next()
            && (ch == '>' || is_js_space(ch))
        {
            n += 1;
        }
        i += 1;
    }
    n
}

fn pre_delta(s: &str) -> i64 {
    count_pre_openers(s) as i64 - count_ci(s, b"</pre>") as i64
}

pub fn parse_markdown_prose(markdown: &str) -> String {
    if markdown.is_empty() {
        return String::new();
    }
    let mut rendered: Vec<String> = Vec::new();
    let mut in_list = false;
    // <pre> depth: one line can close a block and open the next, so a bool would leave code early.
    let mut pre_depth: i64 = 0;

    for raw in markdown.split('\n') {
        // Inside <pre> every line is code: verbatim, no <p>.
        if pre_depth > 0 {
            rendered.push(raw.to_string());
            pre_depth += pre_delta(raw);
            continue;
        }

        let line = js_trim(raw);

        // Authored HTML passes through: wrapping a block in <p> would nest it.
        if line.starts_with('<') || count_pre_openers(line) > 0 {
            if in_list {
                rendered.push("</ul>".to_string());
                in_list = false;
            }
            rendered.push(raw.to_string());
            pre_depth += pre_delta(line);
            continue;
        }

        if line.is_empty() {
            if in_list {
                rendered.push("</ul>".to_string());
                in_list = false;
            }
            continue;
        }

        if line.starts_with("- ") || line.starts_with("* ") {
            if !in_list {
                rendered.push("<ul class=\"prose-list\">".to_string());
                in_list = true;
            }
            let item = format_inline_markdown(&line[2..]);
            rendered.push(format!("  <li>{item}</li>"));
            continue;
        }

        if in_list {
            rendered.push("</ul>".to_string());
            in_list = false;
        }

        if let Some(rest) = line.strip_prefix("### ") {
            rendered.push(format!("<h4>{}</h4>", format_inline_markdown(rest)));
        } else if let Some(rest) = line.strip_prefix("## ") {
            rendered.push(format!("<h3>{}</h3>", format_inline_markdown(rest)));
        } else {
            rendered.push(format!("<p>{}</p>", format_inline_markdown(line)));
        }
    }

    if in_list {
        rendered.push("</ul>".to_string());
    }
    rendered.join("\n")
}

pub fn render_math_html(math: Option<&Value>) -> String {
    let Some(items) = math.and_then(Value::as_array) else {
        return String::new();
    };
    if items.is_empty() {
        return String::new();
    }
    let cards: Vec<String> = items
        .iter()
        .map(|m| {
            let s = text(Some(m));
            let formatted = if s.starts_with("$$") && s.ends_with("$$") {
                s.to_string()
            } else {
                format!("$$ {s} $$")
            };
            format!("<div class=\"math-card\">{formatted}</div>")
        })
        .collect();
    format!(
        "<div class=\"node-math-container\">{}</div>",
        cards.join("\n")
    )
}

pub fn render_code_html(blocks: Option<&Value>) -> String {
    let Some(items) = blocks.and_then(Value::as_array) else {
        return String::new();
    };
    if items.is_empty() {
        return String::new();
    }
    items
        .iter()
        .map(|cb| {
            let caption = cb.get("caption");
            let caption_html = if js_truthy(caption) {
                format!(
                    "<div class=\"code-caption\">{}</div>",
                    escape_html(text(caption))
                )
            } else {
                String::new()
            };
            let language = if js_truthy(cb.get("language")) {
                text(cb.get("language"))
            } else {
                ""
            };
            format!(
                "<div class=\"code-container\">{caption_html}<pre><code class=\"language-{}\">{}</code></pre></div>",
                escape_html(language),
                escape_html(text(cb.get("code")))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_visual_html(visuals: Option<&Value>) -> String {
    let Some(items) = visuals.and_then(Value::as_array) else {
        return String::new();
    };
    if items.is_empty() {
        return String::new();
    }
    items
        .iter()
        .map(|v| {
            if let Some(s) = v.as_str() {
                if js_trim(s).starts_with("<svg") {
                    return format!("<figure class=\"svg-figure\">{s}</figure>");
                }
                return format!(
                    "<figure class=\"svg-figure\"><img src=\"{}\" alt=\"Visual\"/></figure>",
                    escape_html(s)
                );
            }
            let caption = v.get("caption");
            let caption_html = if js_truthy(caption) {
                escape_html(text(caption))
            } else {
                String::new()
            };
            match v.get("type").and_then(Value::as_str) {
                Some("svg-inline") => {
                    let figcaption = if caption_html.is_empty() {
                        String::new()
                    } else {
                        format!("<figcaption>{caption_html}</figcaption>")
                    };
                    format!(
                        "<figure class=\"svg-figure\">{}{figcaption}</figure>",
                        js_concat(v.get("content"))
                    )
                }
                Some("mermaid") => {
                    let title = if caption_html.is_empty() {
                        String::new()
                    } else {
                        format!("<div class=\"diagram-title\">{caption_html}</div>")
                    };
                    format!(
                        "<div class=\"diagram-container\"><pre class=\"mermaid\">{}</pre>{title}</div>",
                        js_concat(v.get("content"))
                    )
                }
                _ => {
                    let figcaption = if caption_html.is_empty() {
                        String::new()
                    } else {
                        format!("<figcaption>{caption_html}</figcaption>")
                    };
                    format!(
                        "<figure class=\"svg-figure\"><img src=\"{}\" alt=\"{}\"/>{figcaption}</figure>",
                        escape_html(text(v.get("content"))),
                        escape_html(text(v.get("alt")))
                    )
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// JS `ToNumber`, needed only because quiz.correctIndex is compared with `<`/`>`.
fn js_to_number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Null => 0.0,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::String(s) => str_to_number(s),
        Value::Array(_) | Value::Object(_) => f64::NAN,
    }
}

fn str_to_number(s: &str) -> f64 {
    let t = js_trim(s);
    if t.is_empty() {
        return 0.0;
    }
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return radix_to_number(rest, 16);
    }
    if let Some(rest) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return radix_to_number(rest, 8);
    }
    if let Some(rest) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return radix_to_number(rest, 2);
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    decimal_to_number(t).unwrap_or(f64::NAN)
}

fn radix_to_number(digits: &str, radix: u32) -> f64 {
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return f64::NAN;
    }
    let mut value = 0.0;
    for c in digits.chars() {
        value = value * radix as f64 + c.to_digit(radix).unwrap() as f64;
    }
    value
}

/// StrDecimalLiteral; Rust's parser alone would also take `inf`/`nan`, which JS rejects.
fn decimal_to_number(t: &str) -> Option<f64> {
    let bytes = t.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let mut exp_digits = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            exp_digits += 1;
        }
        if exp_digits == 0 {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }
    t.parse::<f64>().ok()
}

fn render_quiz_html(node: &Value) -> Result<String, String> {
    if !js_truthy(node.get("quiz")) {
        return Ok(String::new());
    }
    let quiz = node.get("quiz").unwrap();
    let options = quiz.get("options").and_then(Value::as_array);
    let option_count = options.map_or(0, Vec::len);

    let mut options_html = String::new();
    match options {
        Some(options) if !options.is_empty() => {
            // 1-based, matching the "1. / 2. / 3." the learner sees.
            if let Some(correct_index) = quiz.get("correctIndex") {
                let value = js_to_number(correct_index);
                if value < 1.0 || value > option_count as f64 {
                    return Err(format!(
                        "quiz.correctIndex is 1-based: got {} for {} options",
                        js_to_string(correct_index),
                        option_count
                    ));
                }
            }
            let items: Vec<String> = options
                .iter()
                .enumerate()
                .map(|(i, opt)| {
                    let one_based = (i + 1) as f64;
                    // Strict ===: a string "2" passes validation but marks nothing correct.
                    let is_correct = quiz.get("correctIndex").is_some_and(|ci| match ci {
                        Value::Number(n) => n.as_f64() == Some(one_based),
                        _ => false,
                    });
                    let letter = char::from_u32(65 + i as u32).unwrap_or('?');
                    let misconception = quiz
                        .get("misconceptions")
                        .and_then(Value::as_array)
                        .and_then(|items| {
                            items.iter().find(|m| {
                                m.get("optionIndex").is_some_and(|oi| match oi {
                                    Value::Number(n) => n.as_f64() == Some(one_based),
                                    _ => false,
                                })
                            })
                        });
                    let attr = misconception.map_or(String::new(), |m| {
                        format!(
                            " data-misconception=\"{}\"",
                            escape_html(text(m.get("misconception")))
                        )
                    });
                    format!(
                        "<li class=\"quiz-option-item\"><button type=\"button\" class=\"quiz-option quiz-option-btn{}\" data-index=\"{}\"{attr}><span class=\"quiz-option-letter\">{letter}</span><span class=\"quiz-option-text\">{}</span><span class=\"quiz-option-status\" aria-hidden=\"true\"></span></button></li>",
                        if is_correct { " is-correct-target" } else { "" },
                        i + 1,
                        escape_html(text(Some(opt)))
                    )
                })
                .collect();
            options_html = format!("<ul class=\"quiz-options\">{}</ul>", items.join("\n"));
        }
        _ => {
            if js_truthy(quiz.get("correctAnswer")) {
                options_html = format!(
                    "<ul class=\"quiz-options\"><li class=\"quiz-option-item\"><button type=\"button\" class=\"quiz-option quiz-option-btn is-correct-target\" data-index=\"1\"><span class=\"quiz-option-letter\">A</span><span class=\"quiz-option-text\">{}</span><span class=\"quiz-option-status\" aria-hidden=\"true\"></span></button></li></ul>",
                    escape_html(text(quiz.get("correctAnswer")))
                );
            }
        }
    }

    let explanation = if js_truthy(quiz.get("explanation")) {
        parse_markdown_prose(text(quiz.get("explanation")))
    } else {
        String::new()
    };
    let instruction = if option_count > 1 {
        "<div class=\"quiz-instruction\">Seleziona un'opzione per verificare la comprensione:</div>"
    } else {
        ""
    };
    let correct_claim = if js_truthy(quiz.get("correctAnswer")) {
        format!(
            "<strong>Correct Claim:</strong> {}<br/>",
            escape_html(text(quiz.get("correctAnswer")))
        )
    } else {
        String::new()
    };

    Ok(format!(
        "<div class=\"step-quiz\" data-quiz-interactive=\"true\"><div class=\"step-label\" style=\"color: var(--step-quiz-text);\">🎯 Diagnostic Checkpoint</div><div class=\"quiz-question\">{}</div>{instruction}{options_html}<div class=\"quiz-feedback-banner\" style=\"display: none;\" role=\"alert\"><div class=\"quiz-feedback-title\"></div><div class=\"quiz-feedback-detail\"></div></div><div class=\"quiz-action-bar\" style=\"display: none;\"><button type=\"button\" class=\"quiz-retry-btn\">🔄 Riprova il Quiz</button></div><details class=\"quiz-reveal\"><summary>Reveal Answer & Diagnostic Reasoning</summary><div class=\"quiz-explanation\">{correct_claim}{explanation}</div></details></div>",
        parse_markdown_prose(text(quiz.get("question")))
    ))
}

fn render_step(step: &Value) -> String {
    let step_id = escape_html(text(step.get("stepId")));
    let title = if js_truthy(step.get("title")) {
        format!(
            "<h3 class=\"step-title\">{}</h3>",
            escape_html(text(step.get("title")))
        )
    } else {
        String::new()
    };
    let math = render_math_html(step.get("math"));
    let code = render_code_html(step.get("codeBlocks"));
    let visual = render_visual_html(step.get("visuals"));

    let mut out = format!(
        "<div class=\"node-step\" id=\"step-{step_id}\" data-step-id=\"{step_id}\"><div class=\"step-header\"><span class=\"step-id\">{step_id}</span>{title}</div><div class=\"step-content prose\">{}</div>",
        parse_markdown_prose(text(step.get("content")))
    );
    for block in [math, code, visual] {
        if !block.is_empty() {
            out.push('\n');
            out.push_str(&block);
        }
    }
    out.push_str("</div>");
    out
}

pub fn render_node_html(node: &Value, index: usize) -> Result<String, String> {
    let tier = node.get("tier").and_then(Value::as_str).unwrap_or("");
    let is_axiom = tier == "unconditional-truth" || tier == "axiom";
    let (tier_class, tier_label) = if is_axiom {
        ("badge-axiom", "Unconditional Truth")
    } else {
        ("badge-derivation", "Derivation")
    };

    let math_html = render_math_html(node.get("math"));
    let code_html = render_code_html(node.get("codeBlocks"));
    let visual_html = render_visual_html(node.get("visuals"));
    let quiz_html = render_quiz_html(node)?;

    let motivate = if js_truthy(node.get("motivation")) {
        format!(
            "<div class=\"step-motivate\"><div class=\"step-label\">⚡ Motivating Tension</div>{}</div>",
            parse_markdown_prose(text(node.get("motivation")))
        )
    } else {
        String::new()
    };

    let connect = if js_truthy(node.get("connection")) {
        format!(
            "<div class=\"step-connect\"><div class=\"step-label\">🔗 Connessione Concettuale</div>{}</div>",
            parse_markdown_prose(text(node.get("connection")))
        )
    } else if let Some(deps) = node.get("dependencies").and_then(Value::as_array) {
        if deps.is_empty() {
            String::new()
        } else {
            let linked: Vec<String> = deps
                .iter()
                .map(|d| format!("<code>{}</code>", escape_html(text(Some(d)))))
                .collect();
            format!(
                "<div class=\"step-connect\"><div class=\"step-label\">🔗 Connessione Concettuale</div><p>Si collega a: {}</p></div>",
                linked.join(", ")
            )
        }
    } else {
        String::new()
    };

    let anki = node.get("ankiParams");
    let deck = anki
        .and_then(|a| a.get("deck"))
        .filter(|d| js_truthy(Some(d)))
        .map_or_else(
            || format!("{DECK_ROOT}::Topic"),
            |d| text(Some(d)).to_string(),
        );
    // Mirrors generateCardsFromNode: the question card wins, the cloze is the fallback.
    let card_type = if anki
        .is_some_and(|a| js_truthy(a.get("targetQuestion")) && js_truthy(a.get("targetFact")))
    {
        "Basic Front/Back"
    } else if anki.is_some_and(|a| js_truthy(a.get("suggestedCloze"))) {
        "Cloze"
    } else {
        "nessuna"
    };
    let capture = format!(
        "<footer class=\"step-capture\"><span>Captured in Deck: <code>{}</code></span><span>Card Type: {card_type}</span></footer>",
        escape_html(&deck)
    );

    let steps = node
        .get("steps")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty());
    let core = if let Some(steps) = steps {
        let rendered: Vec<String> = steps.iter().map(render_step).collect();
        format!(
            "<div class=\"node-steps\">\n{}\n</div>{math_html}{code_html}{visual_html}",
            rendered.join("\n")
        )
    } else {
        format!(
            "<div class=\"step-establish prose\">{}</div>{math_html}{code_html}{visual_html}",
            parse_markdown_prose(text(node.get("content")))
        )
    };

    Ok(format!(
        "<article class=\"cognitive-node\" id=\"node-{}\" data-tier=\"{}\"><header class=\"node-header\"><div class=\"node-title-group\"><span class=\"node-number\">Node {:02}</span><h2 class=\"node-title\">{}</h2></div><span class=\"badge {tier_class}\">{tier_label}</span></header>{motivate}{core}{connect}{quiz_html}{capture}</article>",
        escape_html(text(node.get("nodeId"))),
        escape_html(text(node.get("tier"))),
        index + 1,
        escape_html(text(node.get("title")))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markdown_vectors() {
        assert_eq!(
            parse_markdown_prose("**a** *b* `c`"),
            "<p><strong>a</strong> <em>b</em> <code>c</code></p>"
        );
        assert_eq!(parse_markdown_prose("****"), "<p><em>*</em>*</p>");
        assert_eq!(
            parse_markdown_prose("**a*b**"),
            "<p><strong>a*b</strong></p>"
        );
        assert_eq!(
            parse_markdown_prose("*a **b** c*"),
            "<p><em>a <strong>b</strong> c</em></p>"
        );
        assert_eq!(
            parse_markdown_prose("- uno\n- **due**\n\ntesto"),
            "<ul class=\"prose-list\">\n  <li>uno</li>\n  <li><strong>due</strong></li>\n</ul>\n<p>testo</p>"
        );
        assert_eq!(
            parse_markdown_prose("## T\n### U"),
            "<h3>T</h3>\n<h4>U</h4>"
        );
        assert_eq!(
            parse_markdown_prose("<pre>\nx\n</pre>\nok"),
            "<pre>\nx\n</pre>\n<p>ok</p>"
        );
        assert_eq!(
            parse_markdown_prose("a\r\n**b**"),
            "<p>a</p>\n<p><strong>b</strong></p>"
        );
        assert_eq!(parse_markdown_prose("`a\nb`"), "<p>`a</p>\n<p>b`</p>");
        assert_eq!(
            parse_markdown_prose("* x"),
            "<ul class=\"prose-list\">\n  <li>x</li>\n</ul>"
        );
        assert_eq!(parse_markdown_prose(""), "");
    }

    #[test]
    fn pre_blocks_stay_verbatim() {
        let html = "<p>a</p><pre><code>x = 1;\ny = 2;</code></pre><p>b</p><pre><code>z = 3;\n  w = 4;\n</code></pre><p>c</p>";
        let out = parse_markdown_prose(html);
        assert!(
            !out.contains("<p>y = 2"),
            "second line of first block was wrapped"
        );
        assert!(
            out.contains("\n  w = 4;\n"),
            "indentation inside the second block was lost"
        );
        assert_eq!(out.matches("<p>").count(), 3);

        assert_eq!(
            parse_markdown_prose("solo `testo`"),
            "<p>solo <code>testo</code></p>"
        );
    }

    #[test]
    fn node_with_steps_quiz_and_visuals_matches_node() {
        let node = json!({
            "nodeId": "n1(v2)",
            "title": "T <x>",
            "tier": "axiom",
            "content": "",
            "steps": [{ "stepId": "s1", "content": "ciao" }],
            "visuals": [
                { "type": "svg-inline" },
                "  <svg/>",
                "pic.png",
                { "type": "mermaid", "content": "A-->B", "caption": "Cap" },
                { "type": "image", "content": "a.png" }
            ],
            "dependencies": ["d1", "d2"],
            "quiz": {
                "question": "Q?",
                "options": ["a", "b"],
                "correctIndex": 2,
                "misconceptions": [{ "optionIndex": 1, "misconception": "m\"1" }],
                "explanation": "E",
                "correctAnswer": "b"
            },
            "ankiParams": { "suggestedCloze": "x" }
        });

        let expected = "<article class=\"cognitive-node\" id=\"node-n1(v2)\" data-tier=\"axiom\"><header class=\"node-header\"><div class=\"node-title-group\"><span class=\"node-number\">Node 03</span><h2 class=\"node-title\">T &lt;x&gt;</h2></div><span class=\"badge badge-axiom\">Unconditional Truth</span></header><div class=\"node-steps\">\n<div class=\"node-step\" id=\"step-s1\" data-step-id=\"s1\"><div class=\"step-header\"><span class=\"step-id\">s1</span></div><div class=\"step-content prose\"><p>ciao</p></div></div>\n</div><figure class=\"svg-figure\">undefined</figure>\n<figure class=\"svg-figure\">  <svg/></figure>\n<figure class=\"svg-figure\"><img src=\"pic.png\" alt=\"Visual\"/></figure>\n<div class=\"diagram-container\"><pre class=\"mermaid\">A-->B</pre><div class=\"diagram-title\">Cap</div></div>\n<figure class=\"svg-figure\"><img src=\"a.png\" alt=\"\"/></figure><div class=\"step-connect\"><div class=\"step-label\">🔗 Connessione Concettuale</div><p>Si collega a: <code>d1</code>, <code>d2</code></p></div><div class=\"step-quiz\" data-quiz-interactive=\"true\"><div class=\"step-label\" style=\"color: var(--step-quiz-text);\">🎯 Diagnostic Checkpoint</div><div class=\"quiz-question\"><p>Q?</p></div><div class=\"quiz-instruction\">Seleziona un'opzione per verificare la comprensione:</div><ul class=\"quiz-options\"><li class=\"quiz-option-item\"><button type=\"button\" class=\"quiz-option quiz-option-btn\" data-index=\"1\" data-misconception=\"m&quot;1\"><span class=\"quiz-option-letter\">A</span><span class=\"quiz-option-text\">a</span><span class=\"quiz-option-status\" aria-hidden=\"true\"></span></button></li>\n<li class=\"quiz-option-item\"><button type=\"button\" class=\"quiz-option quiz-option-btn is-correct-target\" data-index=\"2\"><span class=\"quiz-option-letter\">B</span><span class=\"quiz-option-text\">b</span><span class=\"quiz-option-status\" aria-hidden=\"true\"></span></button></li></ul><div class=\"quiz-feedback-banner\" style=\"display: none;\" role=\"alert\"><div class=\"quiz-feedback-title\"></div><div class=\"quiz-feedback-detail\"></div></div><div class=\"quiz-action-bar\" style=\"display: none;\"><button type=\"button\" class=\"quiz-retry-btn\">🔄 Riprova il Quiz</button></div><details class=\"quiz-reveal\"><summary>Reveal Answer & Diagnostic Reasoning</summary><div class=\"quiz-explanation\"><strong>Correct Claim:</strong> b<br/><p>E</p></div></details></div><footer class=\"step-capture\"><span>Captured in Deck: <code>Proxi::Topic</code></span><span>Card Type: Cloze</span></footer></article>";

        assert_eq!(render_node_html(&node, 2).unwrap(), expected);
    }

    #[test]
    fn node_without_steps_matches_node() {
        let node = json!({
            "nodeId": "n2",
            "title": "Due",
            "tier": "derivation",
            "motivation": "Perché **ora**",
            "content": "Testo",
            "math": ["x^2", "$$y$$"],
            "codeBlocks": [
                { "language": "ts", "code": "a < b && c", "caption": "Cap" },
                { "code": "z" }
            ],
            "connection": "Vedi `n1`",
            "ankiParams": {
                "targetQuestion": "Q",
                "targetFact": "F",
                "deck": "Proxi::Reti"
            }
        });

        let expected = "<article class=\"cognitive-node\" id=\"node-n2\" data-tier=\"derivation\"><header class=\"node-header\"><div class=\"node-title-group\"><span class=\"node-number\">Node 01</span><h2 class=\"node-title\">Due</h2></div><span class=\"badge badge-derivation\">Derivation</span></header><div class=\"step-motivate\"><div class=\"step-label\">⚡ Motivating Tension</div><p>Perché <strong>ora</strong></p></div><div class=\"step-establish prose\"><p>Testo</p></div><div class=\"node-math-container\"><div class=\"math-card\">$$ x^2 $$</div>\n<div class=\"math-card\">$$y$$</div></div><div class=\"code-container\"><div class=\"code-caption\">Cap</div><pre><code class=\"language-ts\">a &lt; b &amp;&amp; c</code></pre></div>\n<div class=\"code-container\"><pre><code class=\"language-\">z</code></pre></div><div class=\"step-connect\"><div class=\"step-label\">🔗 Connessione Concettuale</div><p>Vedi <code>n1</code></p></div><footer class=\"step-capture\"><span>Captured in Deck: <code>Proxi::Reti</code></span><span>Card Type: Basic Front/Back</span></footer></article>";

        assert_eq!(render_node_html(&node, 0).unwrap(), expected);
    }

    #[test]
    fn steps_render_and_legacy_nodes_keep_establish() {
        let node = json!({
            "nodeId": "exponential-growth",
            "title": "Crescita Esponenziale",
            "tier": "derivation",
            "content": "",
            "steps": [
                {
                    "stepId": "step-1-observe",
                    "title": "Osservazione del pattern",
                    "content": "Notiamo che il segnale **cresce esponenzialmente**.",
                    "math": ["f(x) = 2^x"],
                    "codeBlocks": [{ "language": "ts", "code": "const y = Math.pow(2, x);", "caption": "Calcolo" }],
                    "visuals": [{ "type": "svg-inline", "content": "<svg viewBox=\"0 0 10 10\"><circle cx=\"5\" cy=\"5\" r=\"5\"/></svg>" }]
                },
                {
                    "stepId": "step-2-derive",
                    "title": "Derivazione della proprietà",
                    "content": "La derivata preserva la forma esponenziale."
                }
            ]
        });

        let html = render_node_html(&node, 0).unwrap();

        assert!(html.contains("class=\"node-steps\""));
        assert!(html.contains("id=\"step-step-1-observe\""));
        assert!(html.contains("Osservazione del pattern"));
        assert!(html.contains("<strong>cresce esponenzialmente</strong>"));
        assert!(html.contains("f(x) = 2^x"));
        assert!(html.contains("const y = Math.pow(2, x);"));
        assert!(html.contains("Calcolo"));
        assert!(html.contains("<svg viewBox=\"0 0 10 10\">"));
        assert!(html.contains("id=\"step-step-2-derive\""));
        assert!(html.contains("Derivazione della proprietà"));
        assert!(!html.contains("step-establish"));

        let legacy = json!({
            "nodeId": "legacy-node",
            "title": "Nodo Classico",
            "tier": "unconditional-truth",
            "motivation": "Perché serve questo concetto",
            "content": "Contenuto stabilito direttamente nel nodo."
        });

        let html = render_node_html(&legacy, 0).unwrap();

        assert!(html.contains("step-motivate"));
        assert!(html.contains("Perché serve questo concetto"));
        assert!(html.contains("step-establish prose"));
        assert!(html.contains("Contenuto stabilito direttamente nel nodo."));
        assert!(!html.contains("node-steps"));
    }

    fn quiz_node(correct_index: Value) -> Value {
        json!({
            "nodeId": "q",
            "title": "Quiz",
            "tier": "derivation",
            "content": "x",
            "quiz": {
                "question": "Q?",
                "options": ["a", "b", "c"],
                "correctIndex": correct_index,
                "explanation": "E"
            }
        })
    }

    #[test]
    fn correct_index_out_of_range_is_an_error() {
        for (index, got) in [
            (json!(0), "0"),
            (json!(4), "4"),
            (json!(null), "null"),
            (json!("0"), "0"),
            (json!(""), ""),
            (json!("4"), "4"),
            (json!(" 4 "), " 4 "),
            (json!("0x4"), "0x4"),
        ] {
            assert_eq!(
                render_node_html(&quiz_node(index), 0).unwrap_err(),
                format!("quiz.correctIndex is 1-based: got {got} for 3 options")
            );
        }

        let ok = render_node_html(&quiz_node(json!("2")), 0).unwrap();
        assert!(!ok.contains("is-correct-target"));
        for index in [json!("abc"), json!("inf"), json!("1e0")] {
            assert!(render_node_html(&quiz_node(index), 0).is_ok());
        }
    }
}
