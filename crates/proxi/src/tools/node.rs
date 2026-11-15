//! The `write_node` tool (port of app/server/tools/node.ts).

use serde_json::{Map, Value, json};

use crate::graph::store::GraphStore;
use crate::js;
use crate::lesson::doc;
use crate::lesson::js::text;
use crate::sessions::{invalid_type, issue, utf16_len};
use crate::tools::util::{self, fail};
use crate::tools::{ToolDef, ToolFuture, ToolHost};

/// Submitted HTML goes straight into the artifact: refuse active content.
///
/// An `on<event>=` attribute only counts inside a real tag: escaped samples (`&lt;button onClick=…`)
/// are inert text. Only real tags (`<letter…>`) outside `<pre>`/`<code>` content are inspected, so
/// prose like `x < y` passes.
pub fn unsafe_html(v: &str) -> bool {
    if js::find_ci(v, "<script", 0).is_some() || js::find_ci(v, "<iframe", 0).is_some() {
        return true;
    }
    let stripped = blank_pre_code(v);
    tags(&stripped)
        .into_iter()
        .any(|tag| active_handler(tag) || js::find_ci(tag, "javascript:", 0).is_some())
}

/// Node's `v.replace(/(<(pre|code)\b[^>]*>)[\s\S]*?(<\/\2>)/gi, "$1$3")`: keep both tags, drop
/// the code between them. An open tag without its closing one is left alone, as the regex does.
fn blank_pre_code(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut copied = 0;
    let mut from = 0;
    while let Some((start, name, after_open)) = open_tag(v, from) {
        let Some(close) = js::find_ci(v, &format!("</{name}>"), after_open) else {
            from = start + 1;
            continue;
        };
        out.push_str(&v[copied..after_open]);
        let close_end = close + name.len() + 3;
        out.push_str(&v[close..close_end]);
        copied = close_end;
        from = close_end;
    }
    out.push_str(&v[copied..]);
    out
}

/// `<(pre|code)\b[^>]*>`: the `<`, the name, a word boundary, then everything up to the first `>`.
fn open_tag(v: &str, from: usize) -> Option<(usize, &'static str, usize)> {
    let bytes = v.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            for name in ["pre", "code"] {
                let end = i + 1 + name.len();
                if end <= bytes.len()
                    && bytes[i + 1..end].eq_ignore_ascii_case(name.as_bytes())
                    && v[end..]
                        .chars()
                        .next()
                        .is_some_and(|c| !(c.is_ascii_alphanumeric() || c == '_'))
                    && let Some(offset) = v[end..].find('>')
                {
                    return Some((i, name, end + offset + 1));
                }
            }
        }
        i += 1;
    }
    None
}

