//! Demo role (port of app/server/jobs/roles/demo.ts).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use base64::Engine as _;
use serde_json::{Map, Value};

use crate::jobs::render::{RenderError, DEFAULT_SETTLE};
use crate::jobs::sanitize::check_demo;
use crate::js;
use crate::sessions::utf16_len;
use crate::tools::util::{fail, string_object, Field};
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolImage, ToolOutcome};

use super::{error_text, not_a_job, read_prompt, Role, RoleEnv};

/// Per-job draft; lives in memory only for the minutes a job runs.
struct Draft {
    html: String,
    /// A `run_demo` with zero errors sets it, the next `write_demo` clears it: submit wants a passing run.
    passed: bool,
}

static DRAFTS: LazyLock<Mutex<HashMap<String, Draft>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn drafts() -> &'static Mutex<HashMap<String, Draft>> {
    &DRAFTS
}

const WRITE_DEMO_FIELDS: &[Field] = &[Field {
    key: "html",
    min: 1,
    required: true,
}];

const SUBMIT_DEMO_FIELDS: &[Field] = &[Field {
    key: "notes",
    min: 0,
    required: false,
}];

/// The `write_demo` tool.
pub const WRITE_DEMO: ToolDef = ToolDef {
    name: "write_demo",
    description: "Save the demo as one self-contained HTML file (replaces the previous one). No external network resources.",
    schema: write_demo_schema,
    validate: validate_write_demo,
    run: run_write_demo,
    writes: false,
};

/// The `run_demo` tool.
pub const RUN_DEMO: ToolDef = ToolDef {
    name: "run_demo",
    description: "Load the current demo headless for about 1.5 s; returns console errors, page errors and a screenshot.",
    schema: run_demo_schema,
    validate: validate_empty,
    run: run_run_demo,
    writes: false,
};

/// The `submit_demo` tool.
pub const SUBMIT_DEMO: ToolDef = ToolDef {
    name: "submit_demo",
    description: "Deliver the demo. Allowed only when the latest run_demo passed with zero errors.",
    schema: submit_demo_schema,
    validate: validate_submit_demo,
    run: run_submit_demo,
    writes: false,
};

fn validate_write_demo(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, WRITE_DEMO_FIELDS)
}

/// `z.object({})`: any object cleans to `{}`, unknown keys dropped.
fn validate_empty(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, &[])
}

fn validate_submit_demo(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, SUBMIT_DEMO_FIELDS)
}

fn write_demo_schema() -> Value {
    serde_json::from_str(include_str!("../../tools/schemas/write_demo.json"))
        .expect("the write_demo schema is valid JSON")
}

fn run_demo_schema() -> Value {
    serde_json::from_str(include_str!("../../tools/schemas/run_demo.json"))
        .expect("the run_demo schema is valid JSON")
}

fn submit_demo_schema() -> Value {
    serde_json::from_str(include_str!("../../tools/schemas/submit_demo.json"))
        .expect("the submit_demo schema is valid JSON")
}

fn run_write_demo<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(async move {
        let Some(ctx) = host.job() else {
            return not_a_job();
        };
        // Only the validated input reaches `run`, so `html` is a string.
        let html = input.get("html").and_then(Value::as_str).unwrap_or("");
        let errors = check_demo(html);
        if !errors.is_empty() {
            return fail(format!(
                "HTML rifiutato: {}. Rendilo autosufficiente e richiama write_demo.",
                errors.join("; ")
            ));
        }
        drafts().lock().expect("draft lock").insert(
            ctx.job_id().to_string(),
            Draft {
                html: html.to_string(),
                passed: false,
            },
        );
        ctx.set_activity("scrive la demo");
        ToolOutcome::ok(format!(
            "Demo salvata ({} caratteri). Chiama run_demo per provarla.",
            utf16_len(html)
        ))
    })
}

fn mark_passed(job_id: &str, html: &str, passed: bool) {
    let mut drafts = drafts().lock().expect("draft lock");
    if let Some(draft) = drafts.get_mut(job_id) {
        if draft.html == html {
            draft.passed = passed;
        }
    }
}

