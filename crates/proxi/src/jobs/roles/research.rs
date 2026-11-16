//! Research role (port of app/server/jobs/roles/research.ts).

use serde_json::{Map, Value, json};

use crate::engine::Grant;
use crate::js;
use crate::jobs::roles::{Role, RoleEnv, no_cleanup, read_prompt};
use crate::sessions::{invalid_type, issue};
use crate::tools::util::{Field, fail, string_value};
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolOutcome};

const BRIEF_FIELD: Field = Field {
    key: "brief",
    min: 80,
    required: true,
};

const TITLE_FIELD: Field = Field {
    key: "title",
    min: 1,
    required: true,
};

const URL_FIELD: Field = Field {
    key: "url",
    min: 0,
    required: true,
};

/// The `submit_research` tool: the role's only way to deliver.
pub const SUBMIT: ToolDef = ToolDef {
    name: "submit_research",
    description: "Deliver the finished research brief and its sources. Call it exactly once, as the last step.",
    schema,
    validate,
    run,
    writes: false,
};

/// What `researchRole` is in Node: web tools, 20 turns, one submit tool.
pub static ROLE: Role = Role {
    kind: "research",
    grant: &[Grant::Web],
    max_turns: 20,
    system_prompt,
    first_message,
    tools,
    cleanup: no_cleanup,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("../../tools/schemas/submit_research.json"))
        .expect("the submit_research schema parses")
}

/// The zod `schema`: cleaned data in schema key order, unknown keys dropped, every issue reported.
fn validate(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    match string_value(object.get("brief"), &BRIEF_FIELD) {
        Ok(Some(brief)) => {
            data.insert("brief".to_string(), Value::String(brief));
        }
        Ok(None) => {}
        Err(problem) => issues.push(problem),
    }

    match object.get("sources") {
        None => issues.push(issue("sources", &invalid_type("array", None))),
        Some(sources) => match sources.as_array() {
            None => issues.push(issue("sources", &invalid_type("array", Some(sources)))),
            Some(items) => {
                if items.is_empty() {
                    issues.push(issue("sources", "Too small: expected array to have >=1 items"));
                }
                let clean: Vec<Value> = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| read_source(item, index, &mut issues))
                    .collect();
                data.insert("sources".to_string(), Value::Array(clean));
            }
        },
    }

    if issues.is_empty() {
        Ok(Value::Object(data))
    } else {
        Err(issues)
    }
}

fn read_source(item: &Value, index: usize, issues: &mut Vec<String>) -> Value {
    let path = format!("sources.{index}");
    let Some(source) = item.as_object() else {
        issues.push(issue(&path, &invalid_type("object", Some(item))));
        return Value::Null;
    };
    let mut clean = Map::new();
    // `string_value` speaks for a top-level key: the source's path goes in front of its issue.
    match string_value(source.get("title"), &TITLE_FIELD) {
        Ok(Some(title)) => {
            clean.insert("title".to_string(), Value::String(title));
        }
        Ok(None) => {}
        Err(problem) => issues.push(format!("{path}.{problem}")),
    }
    match string_value(source.get("url"), &URL_FIELD) {
        Ok(Some(url)) => {
            if url_ok(&url) {
                clean.insert("url".to_string(), Value::String(url_value(&url)));
            } else {
                issues.push(issue(&format!("{path}.url"), "Invalid URL"));
            }
        }
        Ok(None) => {}
        Err(problem) => issues.push(format!("{path}.{problem}")),
    }
    Value::Object(clean)
}

/// What the URL parser and zod's `.trim()` leave of a URL (tabs and newlines are dropped inside).
fn url_value(url: &str) -> String {
    js::js_trim(url)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

/// `z.string().url()` in zod 4 is `URL.canParse`: any scheme is fine (`ftp:`, `mailto:`, `file:`),
/// and the special schemes need a non-empty host (`http://` and `https:` are not URLs).
fn url_ok(url: &str) -> bool {
    let url = url_value(url);
    let Some(colon) = url.find(':') else {
        return false;
    };
    let scheme = &url[..colon];
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        return false;
    }
    if !matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ws" | "wss" | "ftp"
    ) {
        return true;
    }
    // Node accepts `http:example.com` and `https:/path`: the slashes are optional and folded.
    let rest = url[colon + 1..].trim_start_matches(['/', '\\']);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        let Some(end) = bracketed.find(']') else {
            return false;
        };
        let after = &bracketed[end + 1..];
        if !after.is_empty() {
            let Some(port) = after.strip_prefix(':') else {
                return false;
            };
            if !port_ok(port) {
                return false;
            }
        }
        &bracketed[..end]
    } else if let Some((host, port)) = host_port.rsplit_once(':') {
        if !port_ok(port) {
            return false;
        }
        host
    } else {
        host_port
    };
    !host.is_empty()
        && !host.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^' | '%')
        })
}

/// An empty port is allowed; a named or out-of-range one makes `new URL` throw.
fn port_ok(port: &str) -> bool {
    port.is_empty()
        || (port.chars().all(|c| c.is_ascii_digit())
            && port.parse::<u32>().is_ok_and(|number| number <= 65_535))
}

