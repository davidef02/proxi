//! Lesson island and Sintesi parsing (extensions/html-lesson.ts reader half).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::js;

/// Exact opening tag of a lesson data island (case-sensitive, like the Node reader).
pub const LESSON_OPEN: &str = r#"<script type="application/json" id="lesson-data">"#;
/// Exact opening tag of a reference-sheet data island.
pub const REFERENCE_OPEN: &str = r#"<script type="application/json" id="reference-data">"#;

const MALFORMED: &str = "Malformed #lesson-data island";

pub fn parse_lesson_document(raw: &str) -> Result<Option<Value>, String> {
    let Some(start) = raw.find(LESSON_OPEN) else {
        return Ok(None);
    };
    let rest = &raw[start + LESSON_OPEN.len()..];
    let Some(end) = rest.find("</script>") else {
        return Ok(None);
    };
    match js::parse_json(js::js_trim(&rest[..end])) {
        // An island that parses to a falsy value is no document (Node's callers test `!doc`).
        Ok(doc) if js::truthy(Some(&doc)) => Ok(Some(doc)),
        Ok(_) => Ok(None),
        Err(e) => Err(format!("{MALFORMED}: {e}")),
    }
}

pub fn parse_reference_document(raw: &str) -> Option<Value> {
    let start = raw.find(REFERENCE_OPEN)?;
    let rest = &raw[start + REFERENCE_OPEN.len()..];
    let end = rest.find("</script>")?;
    js::parse_json(&rest[..end]).ok()
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    while let Some(c) = s[i..].chars().next() {
        if js::is_js_ws(c) {
            i += c.len_utf8();
        } else {
            break;
        }
    }
    i
}

fn match_ci_at(s: &str, i: usize, needle: &str) -> bool {
    let hay = &s.as_bytes()[i..];
    let want = needle.as_bytes();
    hay.len() >= want.len() && hay[..want.len()].iter().zip(want).all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// `/<strong>\s*La verit[àa] da tenere senza caveat\.?\s*<\/strong>\s*([^<]+)/i`, hand written.
fn find_caveat_capture(s: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(pos) = js::find_ci(s, "<strong>", from) {
        let after = pos + "<strong>".len();
        if let Some(capture) = caveat_after_strong(s, skip_ws(s, after)) {
            return Some(capture);
        }
        from = after;
    }
    None
}

fn caveat_after_strong(s: &str, start: usize) -> Option<&str> {
    let mut i = start;
    if !match_ci_at(s, i, "la verit") {
        return None;
    }
    i += "la verit".len();
    let accent = s[i..].chars().next()?;
    if !matches!(accent, 'à' | 'À' | 'a' | 'A') {
        return None;
    }
    i += accent.len_utf8();
    if !match_ci_at(s, i, " da tenere senza caveat") {
        return None;
    }
    i += " da tenere senza caveat".len();
    if s[i..].starts_with('.') {
        i += 1;
    }
    i = skip_ws(s, i);
    if !match_ci_at(s, i, "</strong>") {
        return None;
    }
    i += "</strong>".len();
    // `([^<]+)` starts right after the tag, leading whitespace included.
    let rest = &s[i..];
    let end = rest.find('<').unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(&rest[..end])
}

fn steps_of(node: &Value) -> Option<&Vec<Value>> {
    node.get("steps").and_then(Value::as_array)
}

fn caveat_source<'a>(node: &'a Value) -> Option<&'a str> {
    let content = node.get("content").and_then(Value::as_str).unwrap_or("");
    if let Some(capture) = find_caveat_capture(content) {
        return Some(capture);
    }
    // Only when the node's own content has no match, like the Node regex order.
    let step = steps_of(node).and_then(|steps| steps.first())?;
    let step_content = step.get("content").and_then(Value::as_str).unwrap_or("");
    find_caveat_capture(step_content)
}

/// Inner text of the first `<p>` (case-insensitive), like `/<p>([\s\S]*?)<\/p>/i`.
fn p_inner(s: &str) -> Option<&str> {
    let open = js::find_ci(s, "<p>", 0)? + "<p>".len();
    let rest = &s[open..];
    let end = js::find_ci(rest, "</p>", 0)?;
    Some(&rest[..end])
}