fn run_run_demo<'a>(_input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(async move {
        let Some(ctx) = host.job() else {
            return not_a_job();
        };
        let job_id = ctx.job_id().to_string();
        let html = {
            let drafts = drafts().lock().expect("draft lock");
            match drafts.get(&job_id) {
                Some(draft) => draft.html.clone(),
                None => return fail("Nessuna demo: chiama prima write_demo."),
            }
        };
        ctx.set_activity("prova la demo");
        let Some(renderer) = ctx.renderer() else {
            return fail("Prova non riuscita: nessun renderer");
        };
        match renderer
            .run_demo_page(&job_id, &html, DEFAULT_SETTLE)
            .await
        {
            Ok(run) => {
                let passed = run.errors.is_empty();
                mark_passed(&job_id, &html, passed);
                let images = vec![ToolImage {
                    data: base64::engine::general_purpose::STANDARD.encode(&run.png),
                    mime_type: "image/png".to_string(),
                }];
                if passed {
                    ToolOutcome::ok("PASSATO: nessun errore. Guarda lo screenshot; se la demo ti convince, submit_demo.")
                        .with_images(images)
                } else {
                    ToolOutcome::error(format!(
                        "FALLITO, {} errori:\n- {}\nCorreggi con write_demo e riprova.",
                        run.errors.len(),
                        run.errors.join("\n- ")
                    ))
                    .with_images(images)
                }
            }
            // No browser kills the job, not just the call: the teacher reads the reason in the chat.
            Err(RenderError::NoBrowser(message)) => {
                ctx.fail(&message);
                fail(format!("Prova non riuscita: {message}"))
            }
            Err(error) => fail(format!("Prova non riuscita: {}", error_text(error))),
        }
    })
}

fn run_submit_demo<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(async move {
        let Some(ctx) = host.job() else {
            return not_a_job();
        };
        let job_id = ctx.job_id().to_string();
        let html = {
            let drafts = drafts().lock().expect("draft lock");
            match drafts.get(&job_id) {
                None => return fail("Nessuna demo da consegnare."),
                Some(draft) if !draft.passed => {
                    return fail("Non si consegna ciò che non è stato provato: l'ultima versione non ha ancora superato run_demo senza errori.");
                }
                Some(draft) => draft.html.clone(),
            }
        };
        drafts().lock().expect("draft lock").remove(&job_id);
        ctx.set_activity("consegna la demo");
        let job_input = ctx.input();
        let mut result = Map::new();
        result.insert("kind".to_string(), Value::String("demo".to_string()));
        if let Some(title) = job_input.get("title").and_then(Value::as_str) {
            result.insert("title".to_string(), Value::String(title.to_string()));
        }
        result.insert("html".to_string(), Value::String(html));
        // `notes?.trim() || undefined`: empty notes are no key at all.
        let notes = input.get("notes").and_then(Value::as_str).unwrap_or("");
        let notes = js::js_trim(notes);
        if !notes.is_empty() {
            result.insert("notes".to_string(), Value::String(notes.to_string()));
        }
        ctx.finish(Value::Object(result));
        ToolOutcome::ok("Consegnata. Chiudi qui con una riga.")
    })
}

fn system_prompt(env: &RoleEnv, _input: &Value) -> Result<String, String> {
    read_prompt(env.code_root, "demo.md")
}

fn first_message(_env: &RoleEnv, input: &mut Value) -> String {
    format!(
        "Costruisci questa demo, titolo \"{}\":\n{}",
        js::js_string(input.get("title")),
        js::js_string(input.get("brief"))
    )
}

fn tools(_input: &Value) -> Vec<&'static ToolDef> {
    vec![&WRITE_DEMO, &RUN_DEMO, &SUBMIT_DEMO]
}

fn cleanup(job_id: &str) {
    drafts().lock().expect("draft lock").remove(job_id);
}

