//! Card derivation, lesson extraction and TSV export (`anki-sync.ts:289-511`).

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::anki::format::{
    escape_tsv_value, format_default_deck_name, format_latex_value, slugify_topic,
    slugify_topic_value,
};
use crate::course_data::{course_deck_name, course_lesson_tags, DECK_ROOT};
use crate::js::{find_ci, parse_json};
use crate::lesson::js::{is_js_space, js_concat, js_join_item, js_to_string, js_trim, js_truthy};

fn match_ci_at(hay: &str, at: usize, needle: &str) -> bool {
    let hay = hay.as_bytes();
    let needle = needle.as_bytes();
    at + needle.len() <= hay.len()
        && hay[at..at + needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn skip_js_space(s: &str, mut i: usize) -> usize {
    while let Some(c) = s[i..].chars().next() {
        if !is_js_space(c) {
            break;
        }
        i += c.len_utf8();
    }
    i
}

/// `/<script\s+type=["']application\/json["']\s+id=["']lesson-data["']>([\s\S]*?)<\/script>/i`,
/// looser than the engine's exact tag: case-insensitive and single quotes allowed.
pub fn find_lesson_island(html: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(p) = find_ci(html, "<script", from) {
        if let Some(body) = island_after_open(html, p) {
            return Some(body);
        }
        from = p + 1;
    }
    None
}

fn island_after_open(html: &str, p: usize) -> Option<&str> {
    let bytes = html.as_bytes();
    let mut i = p + "<script".len();
    let after_ws = skip_js_space(html, i);
    if after_ws == i {
        return None;
    }
    i = after_ws;
    if !match_ci_at(html, i, "type=") {
        return None;
    }
    i += "type=".len();
    if !matches!(bytes.get(i), Some(&b'"') | Some(&b'\'')) {
        return None;
    }
    i += 1;
    if !match_ci_at(html, i, "application/json") {
        return None;
    }
    i += "application/json".len();
    if !matches!(bytes.get(i), Some(&b'"') | Some(&b'\'')) {
        return None;
    }
    i += 1;
    let after_ws = skip_js_space(html, i);
    if after_ws == i {
        return None;
    }
    i = after_ws;
    if !match_ci_at(html, i, "id=") {
        return None;
    }
    i += "id=".len();
    if !matches!(bytes.get(i), Some(&b'"') | Some(&b'\'')) {
        return None;
    }
    i += 1;
    if !match_ci_at(html, i, "lesson-data") {
        return None;
    }
    i += "lesson-data".len();
    if !matches!(bytes.get(i), Some(&b'"') | Some(&b'\'')) {
        return None;
    }
    i += 1;
    if bytes.get(i) != Some(&b'>') {
        return None;
    }
    let body_start = i + 1;
    // The lazy `([\s\S]*?)` stops at the first close tag; without one no later start can match either.
    let end = find_ci(html, "</script>", body_start)?;
    Some(&html[body_start..end])
}

/// Derives at most one card per node: an authored question wins over a cloze.
pub fn generate_cards_from_node(
    node: &Value,
    topic: &str,
    default_deck: Option<&str>,
    slug: Option<&str>,
) -> Result<Vec<Value>, String> {
    if node.is_null() {
        return Err("TypeError: Cannot read properties of null (reading 'tier')".to_string());
    }
    let topic_slug = slug
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| slugify_topic(topic));
    let deck = default_deck
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format_default_deck_name(topic));

    let tier = node.get("tier");
    let node_id = node.get("nodeId");
    let mut tags: Vec<Value> = vec![
        Value::String("learn-forme".to_string()),
        Value::String(topic_slug),
        tier.cloned().unwrap_or(Value::Null),
        Value::String(format!("tier::{}", js_concat(tier))),
        Value::String(format!("node::{}", js_concat(node_id))),
    ];

    let params = node.get("ankiParams");
    if let Some(Value::Array(extra_tags)) = params.and_then(|p| p.get("tags")) {
        for tag in extra_tags {
            let clean = slugify_topic_value(Some(tag))?;
            if !clean.is_empty() && !tags.iter().any(|t| t.as_str() == Some(clean.as_str())) {
                tags.push(Value::String(clean));
            }
        }
    }

    let param = |key: &str| params.and_then(|p| p.get(key));
    let target_question = param("targetQuestion");
    let target_fact = param("targetFact");
    let suggested_cloze = param("suggestedCloze");
    let cloze_extra = param("clozeExtra");

    let mut cards = Vec::new();
    if js_truthy(target_question) && js_truthy(target_fact) {
        let mut card = Map::new();
        card.insert(
            "id".to_string(),
            Value::String(format!("{}-fact", js_concat(node_id))),
        );
        if let Some(id) = node_id {
            card.insert("nodeId".to_string(), id.clone());
        }
        card.insert("type".to_string(), Value::String("basic".to_string()));
        card.insert("deck".to_string(), Value::String(deck));
        card.insert(
            "front".to_string(),
            Value::String(format_latex_value(target_question)?),
        );
        card.insert(
            "back".to_string(),
            Value::String(format_latex_value(target_fact)?),
        );
        card.insert("tags".to_string(), Value::Array(tags));
        cards.push(Value::Object(card));
    } else if js_truthy(suggested_cloze) {
        let extra = if js_truthy(cloze_extra) {
            Some(format_latex_value(cloze_extra)?)
        } else {
            None
        };
        let mut card = Map::new();
        card.insert(
            "id".to_string(),
            Value::String(format!("{}-cloze", js_concat(node_id))),
        );
        if let Some(id) = node_id {
            card.insert("nodeId".to_string(), id.clone());
        }
        card.insert("type".to_string(), Value::String("cloze".to_string()));
        card.insert("deck".to_string(), Value::String(deck));
        card.insert(
            "text".to_string(),
            Value::String(format_latex_value(suggested_cloze)?),
        );
        if let Some(extra) = extra {
            card.insert("extra".to_string(), Value::String(extra));
        }
        card.insert("tags".to_string(), Value::Array(tags));
        cards.push(Value::Object(card));
    }
    Ok(cards)
}