pub fn extract_distilled_claim(node: &Value) -> Option<Value> {
    if let Some(summary) = node.get("summary").and_then(Value::as_str) {
        let trimmed = js::js_trim(summary);
        if !trimmed.is_empty() {
            return Some(Value::String(trimmed.to_string()));
        }
    }
    let target = node
        .get("ankiParams")
        .and_then(|p| p.get("targetFact"))
        .and_then(Value::as_str);
    if let Some(fact) = target {
        let trimmed = js::js_trim(fact);
        if !trimmed.is_empty() {
            return Some(Value::String(trimmed.to_string()));
        }
    }

    if let Some(capture) = caveat_source(node) {
        let trimmed = js::js_trim(capture);
        if !trimmed.is_empty() {
            return Some(Value::String(trimmed.to_string()));
        }
        // A whitespace-only caveat capture skips the steps[0] caveat search and falls through to
        // the paragraph path.
    }

    let content = node.get("content").and_then(Value::as_str).unwrap_or("");
    let step_content = steps_of(node)
        .and_then(|steps| steps.first())
        .and_then(|step| step.get("content"))
        .and_then(Value::as_str);
    let raw_source = match step_content {
        Some(text) if !js::js_trim(text).is_empty() => text,
        _ => content,
    };

    let raw_text = p_inner(raw_source).unwrap_or(raw_source);
    if !js::js_trim(raw_text).is_empty() {
        let text = js::js_trim(&js::collapse_ws(&js::strip_tags(raw_text))).to_string();
        let sentences = js::split_sentences(&text);
        if !sentences[0].is_empty() {
            let take = sentences.len().min(2);
            return Some(Value::String(js::js_trim(&sentences[..take].join(" ")).to_string()));
        }
        let short: String = text.chars().take(200).collect();
        return Some(Value::String(js::js_trim(&short).to_string()));
    }

    node.get("title").cloned()
}

struct CodeMatch<'a> {
    language: Option<&'a str>,
    code: &'a str,
}

/// `/<pre><code(?:\s+class="language-([^"]+)")?>([\s\S]*?)<\/code><\/pre>/i`, hand written.
fn code_match(content: &str) -> Option<CodeMatch<'_>> {
    let mut from = 0;
    while let Some(pos) = js::find_ci(content, "<pre><code", from) {
        let after = pos + "<pre><code".len();
        if let Some((language, body_start)) = parse_code_open(content, after) {
            let rest = &content[body_start..];
            if let Some(end) = js::find_ci(rest, "</code></pre>", 0) {
                return Some(CodeMatch {
                    language,
                    code: &rest[..end],
                });
            }
        }
        from = after;
    }
    None
}

fn parse_code_open(s: &str, start: usize) -> Option<(Option<&str>, usize)> {
    let mut i = start;
    if s[i..].starts_with('>') {
        return Some((None, i + 1));
    }
    let ws_start = i;
    i = skip_ws(s, i);
    if i == ws_start || !match_ci_at(s, i, "class=\"language-") {
        return None;
    }
    i += "class=\"language-".len();
    let rest = &s[i..];
    let end = rest.find('"')?;
    if end == 0 {
        return None;
    }
    let language = &rest[..end];
    i += end + 1;
    if !s[i..].starts_with('>') {
        return None;
    }
    Some((Some(language), i + 1))
}

/// `/```(\w+)?\n([\s\S]*?)```/`, hand written. Language is empty when the fence has none.
fn fence_match(content: &str) -> Option<(&str, &str)> {
    let mut from = 0;
    while let Some(pos) = content[from..].find("```") {
        let start = from + pos;
        let mut i = start + 3;
        let run_start = i;
        while i < content.len() && js::is_word_byte(content.as_bytes()[i]) {
            i += 1;
        }
        let language = &content[run_start..i];
        if content[i..].starts_with('\n') {
            let body_start = i + 1;
            if let Some(end) = content[body_start..].find("```") {
                return Some((language, &content[body_start..body_start + end]));
            }
        }
        // The regex retries one char later: a four-backtick opening starts the fence at index 1.
        from = start + 1;
    }
    None
}