pub static ROLE: Role = Role {
    kind: "demo",
    grant: &[],
    max_turns: 24,
    system_prompt,
    first_message,
    tools,
    cleanup,
};

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::graph::store::GraphStore;
    use crate::jobs::render::{self, Renderer};
    use crate::jobs::roles::FakeJob;
    use crate::tools::fake::{temp_dir, TempDir};

    /// A self-contained slider: no network, no console errors.
    const CLEAN: &str = "<!doctype html><body><input type=range id=r><output id=o>0</output><script>r.oninput=()=>o.textContent=r.value</script></body>";

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// A job over a temp data root no tool reads, with the input submit_demo carries.
    fn job(tag: &str, job_id: &str) -> (TempDir, FakeJob) {
        let dir = temp_dir(tag);
        let job = FakeJob::new(job_id, json!({"title": "Slider", "brief": "Uno slider"}), &dir);
        (dir, job)
    }

    async fn call(def: &ToolDef, input: Value, job: &FakeJob) -> ToolOutcome {
        (def.run)(input, job).await
    }

    #[tokio::test]
    async fn write_demo_rejects_remote_resources_and_keeps_no_draft() {
        let (_dir, job) = job("demo-remote", "d-remote");
        let html = r#"<script src="https://cdn.example/x.js"></script>"#;

        let out = call(&WRITE_DEMO, json!({"html": html}), &job).await;

        assert!(out.is_error, "{}", out.text);
        assert_eq!(
            out.text,
            format!(
                "HTML rifiutato: {}. Rendilo autosufficiente e richiama write_demo.",
                check_demo(html).join("; ")
            )
        );
        assert!(out.text.contains("rete esterna"), "{}", out.text);
        assert!(drafts().lock().expect("draft lock").get("d-remote").is_none());
        assert!(job.activities().is_empty());
    }

    #[tokio::test]
    async fn write_demo_saves_the_html_and_counts_utf16_units() {
        let (_dir, job) = job("demo-write", "d-write");

        let out = call(&WRITE_DEMO, json!({"html": CLEAN}), &job).await;

        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            out.text,
            format!(
                "Demo salvata ({} caratteri). Chiama run_demo per provarla.",
                utf16_len(CLEAN)
            )
        );
        let drafts = drafts().lock().expect("draft lock");
        assert_eq!(drafts.get("d-write").expect("the draft").html, CLEAN);
        assert!(!drafts.get("d-write").expect("the draft").passed);
        drop(drafts);
        assert_eq!(job.activities(), ["scrive la demo"]);
    }

    #[tokio::test]
    async fn submit_demo_refuses_without_a_passing_run() {
        let (_dir, job) = job("demo-submit", "d-submit");

        assert_eq!(
            call(&SUBMIT_DEMO, json!({}), &job).await,
            ToolOutcome::error("Nessuna demo da consegnare.")
        );

        call(&WRITE_DEMO, json!({"html": "<p>ciao</p>"}), &job).await;
        assert_eq!(
            call(&SUBMIT_DEMO, json!({}), &job).await,
            ToolOutcome::error("Non si consegna ciò che non è stato provato: l'ultima versione non ha ancora superato run_demo senza errori.")
        );
        assert_eq!(job.finished(), None);
        assert!(drafts().lock().expect("draft lock").contains_key("d-submit"));
    }

    #[tokio::test]
    async fn a_new_write_invalidates_the_passing_run() {
        let (_dir, job) = job("demo-invalidate", "d-invalidate");
        call(&WRITE_DEMO, json!({"html": CLEAN}), &job).await;
        // A clean `run_demo` sets this; writing the flag keeps the browser out of this test.
        drafts()
            .lock()
            .expect("draft lock")
            .get_mut("d-invalidate")
            .expect("the draft")
            .passed = true;

        call(&WRITE_DEMO, json!({"html": CLEAN}), &job).await;

        assert!(call(&SUBMIT_DEMO, json!({}), &job).await.is_error);
        assert_eq!(job.finished(), None);
    }

    #[tokio::test]
    async fn run_demo_without_a_renderer_fails_cleanly() {
        let (_dir, job) = job("demo-no-renderer", "d-no-renderer");
        call(&WRITE_DEMO, json!({"html": CLEAN}), &job).await;

        let out = call(&RUN_DEMO, json!({}), &job).await;

        assert_eq!(out, ToolOutcome::error("Prova non riuscita: nessun renderer"));
        assert!(!drafts().lock().expect("draft lock")["d-no-renderer"].passed);
        assert_eq!(job.failed(), None, "a wiring gap is not the job's failure");
    }

    #[tokio::test]
    async fn run_demo_without_a_browser_fails_the_job() {
        let dir = temp_dir("demo-no-browser");
        let missing = dir.join("missing-browser.exe");
        let job = FakeJob::new("d-no-browser", json!({"title": "Slider", "brief": "b"}), &dir)
            .with_renderer(Arc::new(Renderer::new(Some(missing))));
        call(&WRITE_DEMO, json!({"html": CLEAN}), &job).await;

        let out = call(&RUN_DEMO, json!({}), &job).await;

        assert!(out.is_error, "{}", out.text);
        let failed = job.failed().expect("a missing browser fails the job");
        assert!(failed.contains("Microsoft Edge"), "{failed}");
        assert!(failed.contains("Google Chrome"), "{failed}");
        assert_eq!(out.text, format!("Prova non riuscita: {failed}"));
    }

    /// AC7 at the job level, with the browser the suite needs (never skipped).
    #[tokio::test]
    async fn an_erroring_demo_fails_and_a_clean_one_delivers() {
        let dir = temp_dir("demo-browser");
        let renderer = Arc::new(Renderer::new(Some(render::require_test_browser())));
        let job = FakeJob::new("d-browser", json!({"title": "Slider", "brief": "Uno slider"}), &dir)
            .with_renderer(Arc::clone(&renderer));

        call(&WRITE_DEMO, json!({"html": "<p id=a>x</p><script>throw new Error('boom')</script>"}), &job).await;
        let failed = call(&RUN_DEMO, json!({}), &job).await;
        assert!(failed.is_error, "{}", failed.text);
        assert!(failed.text.contains("boom"), "{}", failed.text);
        assert_eq!(failed.images.len(), 1);
        assert_eq!(failed.images[0].mime_type, "image/png");
        assert!(call(&SUBMIT_DEMO, json!({}), &job).await.is_error);

        call(&WRITE_DEMO, json!({"html": CLEAN}), &job).await;
        let passed = call(&RUN_DEMO, json!({}), &job).await;
        assert!(!passed.is_error, "{}", passed.text);
        assert!(passed.text.starts_with("PASSATO"), "{}", passed.text);
        assert_eq!(passed.images.len(), 1);

        assert_eq!(
            call(&SUBMIT_DEMO, json!({"notes": "Muovi lo slider."}), &job).await,
            ToolOutcome::ok("Consegnata. Chiudi qui con una riga.")
        );
        assert_eq!(
            job.finished(),
            Some(json!({"kind": "demo", "title": "Slider", "html": CLEAN, "notes": "Muovi lo slider."}))
        );

        renderer.close().await;
    }

    #[test]
    fn the_validator_speaks_zod() {
        assert_eq!(
            (WRITE_DEMO.validate)(&json!({})),
            Err(vec![
                "html: Invalid input: expected string, received undefined".to_string()
            ])
        );
        assert_eq!(
            (WRITE_DEMO.validate)(&json!({"html": ""})),
            Err(vec![
                "html: Too small: expected string to have >=1 characters".to_string()
            ])
        );
        assert_eq!(
            (WRITE_DEMO.validate)(&json!({"html": "x", "extra": 1})),
            Ok(json!({"html": "x"}))
        );
        assert_eq!(
            (RUN_DEMO.validate)(&json!([])),
            Err(vec![
                "(input): Invalid input: expected object, received array".to_string()
            ])
        );
        assert_eq!(
            (SUBMIT_DEMO.validate)(&json!({"notes": 1})),
            Err(vec![
                "notes: Invalid input: expected string, received number".to_string()
            ])
        );
        assert_eq!(
            (SUBMIT_DEMO.validate)(&json!({"notes": ""})),
            Ok(json!({"notes": ""}))
        );
    }

    #[test]
    fn the_role_is_the_demo_one() {
        let dir = temp_dir("demo-role");
        let graph = GraphStore::new(dir.to_path_buf());
        let code_root = repo_root();
        let env = RoleEnv {
            code_root: &code_root,
            data_root: &dir,
            graph: &graph,
        };

        assert_eq!(ROLE.kind, "demo");
        assert_eq!(ROLE.max_turns, 24);
        assert!(ROLE.grant.is_empty());
        assert_eq!(
            (ROLE.tools)(&json!({"title": "Slider"}))
                .iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>(),
            ["write_demo", "run_demo", "submit_demo"]
        );
        let prompt = (ROLE.system_prompt)(&env, &json!({})).expect("the demo prompt");
        assert!(prompt.contains("HTML"), "{prompt}");
        assert_eq!(
            (ROLE.first_message)(&env, &mut json!({"title": "Slider", "brief": "Uno slider"})),
            "Costruisci questa demo, titolo \"Slider\":\nUno slider"
        );
    }

    #[tokio::test]
    async fn run_demo_does_not_mark_draft_passed_if_replaced_during_run() {
        let dir = temp_dir("demo-replace-during");
        let renderer = Arc::new(Renderer::new(Some(render::require_test_browser())));
        let job = FakeJob::new("d-replace", json!({"title": "Slider", "brief": "Uno slider"}), &dir)
            .with_renderer(Arc::clone(&renderer));
        call(&WRITE_DEMO, json!({"html": CLEAN}), &job).await;

        let job_clone = FakeJob::new("d-replace", json!({"title": "Slider", "brief": "Uno slider"}), &dir);
        let run_handle = tokio::spawn(async move {
            call(&RUN_DEMO, json!({}), &job).await
        });

        // Wait until run_demo has opened a page (it is now running CLEAN).
        // Polling timeout allows sufficient time for cold browser launch in CI.
        let mut saw_open = false;
        for _ in 0..300 {
            if renderer.open_pages() > 0 {
                saw_open = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(saw_open, "expected page to be open during run_demo");

        // Overwrite draft with a new HTML before run_demo finishes and sets passed = true
        let new_html = "<!doctype html><body><p>nuovo</p></body>";
        call(&WRITE_DEMO, json!({"html": new_html}), &job_clone).await;
        assert!(
            renderer.open_pages() > 0,
            "run_demo should still be in flight after overwrite"
        );

        let out = run_handle.await.unwrap();
        assert!(!out.is_error, "clean run should pass: {}", out.text);

        // The draft in storage is now new_html, but passed must be false!
        let drafts = drafts().lock().expect("draft lock");
        let draft = drafts.get("d-replace").expect("draft exists");
        assert_eq!(draft.html, new_html);
        assert!(!draft.passed, "replaced draft must not be marked passed");
        drop(drafts);

        renderer.close().await;
    }
}

