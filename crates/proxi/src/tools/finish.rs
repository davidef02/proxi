//! The `finish_lesson` tool (port of app/server/tools/finish.ts and its `syncLesson`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::anki::sync::{self, SyncOptions};
use crate::anki_client;
use crate::js;
use crate::lesson::artifact;
use crate::lesson::reference::{self, CompileReferenceOptions};
use crate::settings;
use crate::slug;
use crate::tools::util::{self, course_dir, current_lesson, fail, to_rel};
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolOutcome};

const DESCRIPTION: &str = "Close the lesson: compile the reference sheet, write the learning record (for the teacher), optionally append to NOTES.md and sync Anki. If the lesson already has its record, only the reference sheet is refreshed (and Anki resynced).";

/// The five input fields, in Node's schema order; the last one is optional.
const FIELDS: [(&str, bool); 5] = [
    ("recordTitle", true),
    ("summary", true),
    ("evidence", true),
    ("implications", true),
    ("notesAddendum", false),
];

pub const TOOL: ToolDef = ToolDef {
    name: "finish_lesson",
    description: DESCRIPTION,
    schema: schema,
    validate: validate,
    run: run,
    writes: true,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("schemas/finish_lesson.json"))
        .expect("the finish_lesson schema")
}

/// zod's `safeParse`: the cleaned object in schema key order, or one `"<path>: <message>"` per issue.
fn validate(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![format!(
            "(input): Invalid input: expected object, received {}",
            js_type(input)
        )]);
    };
    let mut issues = Vec::new();
    let mut out = Map::new();
    for (key, required) in FIELDS {
        match object.get(key) {
            None if required => {
                issues.push(format!(
                    "{key}: Invalid input: expected string, received undefined"
                ));
            }
            None => {}
            Some(Value::String(text)) => {
                if required && text.is_empty() {
                    issues.push(format!(
                        "{key}: Too small: expected string to have >=1 characters"
                    ));
                } else {
                    out.insert(key.to_string(), Value::String(text.clone()));
                }
            }
            Some(value) => issues.push(format!(
                "{key}: Invalid input: expected string, received {}",
                js_type(value)
            )),
        }
    }
    if issues.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(issues)
    }
}

/// A JSON value's `typeof` as zod names it.
fn js_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn run<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(run_tool(input, host))
}