fn unescape_code(raw: &str) -> String {
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
}

fn key_code(language: &str, raw: &str) -> Value {
    let lines: Vec<&str> = raw.split('\n').collect();
    let truncated = if lines.len() > 12 {
        format!("{}\n// ...", lines[..10].join("\n"))
    } else {
        raw.to_string()
    };
    let mut map = Map::new();
    map.insert("language".into(), Value::String(language.to_string()));
    map.insert("code".into(), Value::String(js::js_trim(&truncated).to_string()));
    Value::Object(map)
}

fn snippet_from_content(content: &str) -> Option<Value> {
    if let Some(found) = code_match(content) {
        let language = found.language.unwrap_or("typescript");
        return Some(key_code(language, &unescape_code(found.code)));
    }
    if let Some((language, code)) = fence_match(content) {
        let language = if language.is_empty() { "typescript" } else { language };
        return Some(key_code(language, code));
    }
    None
}

pub fn extract_key_snippet(node: &Value) -> (Option<Value>, Option<Value>) {
    let mut formula = node
        .get("math")
        .and_then(Value::as_array)
        .and_then(|math| math.first())
        .cloned();
    if formula.is_none() {
        if let Some(steps) = steps_of(node).filter(|steps| !steps.is_empty()) {
            for step in steps {
                if let Some(m) = step.get("math").and_then(Value::as_array).and_then(|m| m.first()) {
                    formula = Some(m.clone());
                    break;
                }
            }
        }
    }

    let mut code = node
        .get("codeBlocks")
        .and_then(Value::as_array)
        .filter(|blocks| !blocks.is_empty())
        .map(|blocks| blocks[0].clone());
    if code.is_none() {
        match steps_of(node) {
            // Steps exist: only their code blocks count, never the content's markup.
            Some(steps) if !steps.is_empty() => {
                for step in steps {
                    if let Some(block) = step
                        .get("codeBlocks")
                        .and_then(Value::as_array)
                        .and_then(|blocks| blocks.first())
                    {
                        code = Some(block.clone());
                        break;
                    }
                }
            }
            _ => {
                let content = node.get("content").and_then(Value::as_str).unwrap_or("");
                code = snippet_from_content(content);
            }
        }
    }

    (formula, code)
}

fn is_record_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 8
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[4] == b'-'
        && name.ends_with(".md")
}

fn has_lesson_marker(content: &str, slug: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = content[from..].find("<!--") {
        let start = from + pos;
        let mut i = skip_ws(content, start + 4);
        if content[i..].starts_with("lesson:") {
            i = skip_ws(content, i + "lesson:".len());
            if content[i..].starts_with(slug) {
                i = skip_ws(content, i + slug.len());
                if content[i..].starts_with("-->") {
                    return true;
                }
            }
        }
        from = start + 4;
    }
    false
}

pub fn find_learning_record_for_lesson(lesson_path: &Path, slug: &str) -> Option<PathBuf> {
    let parent = lesson_path.parent()?;
    let rec_dir = crate::data_root::resolve_path(&parent.join("..").join("learning-records"));
    if !rec_dir.exists() {
        return None;
    }
    let mut files: Vec<String> = fs::read_dir(&rec_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| is_record_name(name))
        .collect();
    files.sort();

    for file in &files {
        let content = fs::read(rec_dir.join(file))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        if has_lesson_marker(&content, slug) {
            return Some(rec_dir.join(file));
        }
    }

    let base = lesson_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let bytes = base.as_bytes();
    if bytes.len() >= 5 && bytes[..4].iter().all(|b| b.is_ascii_digit()) && bytes[4] == b'-' {
        let prefix = &base[..5];
        if let Some(hit) = files.iter().find(|file| file.starts_with(prefix)) {
            return Some(rec_dir.join(hit));
        }
    }
    None
}