/// Cards plus the deck they belong to, as `extract` prints them.
#[derive(Debug)]
pub struct Extracted {
    pub metadata: Value,
    pub deck_name: String,
    pub cards: Vec<Value>,
}

impl Extracted {
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("metadata".to_string(), self.metadata.clone());
        map.insert(
            "deckName".to_string(),
            Value::String(self.deck_name.clone()),
        );
        map.insert("cards".to_string(), Value::Array(self.cards.clone()));
        Value::Object(map)
    }
}

pub fn extract_cards_from_lesson(source: &str) -> Result<Extracted, String> {
    let path = Path::new(source);
    if !path.exists() {
        return Err(format!("Error: Lesson file not found at path: {source}"));
    }
    let raw = fs::read(path).map_err(|e| format!("Error: {e}"))?;
    let html = String::from_utf8_lossy(&raw);
    let not_found =
        || format!("Error: Could not find embedded #lesson-data JSON block in {source}");
    let Some(body) = find_lesson_island(&html) else {
        return Err(not_found());
    };
    if body.is_empty() {
        return Err(not_found());
    }
    let doc = parse_json(js_trim(body))
        .map_err(|e| format!("Error: Failed to parse lesson-data JSON in {source}: {e}"))?;

    if doc.is_null() {
        return Err("TypeError: Cannot read properties of null (reading 'metadata')".to_string());
    }
    let metadata = doc.get("metadata");
    let Some(metadata) = metadata else {
        return Err("TypeError: Cannot read properties of undefined (reading 'topic')".to_string());
    };
    if metadata.is_null() {
        return Err("TypeError: Cannot read properties of null (reading 'topic')".to_string());
    }

    let topic_value = metadata
        .get("topic")
        .filter(|v| js_truthy(Some(*v)))
        .or_else(|| metadata.get("title").filter(|v| js_truthy(Some(*v))));
    let topic = match topic_value {
        None => "Lesson".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => return Err("TypeError: topic.toLowerCase is not a function".to_string()),
    };

    let slug = slugify_topic(&topic);
    let default_deck = course_deck_name(path).unwrap_or_else(|| format_default_deck_name(&topic));

    let Some(Value::Array(nodes)) = doc.get("nodes") else {
        return Err("TypeError: doc.nodes is not iterable".to_string());
    };
    let mut cards = Vec::new();
    for node in nodes {
        cards.extend(generate_cards_from_node(
            node,
            &topic,
            Some(&default_deck),
            Some(&slug),
        )?);
    }

    let extra = course_lesson_tags(path, metadata.get("slug"));
    for card in &mut cards {
        if let Some(tags) = card.get_mut("tags").and_then(Value::as_array_mut) {
            for tag in &extra {
                if !tags.iter().any(|t| t.as_str() == Some(tag.as_str())) {
                    tags.push(Value::String(tag.clone()));
                }
            }
        }
    }

    Ok(Extracted {
        metadata: metadata.clone(),
        deck_name: default_deck,
        cards,
    })
}

