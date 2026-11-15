//! The `read_lesson` tool (port of `app/server/tools/lessontext.ts`).

use serde_json::{Map, Value};

use crate::graph::extract::html_to_text;
use crate::js;
use crate::lesson::artifact;
use crate::sessions::issue;
use crate::tools::util::fail;
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolOutcome};

/// Reading a whole artifact is the biggest token sink of a session; this is the cap on the fallback
/// (prose) path.
const MAX_PROSE: usize = 40_000;

pub const TOOL: ToolDef = ToolDef {
    name: "read_lesson",
    description: "Read a lesson artifact as compact text: per node its title, tier, prose, code, connection, check question and card. Pass nodeIds to read only those nodes. Use this instead of Read on lesson HTML.",
    schema,
    validate,
    run,
    writes: false,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("schemas/read_lesson.json")).expect("a valid schema file")
}

/// One node as compact text (Node's `nodeText`): the header, then every non-empty part.
fn node_text(node: &Value) -> String {
    let mut parts = vec![format!(
        "## {}: {} ({})",
        js::js_string(node.get("nodeId")),
        js::js_string(node.get("title")),
        js::js_string(node.get("tier")),
    )];
    if js::truthy(node.get("motivation")) {
        parts.push(format!(
            "Perché: {}",
            html_to_text(&js::js_string(node.get("motivation")))
        ));
    }
    let content = match node.get("content") {
        None | Some(Value::Null) => String::new(),
        value => js::js_string(value),
    };
    parts.push(html_to_text(&content));
    if let Some(items) = node.get("math").and_then(Value::as_array) {
        for item in items {
            parts.push(format!("$${}$$", js::js_string(Some(item))));
        }
    }
    if let Some(blocks) = node.get("codeBlocks").and_then(Value::as_array) {
        for block in blocks {
            let caption = match block.get("caption") {
                Some(caption) if js::truthy(Some(caption)) => {
                    format!("{}\n", js::js_string(Some(caption)))
                }
                _ => String::new(),
            };
            parts.push(format!(
                "{caption}```{}\n{}\n```",
                js::js_string(block.get("language")),
                js::js_string(block.get("code")),
            ));
        }
    }
    if js::truthy(node.get("connection")) {
        parts.push(format!(
            "Collegamento: {}",
            html_to_text(&js::js_string(node.get("connection")))
        ));
    }
    if js::truthy(node.get("quiz")) {
        let quiz = node.get("quiz").unwrap_or(&Value::Null);
        let right = match quiz.get("correctAnswer") {
            Some(Value::Null) | None => quiz_correct_option(quiz),
            Some(value) => Some(value),
        };
        let mut line = format!("Verifica: {}", html_to_text(&js::js_string(quiz.get("question"))));
        if js::truthy(right) {
            line.push_str(&format!(
                "\nRisposta: {}",
                html_to_text(&js::js_string(right))
            ));
        }
        parts.push(line);
    }
    let anki = node.get("ankiParams");
    let card = anki.and_then(|params| match params.get("targetQuestion") {
        Some(Value::Null) | None => params.get("suggestedCloze"),
        Some(value) => Some(value),
    });
    if js::truthy(card) {
        parts.push(format!("Carta Anki: {}", js::js_string(card)));
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `quiz.options[quiz.correctIndex - 1]`, 1-based; `undefined` when the index names no option.
fn quiz_correct_option(quiz: &Value) -> Option<&Value> {
    let index = quiz.get("correctIndex")?;
    let options = quiz.get("options")?.as_array()?;
    let index = js::to_number(Some(index));
    if !index.is_finite() || index.fract() != 0.0 || index < 1.0 {
        return None;
    }
    options.get((index - 1.0) as usize)
}

/// `/<body[^>]*>([\s\S]*)<\/body>/i` capture, or the whole document when it does not match.
fn body_inner(raw: &str) -> String {
    let Some(open) = js::find_ci(raw, "<body", 0) else {
        return raw.to_string();
    };
    let Some(gt) = raw[open..].find('>') else {
        return raw.to_string();
    };
    let body = &raw[open + gt + 1..];
    let Some(close) = last_ci(body, "</body>") else {
        return raw.to_string();
    };
    body[..close].to_string()
}

fn last_ci(hay: &str, needle: &str) -> Option<usize> {
    let mut from = 0;
    let mut last = None;
    while let Some(found) = js::find_ci(hay, needle, from) {
        last = Some(found);
        from = found + 1;
    }
    last
}

/// `s.slice(0, max)` in UTF-16 units. A cut inside a surrogate pair drops that char (Node would keep
/// a lone surrogate, which a Rust `char` cannot hold).
fn slice_utf16(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    let mut units = 0;
    for c in text.chars() {
        let width = c.len_utf16();
        if units + width > max {
            break;
        }
        units += width;
        out.push(c);
    }
    out
}

/// `input.nodeIds` as a JS `Set`: deduped, first appearance first; `None` when absent or empty.
fn wanted_ids(input: &Value) -> Option<Vec<String>> {
    let ids = input.get("nodeIds")?.as_array()?;
    if ids.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        let id = js::js_string(Some(id));
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Some(out)
}

fn node_has_id(node: &Value, id: &str) -> bool {
    node.get("nodeId").and_then(Value::as_str) == Some(id)
}

fn run(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(async move {
        let lesson_path = js::js_string(input.get("lessonPath"));
        let data_root = host.data_root();
        let lesson = match artifact::read_lesson(data_root, &lesson_path) {
            Ok(lesson) => lesson,
            Err(e) => return fail(e),
        };
        let Some(lesson) = lesson else {
            let raw = artifact::read_lesson_html(data_root, &lesson_path)
                .ok()
                .flatten();
            let Some(raw) = raw else {
                return fail(format!("No lesson at {lesson_path}."));
            };
            return ToolOutcome::ok(slice_utf16(&html_to_text(&body_inner(&raw)), MAX_PROSE));
        };

        let metadata = lesson.doc.get("metadata").unwrap_or(&Value::Null);
        let nodes = lesson
            .doc
            .get("nodes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut head = format!(
            "# {}\nLezione {} · {}",
            js::js_string(metadata.get("title")),
            js::js_string(metadata.get("lessonNumber")),
            lesson.rel,
        );
        if js::truthy(metadata.get("targetGoal")) {
            head.push_str(&format!(
                "\nObiettivo: {}",
                js::js_string(metadata.get("targetGoal"))
            ));
        }

        if nodes.is_empty() {
            let raw = match artifact::read_lesson_html(data_root, &lesson.rel) {
                Ok(raw) => raw.unwrap_or_default(),
                Err(e) => return fail(e),
            };
            return ToolOutcome::ok(format!(
                "{head}\n\n{}",
                slice_utf16(&html_to_text(&body_inner(&raw)), MAX_PROSE)
            ));
        }

        let want = wanted_ids(&input);
        let picked: Vec<&Value> = match &want {
            None => nodes.iter().collect(),
            Some(ids) => nodes
                .iter()
                .filter(|node| ids.iter().any(|id| node_has_id(node, id)))
                .collect(),
        };
        let body = picked
            .iter()
            .map(|node| node_text(node))
            .collect::<Vec<_>>()
            .join("\n\n");
        let missing: Vec<String> = match &want {
            None => Vec::new(),
            Some(ids) => ids
                .iter()
                .filter(|id| !nodes.iter().any(|node| node_has_id(node, id)))
                .cloned()
                .collect(),
        };
        let note = if missing.is_empty() {
            String::new()
        } else {
            let known = nodes
                .iter()
                .map(|node| js::js_string(node.get("nodeId")))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "\n\n(Nodi non trovati: {}. Nodi della lezione: {known})",
                missing.join(", ")
            )
        };
        ToolOutcome::ok(format!("{head}\n\n{body}{note}"))
    })
}

/// Node's `readLessonSchema`, zod 4 wording and schema key order (unknown keys dropped).
fn validate(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue(
            "(input)",
            &format!(
                "Invalid input: expected object, received {}",
                received(input)
            ),
        )]);
    };
    let mut issues = Vec::new();
    let mut out = Map::new();

    match object.get("lessonPath") {
        Some(Value::String(path)) => {
            out.insert("lessonPath".to_string(), Value::String(path.clone()));
        }
        other => issues.push(issue(
            "lessonPath",
            &format!(
                "Invalid input: expected string, received {}",
                received_or_undefined(other)
            ),
        )),
    }

    if let Some(value) = object.get("nodeIds") {
        match value.as_array() {
            None => issues.push(issue(
                "nodeIds",
                &format!(
                    "Invalid input: expected array, received {}",
                    received(value)
                ),
            )),
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    if !item.is_string() {
                        issues.push(issue(
                            &format!("nodeIds.{index}"),
                            &format!(
                                "Invalid input: expected string, received {}",
                                received(item)
                            ),
                        ));
                    }
                }
                out.insert("nodeIds".to_string(), value.clone());
            }
        }
    }

    if issues.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(issues)
    }
}