fn evidence_section(norm: &str) -> Option<&str> {
    let needle = "## Evidence";
    let bytes = norm.as_bytes();
    let mut from = 0;
    while let Some(pos) = norm[from..].find(needle) {
        let start = from + pos;
        from = start + 1;
        if start != 0 && bytes[start - 1] != b'\n' {
            continue;
        }
        let after = start + needle.len();
        let ws_end = skip_ws(norm, after);
        let run = &norm[after..ws_end];
        let Some(last_newline) = run.rfind('\n') else {
            continue;
        };
        // Capture starts after the last newline of the whitespace run, like `\s*\n` backtracking.
        let section_start = after + last_newline + 1;
        let rest = &norm[section_start..];
        let mut end = rest.len();
        if let Some(i) = rest.find("\n##") {
            end = end.min(i);
        }
        if let Some(i) = rest.find("\n<!--") {
            end = end.min(i);
        }
        return Some(js::js_trim(&rest[..end]));
    }
    None
}

fn source_node(desc: &str) -> Option<String> {
    let bytes = desc.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let word = &bytes[i..i + 4];
        if word.eq_ignore_ascii_case(b"nodo") || word.eq_ignore_ascii_case(b"node") {
            let mut j = i + 4;
            let ws_start = j;
            while j < bytes.len() {
                let c = desc[j..].chars().next()?;
                if js::is_js_ws(c) {
                    j += c.len_utf8();
                } else {
                    break;
                }
            }
            if j > ws_start {
                let run_start = j;
                let mut k = j;
                while k < bytes.len() && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > run_start {
                    return Some(desc[run_start..k].to_string());
                }
                k = run_start;
                while k < bytes.len() && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'-') {
                    k += 1;
                }
                if k > run_start {
                    return Some(desc[run_start..k].to_string());
                }
            }
        }
        i += 1;
    }
    None
}

fn misconception_item(desc: &str) -> Value {
    let mut map = Map::new();
    map.insert("description".into(), Value::String(desc.to_string()));
    if let Some(node) = source_node(desc) {
        map.insert("sourceNode".into(), Value::String(format!("Nodo {node}")));
    }
    Value::Object(map)
}

fn strip_bullet(line: &str) -> &str {
    let mut i = 1;
    let mut moved = false;
    while let Some(c) = line[i..].chars().next() {
        if js::is_js_ws(c) {
            i += c.len_utf8();
            moved = true;
        } else {
            break;
        }
    }
    if moved { &line[i..] } else { line }
}

const MISCONCEPTION_HINTS: [&str; 11] = [
    "errore",
    "difficolt",
    "scambia",
    "confond",
    "buchi",
    "slip",
    "sbagli",
    "attento",
    "non colleg",
    "contato",
    "non risposto",
];

pub fn extract_misconceptions_from_record(md: &str) -> Vec<Value> {
    let norm = md.replace("\r\n", "\n");
    let Some(text) = evidence_section(&norm) else {
        return Vec::new();
    };

    let lines: Vec<&str> = text.split('\n').map(js::js_trim).collect();
    let bullets: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.starts_with("- ") || line.starts_with("* "))
        .collect();
    if !bullets.is_empty() {
        return bullets
            .into_iter()
            .map(|line| misconception_item(strip_bullet(line)))
            .collect();
    }

    let sentences = js::split_sentences(&js::strip_tags(text));
    let mut out = Vec::new();
    for sentence in &sentences {
        let clean = js::js_trim(&js::collapse_ws(sentence)).to_string();
        let lower = clean.to_lowercase();
        if MISCONCEPTION_HINTS.iter().any(|hint| lower.contains(hint)) {
            out.push(misconception_item(&clean));
        }
    }
    if out.is_empty() && !sentences.is_empty() {
        let first = js::js_trim(&js::collapse_ws(&sentences[0])).to_string();
        out.push(misconception_item(&first));
    }
    out
}

fn formula(math: &Value, title: Option<&Value>) -> Value {
    let mut map = Map::new();
    map.insert("math".into(), math.clone());
    if let Some(title) = title {
        map.insert("sourceNode".into(), title.clone());
    }
    Value::Object(map)
}