async fn run_tool(input: Value, host: &dyn ToolHost) -> ToolOutcome {
    let course = match course_dir(host) {
        Ok(dir) => dir,
        Err(message) => return fail(message),
    };
    let lesson = current_lesson(host);
    let abs = lesson.as_ref().map(|lesson| lesson.abs.clone());
    let config = host.config();
    if js::js_string(config.get("kind")) == "teach" && abs.is_none() {
        return fail(
            "No lesson artifact yet: nothing to finish. Get the plan approved with propose_plan first.",
        );
    }

    let rec_dir = course.join("learning-records");
    if let Err(error) = fs::create_dir_all(&rec_dir) {
        return fail(error.to_string());
    }
    // The record's last line names the lesson (empty for plan/practice sessions), so a second
    // finish_lesson for it only refreshes the reference sheet.
    let lesson_slug = lesson
        .as_ref()
        .and_then(|lesson| lesson.summary.slug.as_ref())
        .map(|slug| js::js_string(Some(slug)))
        .unwrap_or_default();
    let marker = artifact::lesson_marker(&lesson_slug);
    let done = lesson.as_ref().and_then(|lesson| lesson.record.clone());

    let mut result = Map::new();
    result.insert("recordPath".to_string(), json!(""));
    result.insert("notesUpdated".to_string(), json!(false));
    let mut record_path = done.clone().unwrap_or_default();

    if done.is_none() {
        let title = js::js_string(input.get("recordTitle"));
        let body = format!(
            "# {title}\n\n{}\n\n## Evidence\n\n{}\n\n## Implications\n\n{}\n\n{marker}\n",
            js::js_string(input.get("summary")),
            js::js_string(input.get("evidence")),
            js::js_string(input.get("implications")),
        );
        match create_record(&rec_dir, abs.as_deref(), &title, &body) {
            Ok(path) => record_path = to_rel(host.data_root(), &path),
            Err(message) => return fail(message),
        }
    }
    result.insert("recordPath".to_string(), json!(record_path));

    if let Some(abs) = &abs {
        result.insert(
            "lessonPath".to_string(),
            json!(to_rel(host.data_root(), abs)),
        );
        let compiled = reference::compile_reference(&CompileReferenceOptions {
            lesson_path: Some(abs.to_string_lossy().into_owned()),
            slug: None,
            output_dir: Some(course.join("reference").to_string_lossy().into_owned()),
            base_dir: Some(host.data_root().to_string_lossy().into_owned()),
            // Node resolves the record path against the data root; empty means "no record".
            record_path: match record_path.is_empty() {
                true => None,
                false => Some(
                    host.data_root()
                        .join(&record_path)
                        .to_string_lossy()
                        .into_owned(),
                ),
            },
        });
        match compiled {
            Ok(reference) => {
                let file = js::js_string(reference.get("filePath"));
                result.insert(
                    "referencePath".to_string(),
                    json!(to_rel(host.data_root(), Path::new(&file))),
                );
            }
            Err(message) => return fail(message),
        }
    }

    let anki = js::truthy(config.get("ankiOptIn")) && abs.is_some();
    if done.is_some() {
        if anki {
            let summary = sync_lesson(host).await;
            result.insert("anki".to_string(), json!(summary));
        }
        auto_map(host, abs.as_deref());
        host.emit(json!({ "kind": "finish", "result": result.clone() }));
        let message = match result.contains_key("anki") {
            true => "already closed: reference refreshed, Anki resynced",
            false => "already closed: reference refreshed",
        };
        result.insert("message".to_string(), json!(message));
        return util::json(&Value::Object(result));
    }

    let add = match input.get("notesAddendum") {
        Some(Value::String(text)) => js::js_trim(text).to_string(),
        _ => String::new(),
    };
    if !add.is_empty() {
        let notes = course.join("NOTES.md");
        let prev = match fs::read_to_string(&notes) {
            Ok(text) => text.trim_end_matches(js::is_js_ws).to_string(),
            Err(_) => String::new(),
        };
        if !prev.contains(&add) {
            let body = match prev.is_empty() {
                true => format!("{add}\n"),
                false => format!("{prev}\n\n{add}\n"),
            };
            if let Err(error) = fs::write(&notes, body) {
                return fail(error.to_string());
            }
            result.insert("notesUpdated".to_string(), json!(true));
        }
    }

    if anki {
        let summary = sync_lesson(host).await;
        result.insert("anki".to_string(), json!(summary));
    }
    auto_map(host, abs.as_deref());
    host.emit(json!({ "kind": "finish", "result": result.clone() }));
    util::json(&Value::Object(result))
}