/// Anki TSV with header directives; one row per basic/cloze card.
pub fn cards_to_tsv(cards: &[Value], deck_name: Option<&str>) -> Result<String, String> {
    let deck = deck_name
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match cards.first() {
            Some(first) => js_concat(first.get("deck")),
            None => format!("{DECK_ROOT}::General"),
        });

    let mut has_cloze = false;
    let mut has_basic = false;
    for card in cards {
        if card.is_null() {
            return Err("TypeError: Cannot read properties of null (reading 'type')".to_string());
        }
        match card.get("type").and_then(Value::as_str) {
            Some("cloze") => has_cloze = true,
            Some("basic") => has_basic = true,
            _ => {}
        }
    }
    let notetype = if has_cloze && !has_basic {
        "Cloze"
    } else {
        "Basic"
    };

    let mut lines = vec![
        "#separator:tab".to_string(),
        "#html:true".to_string(),
        "#tags column:3".to_string(),
        format!("#deck:{deck}"),
        format!("#notetype:{notetype}"),
    ];

    for card in cards {
        let tags = match card.get("tags") {
            None => {
                return Err(
                    "TypeError: Cannot read properties of undefined (reading 'join')".to_string(),
                );
            }
            Some(Value::Null) => {
                return Err(
                    "TypeError: Cannot read properties of null (reading 'join')".to_string()
                );
            }
            Some(Value::Array(items)) => items
                .iter()
                .map(|tag| js_join_item(Some(tag)))
                .collect::<Vec<_>>()
                .join(" "),
            Some(_) => {
                return Err("TypeError: card.tags.join is not a function".to_string());
            }
        };

        match card.get("type").and_then(Value::as_str) {
            Some("basic") => lines.push(format!(
                "{}\t{}\t{tags}",
                escape_tsv_value(card.get("front"))?,
                escape_tsv_value(card.get("back"))?,
            )),
            Some("cloze") => lines.push(format!(
                "{}\t{}\t{tags}",
                escape_tsv_value(card.get("text"))?,
                escape_tsv_value(card.get("extra"))?,
            )),
            _ => {}
        }
    }

    Ok(lines.join("\n") + "\n")
}

#[derive(Default)]
pub struct ExportOptions {
    pub cards: Option<Value>,
    pub nodes: Option<Value>,
    pub lesson_path: Option<String>,
    pub topic: Option<Value>,
    pub slug: Option<Value>,
    pub out_file: Option<String>,
    pub base_dir: Option<String>,
}

pub struct ExportResult {
    pub file_path: String,
    pub deck_name: String,
    pub slug: String,
    pub card_count: usize,
    pub basic_count: usize,
    pub cloze_count: usize,
    pub tsv_content: String,
}