/// Node's `/^https?:\/\//i`.
fn http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn run<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(async move {
        let Some(job) = host.job() else {
            return fail("Nessun job in corso: submit_research si usa solo da un subagente.");
        };
        let empty = Vec::new();
        let sources = input.get("sources").and_then(Value::as_array).unwrap_or(&empty);
        if let Some(bad) = sources.iter().find(|source| {
            !http_url(source.get("url").and_then(Value::as_str).unwrap_or_default())
        }) {
            return fail(format!(
                "La fonte \"{}\" non ha un URL http(s): correggi e richiama.",
                js::js_string(bad.get("title"))
            ));
        }
        job.set_activity("consegna la ricerca");
        let brief = input.get("brief").and_then(Value::as_str).unwrap_or_default();
        job.finish(json!({
            "kind": "research",
            "brief": js::js_trim(brief),
            "sources": input.get("sources").cloned().unwrap_or(Value::Array(Vec::new())),
        }));
        ToolOutcome::ok("Consegnato. Chiudi qui con una riga.")
    })
}

fn system_prompt(env: &RoleEnv, _input: &Value) -> Result<String, String> {
    read_prompt(env.code_root, "research.md")
}

fn first_message(_env: &RoleEnv, input: &mut Value) -> String {
    let mut message = format!(
        "Domanda da verificare:\n{}",
        js::js_string(input.get("question"))
    );
    if js::truthy(input.get("context")) {
        message.push_str("\n\nContesto della lezione:\n");
        message.push_str(&js::js_string(input.get("context")));
    }
    message
}