/// Node's numbering loop: the lesson's own number when it is free, else `max + 1`, retried like `wx`.
fn create_record(
    rec_dir: &Path,
    abs: Option<&Path>,
    title: &str,
    body: &str,
) -> Result<PathBuf, String> {
    let lesson_no = abs
        .and_then(|abs| abs.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .and_then(|name| numbered(&name))
        .unwrap_or(0);
    let slug = slug::slugify(title);
    let slug = match slug.is_empty() {
        true => "record".to_string(),
        false => slug,
    };
    let mut tries = 0usize;
    loop {
        let mut files = Vec::new();
        for entry in fs::read_dir(rec_dir).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            files.push(entry.file_name().to_string_lossy().into_owned());
        }
        let max = files
            .iter()
            .filter_map(|name| numbered(name))
            .max()
            .unwrap_or(0);
        let taken = files
            .iter()
            .any(|name| name.starts_with(&format!("{lesson_no:04}-")));
        let n = match tries == 0 && lesson_no != 0 && !taken {
            true => lesson_no,
            false => max + 1,
        };
        let candidate = rec_dir.join(format!("{n:04}-{slug}.md"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(body.as_bytes())
                    .map_err(|error| error.to_string())?;
                return Ok(candidate);
            }
            // Another writer took the name between readdir and open: Node retries a bounded few times.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && tries <= 20 => {
                tries += 1;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// `^\d{4}-` as a number, like Node's `Number(f.slice(0, 4))`.
fn numbered(name: &str) -> Option<usize> {
    let prefix = name.get(..5)?;
    let digits = prefix.get(..4)?;
    if prefix.as_bytes()[4] != b'-' || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Map trigger: a lesson just closed. Failures never fail the close; they become a notice.
fn auto_map(host: &dyn ToolHost, abs: Option<&Path>) {
    let Some(abs) = abs else {
        return;
    };
    let config = host.config();
    if js::js_string(config.get("kind")) != "teach"
        || !settings::read_settings(host.repo_root()).auto_map
    {
        return;
    }
    let course = js::js_string(config.get("course"));
    let stem = abs
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Err(message) = host.start_map(&format!("{course}/{stem}")) {
        host.emit(json!({
            "kind": "notice",
            "level": "info",
            "text": format!("Mappa non aggiornata automaticamente: {message}"),
        }));
    }
}

/// Node's `syncLesson`: never fails, its text is the `anki` field of the result.
async fn sync_lesson(host: &dyn ToolHost) -> String {
    let Some(rel) = host.lesson_path() else {
        return "No lesson artifact: nothing to sync.".to_string();
    };
    // The lesson was read just above, so an unresolvable path cannot happen here.
    let Ok(abs) = artifact::lesson_abs(host.data_root(), &rel) else {
        return "No lesson artifact: nothing to sync.".to_string();
    };
    let options = SyncOptions {
        lesson_path: Some(abs.to_string_lossy().into_owned()),
        base_dir: Some(host.data_root().to_string_lossy().into_owned()),
        anki_url: std::env::var(anki_client::ANKI_URL_ENV)
            .ok()
            .filter(|url| !url.is_empty()),
        ..Default::default()
    };
    // AnkiConnect is a blocking client: it must not run on a tokio worker.
    let synced = tokio::task::spawn_blocking(move || {
        sync::sync_to_anki_connect(&options, &mut std::io::sink())
    })
    .await;
    let result = match synced {
        Ok(Ok(result)) => result,
        Ok(Err(message)) => return format!("Anki sync failed: {message}"),
        Err(error) => return format!("Anki sync failed: {error}"),
    };
    let tsv = match result.tsv_export_path.as_deref() {
        Some(path) => format!(" TSV: {}.", to_rel(host.data_root(), Path::new(path))),
        None => String::new(),
    };
    if !result.synced {
        let reason = result
            .warning
            .or(result.error)
            .unwrap_or_else(|| "unknown".to_string());
        return format!(
            "Anki offline or sync incomplete ({reason}); cards kept in the deck TSV.{tsv}"
        );
    }
    format!(
        "Anki deck \"{}\": {} added, {} duplicates skipped, {} failed ({} cards).{tsv}",
        result.deck_name,
        result.added_count,
        result.duplicate_count,
        result.failed_count,
        result.total_cards
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson::doc;
    use crate::tools::fake::FakeHost;

    const DAG: &str =
        "graph TD\n  N1[\"Nodo 1: la prima cosa è vera\"] --> N2[\"Nodo 2: la seconda segue\"]";

    fn teach(anki_opt_in: bool) -> Value {
        json!({
            "kind": "teach",
            "course": "demo",
            "model": "haiku",
            "ankiOptIn": anki_opt_in,
            "target": {"mode": "planned", "title": "Il primo passo"},
        })
    }

    fn fin() -> Value {
        json!({
            "recordTitle": "Il primo passo: acquisito",
            "summary": "Sa fare X.",
            "evidence": "Quiz ok.",
            "implications": "Procedere.",
        })
    }

    /// Node's `approvedLesson()` + `nodeIn()`: an approved lesson with one written node.
    fn approved_lesson(config: Value) -> FakeHost {
        let host = FakeHost::with_lesson(config, DAG);
        let abs = host.data_root().join(host.lesson_path().expect("a lesson"));
        doc::append_node(doc::AppendOptions {
            lesson_path: Some(abs.to_string_lossy().into_owned()),
            slug: None,
            node: json!({
                "nodeId": "prima-cosa",
                "title": "La prima cosa",
                "tier": "unconditional-truth",
                "content": "<p>vero</p>",
            }),
            base_dir: Some(host.data_root().to_string_lossy().into_owned()),
        })
        .expect("append one node");
        host
    }

    #[test]
    fn validate_reports_missing_fields_and_keeps_the_schema_order() {
        let issues = validate(&json!({"recordTitle": "", "summary": 5})).expect_err("invalid");
        assert_eq!(
            issues,
            vec![
                "recordTitle: Too small: expected string to have >=1 characters".to_string(),
                "summary: Invalid input: expected string, received number".to_string(),
                "evidence: Invalid input: expected string, received undefined".to_string(),
                "implications: Invalid input: expected string, received undefined".to_string(),
            ]
        );

        let cleaned = validate(&json!({
            "implications": "i",
            "recordTitle": "r",
            "summary": "s",
            "evidence": "e",
            "notesAddendum": "n",
            "extra": true,
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&cleaned).unwrap(),
            r#"{"recordTitle":"r","summary":"s","evidence":"e","implications":"i","notesAddendum":"n"}"#
        );
        assert!(validate(&json!("x")).expect_err("not an object")[0].starts_with("(input): "));
    }

    #[tokio::test]
    async fn needs_a_lesson_in_teach_sessions() {
        let host = FakeHost::new(teach(false));
        let out = host.call(&TOOL, fin()).await;
        assert!(out.is_error);
        assert_eq!(
            out.text,
            "No lesson artifact yet: nothing to finish. Get the plan approved with propose_plan first."
        );
    }

    #[tokio::test]
    async fn compiles_the_reference_numbers_the_record_and_appends_notes() {
        let host = approved_lesson(teach(false));
        let learning = host.data_root().join("courses/demo/learning-records");
        fs::create_dir_all(&learning).expect("create learning-records");
        fs::write(learning.join("0007-old.md"), "x").expect("seed a record");

        let mut input = fin();
        input["notesAddendum"] = json!("Gli piacciono gli esempi.");
        let out = host.call(&TOOL, input).await;
        assert!(!out.is_error, "{}", out.text);

        let result: Value = serde_json::from_str(&out.text).expect("a JSON result");
        assert_eq!(
            result["recordPath"],
            json!("courses/demo/learning-records/0001-il-primo-passo-acquisito.md")
        );
        assert_eq!(
            result["referencePath"],
            json!("courses/demo/reference/il-primo-passo.html")
        );
        assert_eq!(result["notesUpdated"], json!(true));
        assert_eq!(
            result["lessonPath"],
            json!("courses/demo/lessons/0001-il-primo-passo.html")
        );
        assert!(result.get("anki").is_none(), "no opt-in, no sync");
        assert!(
            host.data_root()
                .join("courses/demo/reference/il-primo-passo.html")
                .exists()
        );

        let record = fs::read_to_string(
            host.data_root()
                .join("courses/demo/learning-records/0001-il-primo-passo-acquisito.md"),
        )
        .expect("the record");
        assert_eq!(
            record,
            "# Il primo passo: acquisito\n\nSa fare X.\n\n## Evidence\n\nQuiz ok.\n\n## Implications\n\nProcedere.\n\n<!-- lesson: il-primo-passo -->\n"
        );

        let notes =
            fs::read_to_string(host.data_root().join("courses/demo/NOTES.md")).expect("NOTES.md");
        assert_eq!(
            notes.replace("\r\n", "\n"),
            "# Notes\n\nPrefers short answers.\n\nGli piacciono gli esempi.\n"
        );

        let items = host.items();
        assert_eq!(
            items.last().expect("one emitted item")["kind"],
            json!("finish")
        );
        assert_eq!(host.maps(), vec!["demo/0001-il-primo-passo".to_string()]);
    }

    #[tokio::test]
    async fn a_second_close_only_refreshes_the_reference_and_dedupes_the_notes_addendum() {
        let host = approved_lesson(teach(false));
        let mut input = json!({
            "recordTitle": "Il primo passo: acquisito",
            "summary": "s",
            "evidence": "e",
            "implications": "i",
        });
        input["notesAddendum"] = json!("Nota unica.");
        assert!(!host.call(&TOOL, input.clone()).await.is_error);

        let again = host.call(&TOOL, input).await;
        assert!(!again.is_error, "{}", again.text);
        let result: Value = serde_json::from_str(&again.text).expect("a JSON result");
        assert_eq!(
            result["message"],
            json!("already closed: reference refreshed")
        );
        assert_eq!(
            result["recordPath"],
            json!("courses/demo/learning-records/0001-il-primo-passo-acquisito.md")
        );
        let records =
            fs::read_dir(host.data_root().join("courses/demo/learning-records")).expect("the dir");
        assert_eq!(records.count(), 1, "a second close writes no record");

        let notes =
            fs::read_to_string(host.data_root().join("courses/demo/NOTES.md")).expect("NOTES.md");
        assert_eq!(notes.matches("Nota unica").count(), 1);
    }

    #[tokio::test]
    async fn numbering_falls_back_to_max_plus_one_when_the_lesson_number_is_taken() {
        let host = approved_lesson(teach(false));
        let learning = host.data_root().join("courses/demo/learning-records");
        fs::create_dir_all(&learning).expect("create learning-records");
        fs::write(
            learning.join("0001-other.md"),
            "x\n<!-- lesson: other -->\n",
        )
        .expect("seed a record");

        let out = host
            .call(
                &TOOL,
                json!({"recordTitle": "T", "summary": "s", "evidence": "e", "implications": "i"}),
            )
            .await;
        let result: Value = serde_json::from_str(&out.text).expect("a JSON result");
        assert_eq!(
            result["recordPath"],
            json!("courses/demo/learning-records/0002-t.md")
        );
    }

    #[tokio::test]
    async fn the_record_ends_with_the_marker_and_parse_marker_reads_the_slug() {
        let host = approved_lesson(teach(false));
        let out = host.call(&TOOL, fin()).await;
        let result: Value = serde_json::from_str(&out.text).expect("a JSON result");
        let record = fs::read_to_string(
            host.data_root()
                .join(js::js_string(result.get("recordPath"))),
        )
        .expect("the record");
        assert!(record.ends_with("<!-- lesson: il-primo-passo -->\n"));
        assert_eq!(
            artifact::parse_marker(&record).as_deref(),
            Some("il-primo-passo")
        );
    }

    #[tokio::test]
    async fn a_failing_auto_map_is_a_notice_not_a_failed_close() {
        let host = approved_lesson(teach(false));
        host.set_fail_jobs(true);
        let out = host.call(&TOOL, fin()).await;
        assert!(!out.is_error, "{}", out.text);
        let items = host.items();
        let notice = items
            .iter()
            .find(|item| item["kind"] == json!("notice"))
            .expect("the map failure became a notice");
        assert_eq!(
            notice["text"],
            json!(format!(
                "Mappa non aggiornata automaticamente: {}",
                crate::tools::NO_JOBS_MESSAGE
            ))
        );
        // Node emits the notice before the finish item, and the close still succeeds.
        assert_eq!(items.last().expect("one item")["kind"], json!("finish"));
    }

    #[tokio::test]
    async fn auto_map_off_starts_no_map_job() {
        let host = approved_lesson(teach(false));
        fs::create_dir_all(host.data_root().join(".learner")).expect("create .learner");
        fs::write(
            host.data_root().join(".learner/settings.json"),
            "{\"autoMap\": false}\n",
        )
        .expect("seed the settings");
        host.set_fail_jobs(true);
        let out = host.call(&TOOL, fin()).await;
        assert!(!out.is_error, "{}", out.text);
        assert!(host.maps().is_empty());
        assert_eq!(
            host.items().last().expect("one item")["kind"],
            json!("finish")
        );
    }
}
