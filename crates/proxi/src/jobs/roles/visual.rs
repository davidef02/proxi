//! Visual role (port of app/server/jobs/roles/visual.ts).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use base64::Engine as _;
use serde_json::{Map, Value};

use crate::jobs::render::RenderError;
use crate::jobs::sanitize::check_svg;
use crate::js;
use crate::sessions::utf16_len;
use crate::tools::util::{fail, string_object, Field};
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolImage, ToolOutcome};

use super::{error_text, not_a_job, read_prompt, Role, RoleEnv};

/// Per-job draft; lives in memory only for the minutes a job runs.
struct Draft {
    svg: String,
    /// A successful `render_svg` sets it, the next `write_svg` clears it: submit wants a looked-at draft.
    rendered: bool,
}

static DRAFTS: LazyLock<Mutex<HashMap<String, Draft>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn drafts() -> &'static Mutex<HashMap<String, Draft>> {
    &DRAFTS
}

const WRITE_SVG_FIELDS: &[Field] = &[Field {
    key: "svg",
    min: 1,
    required: true,
}];

const SUBMIT_VISUAL_FIELDS: &[Field] = &[Field {
    key: "caption",
    min: 0,
    required: false,
}];

/// The `write_svg` tool.
pub const WRITE_SVG: ToolDef = ToolDef {
    name: "write_svg",
    description: "Save the SVG draft (replaces the previous one). Rejected when it contains scripts, foreignObject, on* attributes, external references or a non-svg root.",
    schema: write_svg_schema,
    validate: validate_write_svg,
    run: run_write_svg,
    writes: false,
};

/// The `render_svg` tool.
pub const RENDER_SVG: ToolDef = ToolDef {
    name: "render_svg",
    description: "Render the current draft to a PNG and return it as an image, so you can look at it.",
    schema: render_svg_schema,
    validate: validate_empty,
    run: run_render_svg,
    writes: false,
};

/// The `submit_visual` tool.
pub const SUBMIT_VISUAL: ToolDef = ToolDef {
    name: "submit_visual",
    description: "Deliver the current draft. Requires that the latest draft has been rendered and looked at.",
    schema: submit_visual_schema,
    validate: validate_submit_visual,
    run: run_submit_visual,
    writes: false,
};

fn validate_write_svg(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, WRITE_SVG_FIELDS)
}

/// `z.object({})`: any object cleans to `{}`, unknown keys dropped.
fn validate_empty(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, &[])
}

fn validate_submit_visual(input: &Value) -> Result<Value, Vec<String>> {
    string_object(input, SUBMIT_VISUAL_FIELDS)
}

fn write_svg_schema() -> Value {
    serde_json::from_str(include_str!("../../tools/schemas/write_svg.json"))
        .expect("the write_svg schema is valid JSON")
}

fn render_svg_schema() -> Value {
    serde_json::from_str(include_str!("../../tools/schemas/render_svg.json"))
        .expect("the render_svg schema is valid JSON")
}

fn submit_visual_schema() -> Value {
    serde_json::from_str(include_str!("../../tools/schemas/submit_visual.json"))
        .expect("the submit_visual schema is valid JSON")
}

fn run_write_svg<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(async move {
        let Some(ctx) = host.job() else {
            return not_a_job();
        };
        // Only the validated input reaches `run`, so `svg` is a string.
        let svg = input.get("svg").and_then(Value::as_str).unwrap_or("");
        let errors = check_svg(svg);
        if !errors.is_empty() {
            return fail(format!(
                "SVG rifiutato: {}. Correggi e richiama write_svg.",
                errors.join("; ")
            ));
        }
        drafts().lock().expect("draft lock").insert(
            ctx.job_id().to_string(),
            Draft {
                svg: js::js_trim(svg).to_string(),
                rendered: false,
            },
        );
        ctx.set_activity("scrive l'SVG");
        ToolOutcome::ok(format!(
            "Bozza salvata ({} caratteri). Chiama render_svg per guardarla.",
            utf16_len(svg)
        ))
    })
}

fn mark_rendered(job_id: &str, svg: &str) -> bool {
    let mut drafts = drafts().lock().expect("draft lock");
    if let Some(draft) = drafts.get_mut(job_id) {
        if draft.svg == svg {
            draft.rendered = true;
            return true;
        }
    }
    false
}