fn received_or_undefined(value: Option<&Value>) -> &'static str {
    value.map_or("undefined", received)
}

/// zod's `received`: the JSON type of the value.
fn received(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::lesson::doc::{self, AppendOptions};
    use crate::tools::fake::FakeHost;

    const DAG: &str = "graph TD\n  N1[\"Nodo 1: la prima cosa\"] --> N2[\"Nodo 2: la seconda\"]";

    fn teach() -> Value {
        json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false})
    }

    fn append(node: Value, host: &FakeHost) {
        let abs = host.data_root().join(host.lesson_path().expect("a lesson"));
        doc::append_node(AppendOptions {
            lesson_path: Some(abs.to_string_lossy().into_owned()),
            slug: None,
            node,
            base_dir: Some(host.data_root().to_string_lossy().into_owned()),
        })
        .expect("append the node");
    }

    #[tokio::test]
    async fn returns_the_nodes_as_compact_text_and_filters_by_node_ids() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let rel = host.lesson_path().expect("a lesson");
        append(
            json!({
                "nodeId": "prima-cosa",
                "title": "La prima cosa",
                "tier": "unconditional-truth",
                "content": "<p>vero <b>sempre</b></p>",
                "codeBlocks": [{ "language": "ts", "code": "const x = ref(0)" }],
            }),
            &host,
        );

        let all = host.call(&TOOL, json!({ "lessonPath": rel })).await;
        assert!(!all.is_error, "{}", all.text);
        assert!(
            all.text.starts_with("# Il primo passo\nLezione 1 · courses/demo/lessons/0001-il-primo-passo.html\nObiettivo: capire\n\n"),
            "{}",
            all.text
        );
        assert!(
            all.text.contains("## prima-cosa: La prima cosa (unconditional-truth)"),
            "{}",
            all.text
        );
        assert!(all.text.contains("vero sempre"), "{}", all.text);
        assert!(
            all.text.contains("```ts\nconst x = ref(0)\n```"),
            "{}",
            all.text
        );
        assert!(!all.text.contains("<p>"), "{}", all.text);
        assert!(!all.text.contains("<style"), "{}", all.text);
        assert!(!all.text.contains("<script"), "{}", all.text);

        let none = host
            .call(&TOOL, json!({ "lessonPath": rel, "nodeIds": ["altro"] }))
            .await;
        assert!(!none.is_error, "{}", none.text);
        assert!(!none.text.contains("## prima-cosa"), "{}", none.text);
        assert!(none.text.contains("Nodi non trovati: altro"), "{}", none.text);
        assert!(
            none.text.contains("Nodi della lezione: prima-cosa"),
            "{}",
            none.text
        );

        let bad = host.call(&TOOL, json!({ "lessonPath": "../secret.html" })).await;
        assert!(bad.is_error, "{}", bad.text);
        assert!(
            bad.text
                .starts_with("Bad lesson path \"../secret.html\":"),
            "{}",
            bad.text
        );
    }

    #[tokio::test]
    async fn a_prose_lesson_answers_its_body_text() {
        let host = FakeHost::new(teach());
        let dir = host.data_root().join("courses/demo/lessons");
        std::fs::create_dir_all(&dir).expect("create the lessons folder");
        std::fs::write(
            dir.join("0002-prosa.html"),
            "<html><head><title>T</title><style>p{}</style></head><body><h1>Titolo</h1><p>Testo <b>grasso</b></p><script>x</script></body></html>",
        )
        .expect("write the prose lesson");

        let out = host
            .call(&TOOL, json!({ "lessonPath": "courses/demo/lessons/0002-prosa.html" }))
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(out.text, "Titolo\nTesto grasso");
    }

    #[tokio::test]
    async fn a_long_prose_lesson_is_cut_at_the_prose_cap() {
        let host = FakeHost::new(teach());
        let dir = host.data_root().join("courses/demo/lessons");
        std::fs::create_dir_all(&dir).expect("create the lessons folder");
        std::fs::write(
            dir.join("0003-lunga.html"),
            format!("<html><body>{}</body></html>", "a".repeat(MAX_PROSE + 5)),
        )
        .expect("write the long prose lesson");

        let out = host
            .call(&TOOL, json!({ "lessonPath": "courses/demo/lessons/0003-lunga.html" }))
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(out.text.encode_utf16().count(), MAX_PROSE);
    }

    #[tokio::test]
    async fn a_missing_lesson_is_an_error() {
        let host = FakeHost::new(teach());
        let out = host
            .call(&TOOL, json!({ "lessonPath": "courses/demo/lessons/0099-nope.html" }))
            .await;
        assert!(out.is_error);
        assert_eq!(out.text, "No lesson at courses/demo/lessons/0099-nope.html.");
    }

    #[tokio::test]
    async fn a_missing_node_is_named_with_the_lesson_nodes() {
        let host = FakeHost::with_lesson(teach(), DAG);
        let rel = host.lesson_path().expect("a lesson");
        append(
            json!({"nodeId": "prima", "title": "Prima", "tier": "axiom", "content": "<p>x</p>"}),
            &host,
        );

        let out = host
            .call(&TOOL, json!({ "lessonPath": rel, "nodeIds": ["prima", "seconda"] }))
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(
            out.text
                .ends_with("\n\n(Nodi non trovati: seconda. Nodi della lezione: prima)"),
            "{}",
            out.text
        );
    }

    #[test]
    fn node_text_renders_every_section_like_node_text() {
        let node = json!({
            "nodeId": "n1",
            "title": "Titolo",
            "tier": "axiom",
            "motivation": "<b>perché</b> sì",
            "content": "<p>testo</p>",
            "math": ["a^2", "b^2"],
            "codeBlocks": [
                { "caption": "Esempio", "language": "ts", "code": "const x = 1" },
                { "code": "senza lingua" },
            ],
            "connection": "<i>collega</i>",
            "quiz": { "question": "<p>Quanto?</p>", "correctAnswer": "<b>Due</b>" },
            "ankiParams": { "targetQuestion": "Quanto fa 1+1?" },
        });
        assert_eq!(
            node_text(&node),
            "## n1: Titolo (axiom)\n\n\
             Perché: perché sì\n\n\
             testo\n\n\
             $$a^2$$\n\n\
             $$b^2$$\n\n\
             Esempio\n```ts\nconst x = 1\n```\n\n\
             ```undefined\nsenza lingua\n```\n\n\
             Collegamento: collega\n\n\
             Verifica: Quanto?\nRisposta: Due\n\n\
             Carta Anki: Quanto fa 1+1?"
        );
    }

    #[test]
    fn node_text_takes_the_correct_index_when_the_answer_is_missing() {
        let node = json!({
            "nodeId": "n2",
            "title": "T",
            "tier": "derivation",
            "quiz": { "question": "Q?", "correctIndex": 2, "options": ["A", "B"] },
        });
        assert_eq!(
            node_text(&node),
            "## n2: T (derivation)\n\nVerifica: Q?\nRisposta: B"
        );

        // An empty correctAnswer is not nullish, so it wins and the answer line stays away.
        let node = json!({
            "nodeId": "n3",
            "title": "T",
            "tier": "derivation",
            "quiz": { "question": "Q?", "correctAnswer": "", "correctIndex": 1, "options": ["A"] },
        });
        assert_eq!(
            node_text(&node),
            "## n3: T (derivation)\n\nVerifica: Q?"
        );
    }

    #[test]
    fn slice_utf16_counts_utf16_units() {
        assert_eq!(slice_utf16("abcdef", 3), "abc");
        assert_eq!(slice_utf16("abc", 10), "abc");
        // An emoji is two units; a cut through it drops the whole char.
        assert_eq!(slice_utf16("😀x", 1), "");
        assert_eq!(slice_utf16("😀x", 2), "😀");
        assert_eq!(slice_utf16("😀x", 3), "😀x");
    }

    #[test]
    fn the_schema_file_is_the_node_capture() {
        assert_eq!(
            serde_json::to_string(&(TOOL.schema)()).unwrap(),
            r#"{"type":"object","properties":{"lessonPath":{"type":"string","description":"courses/<course>/lessons/<NNNN-slug>.html"},"nodeIds":{"description":"Only these nodes (nodeId); omit for the whole lesson.","type":"array","items":{"type":"string"}}},"required":["lessonPath"]}"#
        );
    }

    #[test]
    fn the_validator_reports_zod_issues_in_schema_order() {
        assert_eq!(
            (TOOL.validate)(&json!({})).unwrap_err(),
            vec!["lessonPath: Invalid input: expected string, received undefined"]
        );
        assert_eq!(
            (TOOL.validate)(&json!({ "lessonPath": 3, "nodeIds": [3, null] })).unwrap_err(),
            vec![
                "lessonPath: Invalid input: expected string, received number",
                "nodeIds.0: Invalid input: expected string, received number",
                "nodeIds.1: Invalid input: expected string, received null",
            ]
        );
        assert_eq!(
            (TOOL.validate)(&json!({ "lessonPath": "x", "nodeIds": 0 })).unwrap_err(),
            vec!["nodeIds: Invalid input: expected array, received number"]
        );
        assert_eq!(
            (TOOL.validate)(&json!("x")).unwrap_err(),
            vec!["(input): Invalid input: expected object, received string"]
        );
    }

    #[test]
    fn the_validator_drops_unknown_keys_and_rebuilds_in_schema_order() {
        let data = (TOOL.validate)(&json!({ "nodeIds": ["a"], "lessonPath": "x", "extra": 1 })).unwrap();
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"lessonPath":"x","nodeIds":["a"]}"#
        );
    }
}