/// Node's `TAG`: `<`, a letter, then quoted runs or single chars up to the closing `>`.
fn tags(v: &str) -> Vec<&str> {
    let bytes = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'<'
            && bytes[i + 1].is_ascii_alphabetic()
            && let Some(end) = tag_end(v, i)
        {
            out.push(&v[i..end]);
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

fn tag_end(v: &str, start: usize) -> Option<usize> {
    let bytes = v.as_bytes();
    let mut i = start + 2;
    loop {
        match *bytes.get(i)? {
            b'>' => return Some(i + 1),
            quote @ (b'"' | b'\'') => {
                let mut j = i + 1;
                loop {
                    if *bytes.get(j)? == quote {
                        break;
                    }
                    j += 1;
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
}

/// `/[\s"'/]on[a-z]+\s*=/i`: an `on…` attribute needs a separator before it and `=` after.
fn active_handler(tag: &str) -> bool {
    let chars: Vec<char> = tag.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        let boundary = js::is_js_ws(*c) || matches!(c, '"' | '\'' | '/');
        if !boundary
            || !chars
                .get(i + 1)
                .is_some_and(|c| c.eq_ignore_ascii_case(&'o'))
            || !chars
                .get(i + 2)
                .is_some_and(|c| c.eq_ignore_ascii_case(&'n'))
        {
            continue;
        }
        let mut j = i + 3;
        let mut letters = 0;
        while chars.get(j).is_some_and(char::is_ascii_alphabetic) {
            letters += 1;
            j += 1;
        }
        if letters == 0 {
            continue;
        }
        while chars.get(j).is_some_and(|c| js::is_js_ws(*c)) {
            j += 1;
        }
        if chars.get(j) == Some(&'=') {
            return true;
        }
    }
    false
}

/// Prose and concept names are compared the same way: lowercased, without emphasis, trimmed and
/// with single spaces.
fn normalise(s: &str) -> String {
    let plain: String = s.chars().filter(|c| !matches!(c, '`' | '*' | '_')).collect();
    js::js_trim(&js::collapse_ws(&plain)).to_lowercase()
}

/// What the learner reads, normalised: where an anchor's name shows up in the prose.
fn prose_text(node: &Value) -> String {
    let mut parts = vec![text(node.get("title")).to_string()];
    for key in ["motivation", "content", "connection"] {
        if js::truthy(node.get(key)) {
            parts.push(js::js_string(node.get(key)));
        }
    }
    for step in node
        .get("steps")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if let Some(object) = step.as_object() {
            for value in object.values() {
                if let Value::String(plain) = value {
                    parts.push(plain.clone());
                }
            }
        }
    }
    // Node here strips `<[^>]*>` (empty tags too) and leaves a space, not `js::strip_tags`'s
    // `<[^>]+>` with nothing: a space keeps "Primo<>passo" two words.
    normalise(&strip_empty_tags(&parts.join("\n")))
}

fn strip_empty_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        match rest[start + 1..].find('>') {
            Some(offset) => {
                out.push_str(&rest[..start]);
                out.push(' ');
                rest = &rest[start + 1 + offset + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Live concepts whose name the node's prose already uses: the anchors it should have declared.
fn named_concepts(node: &Value, live: &[Map<String, Value>]) -> Vec<(String, String)> {
    let prose = prose_text(node);
    live.iter()
        .filter(|record| {
            let name = normalise(text(record.get("name")));
            utf16_len(&name) >= 4 && word_contains(&prose, &name)
        })
        .take(3)
        .map(|record| {
            (
                text(record.get("id")).to_string(),
                text(record.get("name")).to_string(),
            )
        })
        .collect()
}

/// `(^|[^a-z0-9])name([^a-z0-9]|$)`: a whole word, never a piece of a longer one.
fn word_contains(hay: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = hay[from..].find(needle) {
        let at = from + offset;
        let before = hay[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let end = at + needle.len();
        let after = hay[end..].chars().next().is_none_or(|c| !is_word_char(c));
        if before && after {
            return true;
        }
        from = end.max(at + 1);
    }
    false
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

#[derive(Default)]
struct Issues {
    out: Vec<String>,
    /// zod skips a schema's refinements once an aborting issue (a type error) is on the payload;
    /// `continue` issues (lengths, formats, custom adds) let them run.
    aborted: bool,
}

impl Issues {
    fn push(&mut self, path: &str, message: impl Into<String>) {
        let path = if path.is_empty() { "(input)" } else { path };
        self.out.push(issue(path, &message.into()));
    }

    fn abort(&mut self, path: &str, message: impl Into<String>) {
        self.push(path, message);
        self.aborted = true;
    }
}

fn expect_object<'a>(
    value: Option<&'a Value>,
    path: &str,
    issues: &mut Issues,
) -> Option<&'a Map<String, Value>> {
    match value.and_then(Value::as_object) {
        Some(object) => Some(object),
        None => {
            issues.abort(path, invalid_type("object", value));
            None
        }
    }
}

fn expect_array<'a>(
    value: Option<&'a Value>,
    path: &str,
    issues: &mut Issues,
) -> Option<&'a [Value]> {
    match value.and_then(Value::as_array) {
        Some(items) => Some(items),
        None => {
            issues.abort(path, invalid_type("array", value));
            None
        }
    }
}

fn expect_string(value: Option<&Value>, path: &str, issues: &mut Issues) -> Option<String> {
    match value {
        Some(Value::String(plain)) => Some(plain.clone()),
        other => {
            issues.abort(path, invalid_type("string", other));
            None
        }
    }
}

fn optional_string(value: Option<&Value>, path: &str, issues: &mut Issues) -> Option<String> {
    value.and_then(|value| expect_string(Some(value), path, issues))
}

fn min_one(text: String, path: &str, issues: &mut Issues) -> Option<String> {
    if text.is_empty() {
        issues.push(path, "Too small: expected string to have >=1 characters");
        return None;
    }
    Some(text)
}

/// `z.number().int()`: the value survives a safe-integer range miss (zod's bounds are `continue`
/// issues), so the refinements see it.
fn optional_int(value: Option<&Value>, path: &str, issues: &mut Issues) -> Option<f64> {
    let value = value?;
    let Some(number) = value.as_f64() else {
        issues.abort(path, invalid_type("number", Some(value)));
        return None;
    };
    if !number.is_finite() || number.fract() != 0.0 {
        issues.abort(path, "Invalid input: expected int, received number");
        return None;
    }
    const MAX_SAFE: f64 = 9007199254740991.0;
    if number > MAX_SAFE {
        issues.push(path, "Too big: expected int to be <=9007199254740991");
    } else if number < -MAX_SAFE {
        issues.push(path, "Too small: expected int to be >=-9007199254740991");
    }
    Some(number)
}

fn is_kebab(value: &str) -> bool {
    value.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

fn is_plan_node(value: &str) -> bool {
    match value.strip_prefix('N') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

fn string_array(value: &Value, path: &str, issues: &mut Issues) -> Vec<Value> {
    let Some(items) = expect_array(Some(value), path, issues) else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            expect_string(Some(item), &format!("{path}.{i}"), issues).map(Value::String)
        })
        .collect()
}

fn parse_code_block(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut out = Map::new();
    let Some(object) = expect_object(Some(value), path, issues) else {
        return Value::Object(out);
    };
    if let Some(language) = optional_string(object.get("language"), &format!("{path}.language"), issues)
    {
        out.insert("language".to_string(), Value::String(language));
    }
    if let Some(code) = expect_string(object.get("code"), &format!("{path}.code"), issues) {
        out.insert("code".to_string(), Value::String(code));
    }
    if let Some(caption) = optional_string(object.get("caption"), &format!("{path}.caption"), issues) {
        out.insert("caption".to_string(), Value::String(caption));
    }
    Value::Object(out)
}

fn parse_code_blocks(value: &Value, path: &str, issues: &mut Issues) -> Vec<Value> {
    let Some(items) = expect_array(Some(value), path, issues) else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .map(|(i, item)| parse_code_block(item, &format!("{path}.{i}"), issues))
        .collect()
}

fn parse_visual(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut out = Map::new();
    let Some(object) = expect_object(Some(value), path, issues) else {
        return Value::Object(out);
    };
    match object.get("type") {
        Some(Value::String(kind)) if matches!(kind.as_str(), "svg-inline" | "image" | "mermaid") => {
            out.insert("type".to_string(), Value::String(kind.clone()));
        }
        _ => issues.abort(
            &format!("{path}.type"),
            "Invalid option: expected one of \"svg-inline\"|\"image\"|\"mermaid\"",
        ),
    }
    if let Some(content) = expect_string(object.get("content"), &format!("{path}.content"), issues)
    {
        out.insert("content".to_string(), Value::String(content));
    }
    for key in ["caption", "alt"] {
        if let Some(plain) = optional_string(object.get(key), &format!("{path}.{key}"), issues) {
            out.insert(key.to_string(), Value::String(plain));
        }
    }
    Value::Object(out)
}

/// `z.union([visual, z.string()])`: the object is tried first, and a failure reports one issue.
fn parse_visual_or_string(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut attempt = Issues::default();
    let parsed = parse_visual(value, path, &mut attempt);
    if attempt.out.is_empty() {
        return parsed;
    }
    if value.is_string() {
        return value.clone();
    }
    issues.abort(path, "Invalid input");
    Value::Null
}

fn parse_step(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut out = Map::new();
    let Some(object) = expect_object(Some(value), path, issues) else {
        return Value::Object(out);
    };
    let step_id = format!("{path}.stepId");
    if let Some(id) = expect_string(object.get("stepId"), &step_id, issues)
        .and_then(|id| min_one(id, &step_id, issues))
    {
        out.insert("stepId".to_string(), Value::String(id));
    }
    if let Some(title) = optional_string(object.get("title"), &format!("{path}.title"), issues) {
        out.insert("title".to_string(), Value::String(title));
    }
    let content_path = format!("{path}.content");
    if let Some(content) = expect_string(object.get("content"), &content_path, issues) {
        if unsafe_html(&content) {
            issues.push(&content_path, "unsafe HTML rejected");
        }
        out.insert("content".to_string(), Value::String(content));
    }
    if let Some(math) = object.get("math") {
        out.insert(
            "math".to_string(),
            Value::Array(string_array(math, &format!("{path}.math"), issues)),
        );
    }
    if let Some(blocks) = object.get("codeBlocks") {
        out.insert(
            "codeBlocks".to_string(),
            Value::Array(parse_code_blocks(
                blocks,
                &format!("{path}.codeBlocks"),
                issues,
            )),
        );
    }
    if let Some(visuals) = object.get("visuals") {
        let path = format!("{path}.visuals");
        out.insert(
            "visuals".to_string(),
            Value::Array(
                expect_array(Some(visuals), &path, issues)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                    .map(|(i, item)| parse_visual(item, &format!("{path}.{i}"), issues))
                    .collect(),
            ),
        );
    }
    Value::Object(out)
}

fn parse_anchor(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut out = Map::new();
    let Some(object) = expect_object(Some(value), path, issues) else {
        return Value::Object(out);
    };
    if let Some(id) = expect_string(object.get("id"), &format!("{path}.id"), issues) {
        if !id.starts_with("n_") || id.len() < 3 || id[2..].chars().any(js::is_js_ws) {
            issues.push(
                &format!("{path}.id"),
                "a concept id like \"n_\u{2026}\" from search_concepts",
            );
        }
        out.insert("id".to_string(), Value::String(id));
    }
    if let Some(name) = expect_string(object.get("name"), &format!("{path}.name"), issues)
        .and_then(|name| min_one(name, &format!("{path}.name"), issues))
    {
        out.insert("name".to_string(), Value::String(name));
    }
    Value::Object(out)
}

fn parse_misconception(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut out = Map::new();
    let Some(object) = expect_object(Some(value), path, issues) else {
        return Value::Object(out);
    };
    if let Some(index) = optional_int(object.get("optionIndex"), &format!("{path}.optionIndex"), issues)
    {
        out.insert("optionIndex".to_string(), js::number(index));
    }
    if let Some(plain) = expect_string(
        object.get("misconception"),
        &format!("{path}.misconception"),
        issues,
    ) {
        out.insert("misconception".to_string(), Value::String(plain));
    }
    Value::Object(out)
}

fn parse_quiz(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut out = Map::new();
    let Some(object) = expect_object(Some(value), path, issues) else {
        return Value::Object(out);
    };
    let question = format!("{path}.question");
    let question = expect_string(object.get("question"), &question, issues)
        .and_then(|plain| min_one(plain, &question, issues));
    let options: Option<Vec<String>> = object.get("options").map(|value| {
        expect_array(Some(value), &format!("{path}.options"), issues)
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                expect_string(Some(item), &format!("{path}.options.{i}"), issues)
            })
            .collect()
    });
    let correct_answer =
        optional_string(object.get("correctAnswer"), &format!("{path}.correctAnswer"), issues);
    let correct_index = optional_int(
        object.get("correctIndex"),
        &format!("{path}.correctIndex"),
        issues,
    );
    let explanation = format!("{path}.explanation");
    let explanation = expect_string(object.get("explanation"), &explanation, issues)
        .and_then(|plain| min_one(plain, &explanation, issues));
    let misconceptions: Option<Vec<Value>> = object.get("misconceptions").map(|value| {
        let path = format!("{path}.misconceptions");
        expect_array(Some(value), &path, issues)
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .map(|(i, item)| parse_misconception(item, &format!("{path}.{i}"), issues))
            .collect()
    });

    if let Some(question) = question {
        out.insert("question".to_string(), Value::String(question));
    }
    if let Some(options) = &options {
        out.insert(
            "options".to_string(),
            Value::Array(options.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(answer) = &correct_answer {
        out.insert("correctAnswer".to_string(), Value::String(answer.clone()));
    }
    if let Some(index) = correct_index {
        out.insert("correctIndex".to_string(), js::number(index));
    }
    if let Some(explanation) = explanation {
        out.insert("explanation".to_string(), Value::String(explanation));
    }
    if let Some(misconceptions) = &misconceptions {
        out.insert(
            "misconceptions".to_string(),
            Value::Array(misconceptions.clone()),
        );
    }

    if !issues.aborted {
        quiz_refine(&options, correct_index, &correct_answer, &misconceptions, path, issues);
    }
    Value::Object(out)
}

/// The `quizCheckpoint` superRefine: the options define the valid range of every 1-based index.
fn quiz_refine(
    options: &Option<Vec<String>>,
    correct_index: Option<f64>,
    correct_answer: &Option<String>,
    misconceptions: &Option<Vec<Value>>,
    path: &str,
    issues: &mut Issues,
) {
    let count = options.as_ref().map(Vec::len).unwrap_or(0);
    if count == 0 {
        return;
    }
    if correct_index.is_none() {
        issues.push(
            &format!("{path}.correctIndex"),
            "required when options are given (1-based)",
        );
    }
    if correct_answer.is_none() {
        issues.push(
            &format!("{path}.correctAnswer"),
            "required when options are given: the text of the correct option",
        );
    }
    if let Some(index) = correct_index {
        if index < 1.0 || index > count as f64 {
            issues.push(
                &format!("{path}.correctIndex"),
                format!("must be an integer from 1 to {count} (1-based)"),
            );
        } else if let (Some(answer), Some(options)) = (correct_answer, options)
            && options.get(index as usize - 1) != Some(answer)
        {
            issues.push(
                &format!("{path}.correctAnswer"),
                "must equal options[correctIndex - 1]",
            );
        }
    }
    if let Some(items) = misconceptions {
        for (i, item) in items.iter().enumerate() {
            let out_of_range = item
                .get("optionIndex")
                .and_then(Value::as_f64)
                .is_some_and(|index| index < 1.0 || index > count as f64);
            if out_of_range {
                issues.push(
                    &format!("{path}.misconceptions.{i}.optionIndex"),
                    format!("must be from 1 to {count} (1-based)"),
                );
            }
        }
    }
}

fn parse_anki_params(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut out = Map::new();
    let Some(object) = expect_object(Some(value), path, issues) else {
        return Value::Object(out);
    };
    for key in ["targetQuestion", "targetFact", "suggestedCloze", "clozeExtra", "deck"] {
        if let Some(plain) = optional_string(object.get(key), &format!("{path}.{key}"), issues) {
            out.insert(key.to_string(), Value::String(plain));
        }
    }
    if let Some(tags) = object.get("tags") {
        out.insert(
            "tags".to_string(),
            Value::Array(string_array(tags, &format!("{path}.tags"), issues)),
        );
    }

    if !issues.aborted {
        let has = |key: &str| {
            out.get(key)
                .and_then(Value::as_str)
                .is_some_and(|plain| !plain.is_empty())
        };
        let question = has("targetQuestion");
        let fact = has("targetFact");
        let cloze = has("suggestedCloze");
        if question != fact {
            issues.push(path, "targetQuestion and targetFact go together");
        }
        if (question || fact) && cloze {
            issues.push(
                path,
                "use either targetQuestion+targetFact or suggestedCloze, never both",
            );
        }
        if cloze && !has_cloze(out.get("suggestedCloze").and_then(Value::as_str).unwrap_or("")) {
            issues.push(
                &format!("{path}.suggestedCloze"),
                "must contain a cloze like {{c1::...}}",
            );
        }
    }
    Value::Object(out)
}

/// `/\{\{c\d+::/`.
fn has_cloze(text: &str) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find("{{c") {
        let digits = rest[start + 3..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digits > 0 && rest[start + 3 + digits..].starts_with("::") {
            return true;
        }
        rest = &rest[start + 1..];
    }
    false
}

fn parse_node(value: &Value, path: &str, issues: &mut Issues) -> Value {
    let mut out = Map::new();
    let Some(object) = expect_object(Some(value), path, issues) else {
        return Value::Object(out);
    };

    let node_id_path = format!("{path}.nodeId");
    if let Some(id) = expect_string(object.get("nodeId"), &node_id_path, issues) {
        if !is_kebab(&id) {
            issues.push(&node_id_path, "kebab-case slug");
        }
        out.insert("nodeId".to_string(), Value::String(id));
    }
    let title_path = format!("{path}.title");
    if let Some(title) = expect_string(object.get("title"), &title_path, issues)
        .and_then(|title| min_one(title, &title_path, issues))
    {
        out.insert("title".to_string(), Value::String(title));
    }
    match object.get("tier") {
        Some(Value::String(tier))
            if matches!(tier.as_str(), "unconditional-truth" | "axiom" | "derivation") =>
        {
            out.insert("tier".to_string(), Value::String(tier.clone()));
        }
        _ => issues.abort(
            &format!("{path}.tier"),
            "Invalid option: expected one of \"unconditional-truth\"|\"axiom\"|\"derivation\"",
        ),
    }
    for key in ["summary", "motivation", "content"] {
        if let Some(plain) = optional_string(object.get(key), &format!("{path}.{key}"), issues) {
            out.insert(key.to_string(), Value::String(plain));
        }
    }
    if let Some(steps) = object.get("steps") {
        let steps_path = format!("{path}.steps");
        out.insert(
            "steps".to_string(),
            Value::Array(
                expect_array(Some(steps), &steps_path, issues)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                    .map(|(i, item)| parse_step(item, &format!("{steps_path}.{i}"), issues))
                    .collect(),
            ),
        );
    }
    if let Some(math) = object.get("math") {
        out.insert(
            "math".to_string(),
            Value::Array(string_array(math, &format!("{path}.math"), issues)),
        );
    }
    if let Some(blocks) = object.get("codeBlocks") {
        out.insert(
            "codeBlocks".to_string(),
            Value::Array(parse_code_blocks(
                blocks,
                &format!("{path}.codeBlocks"),
                issues,
            )),
        );
    }
    if let Some(visuals) = object.get("visuals") {
        let visuals_path = format!("{path}.visuals");
        out.insert(
            "visuals".to_string(),
            Value::Array(
                expect_array(Some(visuals), &visuals_path, issues)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                    .map(|(i, item)| parse_visual_or_string(item, &format!("{visuals_path}.{i}"), issues))
                    .collect(),
            ),
        );
    }
    if let Some(dependencies) = object.get("dependencies") {
        out.insert(
            "dependencies".to_string(),
            Value::Array(string_array(
                dependencies,
                &format!("{path}.dependencies"),
                issues,
            )),
        );
    }
    if let Some(anchors) = object.get("anchors") {
        let anchors_path = format!("{path}.anchors");
        out.insert(
            "anchors".to_string(),
            Value::Array(
                expect_array(Some(anchors), &anchors_path, issues)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                    .map(|(i, item)| parse_anchor(item, &format!("{anchors_path}.{i}"), issues))
                    .collect(),
            ),
        );
    }
    // Schema order: `connection` sits after the anchors, right before `quiz`.
    if let Some(plain) = optional_string(object.get("connection"), &format!("{path}.connection"), issues)
    {
        out.insert("connection".to_string(), Value::String(plain));
    }
    if let Some(quiz) = object.get("quiz") {
        out.insert(
            "quiz".to_string(),
            parse_quiz(quiz, &format!("{path}.quiz"), issues),
        );
    }
    if let Some(anki) = object.get("ankiParams") {
        out.insert(
            "ankiParams".to_string(),
            parse_anki_params(anki, &format!("{path}.ankiParams"), issues),
        );
    }
    Value::Object(out)
}

pub fn validate(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        let mut issues = Issues::default();
        issues.abort("", invalid_type("object", Some(input)));
        return Err(issues.out);
    };
    let mut issues = Issues::default();
    let mut out = Map::new();

    let plan_node = expect_string(object.get("planNode"), "planNode", &mut issues);
    if let Some(plan_node) = &plan_node
        && !is_plan_node(plan_node)
    {
        issues.push("planNode", "must look like \"N3\"");
    }
    let node = match object.get("node") {
        None => {
            issues.abort(
                "node",
                "Invalid input: expected object, received undefined",
            );
            Value::Null
        }
        Some(value) => parse_node(value, "node", &mut issues),
    };
    if !issues.aborted && node.is_object() {
        write_node_refine(&node, &mut issues);
    }
    if !issues.out.is_empty() {
        return Err(issues.out);
    }
    out.insert(
        "planNode".to_string(),
        plan_node.map(Value::String).unwrap_or(Value::Null),
    );
    out.insert("node".to_string(), node);
    Ok(Value::Object(out))
}

/// The `writeNodeSchema` superRefine: derived content, and the active-HTML sweep over prose fields.
fn write_node_refine(node: &Value, issues: &mut Issues) {
    let steps = node
        .get("steps")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let content = node.get("content").and_then(Value::as_str).unwrap_or("");
    if steps.is_empty() && js::js_trim(content).is_empty() {
        issues.push(
            "node.content",
            "content is required when steps are not provided",
        );
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    for key in ["summary", "motivation", "content", "connection"] {
        if let Some(plain) = node.get(key).and_then(Value::as_str) {
            fields.push((format!("node.{key}"), plain.to_string()));
        }
    }
    let quiz = node.get("quiz");
    for key in ["question", "explanation"] {
        if let Some(plain) = quiz.and_then(|quiz| quiz.get(key)).and_then(Value::as_str) {
            fields.push((format!("node.quiz.{key}"), plain.to_string()));
        }
    }
    for (i, visual) in node
        .get("visuals")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let plain = match visual {
            Value::String(plain) => Some(plain.as_str()),
            other => other.get("content").and_then(Value::as_str),
        };
        if let Some(plain) = plain {
            fields.push((format!("node.visuals[{i}]"), plain.to_string()));
        }
    }
    for (step, value) in steps.iter().enumerate() {
        for (i, visual) in value
            .get("visuals")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            if let Some(plain) = visual.get("content").and_then(Value::as_str) {
                fields.push((format!("node.steps[{step}].visuals[{i}]"), plain.to_string()));
            }
        }
    }
    for (path, plain) in fields {
        if !plain.is_empty() && unsafe_html(&plain) {
            issues.push(
                &path,
                "active HTML (<script, <iframe, on*= handlers, javascript:) is not allowed",
            );
        }
    }
}

fn schema() -> Value {
    serde_json::from_str(include_str!("schemas/write_node.json")).expect("write_node.json parses")
}

pub const TOOL: ToolDef = ToolDef {
    name: "write_node",
    description: "Write (or replace, same nodeId) one node into the lesson artifact and show it to the learner. Requires an approved plan. Every new idea carries `anchors` (n_… ids and names from the concept tools) and its prose names those concepts.",
    schema,
    validate,
    run,
    writes: true,
};

pub fn run(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(async move {
        if host.lesson_path().is_none() {
            return fail("No lesson artifact yet: get the plan approved with propose_plan first.");
        }
        let Some(lesson) = util::current_lesson(host) else {
            return fail("The lesson artifact is missing or has no data island.");
        };
        let node = input
            .get("node")
            .cloned()
            .expect("validated write_node input always carries a node");
        let node_id = text(node.get("nodeId")).to_string();
        let plan_node = text(input.get("planNode")).to_string();

        let store = GraphStore::new(host.data_root().to_path_buf());
        let live = store.read_node_lines();
        let previous = lesson
            .doc
            .get("nodes")
            .and_then(Value::as_array)
            .and_then(|nodes| {
                nodes
                    .iter()
                    .find(|candidate| text(candidate.get("nodeId")) == node_id)
            });
        if let Some(anchors) = node.get("anchors").and_then(Value::as_array)
            && !anchors.is_empty()
        {
            let live_ids: Vec<&str> = live.iter().map(|record| text(record.get("id"))).collect();
            let kept: Vec<&str> = previous
                .and_then(|node| node.get("anchors"))
                .and_then(Value::as_array)
                .map(|anchors| anchors.iter().map(|a| text(a.get("id"))).collect())
                .unwrap_or_default();
            // a rewrite may resend an anchor whose concept was deleted since: it just degrades to
            // text
            let unknown: Vec<&str> = anchors
                .iter()
                .map(|anchor| text(anchor.get("id")))
                .filter(|id| !live_ids.contains(id) && !kept.contains(id))
                .collect();
            if !unknown.is_empty() {
                return fail(format!(
                    "anchors: {} not in the concept map. Use ids returned by search_concepts/open_concept/browse_concepts, or drop the anchor.",
                    unknown.join(", ")
                ));
            }
        }

        let mut node = node;
        fill_content(&mut node);
        let mut notes: Vec<String> = Vec::new();
        if node
            .get("anchors")
            .and_then(Value::as_array)
            .is_none_or(|anchors| anchors.is_empty())
        {
            // the prose is the source of truth: if it names a live concept, the anchor is missing,
            // not optional
            let named = named_concepts(&node, &live);
            if !named.is_empty() {
                let names: Vec<String> = named
                    .iter()
                    .map(|(id, name)| format!("{name} ({id})"))
                    .collect();
                notes.push(format!(
                    "anchors missing: the prose names {}. Resend this node with anchors to link it on the map.",
                    names.join(", ")
                ));
            } else if previous
                .and_then(|node| node.get("anchors"))
                .and_then(Value::as_array)
                .is_some_and(|anchors| !anchors.is_empty())
            {
                notes.push(format!(
                    "anchors dropped: this rewrite of {node_id} carries none; resend its anchors or they are lost."
                ));
            }
        } else {
            // an anchor the prose never names is invisible to the learner: ask for the same sentence
            // with the stored name
            let prose = prose_text(&node);
            let unnamed: Vec<String> = node["anchors"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter_map(|anchor| {
                    let id = text(anchor.get("id"));
                    // a kept anchor whose concept was deleted falls back to the name the tutor sent
                    let name = live
                        .iter()
                        .find(|record| text(record.get("id")) == id)
                        .map(|record| text(record.get("name")))
                        .unwrap_or_else(|| text(anchor.get("name")));
                    let named = prose.contains(&normalise(name));
                    (!named).then(|| format!("{name} ({id})"))
                })
                .collect();
            if !unnamed.is_empty() {
                notes.push(format!(
                    "anchors not named in the prose: {}. The prose must name each anchored concept verbatim; resend this node with it.",
                    unnamed.join(", ")
                ));
            }
        }

        if node.get("ankiParams").is_some() {
            if !js::truthy(host.config().get("ankiOptIn")) {
                remove_key(&mut node, "ankiParams");
                notes.push("ankiParams removed: the learner did not opt in to Anki.".to_string());
            } else if !passed_check(host, &plan_node) {
                remove_key(&mut node, "ankiParams");
                notes.push("ankiParams removed: no passed check for this node yet. ankiParams kept after the check passes: call write_node again with ankiParams after a passed check, then sync_anki.".to_string());
            }
        }

        let node_count = lesson
            .doc
            .get("nodes")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let expected = format!("N{}", node_count + 1);
        if previous.is_none() && plan_node != expected {
            notes.push(format!(
                "warning: this is node {} of the lesson but planNode is {plan_node}; nodes are expected in plan order ({expected}).",
                node_count + 1
            ));
        }

        let abs = lesson.abs.clone();
        let lesson_path = util::to_rel(host.data_root(), &abs);
        let appended = doc::append_node(doc::AppendOptions {
            lesson_path: Some(abs.to_string_lossy().into_owned()),
            slug: None,
            node,
            base_dir: Some(host.data_root().to_string_lossy().into_owned()),
        });
        let appended = match appended {
            Ok(appended) => appended,
            Err(message) => return fail(message),
        };

        host.emit(json!({
            "id": format!("node:{node_id}"),
            "kind": "node",
            "nodeId": node_id,
            "planNode": plan_node,
            "title": text(appended.node.get("title")),
            "lessonPath": lesson_path,
            "node": appended.node,
        }));
        host.set_lesson(lesson_path, Some(node_id.clone()));
        util::recompute_plan(host);

        let mut out = Map::new();
        out.insert("ok".to_string(), Value::Bool(true));
        out.insert("nodeCount".to_string(), Value::from(appended.node_count));
        if !notes.is_empty() {
            out.insert(
                "notes".to_string(),
                Value::Array(notes.into_iter().map(Value::String).collect()),
            );
        }
        util::json(&Value::Object(out))
    })
}

fn fill_content(node: &mut Value) {
    let steps = node
        .get("steps")
        .and_then(Value::as_array)
        .filter(|steps| !steps.is_empty());
    let Some(steps) = steps else { return };
    let content = node.get("content").and_then(Value::as_str).unwrap_or("");
    if !js::js_trim(content).is_empty() {
        return;
    }
    let derived: Vec<String> = steps
        .iter()
        .map(|step| text(step.get("content")).to_string())
        .collect();
    node["content"] = Value::String(derived.join("\n\n"));
}

fn remove_key(node: &mut Value, key: &str) {
    if let Some(object) = node.as_object_mut() {
        object.shift_remove(key);
    }
}

/// The latest `check` record for the plan node must have passed: `ankiParams` is only for a passed
/// check.
fn passed_check(host: &dyn ToolHost, plan_node: &str) -> bool {
    let latest = util::lesson_progress(host).into_iter().rfind(|record| {
        record.get("kind").and_then(Value::as_str) == Some("check")
            && record
                .get("nodeRefs")
                .and_then(Value::as_array)
                .is_some_and(|refs| refs.iter().any(|value| value.as_str() == Some(plan_node)))
    });
    latest.is_some_and(|record| {
        record.get("correct") == Some(&Value::Bool(true)) && !js::truthy(record.get("dontKnow"))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use serde_json::Map;

    use super::*;
    use crate::lesson::doc::parse_lesson_document;
    use crate::tools::ToolOutcome;
    use crate::tools::fake::FakeHost;

    const DAG: &str =
        "graph TD\n  N1[\"Nodo 1: la prima cosa è vera\"] --> N2[\"Nodo 2: la seconda segue\"]";

    fn teach() -> Value {
        json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false})
    }

    /// Node's `nodeIn`: the default node with `o`'s values merged in. A `null` value drops the key,
    /// which is how a JS `undefined` override reads on the wire.
    fn node_in(o: Value) -> Value {
        let mut node = Map::from_iter([
            ("nodeId".to_string(), json!("prima-cosa")),
            ("title".to_string(), json!("La prima cosa")),
            ("tier".to_string(), json!("unconditional-truth")),
            ("content".to_string(), json!("<p>vero</p>")),
        ]);
        if let Some(over) = o.as_object() {
            for (key, value) in over {
                if value.is_null() {
                    node.shift_remove(key);
                } else {
                    node.insert(key.clone(), value.clone());
                }
            }
        }
        json!({"planNode": "N1", "node": Value::Object(node)})
    }

    /// `nodeIn` with a quiz checkpoint made of `o` over its defaults.
    fn with_quiz(o: Value) -> Value {
        let mut quiz = Map::from_iter([
            ("question".to_string(), json!("Q")),
            ("options".to_string(), json!(["a", "b"])),
            ("correctIndex".to_string(), json!(2)),
            ("correctAnswer".to_string(), json!("b")),
            ("explanation".to_string(), json!("e")),
        ]);
        if let Some(over) = o.as_object() {
            for (key, value) in over {
                if value.is_null() {
                    quiz.shift_remove(key);
                } else {
                    quiz.insert(key.clone(), value.clone());
                }
            }
        }
        node_in(json!({"quiz": Value::Object(quiz)}))
    }

    fn notes(out: &ToolOutcome) -> Vec<String> {
        let value: Value = serde_json::from_str(&out.text).expect("the tool answers JSON");
        value
            .get("notes")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn lesson_html(host: &FakeHost) -> String {
        let rel = host.lesson_path().expect("a lesson");
        fs::read_to_string(host.data_root().join(rel)).expect("read the lesson")
    }

    fn island(host: &FakeHost) -> Value {
        parse_lesson_document(&lesson_html(host))
            .expect("parse the lesson")
            .expect("a data island")
    }

    /// One more live concept, appended as the Node tests do.
    fn seed_concept(host: &FakeHost, id: &str, name: &str) {
        let record = json!({
            "id": id,
            "name": name,
            "statement": "Un concetto noto.",
            "aliases": [],
            "createdAt": "2026-11-01T00:00:00.000Z",
            "provenance": [{"chatId": "demo/0002-il-secondo-passo", "messageId": "m1"}],
        });
        let path = host.data_root().join("graph").join("nodes.jsonl");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open nodes.jsonl");
        writeln!(file, "{}", serde_json::to_string(&record).expect("one line"))
            .expect("append a concept");
    }

    fn progress(host: &FakeHost, plan_node: &str, correct: bool) -> Map<String, Value> {
        let lesson_path = host.lesson_path().expect("a lesson");
        Map::from_iter([
            ("lessonSlug".to_string(), json!(crate::lesson::artifact::slug_of_lesson(&lesson_path))),
            ("nodeRefs".to_string(), json!([plan_node])),
            ("kind".to_string(), json!("check")),
            ("correct".to_string(), json!(correct)),
            ("dontKnow".to_string(), json!(false)),
        ])
    }

    #[tokio::test]
    async fn requires_a_lesson() {
        let host = FakeHost::new(teach());
        let out = host.call(&TOOL, node_in(json!({}))).await;
        assert!(out.is_error, "{}", out.text);
        assert!(out.text.contains("No lesson artifact yet"), "{}", out.text);
    }

    #[tokio::test]
    async fn writes_upserts_emits_and_updates_the_plan() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let quiz = json!({
            "question": "Q",
            "options": ["a", "b"],
            "correctIndex": 2,
            "correctAnswer": "b",
            "explanation": "e",
            "misconceptions": [{"optionIndex": 1, "misconception": "m"}],
        });
        let out = host.call(&TOOL, node_in(json!({"quiz": quiz}))).await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(out.text, r#"{"ok":true,"nodeCount":1}"#);

        let rel = host.lesson_path().expect("a lesson");
        let html = lesson_html(&host);
        assert!(html.contains("id=\"node-prima-cosa\""), "{html}");

        let item = host.items().last().cloned().expect("an item");
        assert_eq!(item["id"], json!("node:prima-cosa"));
        assert_eq!(item["kind"], json!("node"));
        assert_eq!(item["nodeId"], json!("prima-cosa"));
        assert_eq!(item["planNode"], json!("N1"));
        assert_eq!(item["title"], json!("La prima cosa"));
        assert_eq!(item["lessonPath"], json!(rel));
        assert_eq!(item["node"]["nodeId"], json!("prima-cosa"));
        assert_eq!(item["node"]["quiz"]["correctAnswer"], json!("b"));
        // One object, one stamp: the chat item is the node the island holds, `capturedAt` included.
        assert_eq!(
            item["node"]["capturedAt"],
            island(&host)["nodes"][0]["capturedAt"],
            "{item}"
        );

        assert!(
            host.events().contains(&json!({
                "type": "lesson",
                "lessonPath": rel,
                "nodeId": "prima-cosa",
            })),
            "{:?}",
            host.events()
        );
        assert_eq!(host.plan()["nodeStatus"], json!({"N1": "done", "N2": "current"}));

        let again = host.call(&TOOL, node_in(json!({"title": "Nuovo titolo"}))).await;
        assert_eq!(again.text, r#"{"ok":true,"nodeCount":1}"#);
        assert_eq!(island(&host)["nodes"].as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn stores_anchors_to_existing_concepts_in_the_data_island() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let anchors = json!([{"id": "n_fx_a", "name": "Primo passo"}]);
        let out = host.call(&TOOL, node_in(json!({"anchors": anchors}))).await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(island(&host)["nodes"][0]["anchors"], anchors);
    }

    #[tokio::test]
    async fn reminds_to_anchor_a_node_whose_prose_names_a_live_concept() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let named = host
            .call(
                &TOOL,
                node_in(json!({"content": "<p>Riprendiamo il primo passo di ieri.</p>"})),
            )
            .await;
        assert!(!named.is_error, "{}", named.text);
        assert!(
            notes(&named)[0].contains("anchors missing: the prose names Primo passo (n_fx_a)"),
            "{}",
            named.text
        );

        let anchored = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>Riprendiamo il primo passo di ieri.</p>",
                    "anchors": [{"id": "n_fx_a", "name": "Primo passo"}],
                })),
            )
            .await;
        assert!(notes(&anchored).is_empty(), "{}", anchored.text);

        let dropped = host
            .call(&TOOL, node_in(json!({"content": "<p>senza ancoraggio</p>"})))
            .await;
        assert!(!dropped.is_error, "{}", dropped.text);
        let dropped_notes = notes(&dropped);
        assert!(dropped_notes[0].contains("anchors dropped"), "{dropped_notes:?}");

        let unnamed = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>un ref senza il nome del concetto</p>",
                    "anchors": [{"id": "n_fx_a", "name": "Primo passo"}],
                })),
            )
            .await;
        assert!(
            notes(&unnamed)[0].contains("anchors not named in the prose: Primo passo (n_fx_a)"),
            "{}",
            unnamed.text
        );
    }

    #[tokio::test]
    async fn checks_anchors_against_the_stored_concept_name_normalised_like_the_prose() {
        let host = FakeHost::with_lesson(teach(), DAG);
        seed_concept(&host, "n_fx_snake", "use_state");
        // the tutor's name differs from the stored one: the prose names the stored concept
        let stored = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>Riprendiamo il Primo passo.</p>",
                    "anchors": [{"id": "n_fx_a", "name": "Direzione"}],
                })),
            )
            .await;
        assert!(notes(&stored).is_empty(), "{}", stored.text);
        // the note names the stored concept, not the name the tutor sent
        let unnamed = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>niente</p>",
                    "anchors": [{"id": "n_fx_a", "name": "Direzione"}],
                })),
            )
            .await;
        let unnamed_notes = notes(&unnamed);
        assert!(
            unnamed_notes[0].contains("anchors not named in the prose: Primo passo (n_fx_a)"),
            "{unnamed_notes:?}"
        );
        assert!(!unnamed_notes[0].contains("Direzione"), "{unnamed_notes:?}");
        // an underscore in the name survives the same normalisation as the prose
        let snake = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>Qui conta <code>use_state</code>.</p>",
                    "anchors": [{"id": "n_fx_snake", "name": "use_state"}],
                })),
            )
            .await;
        assert!(notes(&snake).is_empty(), "{}", snake.text);
    }

    #[tokio::test]
    async fn flags_prose_that_names_the_tutors_name_instead_of_the_stored_name() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let out = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>Solo direzione.</p>",
                    "anchors": [{"id": "n_fx_a", "name": "Direzione"}],
                })),
            )
            .await;
        let out_notes = notes(&out);
        assert!(
            out_notes[0].contains("anchors not named in the prose: Primo passo (n_fx_a)"),
            "{out_notes:?}"
        );
        assert!(!out_notes[0].contains("Direzione"), "{out_notes:?}");
    }

    #[tokio::test]
    async fn normalises_emphasis_and_extra_spaces_in_a_stored_name_on_both_paths() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let name = "Props  e `emit`";
        seed_concept(&host, "n_fx_emit", name);

        let anchored = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>Le props e emit di Vue.</p>",
                    "anchors": [{"id": "n_fx_emit", "name": name}],
                })),
            )
            .await;
        assert!(notes(&anchored).is_empty(), "{}", anchored.text);

        let missing = host
            .call(
                &TOOL,
                node_in(json!({"content": "<p>Le props e emit di Vue.</p>"})),
            )
            .await;
        assert!(
            notes(&missing)[0].contains(&format!("anchors missing: the prose names {name} (n_fx_emit)")),
            "{}",
            missing.text
        );
    }

    #[tokio::test]
    async fn trims_surrounding_spaces_in_a_stored_concept_name_before_comparing() {
        let host = FakeHost::with_lesson(teach(), DAG);
        seed_concept(&host, "n_fx_pad", "Primo passo ");
        let anchored = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>Riprendiamo il Primo passo.</p>",
                    "anchors": [{"id": "n_fx_pad", "name": "Primo passo "}],
                })),
            )
            .await;
        assert!(notes(&anchored).is_empty(), "{}", anchored.text);

        // a three-letter concept padded to four characters is still too short to be suggested
        let host = FakeHost::with_lesson(teach(), DAG);
        seed_concept(&host, "n_fx_ref", "Ref ");
        let short = host
            .call(&TOOL, node_in(json!({"content": "<p>una Ref </p>"})))
            .await;
        assert!(notes(&short).is_empty(), "{}", short.text);
    }

    #[tokio::test]
    async fn trims_surrounding_spaces_in_the_anchors_own_name_for_a_deleted_concept() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let anchors = json!([{"id": "n_fx_a", "name": "  Primo passo "}]);
        let first = host
            .call(
                &TOOL,
                node_in(json!({
                    "anchors": anchors,
                    "content": "<p>Riprendiamo il Primo passo.</p>",
                })),
            )
            .await;
        assert!(!first.is_error, "{}", first.text);
        GraphStore::new(host.data_root().to_path_buf())
            .delete_node("n_fx_a")
            .expect("delete the concept");
        // the concept is gone, so the fallback name is the only text: its spaces must not hide it
        let kept = host
            .call(
                &TOOL,
                node_in(json!({
                    "anchors": anchors,
                    "content": "<p>Di nuovo il Primo passo.</p>",
                })),
            )
            .await;
        assert!(notes(&kept).is_empty(), "{}", kept.text);
    }

    #[tokio::test]
    async fn falls_back_to_the_anchors_own_name_for_a_deleted_concept() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let anchors = json!([{"id": "n_fx_a", "name": "Primo passo"}]);
        assert!(
            !host
                .call(
                    &TOOL,
                    node_in(json!({
                        "anchors": anchors,
                        "content": "<p>Riprendiamo il Primo passo.</p>",
                    })),
                )
                .await
                .is_error
        );
        GraphStore::new(host.data_root().to_path_buf())
            .delete_node("n_fx_a")
            .expect("delete the concept");
        let kept = host
            .call(
                &TOOL,
                node_in(json!({
                    "anchors": anchors,
                    "content": "<p>Di nuovo il Primo passo.</p>",
                })),
            )
            .await;
        assert!(notes(&kept).is_empty(), "{}", kept.text);

        let unnamed = host
            .call(&TOOL, node_in(json!({"anchors": anchors, "content": "<p>niente</p>"})))
            .await;
        assert!(
            notes(&unnamed)[0].contains("anchors not named in the prose: Primo passo (n_fx_a)"),
            "{}",
            unnamed.text
        );
        assert_eq!(island(&host)["nodes"][0]["anchors"], anchors);
    }

    #[tokio::test]
    async fn suggests_live_concepts_at_most_three_times_through_markup_and_in_any_field() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let out = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>PRIMO <strong>PASSO</strong>, secondo passo, direzione, progresso.</p>",
                })),
            )
            .await;
        let out_notes = notes(&out);
        assert!(
            out_notes[0].contains("anchors missing: the prose names Primo passo (n_fx_a)"),
            "{out_notes:?}"
        );
        assert_eq!(out_notes[0].matches("(n_fx_").count(), 3, "{}", out.text);

        let in_steps = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": "<p>niente da vedere qui</p>",
                    "steps": [{"stepId": "s1", "title": "Un passo", "content": "Qui conta il Secondo passo."}],
                })),
            )
            .await;
        assert!(
            notes(&in_steps)[0].contains("anchors missing: the prose names Secondo passo (n_fx_b)"),
            "{}",
            in_steps.text
        );
    }

    #[tokio::test]
    async fn does_not_suggest_a_deleted_concept_a_short_name_or_a_name_inside_a_word() {
        let host = FakeHost::with_lesson(teach(), DAG);
        seed_concept(&host, "n_fx_ref", "Ref");
        GraphStore::new(host.data_root().to_path_buf())
            .delete_node("n_fx_a")
            .expect("delete the concept");
        let out = host
            .call(
                &TOOL,
                node_in(json!({"content": "<p>un primo passo, stiamo direzionando, una Ref</p>"})),
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(notes(&out).is_empty(), "{}", out.text);
    }

    #[tokio::test]
    async fn a_lesson_without_anchors_still_parses_and_takes_an_anchored_rewrite() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let first = host
            .call(&TOOL, node_in(json!({"content": "<p>senza ancore</p>"})))
            .await;
        assert!(!first.is_error, "{}", first.text);
        assert!(island(&host)["nodes"][0].get("anchors").is_none());

        let anchors = json!([{"id": "n_fx_b", "name": "Secondo passo"}]);
        let second = host
            .call(&TOOL, node_in(json!({"anchors": anchors, "content": "<p>con un'ancora</p>"})))
            .await;
        assert!(!second.is_error, "{}", second.text);
        assert_eq!(island(&host)["nodes"][0]["anchors"], anchors);
    }

    #[tokio::test]
    async fn rejects_a_malformed_anchor_id_or_an_empty_name_before_the_concept_map() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let malformed = host
            .call(
                &TOOL,
                node_in(json!({"anchors": [{"id": "vue_ref", "name": "Ref"}]})),
            )
            .await;
        assert!(malformed.is_error, "{}", malformed.text);
        assert!(malformed.text.starts_with("Invalid input"), "{}", malformed.text);
        assert!(
            malformed
                .text
                .contains("node.anchors.0.id: a concept id like \"n_\u{2026}\" from search_concepts"),
            "{}",
            malformed.text
        );

        let unnamed = host
            .call(&TOOL, node_in(json!({"anchors": [{"id": "n_fx_a", "name": ""}]})))
            .await;
        assert!(unnamed.is_error, "{}", unnamed.text);
        assert!(unnamed.text.starts_with("Invalid input"), "{}", unnamed.text);
        assert_eq!(host.requests().len(), 0);
    }

    #[tokio::test]
    async fn rejects_an_unknown_anchor_id_naming_it() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let out = host
            .call(
                &TOOL,
                node_in(json!({"anchors": [{"id": "n_nope", "name": "X"}]})),
            )
            .await;
        assert!(out.is_error, "{}", out.text);
        assert_eq!(
            out.text,
            "anchors: n_nope not in the concept map. Use ids returned by search_concepts/open_concept/browse_concepts, or drop the anchor."
        );
        assert_eq!(island(&host)["nodes"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn names_every_unknown_anchor_id_and_keeps_the_known_ones_out_of_the_error() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let out = host
            .call(
                &TOOL,
                node_in(json!({
                    "anchors": [
                        {"id": "n_fx_a", "name": "Primo passo"},
                        {"id": "n_nope1", "name": "X"},
                        {"id": "n_nope2", "name": "Y"},
                    ],
                })),
            )
            .await;
        assert!(out.is_error, "{}", out.text);
        assert!(out.text.contains("n_nope1, n_nope2"), "{}", out.text);
        assert!(!out.text.contains("n_fx_a"), "{}", out.text);
        assert_eq!(island(&host)["nodes"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn a_rewrite_keeps_an_anchor_whose_concept_was_deleted() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let anchors = json!([{"id": "n_fx_a", "name": "Primo passo"}]);
        let first = host
            .call(&TOOL, node_in(json!({"anchors": anchors})))
            .await;
        assert!(!first.is_error, "{}", first.text);
        GraphStore::new(host.data_root().to_path_buf())
            .delete_node("n_fx_a")
            .expect("delete the concept");
        let rewrite = host
            .call(
                &TOOL,
                node_in(json!({"anchors": anchors, "content": "<p>di nuovo</p>"})),
            )
            .await;
        assert!(!rewrite.is_error, "{}", rewrite.text);
        assert_eq!(island(&host)["nodes"][0]["anchors"], anchors);

        let mut both = anchors.as_array().cloned().unwrap_or_default();
        both.push(json!({"id": "n_nope", "name": "X"}));
        let bad = host
            .call(
                &TOOL,
                node_in(json!({"anchors": both, "content": "<p>terzo</p>"})),
            )
            .await;
        assert!(bad.is_error, "{}", bad.text);
        assert!(bad.text.contains("n_nope"), "{}", bad.text);
        assert!(!bad.text.contains("n_fx_a"), "{}", bad.text);
    }

    #[tokio::test]
    async fn strips_anki_params_without_opt_in_and_keeps_them_after_a_passed_check() {
        let anki = json!({"targetQuestion": "Q?", "targetFact": "F"});

        let host = FakeHost::with_lesson(teach(), DAG);
        let out = host
            .call(&TOOL, node_in(json!({"ankiParams": anki})))
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("ankiParams removed"), "{}", out.text);
        assert!(!lesson_html(&host).contains("targetQuestion"));

        let mut config = teach();
        config["ankiOptIn"] = json!(true);
        let host = FakeHost::with_lesson(config, DAG);
        // no check yet, then a missed check: stripped, with the instruction on how to keep them
        let none = host
            .call(&TOOL, node_in(json!({"ankiParams": anki})))
            .await;
        assert!(none.text.contains("kept after the check passes"), "{}", none.text);
        host.log_progress(progress(&host, "N1", false))
            .expect("append a missed check");
        let missed = host
            .call(&TOOL, node_in(json!({"ankiParams": anki})))
            .await;
        assert!(
            missed.text.contains("kept after the check passes"),
            "{}",
            missed.text
        );
        // a passed check keeps them
        host.log_progress(progress(&host, "N1", true))
            .expect("append a passed check");
        let passed = host
            .call(&TOOL, node_in(json!({"ankiParams": anki})))
            .await;
        assert!(!passed.text.contains("removed"), "{}", passed.text);
        assert!(lesson_html(&host).contains("targetQuestion"));
    }

    #[tokio::test]
    async fn hardens_the_payload() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let bads = [
            with_quiz(json!({"correctIndex": null})),
            with_quiz(json!({"correctAnswer": null})),
            with_quiz(json!({"correctAnswer": "a"})),
            node_in(json!({"ankiParams": {"targetQuestion": "Q?"}})),
            node_in(json!({"ankiParams": {"targetFact": "F"}})),
            node_in(json!({"ankiParams": {"targetQuestion": "Q?", "targetFact": "F", "suggestedCloze": "{{c1::x}}"}})),
            node_in(json!({"ankiParams": {"suggestedCloze": "no cloze here"}})),
            node_in(json!({"content": "<p>x</p><script>alert(1)</script>"})),
            node_in(json!({"motivation": "<iframe src=x></iframe>"})),
            node_in(json!({"connection": "<img src=x onerror=\"alert(1)\">"})),
            node_in(json!({"content": "<a href=\"javascript:alert(1)\">x</a>"})),
            with_quiz(json!({"explanation": "<script>x</script>"})),
            node_in(json!({"visuals": [{"type": "svg-inline", "content": "<svg onload=\"x()\"></svg>"}]})),
        ];
        for bad in &bads {
            let out = host.call(&TOOL, bad.clone()).await;
            assert!(out.is_error, "{} accepted: {}", bad, out.text);
            assert!(out.text.starts_with("Invalid input"), "{}: {}", bad, out.text);
        }
        let good = host
            .call(
                &TOOL,
                node_in(json!({
                    "ankiParams": {"suggestedCloze": "Il {{c2::ref}} è una scatola"},
                    "content": "<p>una <code>&lt;button&gt;</code> e un <strong>on</strong> click</p>",
                })),
            )
            .await;
        assert!(!good.is_error, "{}", good.text);
    }

    #[test]
    fn every_real_node_in_the_learners_lessons_passes_the_schema() {
        let root = crate::data_root::resolve_data_root(
            &crate::data_root::repo_root(),
            std::env::var("LEARNER_DATA_ROOT").ok().as_deref(),
            &crate::data_root::home_dir(),
        )
        .data_root;
        let courses = root.join("courses");
        if !courses.is_dir() {
            eprintln!("no courses in the data folder: skipped");
            return;
        }
        let mut checked = 0;
        for course in fs::read_dir(&courses).expect("read the courses") {
            let course = course.expect("a course entry");
            let lessons = course.path().join("lessons");
            if !course.path().is_dir() || !lessons.is_dir() {
                continue;
            }
            for entry in fs::read_dir(&lessons).expect("read the lessons") {
                let entry = entry.expect("a lesson entry");
                if !entry.file_name().to_string_lossy().ends_with(".html") {
                    continue;
                }
                let raw = fs::read_to_string(entry.path()).expect("read the lesson");
                let Some(doc) = parse_lesson_document(&raw).expect("parse the lesson") else {
                    continue;
                };
                for (i, node) in doc["nodes"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .enumerate()
                {
                    let mut node = node.clone();
                    let node_id = text(node.get("nodeId")).to_string();
                    // legacy fact-only cards (no question) never produced a card; new writes must
                    // give both, and the engine keeps only the question card
                    if let Some(anki) = node.get("ankiParams") {
                        let question = js::truthy(anki.get("targetQuestion"));
                        let fact = js::truthy(anki.get("targetFact"));
                        if question != fact {
                            remove_key(&mut node, "ankiParams");
                        }
                        if question
                            && let Some(anki) =
                                node.get_mut("ankiParams").and_then(Value::as_object_mut)
                        {
                            anki.shift_remove("suggestedCloze");
                        }
                    }
                    let input = json!({"planNode": format!("N{}", i + 1), "node": node});
                    assert!(
                        validate(&input).is_ok(),
                        "{} {node_id}: {:?}",
                        entry.path().display(),
                        validate(&input).err()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no node checked");
    }

    #[tokio::test]
    async fn supports_steps_and_derives_node_content() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let out = host
            .call(
                &TOOL,
                node_in(json!({
                    "content": null,
                    "steps": [
                        {"stepId": "s1", "title": "P1", "content": "Primo passo di spiegazione."},
                        {"stepId": "s2", "title": "P2", "content": "Secondo passo di spiegazione."},
                    ],
                })),
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.starts_with("{\"ok\":true,\"nodeCount\":1"), "{}", out.text);
        let item = host.items().last().cloned().expect("an item");
        assert_eq!(
            item["node"]["content"],
            json!("Primo passo di spiegazione.\n\nSecondo passo di spiegazione.")
        );
        assert_eq!(item["node"]["steps"].as_array().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn rejects_unsafe_html_inside_step_content() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let out = host
            .call(
                &TOOL,
                json!({"planNode": "N1", "node": {
                    "nodeId": "bad-step-node",
                    "title": "Nodo cattivo",
                    "tier": "derivation",
                    "steps": [
                        {"stepId": "s1", "content": "Valido"},
                        {"stepId": "s2", "content": "<script>alert(1)</script>"},
                    ],
                }}),
            )
            .await;
        assert!(out.is_error, "{}", out.text);
        assert!(out.text.contains("unsafe HTML rejected"), "{}", out.text);
    }

    #[tokio::test]
    async fn warns_when_plan_node_is_out_of_order() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let mut input = node_in(json!({}));
        input["planNode"] = json!("N2");
        let out = host.call(&TOOL, input).await;
        assert!(!out.is_error, "{}", out.text);
        assert!(
            out.text.starts_with("{\"ok\":true,\"nodeCount\":1,\"notes\":[\"warning: this is node 1 of the lesson but planNode is N2;"),
            "{}",
            out.text
        );
        assert!(out.text.contains("warning"), "{}", out.text);
        assert!(
            out.text.contains("this is node 1 of the lesson but planNode is N2"),
            "{}",
            out.text
        );
    }

    #[tokio::test]
    async fn validates_the_payload() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let mut bad_plan_node = node_in(json!({}));
        bad_plan_node["planNode"] = json!("3");
        let bads = [
            node_in(json!({"nodeId": "Bad Id"})),
            node_in(json!({"tier": "weird"})),
            node_in(json!({"content": ""})),
            with_quiz(json!({"correctIndex": 0})),
            with_quiz(json!({"correctIndex": 3})),
            with_quiz(json!({"misconceptions": [{"optionIndex": 3, "misconception": "m"}]})),
            bad_plan_node,
        ];
        for bad in &bads {
            let out = host.call(&TOOL, bad.clone()).await;
            assert!(out.is_error, "{} accepted: {}", bad, out.text);
            assert!(out.text.starts_with("Invalid input"), "{}: {}", bad, out.text);
        }
    }

    #[test]
    fn unsafe_html_only_flags_real_tags() {
        assert!(unsafe_html("<script>x</script>"));
        assert!(unsafe_html("<SCRIPT>x</SCRIPT>"));
        assert!(unsafe_html("<iframe src=x></iframe>"));
        // the content of pre/code is code, not markup: its tags are never inspected
        assert!(!unsafe_html("<pre><a onclick=\"x\">y</a></pre>"));
        assert!(!unsafe_html("<CODE><img src=x onerror=y></CODE>"));
        assert!(!unsafe_html("<prefix>onload=x</prefix>"));
        // prose and escaped samples
        assert!(!unsafe_html("x < y"));
        assert!(!unsafe_html("&lt;button onClick=&quot;x&quot;&gt;"));
        assert!(!unsafe_html("php e javascript: un URL"));
        assert!(unsafe_html("<a href=\"javascript:alert(1)\">x</a>"));
        assert!(unsafe_html("<img src=x onerror=y>"));
        assert!(unsafe_html("<img src=x onerror = y>"));
        assert!(unsafe_html("<img/src=x/onerror=y>"));
        assert!(!unsafe_html("<img src=x data-on=y>"));
        // malformed HTML is text: an unclosed pre leaves the rest as it is, an unterminated tag
        // never matches
        assert!(!unsafe_html("<"));
        assert!(!unsafe_html("<>"));
        assert!(!unsafe_html("<pre>unclosed onerror=x"));
        assert!(!unsafe_html("<img \"unterminated>onerror=y>"));
        assert!(!unsafe_html("<pre>x</pre>"));
        assert!(unsafe_html("<pre>x</pre><img onerror=y>"));
    }

    #[test]
    fn validate_drops_unknown_keys_and_rebuilds_in_schema_order() {
        let input = json!({
            "extra": 1,
            "node": {
                "bogus": true,
                "content": "<p>x</p>",
                "tier": "axiom",
                "title": "T",
                "nodeId": "a",
                "steps": [{"content": "c", "extra": 1, "stepId": "s1"}],
            },
            "planNode": "N1",
        });
        let out = validate(&input).expect("a valid node");
        assert_eq!(
            serde_json::to_string(&out).expect("one line"),
            r#"{"planNode":"N1","node":{"nodeId":"a","title":"T","tier":"axiom","content":"<p>x</p>","steps":[{"stepId":"s1","content":"c"}]}}"#
        );
    }

    #[test]
    fn validate_reports_every_issue_that_zod_reports() {
        let err = validate(&json!({})).expect_err("both keys are required");
        assert_eq!(
            err,
            [
                "planNode: Invalid input: expected string, received undefined",
                "node: Invalid input: expected object, received undefined",
            ]
        );

        let err = validate(&json!({"planNode": "N1", "node": {"nodeId": "a", "title": "", "tier": "weird"}}))
            .expect_err("bad tier");
        assert_eq!(
            err,
            [
                "node.title: Too small: expected string to have >=1 characters",
                "node.tier: Invalid option: expected one of \"unconditional-truth\"|\"axiom\"|\"derivation\"",
            ]
        );

        // the content check is a superRefine: it does not run after a type error
        let err = validate(&json!({"planNode": "N1", "node": {"nodeId": "a", "title": "t", "tier": 5, "content": ""}}))
            .expect_err("bad tier");
        assert_eq!(
            err,
            ["node.tier: Invalid option: expected one of \"unconditional-truth\"|\"axiom\"|\"derivation\""]
        );
    }
}