fn run_render_svg<'a>(_input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(async move {
        let Some(ctx) = host.job() else {
            return not_a_job();
        };
        let job_id = ctx.job_id().to_string();
        let svg = {
            let drafts = drafts().lock().expect("draft lock");
            match drafts.get(&job_id) {
                Some(draft) => draft.svg.clone(),
                None => return fail("Nessuna bozza: chiama prima write_svg."),
            }
        };
        ctx.set_activity("render SVG");
        let Some(renderer) = ctx.renderer() else {
            return fail("Rendering non riuscito: nessun renderer");
        };
        match renderer.render_svg_png(&job_id, &svg).await {
            Ok(png) => {
                mark_rendered(&job_id, &svg);
                ToolOutcome::ok("Ecco il rendering della bozza corrente su fondo scuro. Guardalo: se qualcosa non va, correggi con write_svg; altrimenti submit_visual.")
                    .with_images(vec![ToolImage {
                        data: base64::engine::general_purpose::STANDARD.encode(png),
                        mime_type: "image/png".to_string(),
                    }])
            }
            // No browser kills the job, not just the call: the teacher reads the reason in the chat.
            Err(RenderError::NoBrowser(message)) => {
                ctx.fail(&message);
                fail(format!("Rendering non riuscito: {message}"))
            }
            Err(error) => fail(format!("Rendering non riuscito: {}", error_text(error))),
        }
    })
}

fn run_submit_visual<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(async move {
        let Some(ctx) = host.job() else {
            return not_a_job();
        };
        let job_id = ctx.job_id().to_string();
        let svg = {
            let drafts = drafts().lock().expect("draft lock");
            match drafts.get(&job_id) {
                Some(draft) if draft.rendered => draft.svg.clone(),
                Some(_) => return fail("L'ultima bozza non è stata ancora guardata: chiama render_svg, controlla l'immagine e poi consegna."),
                None => return fail("Nessuna bozza da consegnare."),
            }
        };
        // Consumed only now, after both checks, like Node's `delete` after them.
        drafts().lock().expect("draft lock").remove(&job_id);
        ctx.set_activity("consegna la figura");
        let mut result = Map::new();
        result.insert("kind".to_string(), Value::String("visual".to_string()));
        result.insert("svg".to_string(), Value::String(svg));
        // `caption?.trim() || undefined`: an empty caption is no key at all.
        let caption = input.get("caption").and_then(Value::as_str).unwrap_or("");
        let caption = js::js_trim(caption);
        if !caption.is_empty() {
            result.insert("caption".to_string(), Value::String(caption.to_string()));
        }
        ctx.finish(Value::Object(result));
        ToolOutcome::ok("Consegnata. Chiudi qui con una riga.")
    })
}

fn system_prompt(env: &RoleEnv, _input: &Value) -> Result<String, String> {
    read_prompt(env.code_root, "visual.md")
}

fn first_message(_env: &RoleEnv, input: &mut Value) -> String {
    format!(
        "Disegna questa figura:\n{}",
        js::js_string(input.get("brief"))
    )
}

fn tools(_input: &Value) -> Vec<&'static ToolDef> {
    vec![&WRITE_SVG, &RENDER_SVG, &SUBMIT_VISUAL]
}

fn cleanup(job_id: &str) {
    drafts().lock().expect("draft lock").remove(job_id);
}