fn tools(_input: &Value) -> Vec<&'static ToolDef> {
    vec![&SUBMIT]
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::*;
    use crate::graph::store::GraphStore;
    use crate::jobs::roles::FakeJob;
    use crate::tools::fake::temp_dir;
    use crate::tools::{ToolOutcome, invoke};

    const BRIEF: &str = "Vue 3.5 rende reattivo un ref leggendo .value dentro un effect: la dipendenza viene registrata al momento della lettura. Fonte primaria: la documentazione ufficiale.";

    fn code_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[tokio::test]
    async fn submit_research_finishes_with_the_trimmed_brief_and_sources() {
        let dir = temp_dir("research");
        let job = FakeJob::new("j1", json!({}), &dir);
        let clean = (SUBMIT.validate)(&json!({
            "brief": format!("  {BRIEF}\n"),
            "sources": [{"title": "Vue docs", "url": "https://vuejs.org/"}],
        }))
        .expect("valid input");

        let out = invoke(&SUBMIT, clean, &job).await;

        assert_eq!(out, ToolOutcome::ok("Consegnato. Chiudi qui con una riga."));
        assert_eq!(job.activities(), ["consegna la ricerca"]);
        assert_eq!(
            serde_json::to_string(&job.finished().expect("the result")).unwrap(),
            format!(
                r#"{{"kind":"research","brief":"{BRIEF}","sources":[{{"title":"Vue docs","url":"https://vuejs.org/"}}]}}"#
            )
        );
    }

    #[tokio::test]
    async fn submit_research_rejects_a_source_that_is_not_http() {
        let dir = temp_dir("research");
        let job = FakeJob::new("j1", json!({}), &dir);
        let clean = (SUBMIT.validate)(&json!({
            "brief": BRIEF,
            "sources": [{"title": "x", "url": "ftp://x/y"}],
        }))
        .expect("zod accepts any URL");

        let out = invoke(&SUBMIT, clean, &job).await;

        assert_eq!(
            out,
            ToolOutcome::error("La fonte \"x\" non ha un URL http(s): correggi e richiama.")
        );
        assert_eq!(job.finished(), None);
        assert!(job.activities().is_empty());
    }

    #[tokio::test]
    async fn submit_research_accepts_http_in_any_case() {
        let dir = temp_dir("research");
        let job = FakeJob::new("j1", json!({}), &dir);
        let clean = (SUBMIT.validate)(&json!({
            "brief": BRIEF,
            "sources": [{"title": "x", "url": "HTTPS://X.Y"}],
        }))
        .expect("valid input");

        assert!(!invoke(&SUBMIT, clean, &job).await.is_error);
        assert_eq!(job.finished().expect("the result")["sources"][0]["url"], "HTTPS://X.Y");
    }

    #[test]
    fn submit_research_validator_reports_zods_issues() {
        assert_eq!(
            (SUBMIT.validate)(&json!({})),
            Err(vec![
                "brief: Invalid input: expected string, received undefined".to_string(),
                "sources: Invalid input: expected array, received undefined".to_string(),
            ])
        );
        assert_eq!(
            (SUBMIT.validate)(&json!({"brief": "x".repeat(79), "sources": [{"title": "", "url": "nope"}]})),
            Err(vec![
                "brief: Too small: expected string to have >=80 characters".to_string(),
                "sources.0.title: Too small: expected string to have >=1 characters".to_string(),
                "sources.0.url: Invalid URL".to_string(),
            ])
        );
        assert_eq!(
            (SUBMIT.validate)(&json!({"brief": BRIEF, "sources": []})),
            Err(vec![
                "sources: Too small: expected array to have >=1 items".to_string(),
            ])
        );
        assert_eq!(
            (SUBMIT.validate)(&json!({"brief": BRIEF, "sources": [{"title": "t"}]})),
            Err(vec![
                "sources.0.url: Invalid input: expected string, received undefined".to_string(),
            ])
        );
        assert_eq!(
            (SUBMIT.validate)(&json!({"brief": BRIEF, "sources": ["x"]})),
            Err(vec![
                "sources.0: Invalid input: expected object, received string".to_string(),
            ])
        );
        assert_eq!(
            (SUBMIT.validate)(&json!({"brief": 1, "sources": "x"})),
            Err(vec![
                "brief: Invalid input: expected string, received number".to_string(),
                "sources: Invalid input: expected array, received string".to_string(),
            ])
        );
        assert_eq!(
            (SUBMIT.validate)(&json!({
                "brief": BRIEF,
                "sources": [{"title": "", "url": "https://a.b"}, {"title": "t2", "url": "nope"}],
            })),
            Err(vec![
                "sources.0.title: Too small: expected string to have >=1 characters".to_string(),
                "sources.1.url: Invalid URL".to_string(),
            ])
        );
        assert_eq!(
            (SUBMIT.validate)(&Value::Null),
            Err(vec![
                "(input): Invalid input: expected object, received null".to_string(),
            ])
        );
    }

    #[test]
    fn submit_research_validator_drops_unknown_keys_and_trims_the_url() {
        let clean = (SUBMIT.validate)(&json!({
            "brief": BRIEF,
            "extra": true,
            "sources": [{"title": "Vue docs", "url": " https://vuejs.org/ ", "extra": 1}],
        }))
        .expect("valid input");

        assert_eq!(
            serde_json::to_string(&clean).unwrap(),
            format!(
                r#"{{"brief":"{BRIEF}","sources":[{{"title":"Vue docs","url":"https://vuejs.org/"}}]}}"#
            )
        );
    }

    #[test]
    fn submit_research_urls_match_zods_url_can_parse() {
        let accepts = |url: &str| {
            (SUBMIT.validate)(&json!({"brief": BRIEF, "sources": [{"title": "t", "url": url}]}))
                .is_ok()
        };
        for url in [
            "https://vuejs.org/guide/essentials/reactivity-fundamentals.html",
            "http://example.com",
            "HTTP://X.Y",
            "ftp://x/y",
            "mailto:x@example.com",
            "file:",
            "http:example.com",
            "https:/path",
            "http://127.0.0.1:9/x",
            "https://user:pass@host:8080/p",
        ] {
            assert!(accepts(url), "{url} is a URL");
        }
        for url in [
            "nope",
            "example.com",
            "http://",
            "https:",
            "//example.com",
            "1http://x",
            "http://exa mple.com",
            "http://x:abc",
            "http://x:999999",
            "http://[bad",
        ] {
            assert!(!accepts(url), "{url} is not a URL");
        }
    }

    #[test]
    fn research_role_configuration() {
        assert_eq!(ROLE.kind, "research");
        assert_eq!(ROLE.grant.len(), 1);
        assert_eq!(ROLE.grant[0], Grant::Web);
        assert_eq!(ROLE.max_turns, 20);

        let dir = temp_dir("research-role");
        let graph = GraphStore::new(dir.0.clone());
        let env = RoleEnv {
            code_root: &code_root(),
            data_root: &dir,
            graph: &graph,
        };

        let prompt = (ROLE.system_prompt)(&env, &json!({})).expect("the research prompt");
        assert!(prompt.starts_with("# Role: researcher"));
        assert!(prompt.encode_utf16().count() > 200);

        let tools = (ROLE.tools)(&json!({}));
        assert_eq!(
            tools.iter().map(|tool| tool.name).collect::<Vec<_>>(),
            ["submit_research"]
        );

        (ROLE.cleanup)("j1");
    }

    #[test]
    fn research_first_message_adds_the_lesson_context_only_when_present() {
        let dir = temp_dir("research-message");
        let graph = GraphStore::new(dir.0.clone());
        let env = RoleEnv {
            code_root: &code_root(),
            data_root: &dir,
            graph: &graph,
        };
        let question = "Chi ha inventato il calcolo infinitesimale?";
        let context = "La lezione parla di derivate.";

        let mut input = json!({"question": question, "context": context});
        assert_eq!(
            (ROLE.first_message)(&env, &mut input),
            format!("Domanda da verificare:\n{question}\n\nContesto della lezione:\n{context}")
        );
        assert_eq!(
            input,
            json!({"question": question, "context": context}),
            "a research input is not rewritten"
        );

        input["context"] = json!("");
        assert_eq!(
            (ROLE.first_message)(&env, &mut input),
            format!("Domanda da verificare:\n{question}")
        );

        input.as_object_mut().expect("an object").remove("context");
        assert_eq!(
            (ROLE.first_message)(&env, &mut input),
            format!("Domanda da verificare:\n{question}")
        );
    }
}
