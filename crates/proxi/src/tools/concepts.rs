//! The `search_concepts`, `open_concept` and `browse_concepts` tools (port of app/server/tools/concepts.ts).

use serde_json::{Map, Value, json};

use crate::concepts::{self, NodeText, Ranker, SEARCH_LIMIT};
use crate::js;
use crate::sessions::{invalid_type, issue};
use crate::tools::util::fail;
use crate::tools::{ToolDef, ToolFuture, ToolHost};

/// Node's `searchConceptsTool`.
pub const SEARCH: ToolDef = ToolDef {
    name: "search_concepts",
    description: "Search his knowledge graph by meaning (Italian or English) over name, statement and aliases; falls back to lexical matching when the local model is missing. Returns at most 10 concepts, one per line: id | name | status (solido, fragile, insegnato, mancato) | course | lesson.",
    schema: search_schema,
    validate: validate_search,
    run: run_search,
    writes: false,
};

/// Node's `openConceptTool`.
pub const OPEN: ToolDef = ToolDef {
    name: "open_concept",
    description: "Open one concept of his knowledge graph: statement, status, courses, lessons, MapNode keys, premises and consequences.",
    schema: open_schema,
    validate: validate_open,
    run: run_open,
    writes: false,
};

/// Node's `browseConceptsTool`.
pub const BROWSE: ToolDef = ToolDef {
    name: "browse_concepts",
    description: "Browse his knowledge graph: no input lists courses, course lists its lessons, course + lesson lists that lesson's concepts with their status.",
    schema: browse_schema,
    validate: validate_browse,
    run: run_browse,
    writes: false,
};

fn search_schema() -> Value {
    serde_json::from_str(include_str!("schemas/search_concepts.json")).expect("the search schema parses")
}

fn open_schema() -> Value {
    serde_json::from_str(include_str!("schemas/open_concept.json")).expect("the open schema parses")
}

fn browse_schema() -> Value {
    serde_json::from_str(include_str!("schemas/browse_concepts.json")).expect("the browse schema parses")
}

/// Node's `searchConceptsTool` schema, hand-rolled: `z.object({query, limit?})`.
fn validate_search(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    match object.get("query") {
        Some(Value::String(query)) => {
            data.insert("query".to_string(), Value::String(query.clone()));
        }
        other => issues.push(issue("query", &invalid_type("string", other))),
    }

    if let Some(value) = object.get("limit") {
        match value.as_f64() {
            None => issues.push(issue("limit", &invalid_type("number", Some(value)))),
            // `.int()` aborts before the bounds, like zod 4.
            Some(number) if number.fract() != 0.0 => {
                issues.push(issue("limit", "Invalid input: expected int, received number"));
            }
            Some(number) if number < 1.0 => {
                issues.push(issue("limit", "Too small: expected number to be >=1"));
            }
            Some(number) if number > 10.0 => {
                issues.push(issue("limit", "Too big: expected number to be <=10"));
            }
            Some(number) => {
                data.insert("limit".to_string(), js::normalise_numbers(json!(number)));
            }
        }
    }

    if issues.is_empty() {
        Ok(Value::Object(data))
    } else {
        Err(issues)
    }
}

/// Node's `openConceptTool` schema: `z.object({id})`.
fn validate_open(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut data = Map::new();
    match object.get("id") {
        Some(Value::String(id)) => {
            data.insert("id".to_string(), Value::String(id.clone()));
        }
        other => {
            return Err(vec![issue("id", &invalid_type("string", other))]);
        }
    }
    Ok(Value::Object(data))
}

/// Node's `browseConceptsTool` schema: `z.object({course?, lesson?})`.
fn validate_browse(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    for key in ["course", "lesson"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        match value {
            Value::String(text) => {
                data.insert(key.to_string(), Value::String(text.clone()));
            }
            other => issues.push(issue(key, &invalid_type("string", Some(other)))),
        }
    }

    if issues.is_empty() {
        Ok(Value::Object(data))
    } else {
        Err(issues)
    }
}