pub fn export_deck_tsv(options: &ExportOptions) -> Result<ExportResult, String> {
    let base = options
        .base_dir
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        });
    let decks_dir = Path::new(&base).join("decks");

    let mut cards: Vec<Value> = match &options.cards {
        Some(value) if js_truthy(Some(value)) => match value {
            Value::Array(items) => items.clone(),
            _ => return Err("TypeError: cards.some is not a function".to_string()),
        },
        _ => Vec::new(),
    };
    let mut topic = match &options.topic {
        Some(value) if js_truthy(Some(value)) => Some(value.clone()),
        _ => Some(Value::String("Lesson".to_string())),
    };
    let mut slug = match &options.slug {
        Some(value) if js_truthy(Some(value)) => js_to_string(value),
        _ => String::new(),
    };
    let mut lesson_deck: Option<String> = None;

    if let Some(lesson_path) = options.lesson_path.as_deref().filter(|p| !p.is_empty()) {
        let extracted = extract_cards_from_lesson(lesson_path)?;
        cards = extracted.cards;
        // Raw metadata: a lesson without a topic fails on the slug below, like Node.
        topic = extracted.metadata.get("topic").cloned();
        slug = extracted
            .metadata
            .get("slug")
            .filter(|value| js_truthy(Some(*value)))
            .map(js_to_string)
            .unwrap_or_default();
        lesson_deck = Some(extracted.deck_name);
    } else if js_truthy(options.nodes.as_ref()) {
        let Value::Array(nodes) = options.nodes.as_ref().expect("nodes is Some") else {
            return Err("TypeError: options.nodes is not iterable".to_string());
        };
        let Some(topic_text) = topic.as_ref().and_then(Value::as_str) else {
            return Err("TypeError: topic.toLowerCase is not a function".to_string());
        };
        if slug.is_empty() {
            slug = slugify_topic(topic_text);
        }
        cards = Vec::new();
        for node in nodes {
            cards.extend(generate_cards_from_node(
                node,
                topic_text,
                None,
                Some(&slug),
            )?);
        }
    }

    if slug.is_empty() {
        slug = slugify_topic_value(topic.as_ref())?;
    }

    let deck_name = match lesson_deck {
        Some(deck) => deck,
        None => {
            let Some(topic_text) = topic.as_ref().and_then(Value::as_str) else {
                return Err("TypeError: topic.replace is not a function".to_string());
            };
            format_default_deck_name(topic_text)
        }
    };

    let tsv_content = cards_to_tsv(&cards, Some(&deck_name))?;

    fs::create_dir_all(&decks_dir).map_err(|e| format!("Error: {e}"))?;
    let target_file = match options.out_file.as_deref().filter(|s| !s.is_empty()) {
        Some(out) => std::path::PathBuf::from(out),
        None => decks_dir.join(format!("{slug}.tsv")),
    };
    fs::write(&target_file, &tsv_content).map_err(|e| format!("Error: {e}"))?;

    let basic_count = cards
        .iter()
        .filter(|card| card.get("type").and_then(Value::as_str) == Some("basic"))
        .count();
    let cloze_count = cards
        .iter()
        .filter(|card| card.get("type").and_then(Value::as_str) == Some("cloze"))
        .count();

    Ok(ExportResult {
        file_path: target_file.display().to_string(),
        deck_name,
        slug,
        card_count: cards.len(),
        basic_count,
        cloze_count,
        tsv_content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson::js::{render_data_island, stringify_pretty};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "proxi-anki-cards-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn write_raw(root: &Path, relative: &str, html: &str) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("lesson has a parent")).expect("create dir");
        fs::write(&path, html).expect("write lesson");
        path
    }

    fn write_lesson(root: &Path, relative: &str, doc: &Value) -> PathBuf {
        write_raw(
            root,
            relative,
            &format!("<html><body>{}</body></html>", render_data_island(doc)),
        )
    }

    fn display(path: &Path) -> String {
        path.display().to_string()
    }

    fn tags_of(card: &Value) -> Vec<String> {
        card.get("tags")
            .and_then(Value::as_array)
            .expect("card has tags")
            .iter()
            .map(|tag| js_join_item(Some(tag)))
            .collect()
    }

    #[test]
    fn cloze_from_target_fact_and_cloze() {
        let node = json!({
            "nodeId": "diff-quotient",
            "title": "Difference Quotient & Limit",
            "tier": "unconditional-truth",
            "content": "Average rate of change over interval $\\Delta t$.",
            "ankiParams": {
                "targetFact": "The derivative is the limit as \\(\\Delta t \\to 0\\).",
                "suggestedCloze": "The instantaneous rate of change is {{c1:\\(\\lim_{\\Delta t \\to 0} \\frac{\\Delta x}{\\Delta t}\\) }}.",
                "tags": ["calculus", "derivatives"]
            }
        });

        let cards = generate_cards_from_node(&node, "Calculus", None, None).unwrap();
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card["type"], json!("cloze"));
        assert!(card["text"].as_str().unwrap().contains("{{c1:"));
        let tags = tags_of(card);
        for tag in [
            "learn-forme",
            "calculus",
            "tier::unconditional-truth",
            "node::diff-quotient",
        ] {
            assert!(tags.iter().any(|t| t == tag), "missing {tag}: {tags:?}");
        }
    }

    #[test]
    fn question_card_wins_over_cloze() {
        let node = json!({
            "nodeId": "both",
            "tier": "unconditional-truth",
            "ankiParams": {
                "targetQuestion": "Q?",
                "targetFact": "A.",
                "suggestedCloze": "The answer is {{c1::A}}."
            }
        });

        let cards = generate_cards_from_node(&node, "Vue", None, None).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["type"], json!("basic"));
        assert_eq!(cards[0]["front"], json!("Q?"));
    }

    #[test]
    fn basic_only_from_authored_question() {
        let node = json!({
            "nodeId": "packet-switching",
            "title": "Packets as Atomic Unit of Network Communication",
            "tier": "unconditional-truth",
            "content": "ALL communication between internet hosts is performed through packet transfer.",
            "ankiParams": {
                "targetQuestion": "Which unit carries all communication between internet hosts?",
                "targetFact": "All internet communication is performed by transmitting discrete, independent packets."
            }
        });

        let cards = generate_cards_from_node(&node, "Computer Networking", None, None).unwrap();
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card["type"], json!("basic"));
        assert_eq!(
            card["front"],
            json!("Which unit carries all communication between internet hosts?")
        );
        assert!(
            !card["front"]
                .as_str()
                .unwrap()
                .contains("Packets as Atomic Unit"),
            "the title states the answer, it must not be the cue"
        );
        assert!(
            card["back"]
                .as_str()
                .unwrap()
                .contains("discrete, independent packets")
        );
        assert!(tags_of(card).contains(&"tier::unconditional-truth".to_string()));

        let mut without_question = node.clone();
        without_question["ankiParams"]
            .as_object_mut()
            .unwrap()
            .remove("targetQuestion");
        assert!(
            generate_cards_from_node(&without_question, "Computer Networking", None, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cloze_extra_only_when_authored() {
        let bare = json!({
            "nodeId": "ref-getter",
            "tier": "unconditional-truth",
            "motivation": "A very long motivation paragraph that must never end up on a card.",
            "content": "Long content.",
            "ankiParams": { "suggestedCloze": "Vue 3: reading {{c1::.value}} runs the getter." }
        });
        let cards = generate_cards_from_node(&bare, "Vue", None, None).unwrap();
        assert_eq!(cards[0]["type"], json!("cloze"));
        assert!(cards[0].get("extra").is_none());

        let hinted = json!({
            "nodeId": "ref-getter",
            "tier": "unconditional-truth",
            "ankiParams": {
                "suggestedCloze": "Vue 3: reading {{c1::.value}} runs the getter.",
                "clozeExtra": "Same mechanism as an accessor property."
            }
        });
        let cards = generate_cards_from_node(&hinted, "Vue", None, None).unwrap();
        assert_eq!(
            cards[0]["extra"],
            json!("Same mechanism as an accessor property.")
        );
    }

    #[test]
    fn missing_tier_and_node_id() {
        let node = json!({ "ankiParams": { "suggestedCloze": "x" } });

        let cards = generate_cards_from_node(&node, "T", None, None).unwrap();
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card["id"], json!("undefined-cloze"));
        assert!(card.get("nodeId").is_none());
        assert_eq!(
            card["tags"],
            json!([
                "learn-forme",
                "t",
                null,
                "tier::undefined",
                "node::undefined"
            ])
        );
        assert_eq!(card["deck"], json!("Proxi::T"));
    }

    #[test]
    fn card_key_order() {
        let basic_node = json!({
            "nodeId": "n",
            "tier": "axiom",
            "ankiParams": { "targetQuestion": "Q?", "targetFact": "A." }
        });
        let basic = generate_cards_from_node(&basic_node, "T", None, None).unwrap();
        assert_eq!(
            stringify_pretty(&basic[0]),
            r#"{
  "id": "n-fact",
  "nodeId": "n",
  "type": "basic",
  "deck": "Proxi::T",
  "front": "Q?",
  "back": "A.",
  "tags": [
    "learn-forme",
    "t",
    "axiom",
    "tier::axiom",
    "node::n"
  ]
}"#
        );

        let cloze_node = json!({
            "nodeId": "n",
            "tier": "axiom",
            "ankiParams": { "suggestedCloze": "Il {{c1::ref}}", "clozeExtra": "e" }
        });
        let cloze = generate_cards_from_node(&cloze_node, "T", None, None).unwrap();
        assert_eq!(
            stringify_pretty(&cloze[0]),
            r#"{
  "id": "n-cloze",
  "nodeId": "n",
  "type": "cloze",
  "deck": "Proxi::T",
  "text": "Il {{c1::ref}}",
  "extra": "e",
  "tags": [
    "learn-forme",
    "t",
    "axiom",
    "tier::axiom",
    "node::n"
  ]
}"#
        );
    }

    #[test]
    fn tags_from_anki_params_dedup() {
        let node = json!({
            "nodeId": "n",
            "tier": "axiom",
            "ankiParams": {
                "suggestedCloze": "x",
                "tags": ["Reattività & Co", "t"]
            }
        });
        let cards = generate_cards_from_node(&node, "T", None, None).unwrap();
        let tags = tags_of(&cards[0]);
        assert_eq!(tags.iter().filter(|tag| *tag == "t").count(), 1);
        assert!(tags.contains(&"reattivita-co".to_string()));

        let bad = json!({
            "nodeId": "n",
            "tier": "axiom",
            "ankiParams": { "suggestedCloze": "x", "tags": [5] }
        });
        assert!(
            generate_cards_from_node(&bad, "T", None, None)
                .unwrap_err()
                .starts_with("TypeError: ")
        );
    }

    #[test]
    fn null_node_fails() {
        assert_eq!(
            generate_cards_from_node(&Value::Null, "T", None, None).unwrap_err(),
            "TypeError: Cannot read properties of null (reading 'tier')"
        );
    }

    #[test]
    fn tsv_headers_and_rows() {
        let cards = json!([
            {
                "id": "card-1",
                "nodeId": "node-1",
                "type": "basic",
                "deck": "Proxi::LinearAlgebra",
                "front": "What is an eigenvector of matrix \\(A\\)?",
                "back": "A non-zero vector \\(v\\) whose direction is unchanged by \\(A\\):<br>\\[Av = \\lambda v\\]",
                "tags": ["learn-forme", "linear-algebra", "tier::unconditional-truth", "node::node-1"]
            },
            {
                "id": "card-2",
                "nodeId": "node-2",
                "type": "cloze",
                "deck": "Proxi::LinearAlgebra",
                "text": "Eigenvalues are roots of the characteristic polynomial {{c1:\\(\\det(A - \\lambda I) = 0\\) }}.",
                "extra": "Roots give scale factor \\(\\lambda\\).",
                "tags": ["learn-forme", "linear-algebra", "tier::derivation", "node::node-2"]
            }
        ]);

        let tsv = cards_to_tsv(cards.as_array().unwrap(), Some("Proxi::LinearAlgebra")).unwrap();
        assert!(tsv.starts_with(
            "#separator:tab\n#html:true\n#tags column:3\n#deck:Proxi::LinearAlgebra\n#notetype:Basic\n"
        ));

        let rows: Vec<&str> = tsv
            .split('\n')
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .collect();
        assert_eq!(rows.len(), 2);

        let first: Vec<&str> = rows[0].split('\t').collect();
        assert_eq!(first.len(), 3);
        assert_eq!(first[0], "What is an eigenvector of matrix \\(A\\)?");
        assert!(first[1].contains("Av = \\lambda v"));
        assert!(
            first[2].contains("learn-forme linear-algebra tier::unconditional-truth"),
            "{}",
            first[2]
        );

        let second: Vec<&str> = rows[1].split('\t').collect();
        assert_eq!(second.len(), 3);
        assert!(second[0].contains("{{c1:"));
        assert_eq!(second[1], "Roots give scale factor \\(\\lambda\\).");
    }

    #[test]
    fn tsv_empty_falls_back_to_general() {
        let tsv = cards_to_tsv(&[], None).unwrap();
        assert!(tsv.contains("#deck:Proxi::General"));
        assert!(tsv.contains("#notetype:Basic"));
    }

    #[test]
    fn tsv_cloze_only_notetype() {
        let cloze = json!([{
            "type": "cloze",
            "text": "a {{c1::b}}",
            "extra": "x",
            "tags": ["t"]
        }]);
        assert!(
            cards_to_tsv(cloze.as_array().unwrap(), None)
                .unwrap()
                .contains("#notetype:Cloze")
        );

        let mixed = json!([
            { "type": "cloze", "text": "a {{c1::b}}", "tags": ["t"] },
            { "type": "basic", "front": "f", "back": "b", "tags": ["t"] }
        ]);
        assert!(
            cards_to_tsv(mixed.as_array().unwrap(), None)
                .unwrap()
                .contains("#notetype:Basic")
        );
    }

    #[test]
    fn tsv_skips_unknown_type_and_joins_null_tags() {
        let cards = json!([
            { "type": "basic", "front": "f", "back": "b", "tags": ["a", null, "b"] },
            { "type": "other", "tags": ["z"] }
        ]);

        let tsv = cards_to_tsv(cards.as_array().unwrap(), None).unwrap();
        let rows: Vec<&str> = tsv
            .split('\n')
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].split('\t').nth(2), Some("a  b"));
    }

    #[test]
    fn tsv_missing_tags_fails() {
        let missing = json!([{ "type": "basic", "front": "f", "back": "b" }]);
        let err = cards_to_tsv(missing.as_array().unwrap(), None).unwrap_err();
        assert!(err.contains("reading 'join'"), "{err}");

        let null = json!([{ "type": "basic", "front": "f", "back": "b", "tags": null }]);
        let err = cards_to_tsv(null.as_array().unwrap(), None).unwrap_err();
        assert!(err.contains("null"), "{err}");
    }

    #[test]
    fn export_writes_topic_slug_file() {
        let t = temp_dir("export-topic");
        let cards = json!([{
            "id": "c1",
            "nodeId": "n1",
            "type": "basic",
            "deck": "Proxi::Calculus",
            "front": "Front 1",
            "back": "Back 1",
            "tags": ["learn-forme", "calculus"]
        }]);
        let options = ExportOptions {
            cards: Some(cards),
            topic: Some(json!("Calculus: Derivatives")),
            base_dir: Some(display(&t.0)),
            ..ExportOptions::default()
        };

        let result = export_deck_tsv(&options).unwrap();
        assert_eq!(result.slug, "calculus-derivatives");
        assert_eq!(result.card_count, 1);
        assert_eq!(result.basic_count, 1);
        assert_eq!(result.cloze_count, 0);

        let expected = t.0.join("decks").join("calculus-derivatives.tsv");
        assert_eq!(Path::new(&result.file_path), expected);
        let content = fs::read_to_string(&result.file_path).unwrap();
        assert!(
            content.contains("Front 1\tBack 1\tlearn-forme calculus"),
            "{content}"
        );
    }

    #[test]
    fn export_nodes_win_over_cards() {
        let t = temp_dir("export-nodes");
        let options = ExportOptions {
            cards: Some(json!([{
                "type": "basic",
                "front": "f",
                "back": "b",
                "tags": ["x"]
            }])),
            nodes: Some(json!([])),
            topic: Some(json!("T")),
            base_dir: Some(display(&t.0)),
            ..ExportOptions::default()
        };

        let result = export_deck_tsv(&options).unwrap();
        assert_eq!(result.card_count, 0);
        assert_eq!(result.basic_count, 0);
        assert_eq!(result.cloze_count, 0);
    }

    #[test]
    fn extract_from_lesson_file() {
        let t = temp_dir("extract-loose");
        let path = write_lesson(
            &t.0,
            "lessons-sciolte/0001-raft.html",
            &json!({
                "metadata": {
                    "topic": "Raft Consensus",
                    "title": "Understanding Raft from First Principles",
                    "slug": "raft-consensus"
                },
                "nodes": [{
                    "nodeId": "quorum-invariant",
                    "title": "Majority Quorum Overlap",
                    "tier": "unconditional-truth",
                    "content": "Any two majorities of cluster size N overlap by at least one node.",
                    "ankiParams": {
                        "targetFact": "Any two majorities of size N must share at least one node, preventing split-brain.",
                        "suggestedCloze": "Raft requires a majority quorum because any two majorities are guaranteed to {{c1::overlap by at least one node}}."
                    }
                }]
            }),
        );

        let extracted = extract_cards_from_lesson(&display(&path)).unwrap();
        assert_eq!(extracted.metadata["topic"], json!("Raft Consensus"));
        assert_eq!(extracted.cards.len(), 1);
        let card = &extracted.cards[0];
        assert_eq!(card["type"], json!("cloze"));
        assert!(tags_of(card).contains(&"raft-consensus".to_string()));
        assert_eq!(extracted.deck_name, "Proxi::Raft Consensus");

        let keys: Vec<String> = extracted
            .to_json()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, vec!["metadata", "deckName", "cards"]);
    }

    #[test]
    fn extract_course_lesson_uses_chapter_deck_and_tags() {
        let t = temp_dir("extract-course");
        fs::write(
            t.0.join("curriculum.json"),
            json!({ "chapters": [{ "title": "Vue.js & Vetrine", "slug": "vue-vetrine", "lessons": [] }] })
                .to_string(),
        )
        .unwrap();
        let path = write_lesson(
            &t.0,
            "courses/vue-vetrine/lessons/0001-reattivita.html",
            &json!({
                "metadata": {
                    "topic": "Reattività di Vue 3",
                    "title": "Reattività",
                    "slug": "l1"
                },
                "nodes": [
                    {
                        "nodeId": "a",
                        "tier": "unconditional-truth",
                        "ankiParams": { "suggestedCloze": "Il ref {{c1::traccia}} chi lo legge." }
                    },
                    {
                        "nodeId": "b",
                        "tier": "axiom",
                        "ankiParams": { "targetQuestion": "Q?", "targetFact": "A." }
                    }
                ]
            }),
        );

        let extracted = extract_cards_from_lesson(&display(&path)).unwrap();
        assert_eq!(extracted.deck_name, "Proxi::Vue.js & Vetrine");
        assert_eq!(extracted.cards.len(), 2);
        for card in &extracted.cards {
            assert_eq!(card["deck"], json!("Proxi::Vue.js & Vetrine"));
            let tags = tags_of(card);
            assert_eq!(
                tags[tags.len() - 2..].to_vec(),
                vec!["course::vue-vetrine", "lesson::l1"]
            );
        }
    }

    #[test]
    fn extract_lezioni_singole_and_export() {
        let t = temp_dir("extract-singole");
        let path = write_lesson(
            &t.0,
            "courses/lezioni-singole/lessons/0001-microstati.html",
            &json!({
                "metadata": {
                    "topic": "Entropia",
                    "title": "L'entropia misura il numero di microstati",
                    "slug": "microstati"
                },
                "nodes": [{
                    "nodeId": "microstati",
                    "tier": "unconditional-truth",
                    "ankiParams": {
                        "targetQuestion": "Che cosa misura l'entropia?",
                        "targetFact": "Il numero di microstati compatibili con il macrostato."
                    }
                }]
            }),
        );
        let source = display(&path);

        let extracted = extract_cards_from_lesson(&source).unwrap();
        assert_eq!(extracted.deck_name, "Proxi::Lezioni singole");
        assert_eq!(extracted.cards[0]["deck"], json!("Proxi::Lezioni singole"));

        let result = export_deck_tsv(&ExportOptions {
            lesson_path: Some(source),
            base_dir: Some(display(&t.0)),
            ..ExportOptions::default()
        })
        .unwrap();
        assert_eq!(result.deck_name, "Proxi::Lezioni singole");
        let tsv = fs::read_to_string(&result.file_path).unwrap();
        assert!(tsv.contains("#deck:Proxi::Lezioni singole"), "{tsv}");
    }

    #[test]
    fn island_reader_is_loose_like_node() {
        assert_eq!(
            find_lesson_island("<SCRIPT TYPE='application/json'\n ID=\"lesson-data\">{}</SCRIPT>"),
            Some("{}")
        );

        // A non-matching script before the island does not stop the scan.
        assert_eq!(
            find_lesson_island(
                "<html><body><script src=\"x\"></script><script type=\"application/json\" id=\"lesson-data\">{\"a\":1}</script></body></html>"
            ),
            Some("{\"a\":1}")
        );

        assert_eq!(
            find_lesson_island("<script type=\"application/json\" id=\"lesson-data\">x"),
            None
        );
        assert_eq!(
            find_lesson_island("<script type=\"application/json\" id=\"lesson-data\"></script>"),
            Some("")
        );
        // Multibyte text before the tag must not slice inside a char.
        assert_eq!(
            find_lesson_island(
                "città è <script type=\"application/json\" id=\"lesson-data\">ok</script>"
            ),
            Some("ok")
        );
    }

    #[test]
    fn extract_errors() {
        let t = temp_dir("extract-errors");

        let missing = t.0.join("broken/nope.html");
        assert_eq!(
            extract_cards_from_lesson(&display(&missing)).unwrap_err(),
            format!(
                "Error: Lesson file not found at path: {}",
                display(&missing)
            )
        );

        let no_island = write_raw(
            &t.0,
            "broken/no-island.html",
            "<html><body>niente</body></html>",
        );
        assert_eq!(
            extract_cards_from_lesson(&display(&no_island)).unwrap_err(),
            format!(
                "Error: Could not find embedded #lesson-data JSON block in {}",
                display(&no_island)
            )
        );

        let empty_island = write_raw(
            &t.0,
            "broken/empty-island.html",
            "<html><body><script type=\"application/json\" id=\"lesson-data\"></script></body></html>",
        );
        assert_eq!(
            extract_cards_from_lesson(&display(&empty_island)).unwrap_err(),
            format!(
                "Error: Could not find embedded #lesson-data JSON block in {}",
                display(&empty_island)
            )
        );

        let bad_json = write_raw(
            &t.0,
            "broken/bad-json.html",
            "<html><body><script type=\"application/json\" id=\"lesson-data\">{ nope</script></body></html>",
        );
        let err = extract_cards_from_lesson(&display(&bad_json)).unwrap_err();
        assert!(
            err.starts_with(&format!(
                "Error: Failed to parse lesson-data JSON in {}: ",
                display(&bad_json)
            )),
            "{err}"
        );

        let no_topic = write_lesson(&t.0, "broken/no-topic.html", &json!({ "nodes": [] }));
        let err = extract_cards_from_lesson(&display(&no_topic)).unwrap_err();
        assert!(
            err.starts_with("TypeError: Cannot read properties of undefined (reading 'topic')"),
            "{err}"
        );

        let nodes_number = write_lesson(
            &t.0,
            "broken/nodes-number.html",
            &json!({ "metadata": { "topic": "x" }, "nodes": 5 }),
        );
        assert_eq!(
            extract_cards_from_lesson(&display(&nodes_number)).unwrap_err(),
            "TypeError: doc.nodes is not iterable"
        );
    }
}