pub static ROLE: Role = Role {
    kind: "visual",
    grant: &[],
    max_turns: 14,
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
    use crate::jobs::render::Renderer;
    use crate::jobs::roles::FakeJob;
    use crate::tools::fake::temp_dir;

    const OK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><circle r="4"/></svg>"#;

    async fn call(def: &ToolDef, input: Value, job: &FakeJob) -> ToolOutcome {
        (def.run)(input, job).await
    }

    fn fake(tag: &str, id: &str) -> (crate::tools::fake::TempDir, FakeJob) {
        let dir = temp_dir(tag);
        let job = FakeJob::new(id, json!({"brief": "un albero con tre nodi"}), &dir);
        (dir, job)
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    #[tokio::test]
    async fn write_svg_rejects_a_hostile_svg_and_keeps_no_draft() {
        let (_dir, job) = fake("visual-hostile", "v-hostile");
        let svg = r#"<svg onload="x()"></svg>"#;

        let out = call(&WRITE_SVG, json!({"svg": svg}), &job).await;

        assert!(out.is_error);
        assert_eq!(
            out.text,
            format!(
                "SVG rifiutato: {}. Correggi e richiama write_svg.",
                check_svg(svg).join("; ")
            )
        );
        assert!(drafts().lock().expect("draft lock").get("v-hostile").is_none());
    }

    #[tokio::test]
    async fn write_svg_saves_the_draft_trimmed_and_counts_utf16_units() {
        let (_dir, job) = fake("visual-write", "v-write");
        let svg = format!("  {OK_SVG}\n");

        let out = call(&WRITE_SVG, json!({"svg": svg}), &job).await;

        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            out.text,
            format!(
                "Bozza salvata ({} caratteri). Chiama render_svg per guardarla.",
                utf16_len(&svg)
            )
        );
        assert_eq!(
            drafts().lock().expect("draft lock").get("v-write").expect("the draft").svg,
            OK_SVG
        );
        assert_eq!(job.activities(), ["scrive l'SVG"]);
    }

    #[tokio::test]
    async fn submit_visual_needs_a_rendered_draft() {
        let (_dir, job) = fake("visual-submit", "v-submit");

        assert_eq!(
            call(&SUBMIT_VISUAL, json!({}), &job).await,
            ToolOutcome::error("Nessuna bozza da consegnare.")
        );

        call(&WRITE_SVG, json!({"svg": OK_SVG}), &job).await;
        assert_eq!(
            call(&SUBMIT_VISUAL, json!({"caption": "c"}), &job).await,
            ToolOutcome::error(
                "L'ultima bozza non è stata ancora guardata: chiama render_svg, controlla l'immagine e poi consegna."
            )
        );
        assert_eq!(job.finished(), None);
        assert!(
            drafts().lock().expect("draft lock").contains_key("v-submit"),
            "a refused submit keeps the draft"
        );
    }

    #[tokio::test]
    async fn a_new_write_invalidates_the_render() {
        let (_dir, job) = fake("visual-invalidate", "v-invalidate");
        call(&WRITE_SVG, json!({"svg": OK_SVG}), &job).await;
        // What a render sets; simulating it keeps the browser out of this test.
        drafts()
            .lock()
            .expect("draft lock")
            .get_mut("v-invalidate")
            .expect("the draft")
            .rendered = true;

        call(&WRITE_SVG, json!({"svg": OK_SVG}), &job).await;

        assert!(call(&SUBMIT_VISUAL, json!({}), &job).await.is_error);
        assert_eq!(job.finished(), None);
    }

    #[tokio::test]
    async fn render_svg_without_a_renderer_fails_cleanly() {
        let (_dir, job) = fake("visual-no-renderer", "v-no-renderer");
        call(&WRITE_SVG, json!({"svg": OK_SVG}), &job).await;

        let out = call(&RENDER_SVG, json!({}), &job).await;

        assert_eq!(
            out,
            ToolOutcome::error("Rendering non riuscito: nessun renderer")
        );
        let drafts = drafts().lock().expect("draft lock");
        assert!(!drafts.get("v-no-renderer").expect("the draft").rendered);
    }

    #[tokio::test]
    async fn render_svg_without_a_browser_fails_the_job() {
        let dir = temp_dir("visual-no-browser");
        let missing = dir.join("missing-browser.exe");
        let job = FakeJob::new("v-no-browser", json!({"brief": "un albero"}), &dir)
            .with_renderer(Arc::new(Renderer::new(Some(missing))));
        call(&WRITE_SVG, json!({"svg": OK_SVG}), &job).await;

        let out = call(&RENDER_SVG, json!({}), &job).await;

        assert!(out.is_error);
        let failed = job.failed().expect("the missing browser fails the job");
        assert!(failed.contains("Microsoft Edge"), "{failed}");
        assert!(failed.contains("Google Chrome"), "{failed}");
        assert_eq!(out.text, format!("Rendering non riuscito: {failed}"));
    }

    #[test]
    fn the_validators_speak_zod() {
        assert_eq!(
            validate_write_svg(&json!({})),
            Err(vec![
                "svg: Invalid input: expected string, received undefined".to_string()
            ])
        );
        assert_eq!(
            validate_write_svg(&json!({"svg": ""})),
            Err(vec![
                "svg: Too small: expected string to have >=1 characters".to_string()
            ])
        );
        assert_eq!(
            validate_write_svg(&json!({"svg": 1})),
            Err(vec![
                "svg: Invalid input: expected string, received number".to_string()
            ])
        );
        assert_eq!(
            validate_empty(&json!([])),
            Err(vec![
                "(input): Invalid input: expected object, received array".to_string()
            ])
        );
        assert_eq!(
            validate_submit_visual(&json!({"caption": 1})),
            Err(vec![
                "caption: Invalid input: expected string, received number".to_string()
            ])
        );
        // Unknown keys dropped, `caption: ""` kept: what Node stores and the tool receives.
        assert_eq!(
            validate_submit_visual(&json!({"extra": 1, "caption": ""})),
            Ok(json!({"caption": ""}))
        );
        assert_eq!(validate_write_svg(&json!({"svg": "x", "extra": 1})), Ok(json!({"svg": "x"})));
        assert_eq!(validate_empty(&json!({"x": 1})), Ok(json!({})));
    }

    #[test]
    fn the_role_is_the_visual_one() {
        let dir = temp_dir("visual-role");
        let graph = GraphStore::new(dir.to_path_buf());
        let env = RoleEnv {
            code_root: &repo_root(),
            data_root: &dir,
            graph: &graph,
        };

        assert_eq!(ROLE.kind, "visual");
        assert_eq!(ROLE.max_turns, 14);
        assert!(ROLE.grant.is_empty());
        assert_eq!(
            (ROLE.tools)(&json!({}))
                .iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>(),
            ["write_svg", "render_svg", "submit_visual"]
        );
        let prompt = (ROLE.system_prompt)(&env, &json!({})).expect("the visual prompt");
        assert!(prompt.starts_with('#'), "{prompt}");
        assert!(prompt.contains("SVG"), "{prompt}");
        assert_eq!(
            (ROLE.first_message)(&env, &mut json!({"brief": "un albero"})),
            "Disegna questa figura:\nun albero"
        );

        (ROLE.cleanup)("v-role");
    }

    #[tokio::test]
    async fn write_render_and_submit_delivers_the_svg() {
        let dir = temp_dir("visual-browser");
        let renderer = Arc::new(Renderer::new(Some(crate::jobs::render::require_test_browser())));
        let job = FakeJob::new("v-browser", json!({"brief": "un riquadro"}), &dir)
            .with_renderer(Arc::clone(&renderer));
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 60" width="240" height="120"><rect x="5" y="5" width="110" height="50" fill="none" stroke="#e6e6e6"/><text x="20" y="35" fill="#e6e6e6">ref</text></svg>"##;

        let out = call(&WRITE_SVG, json!({"svg": svg}), &job).await;
        assert!(!out.is_error, "{}", out.text);
        let out = call(&RENDER_SVG, json!({}), &job).await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].mime_type, "image/png");
        let png = base64::engine::general_purpose::STANDARD
            .decode(out.images[0].data.as_str())
            .expect("the image is base64");
        assert_eq!(&png[1..4], b"PNG", "not a png: {:?}", &png[..png.len().min(8)]);

        let out = call(&SUBMIT_VISUAL, json!({"caption": "Un riquadro."}), &job).await;
        assert_eq!(out, ToolOutcome::ok("Consegnata. Chiudi qui con una riga."));
        assert_eq!(
            job.finished(),
            Some(json!({"kind": "visual", "svg": svg, "caption": "Un riquadro."}))
        );
        assert!(drafts().lock().expect("draft lock").get("v-browser").is_none());

        // A new write invalidates the render: submit refuses again.
        call(&WRITE_SVG, json!({"svg": svg}), &job).await;
        assert!(call(&SUBMIT_VISUAL, json!({}), &job).await.is_error);
        assert_eq!(job.finished(), Some(json!({"kind": "visual", "svg": svg, "caption": "Un riquadro."})));

        renderer.close().await;
    }

    #[tokio::test]
    async fn render_svg_does_not_mark_draft_rendered_if_replaced_during_render() {
        let (_dir, job) = fake("visual-replace-during", "v-replace");
        call(&WRITE_SVG, json!({"svg": OK_SVG}), &job).await;

        // Snapshot the svg as render_svg does before invoking the renderer
        let rendered_svg = OK_SVG;

        // Overwrite draft with a new SVG before render_svg finishes and sets rendered = true
        let new_svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><rect width="10" height="10"/></svg>"#;
        call(&WRITE_SVG, json!({"svg": new_svg}), &job).await;

        // Applying the post-render flag step with the old SVG must not mark the new draft rendered
        let marked = mark_rendered("v-replace", rendered_svg);
        assert!(!marked, "replaced draft must not be marked rendered");

        let guard = drafts().lock().expect("draft lock");
        let draft = guard.get("v-replace").expect("draft exists");
        assert_eq!(draft.svg, new_svg);
        assert!(!draft.rendered, "replaced draft must not be marked rendered");
        drop(guard);

        // Confirm that mark_rendered does mark rendered when the svg matches
        let marked_matching = mark_rendered("v-replace", new_svg);
        assert!(marked_matching, "matching draft must be marked rendered");
        assert!(drafts().lock().expect("draft lock").get("v-replace").unwrap().rendered);
    }
}