fn run_search(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(async move {
        let data_root = host.data_root().to_path_buf();
        let semantic = host.semantic();
        let min_score = semantic.config().min_score;
        let query = input.get("query").and_then(Value::as_str).unwrap_or_default().to_string();
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(SEARCH_LIMIT, |limit| limit as usize);

        // The catalog rebuild and the ONNX pass are blocking: both leave the tokio workers.
        let out = tokio::task::spawn_blocking(move || {
            let cat = concepts::load_catalog(&data_root);
            let ranker: Ranker<'_> = &|query: &str, nodes: &[NodeText]| semantic.rank(query, nodes);
            concepts::search_concepts(&cat, &query, limit, Some(ranker), min_score)
        })
        .await;
        match out {
            Ok(out) => out,
            Err(error) => fail(error.to_string()),
        }
    })
}

fn run_open(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(async move {
        let data_root = host.data_root().to_path_buf();
        let id = input.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let out = tokio::task::spawn_blocking(move || {
            concepts::open_concept(&concepts::load_catalog(&data_root), &id)
        })
        .await;
        match out {
            Ok(out) => out,
            Err(error) => fail(error.to_string()),
        }
    })
}

fn run_browse(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(async move {
        let data_root = host.data_root().to_path_buf();
        let course = input.get("course").and_then(Value::as_str).map(str::to_string);
        let lesson = input.get("lesson").and_then(Value::as_str).map(str::to_string);
        let out = tokio::task::spawn_blocking(move || {
            concepts::browse_concepts(
                &concepts::load_catalog(&data_root),
                course.as_deref(),
                lesson.as_deref(),
            )
        })
        .await;
        match out {
            Ok(out) => out,
            Err(error) => fail(error.to_string()),
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::concepts::SEMANTIC_UNAVAILABLE;
    use crate::tools::fake::FakeHost;
    use crate::tools::ToolOutcome;

    fn teach() -> FakeHost {
        FakeHost::new(json!({"kind": "teach", "course": "demo", "model": "haiku", "ankiOptIn": false}))
    }

    #[tokio::test]
    async fn search_finds_the_fixture_concept_and_open_rejects_unknown_ids() {
        let host = teach();
        let found = host.call(&SEARCH, json!({ "query": "passo" })).await;
        assert!(!found.is_error, "{}", found.text);
        assert!(found.text.contains("n_fx_a"), "{}", found.text);
        // No model in the temp repo: the lexical hits carry the fallback note.
        assert!(found.text.ends_with(SEMANTIC_UNAVAILABLE), "{}", found.text);

        let opened = host.call(&OPEN, json!({ "id": "n_fx_a" })).await;
        assert!(!opened.is_error, "{}", opened.text);
        assert!(
            opened
                .text
                .contains("Statement: Ogni percorso comincia da un primo passo concreto."),
            "{}",
            opened.text
        );

        let browsed = host.call(&BROWSE, json!({ "course": "demo" })).await;
        assert!(!browsed.is_error, "{}", browsed.text);
        assert!(browsed.text.starts_with("Lessons of Demo"), "{}", browsed.text);

        let nope = host.call(&OPEN, json!({ "id": "nope" })).await;
        assert_eq!(
            nope,
            ToolOutcome::error(
                "Unknown concept id \"nope\". Use search_concepts or browse_concepts to find ids."
            )
        );
    }

    #[tokio::test]
    async fn browse_without_a_course_lists_the_courses() {
        let host = teach();
        let all = host.call(&BROWSE, json!({})).await;
        assert!(!all.is_error, "{}", all.text);
        assert!(
            all.text.starts_with("Courses (slug | title | concepts):"),
            "{}",
            all.text
        );
        assert!(all.text.contains("- demo | Demo | 4 concepts"), "{}", all.text);

        // Empty strings are Node's falsy course/lesson: the same listing.
        let empty = host.call(&BROWSE, json!({ "course": "", "lesson": "" })).await;
        assert_eq!(empty, all);

        let dangling = host.call(&BROWSE, json!({ "lesson": "0002-il-secondo-passo" })).await;
        assert_eq!(dangling, ToolOutcome::error("Pass course with lesson."));
    }

    #[tokio::test]
    async fn an_empty_query_is_an_error_result() {
        let host = teach();
        let out = host.call(&SEARCH, json!({ "query": "   " })).await;
        assert_eq!(
            out,
            ToolOutcome::error("Empty query: pass words from the concept's name or statement.")
        );
    }

    #[tokio::test]
    async fn the_limit_bounds_are_zods() {
        let host = teach();
        assert_eq!(
            host.call(&SEARCH, json!({ "query": "passo", "limit": 0 })).await,
            ToolOutcome::error(
                "Invalid input: limit: Too small: expected number to be >=1. Fix and call again."
            )
        );
        assert_eq!(
            host.call(&SEARCH, json!({ "query": "passo", "limit": 11 })).await,
            ToolOutcome::error(
                "Invalid input: limit: Too big: expected number to be <=10. Fix and call again."
            )
        );
    }

    #[test]
    fn invalid_input_uses_zods_text() {
        assert_eq!(
            (SEARCH.validate)(&json!({})),
            Err(vec![
                "query: Invalid input: expected string, received undefined".to_string(),
            ])
        );
        assert_eq!(
            (SEARCH.validate)(&json!({ "query": "x", "limit": 1.5 })),
            Err(vec![
                "limit: Invalid input: expected int, received number".to_string(),
            ])
        );
        assert_eq!(
            (SEARCH.validate)(&json!({ "query": "x", "limit": "3" })),
            Err(vec![
                "limit: Invalid input: expected number, received string".to_string(),
            ])
        );
        assert_eq!(
            (OPEN.validate)(&json!({})),
            Err(vec![
                "id: Invalid input: expected string, received undefined".to_string(),
            ])
        );
        assert_eq!(
            (BROWSE.validate)(&json!({ "course": 1, "lesson": null })),
            Err(vec![
                "course: Invalid input: expected string, received number".to_string(),
                "lesson: Invalid input: expected string, received null".to_string(),
            ])
        );
        assert_eq!(
            (BROWSE.validate)(&Value::Null),
            Err(vec![
                "(input): Invalid input: expected object, received null".to_string(),
            ])
        );
    }

    #[test]
    fn the_validator_drops_unknown_keys_and_keeps_schema_order() {
        let data = (SEARCH.validate)(&json!({ "limit": 3.0, "query": "passo", "extra": 1 }))
            .expect("valid");
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"query":"passo","limit":3}"#
        );
        let data = (BROWSE.validate)(&json!({ "lesson": "x", "extra": 1 })).expect("valid");
        assert_eq!(serde_json::to_string(&data).unwrap(), r#"{"lesson":"x"}"#);
    }

    #[test]
    fn the_schema_files_are_the_node_captures() {
        let search: Value =
            serde_json::from_str(include_str!("schemas/search_concepts.json")).expect("parses");
        assert_eq!(
            serde_json::to_string(&(SEARCH.schema)()).unwrap(),
            serde_json::to_string(&search).unwrap()
        );
        assert_eq!(search["required"], json!(["query"]));
        assert_eq!(search["properties"]["limit"]["maximum"], json!(10));
        assert_eq!(search["properties"]["limit"]["minimum"], json!(1));

        let open: Value =
            serde_json::from_str(include_str!("schemas/open_concept.json")).expect("parses");
        assert_eq!(
            serde_json::to_string(&(OPEN.schema)()).unwrap(),
            serde_json::to_string(&open).unwrap()
        );
        assert_eq!(open["required"], json!(["id"]));

        let browse: Value =
            serde_json::from_str(include_str!("schemas/browse_concepts.json")).expect("parses");
        assert_eq!(
            serde_json::to_string(&(BROWSE.schema)()).unwrap(),
            serde_json::to_string(&browse).unwrap()
        );
        assert!(browse.get("required").is_none());
    }
}