fn read_record(path: &Path) -> Result<String, String> {
    if path.is_dir() {
        // Node's readFileSync text for a directory.
        return Err("EISDIR: illegal operation on a directory, read".to_string());
    }
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|error| error.to_string())
}

pub fn build_sintesi_document(doc: &Value, lesson_path: &Path) -> Result<Value, String> {
    build_sintesi_document_with_record(doc, lesson_path, None)
}

/// `buildSintesiDocument` with Node's `learningRecordPath`: an existing, non-empty read wins
/// over the sibling `learning-records/` scan.
pub fn build_sintesi_document_with_record(
    doc: &Value,
    lesson_path: &Path,
    record_path: Option<&str>,
) -> Result<Value, String> {
    let metadata = doc.get("metadata").filter(|value| value.is_object());
    let nodes = doc.get("nodes").and_then(Value::as_array);
    let (Some(metadata), Some(nodes)) = (metadata, nodes) else {
        return Err(format!("{MALFORMED}: metadata and nodes are required"));
    };

    let slug = js::js_string(metadata.get("slug"));
    let record = record_path
        .filter(|path| !path.is_empty())
        .filter(|path| Path::new(path).exists())
        .map(|path| read_record(Path::new(path)))
        .transpose()?
        .filter(|text| !text.is_empty())
        .or_else(|| {
            find_learning_record_for_lesson(lesson_path, &slug)
                .and_then(|path| fs::read(path).ok())
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .filter(|text| !text.is_empty())
        });
    let misconceptions = record
        .as_deref()
        .map(extract_misconceptions_from_record)
        .unwrap_or_default();

    let mut sintesi_nodes = Vec::with_capacity(nodes.len());
    let mut formulas = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let mut map = Map::new();
        if let Some(value) = node.get("nodeId") {
            map.insert("nodeId".into(), value.clone());
        }
        map.insert("number".into(), Value::from(index + 1));
        for key in ["title", "tier"] {
            if let Some(value) = node.get(key) {
                map.insert(key.into(), value.clone());
            }
        }
        if let Some(value) = extract_distilled_claim(node) {
            map.insert("summary".into(), value);
        }
        let (key_formula, key_code) = extract_key_snippet(node);
        if let Some(value) = key_formula {
            map.insert("keyFormula".into(), value);
        }
        if let Some(value) = key_code {
            map.insert("keyCode".into(), value);
        }
        sintesi_nodes.push(Value::Object(map));

        let title = node.get("title");
        if let Some(math) = node.get("math").and_then(Value::as_array) {
            for m in math {
                formulas.push(formula(m, title));
            }
        }
        if let Some(steps) = steps_of(node) {
            for step in steps {
                if let Some(math) = step.get("math").and_then(Value::as_array) {
                    for m in math {
                        formulas.push(formula(m, title));
                    }
                }
            }
        }
    }

    let mut out = Map::new();
    let title = metadata
        .get("title")
        .filter(|value| js::truthy(Some(*value)))
        .or_else(|| metadata.get("topic"));
    if let Some(value) = title {
        out.insert("title".into(), value.clone());
    }
    for key in ["topic", "slug", "initialDag"] {
        if let Some(value) = metadata.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    out.insert("nodes".into(), Value::Array(sintesi_nodes));
    out.insert("formulas".into(), Value::Array(formulas));
    out.insert("misconceptions".into(), Value::Array(misconceptions));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_lesson_document_trims_and_keeps_key_order() {
        let raw = "<html><script type=\"application/json\" id=\"lesson-data\">\n  {\"b\":1.0,\"a\":[2e2]}  \n</script></html>";
        let doc = parse_lesson_document(raw).unwrap().expect("island");
        assert_eq!(
            serde_json::to_string(&doc).unwrap(),
            "{\"b\":1,\"a\":[200]}"
        );
        assert!(parse_lesson_document("<html></html>").unwrap().is_none());
        assert!(
            parse_lesson_document("<script type=\"application/json\" id=\"lesson-data\">{}</script")
                .unwrap()
                .is_none()
        );
        assert!(parse_lesson_document("<script type=\"application/json\" id=\"lesson-data\">null</script>")
            .unwrap()
            .is_none());
    }

    #[test]
    fn parse_lesson_document_reports_malformed_islands() {
        let err = parse_lesson_document("<script type=\"application/json\" id=\"lesson-data\">{nope</script>")
            .unwrap_err();
        assert!(err.starts_with("Malformed #lesson-data island:"), "{err}");
    }

    #[test]
    fn parse_reference_document_reads_an_island_or_nothing() {
        let raw = r#"<html><script type="application/json" id="reference-data">{"title":"x","n":1.0}</script></html>"#;
        let doc = parse_reference_document(raw).expect("island");
        assert_eq!(serde_json::to_string(&doc).unwrap(), "{\"title\":\"x\",\"n\":1}");
        assert!(parse_reference_document("<html></html>").is_none());
        assert!(
            parse_reference_document(
                "<script type=\"application/json\" id=\"reference-data\">{nope</script>"
            )
            .is_none()
        );
    }

    #[test]
    fn distilled_claim_prefers_summary_fact_and_caveat() {
        let steps = json!([{ "stepId": "step-1", "content": "La prima affermazione fondamentale del nodo. Questa è la seconda frase esplicativa. E una terza frase non inclusa." }]);
        let step_node = json!({ "title": "Titolo del Nodo", "content": "", "steps": steps });
        assert_eq!(
            extract_distilled_claim(&step_node).unwrap(),
            json!("La prima affermazione fondamentale del nodo. Questa è la seconda frase esplicativa.")
        );

        let summary = json!({
            "title": "Titolo",
            "content": "",
            "summary": "Sintesi esplicita dell'insegnante.",
            "steps": [{ "stepId": "s1", "content": "Contenuto del passo 1." }]
        });
        assert_eq!(
            extract_distilled_claim(&summary).unwrap(),
            json!("Sintesi esplicita dell'insegnante.")
        );

        let fact = json!({
            "title": "Titolo",
            "content": "",
            "ankiParams": { "targetFact": "Fatto chiave memorizzabile." },
            "steps": [{ "stepId": "s1", "content": "Contenuto del passo 1." }]
        });
        assert_eq!(
            extract_distilled_claim(&fact).unwrap(),
            json!("Fatto chiave memorizzabile.")
        );

        let caveat = json!({
            "title": "Titolo",
            "content": "",
            "steps": [{ "stepId": "s1", "content": "<strong>La verità da tenere senza caveat.</strong> Verità estratta dal caveat." }]
        });
        assert_eq!(
            extract_distilled_claim(&caveat).unwrap(),
            json!("Verità estratta dal caveat.")
        );
    }

    #[test]
    fn distilled_claim_handles_accented_caveats_and_the_title_fallback() {
        let accented = json!({
            "content": "<STRONG>LA VERITÀ DA TENERE SENZA CAVEAT</STRONG>\n À la carte."
        });
        assert_eq!(extract_distilled_claim(&accented).unwrap(), json!("À la carte."));

        let no_accent = json!({ "content": "<strong>La verita da tenere senza caveat.</strong> Verita senza accento." });
        assert_eq!(
            extract_distilled_claim(&no_accent).unwrap(),
            json!("Verita senza accento.")
        );

        let titled = json!({ "title": "Solo titolo", "content": "" });
        assert_eq!(extract_distilled_claim(&titled).unwrap(), json!("Solo titolo"));
        assert!(extract_distilled_claim(&json!({ "content": "" })).is_none());
    }

    #[test]
    fn key_snippet_reads_math_from_node_then_steps() {
        let (formula, code) = extract_key_snippet(&json!({ "math": ["a^2"] }));
        assert_eq!(formula.unwrap(), json!("a^2"));
        assert!(code.is_none());

        let (formula, code) = extract_key_snippet(&json!({ "steps": [{ "content": "x", "math": ["b"] }] }));
        assert_eq!(formula.unwrap(), json!("b"));
        assert!(code.is_none());
    }

    #[test]
    fn key_snippet_keeps_code_blocks_verbatim() {
        let block = json!({ "language": "js", "code": "1", "caption": "c" });
        let (formula, code) = extract_key_snippet(&json!({ "codeBlocks": [block] }));
        assert!(formula.is_none());
        assert_eq!(
            serde_json::to_string(&code.unwrap()).unwrap(),
            "{\"language\":\"js\",\"code\":\"1\",\"caption\":\"c\"}"
        );
    }

    #[test]
    fn key_snippet_reads_pre_code_and_truncates_twelve_lines() {
        let lines = [
            "l1 &lt;b&gt;", "l2", "l3", "l4", "l5", "l6", "l7", "l8", "l9", "l10", "l11", "l12", "l13",
        ];
        let content = format!(
            "<p>x</p><pre><code class=\"language-python\">{}</code></pre>",
            lines.join("\n")
        );
        let (_, code) = extract_key_snippet(&json!({ "content": content }));
        let code = code.expect("code");
        assert_eq!(code["language"], json!("python"));
        assert_eq!(
            code["code"],
            json!("l1 <b>\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n// ...")
        );

        let (_, plain) = extract_key_snippet(&json!({ "content": "<pre><code>let x = 1;</code></pre>" }));
        let plain = plain.expect("code");
        assert_eq!(plain["language"], json!("typescript"));
        assert_eq!(plain["code"], json!("let x = 1;"));
    }

    #[test]
    fn key_snippet_reads_fenced_code() {
        let (_, code) = extract_key_snippet(&json!({ "content": "Testo\n```rust\nfn x() {}\n```" }));
        let code = code.expect("code");
        assert_eq!(code["language"], json!("rust"));
        assert_eq!(code["code"], json!("fn x() {}"));

        let (_, plain) = extract_key_snippet(&json!({ "content": "```\nplain\n```" }));
        let plain = plain.expect("code");
        assert_eq!(plain["language"], json!("typescript"));
        assert_eq!(plain["code"], json!("plain"));

        // Four backticks: the regex retries one char later, so the fence opens at index 1.
        let (_, four) = extract_key_snippet(&json!({ "content": "````js\nfn x() {}\n```" }));
        let four = four.expect("code");
        assert_eq!(four["language"], json!("js"));
        assert_eq!(four["code"], json!("fn x() {}"));
    }

    #[test]
    fn misconceptions_read_bullets_with_their_node() {
        let md = "# x\r\n\r\n## Evidence\r\n\r\n- Confonde il Nodo 2 con il 3\r\n* Slip sul node abc-1\r\n\r\n<!-- lesson: seconda -->\r\n";
        let items = extract_misconceptions_from_record(md);
        assert_eq!(items.len(), 2);
        assert_eq!(
            serde_json::to_string(&items[0]).unwrap(),
            "{\"description\":\"Confonde il Nodo 2 con il 3\",\"sourceNode\":\"Nodo 2\"}"
        );
        assert_eq!(items[1]["description"], json!("Slip sul node abc-1"));
        assert_eq!(items[1]["sourceNode"], json!("Nodo abc-1"));
    }

    #[test]
    fn misconceptions_fall_back_to_sentences() {
        let md = "# y\n\n## Evidence\n\nTutto bene. Un errore sul segno. Fine\n\n## Next\n";
        let items = extract_misconceptions_from_record(md);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["description"], json!("Un errore sul segno."));

        let plain = "# y\n\n## Evidence\n\nTutto bene. Fine\n";
        let items = extract_misconceptions_from_record(plain);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["description"], json!("Tutto bene."));

        assert!(extract_misconceptions_from_record("# y\n\n## Next\n").is_empty());
    }

    #[test]
    fn sintesi_document_key_order_and_omitted_keys() {
        let doc = json!({
            "metadata": { "topic": "T" },
            "nodes": [{
                "nodeId": "n1",
                "title": "N",
                "tier": "axiom",
                "content": "<p>Prima. Seconda.</p>",
                "math": ["m1"]
            }]
        });
        let out = build_sintesi_document(&doc, Path::new("none/lessons/0001-x.html")).unwrap();
        assert_eq!(
            serde_json::to_string(&out).unwrap(),
            r#"{"title":"T","topic":"T","nodes":[{"nodeId":"n1","number":1,"title":"N","tier":"axiom","summary":"Prima. Seconda.","keyFormula":"m1"}],"formulas":[{"math":"m1","sourceNode":"N"}],"misconceptions":[]}"#
        );

        let full = json!({
            "metadata": { "title": "Titolo", "topic": "T", "slug": "s", "initialDag": "graph TD" },
            "nodes": [{ "content": "" }]
        });
        let out = build_sintesi_document(&full, Path::new("none/lessons/0001-x.html")).unwrap();
        assert_eq!(
            serde_json::to_string(&out).unwrap(),
            r#"{"title":"Titolo","topic":"T","slug":"s","initialDag":"graph TD","nodes":[{"number":1}],"formulas":[],"misconceptions":[]}"#
        );
    }

    #[test]
    fn sintesi_document_requires_metadata_and_nodes() {
        let err = build_sintesi_document(&json!({ "nodes": [] }), Path::new("x.html")).unwrap_err();
        assert!(err.starts_with("Malformed #lesson-data island:"), "{err}");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("proxi-html-lesson-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record_fixture(dir: &Path, lesson_name: &str) -> PathBuf {
        fs::create_dir_all(dir.join("lessons")).unwrap();
        fs::create_dir_all(dir.join("learning-records")).unwrap();
        let lesson = dir.join("lessons").join(lesson_name);
        fs::write(&lesson, "").unwrap();
        fs::write(
            dir.join("learning-records").join("0004-a.md"),
            "## Evidence\n- Nodo 1: scambia A\n",
        )
        .unwrap();
        lesson
    }

    #[test]
    fn sintesi_record_path_wins_over_sibling_scan() {
        let dir = temp_dir("record-wins");
        let lesson = record_fixture(&dir, "0004-x.html");
        let override_path = dir.join("override.md");
        fs::write(&override_path, "## Evidence\n- Nodo 2: confonde B\n").unwrap();

        let doc = json!({ "metadata": { "slug": "x" }, "nodes": [] });
        let out =
            build_sintesi_document_with_record(&doc, &lesson, Some(override_path.to_str().unwrap()))
                .unwrap();
        assert_eq!(
            serde_json::to_string(&out["misconceptions"]).unwrap(),
            r#"[{"description":"Nodo 2: confonde B","sourceNode":"Nodo 2"}]"#
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sintesi_missing_or_empty_record_path_falls_back_to_scan() {
        let dir = temp_dir("record-fallback");
        let lesson = record_fixture(&dir, "0004-x.html");
        let empty_path = dir.join("empty.md");
        fs::write(&empty_path, "").unwrap();

        let doc = json!({ "metadata": { "slug": "x" }, "nodes": [] });
        let expected = r#"[{"description":"Nodo 1: scambia A","sourceNode":"Nodo 1"}]"#;
        for path in [dir.join("missing.md"), empty_path] {
            let out =
                build_sintesi_document_with_record(&doc, &lesson, Some(path.to_str().unwrap()))
                    .unwrap();
            assert_eq!(
                serde_json::to_string(&out["misconceptions"]).unwrap(),
                expected
            );
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sintesi_record_path_directory_is_eisdir() {
        let dir = temp_dir("record-dir");
        let doc = json!({ "metadata": { "slug": "x" }, "nodes": [] });
        let lesson = dir.join("lessons").join("0004-x.html");
        let err =
            build_sintesi_document_with_record(&doc, &lesson, Some(dir.to_str().unwrap()))
                .unwrap_err();
        assert_eq!(err, "EISDIR: illegal operation on a directory, read");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sintesi_none_record_matches_build_sintesi_document() {
        let dir = temp_dir("record-none");
        let lesson = record_fixture(&dir, "0004-x.html");
        let doc = json!({
            "metadata": { "slug": "x", "topic": "T" },
            "nodes": [{ "title": "N", "content": "<p>Prima. Seconda.</p>" }]
        });
        assert_eq!(
            build_sintesi_document_with_record(&doc, &lesson, None).unwrap(),
            build_sintesi_document(&doc, &lesson).unwrap()
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
