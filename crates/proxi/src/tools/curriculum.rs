//! The `propose_curriculum` tool (port of app/server/tools/curriculum.ts).

use std::collections::{HashMap, HashSet};
use std::fs;

use serde_json::{Map, Value, json};

use crate::chapter;
use crate::course_data::valid_course;
use crate::js;
use crate::lesson::artifact;
use crate::sessions::{invalid_type, issue};
use crate::slug::lesson_slug;
use crate::tools::util::{self, fail};
use crate::tools::{Cancelled, ToolDef, ToolFuture, ToolHost, ToolOutcome};

/// Node's `proposeCurriculumTool`.
pub const TOOL: ToolDef = ToolDef {
    name: "propose_curriculum",
    description: "Show the learner the destination and ordered lesson list and wait for approval (blocks); on approval writes curriculum.json (and MISSION.md for a new course).",
    schema,
    validate,
    run,
    writes: true,
};

fn schema() -> Value {
    serde_json::from_str(include_str!("schemas/propose_curriculum.json"))
        .expect("the propose_curriculum schema parses")
}

const TOO_SMALL_ONE: &str = "Too small: expected string to have >=1 characters";
const MISSING_STRING: &str = "Invalid input: expected string, received undefined";
const BAD_SLUG: &str = r#"Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/"#;

/// Node's `schema`, hand-rolled: the cleaned data or one `"<path>: <message>"` per issue.
fn validate(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Vec::new();
    let mut data = Map::new();

    match object.get("chapter") {
        Some(Value::String(chapter)) if chapter.is_empty() => {
            issues.push(issue("chapter", TOO_SMALL_ONE));
        }
        Some(Value::String(chapter)) => {
            data.insert("chapter".to_string(), Value::String(chapter.clone()));
        }
        Some(other) => issues.push(issue("chapter", &invalid_type("string", Some(other)))),
        None => issues.push(issue("chapter", MISSING_STRING)),
    }

    // No kebab check here: Node leaves that to `run`, so a bad slug still asks zod's way first.
    match object.get("slug") {
        Some(Value::String(slug)) => {
            data.insert("slug".to_string(), Value::String(slug.clone()));
        }
        Some(other) => issues.push(issue("slug", &invalid_type("string", Some(other)))),
        None => issues.push(issue("slug", MISSING_STRING)),
    }

    match object.get("destination") {
        Some(Value::String(destination)) if destination.is_empty() => {
            issues.push(issue("destination", TOO_SMALL_ONE));
        }
        Some(Value::String(destination)) => {
            data.insert(
                "destination".to_string(),
                Value::String(destination.clone()),
            );
        }
        Some(other) => issues.push(issue("destination", &invalid_type("string", Some(other)))),
        None => issues.push(issue("destination", MISSING_STRING)),
    }

    if let Some(mission) = object.get("mission") {
        match mission {
            Value::String(_) => {
                data.insert("mission".to_string(), mission.clone());
            }
            other => issues.push(issue("mission", &invalid_type("string", Some(other)))),
        }
    }

    match object.get("lessons") {
        Some(Value::Array(items)) => {
            let mut clean = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let path = format!("lessons.{index}");
                match item.as_object() {
                    Some(lesson) => clean.push(read_lesson(lesson, &path, &mut issues)),
                    None => issues.push(issue(&path, &invalid_type("object", Some(item)))),
                }
            }
            // zod checks the items, then the minimum.
            if items.is_empty() {
                issues.push(issue(
                    "lessons",
                    "Too small: expected array to have >=1 items",
                ));
            }
            data.insert("lessons".to_string(), Value::Array(clean));
        }
        Some(other) => issues.push(issue("lessons", &invalid_type("array", Some(other)))),
        None => issues.push(issue(
            "lessons",
            "Invalid input: expected array, received undefined",
        )),
    }

    if issues.is_empty() {
        Ok(Value::Object(data))
    } else {
        Err(issues)
    }
}

fn read_lesson(lesson: &Map<String, Value>, path: &str, issues: &mut Vec<String>) -> Value {
    let mut clean = Map::new();
    let title_path = format!("{path}.title");
    match lesson.get("title") {
        Some(Value::String(title)) if title.is_empty() => {
            issues.push(issue(&title_path, TOO_SMALL_ONE));
        }
        Some(Value::String(title)) => {
            clean.insert("title".to_string(), Value::String(title.clone()));
        }
        Some(other) => issues.push(issue(&title_path, &invalid_type("string", Some(other)))),
        None => issues.push(issue(&title_path, MISSING_STRING)),
    }
    if let Some(planned) = lesson.get("plannedNodes") {
        let planned_path = format!("{path}.plannedNodes");
        if let Some(number) = planned_nodes(planned, &planned_path, issues) {
            // JS keeps 2.0 as 2: the plan must read the same either way.
            clean.insert("plannedNodes".to_string(), js::number(number));
        }
    }
    if let Some(slug) = lesson.get("slug") {
        let slug_path = format!("{path}.slug");
        match slug {
            Value::String(text) if valid_course(text) => {
                clean.insert("slug".to_string(), Value::String(text.clone()));
            }
            Value::String(_) => issues.push(issue(&slug_path, BAD_SLUG)),
            other => issues.push(issue(&slug_path, &invalid_type("string", Some(other)))),
        }
    }
    Value::Object(clean)
}

/// `z.number().int().positive()`: the aborting int check, then the two continue checks.
fn planned_nodes(value: &Value, path: &str, issues: &mut Vec<String>) -> Option<f64> {
    let Some(number) = value.as_f64() else {
        issues.push(issue(path, &invalid_type("number", Some(value))));
        return None;
    };
    if !number.is_finite() || number.fract() != 0.0 {
        issues.push(issue(path, "Invalid input: expected int, received number"));
        return None;
    }
    const MAX_SAFE: f64 = 9007199254740991.0;
    if number > MAX_SAFE {
        issues.push(issue(
            path,
            "Too big: expected int to be <=9007199254740991",
        ));
    } else if number < -MAX_SAFE {
        issues.push(issue(
            path,
            "Too small: expected int to be >=-9007199254740991",
        ));
    }
    if number <= 0.0 {
        issues.push(issue(path, "Too small: expected number to be >0"));
    }
    Some(number)
}

/// `^[a-z0-9]+(?:-[a-z0-9]+)*$` via the one course-slug rule.
fn is_kebab(slug: &str) -> bool {
    valid_course(slug)
}

fn run(input: Value, host: &dyn ToolHost) -> ToolFuture<'_> {
    Box::pin(curriculum(input, host))
}

async fn curriculum(input: Value, host: &dyn ToolHost) -> ToolOutcome {
    let slug = input
        .get("slug")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !is_kebab(&slug) {
        return fail(format!(
            "slug \"{slug}\" must be kebab-case (lowercase letters, digits, hyphens)."
        ));
    }
    let course = js::js_string(host.config().get("course"));
    if slug != course {
        return fail(format!(
            "slug \"{slug}\" must be this session's course \"{course}\"."
        ));
    }
    let chapter_title = input
        .get("chapter")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mission = input
        .get("mission")
        .and_then(Value::as_str)
        .map(js::js_trim)
        .unwrap_or("")
        .to_string();
    let lesson_count = input
        .get("lessons")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    let data_root = host.data_root();
    let file = chapter::curriculum_file(data_root, None);
    let old = match chapter::read_curriculum(&file) {
        Ok(curriculum) => curriculum
            .chapters
            .iter()
            .find(|chapter| chapter::chapter_slug(chapter) == slug)
            .map(chapter::planned_lessons)
            .unwrap_or_default(),
        Err(message) => return fail(message),
    };

    // Re-planning keeps a legacy slug of an already-taught lesson (matched by title), so the
    // lesson on disk stays claimed by the plan.
    let mut lessons: Vec<Value> = Vec::new();
    let mut slugs: HashMap<String, String> = HashMap::new();
    for lesson in input
        .get("lessons")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let title = lesson
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let given = lesson.get("slug").and_then(Value::as_str);
        let prev = old
            .iter()
            .find(|entry| js::js_trim(&entry.title) == js::js_trim(&title));
        let kept = prev
            .filter(|entry| entry.slug != lesson_slug(&title))
            .map(|entry| entry.slug.clone());
        let resolved = given.map(str::to_string).or(kept);

        let planned_slug = resolved.clone().unwrap_or_else(|| lesson_slug(&title));
        if planned_slug.is_empty() {
            return fail(format!("lesson title \"{title}\" produces an empty slug."));
        }
        if let Some(first) = slugs.get(&planned_slug) {
            return fail(format!(
                "lessons \"{first}\" and \"{title}\" share the slug \"{planned_slug}\": give them distinct titles."
            ));
        }
        slugs.insert(planned_slug, title.clone());

        let mut entry = Map::new();
        entry.insert("title".to_string(), Value::String(title));
        if let Some(planned) = lesson.get("plannedNodes") {
            entry.insert("plannedNodes".to_string(), planned.clone());
        }
        if let Some(resolved) = resolved {
            entry.insert("slug".to_string(), Value::String(resolved));
        }
        lessons.push(Value::Object(entry));
    }

    let dir = data_root.join("courses").join(&slug);
    let lessons_dir = dir.join("lessons");
    let mut taught: HashSet<String> = HashSet::new();
    if lessons_dir.exists() {
        let entries = match fs::read_dir(&lessons_dir) {
            Ok(entries) => entries,
            Err(error) => return fail(error.to_string()),
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".html") {
                taught.insert(artifact::slug_of_lesson(&name));
            }
        }
    }
    // A plan that drops a taught lesson orphans its file: the learner would lose it from the plan
    // with nothing left pointing at it.
    let dropped: Vec<&chapter::PlannedLesson> = old
        .iter()
        .filter(|entry| taught.contains(&entry.slug) && !slugs.contains_key(&entry.slug))
        .collect();
    if !dropped.is_empty() {
        let titles: Vec<String> = dropped
            .iter()
            .map(|entry| format!("\"{}\"", entry.title))
            .collect();
        return fail(format!(
            "The new plan drops already taught lessons: {}. Keep them with the exact same title (or their slug) in the same plan, then call again.",
            titles.join(", ")
        ));
    }

    let mission_file = dir.join("MISSION.md");
    if !dir.exists() && mission.is_empty() {
        return fail(format!(
            "courses/{slug} is new: mission (the full MISSION.md) is required."
        ));
    }

    let answer = match host.request(json!({ "kind": "curriculum", "input": input })) {
        Ok(receiver) => match receiver.await {
            Ok(Ok(answer)) => answer,
            _ => return fail(util::CANCELLED),
        },
        Err(Cancelled) => return fail(util::CANCELLED),
    };
    if !js::truthy(answer.get("approved")) {
        let feedback = match answer.get("feedback") {
            None | Some(Value::Null) => Value::String(String::new()),
            Some(value) => value.clone(),
        };
        let mut out = Map::new();
        out.insert("approved".to_string(), Value::Bool(false));
        out.insert("feedback".to_string(), feedback);
        return util::json(&Value::Object(out));
    }

    let mut notes: Vec<String> = Vec::new();
    if !dir.exists() {
        for name in ["lessons", "reference", "learning-records"] {
            if let Err(error) = fs::create_dir_all(dir.join(name)) {
                return fail(error.to_string());
            }
        }
    }
    if mission_file.exists() {
        // The mission is the learner's: rewrite it only when it is not there yet.
        if !mission.is_empty() {
            notes.push("MISSION.md already exists and was left untouched.".to_string());
        }
    } else if !mission.is_empty() {
        if let Err(error) = fs::create_dir_all(&dir) {
            return fail(error.to_string());
        }
        if let Err(error) = fs::write(&mission_file, format!("{mission}\n")) {
            return fail(error.to_string());
        }
    }
    if let Err(message) = chapter::plan_chapter(&file, &chapter_title, Some(&slug), &lessons) {
        return fail(message);
    }

    let mut out = Map::new();
    out.insert("approved".to_string(), Value::Bool(true));
    out.insert("course".to_string(), Value::String(slug));
    out.insert("lessons".to_string(), json!(lesson_count));
    if !notes.is_empty() {
        out.insert(
            "notes".to_string(),
            Value::Array(notes.into_iter().map(Value::String).collect()),
        );
    }
    util::json(&Value::Object(out))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::tools::fake::FakeHost;

    fn plan_config() -> Value {
        json!({"kind": "plan", "course": "demo", "model": "haiku", "chapterTitle": "Demo"})
    }

    fn nuovo_corso_config() -> Value {
        json!({"kind": "plan", "course": "nuovo-corso", "model": "haiku", "chapterTitle": "Nuovo corso"})
    }

    /// The re-planning fixture's card: chapter, slug and destination fixed, lessons given.
    fn cur(lessons: Value) -> Value {
        json!({"chapter": "Demo", "slug": "demo", "destination": "x", "lessons": lessons})
    }

    /// The new-course card, with `o` laid over it (a null value drops the key, like `undefined`).
    fn corso(o: Value) -> Value {
        let mut card = json!({
            "chapter": "Nuovo corso",
            "slug": "nuovo-corso",
            "destination": "Arrivare.",
            "mission": "# Mission\n\n## Why\n\nx",
            "lessons": [{"title": "Prima lezione", "plannedNodes": 3}, {"title": "Seconda lezione"}],
        });
        let object = card.as_object_mut().expect("a card");
        for (key, value) in o.as_object().expect("a patch object") {
            if value.is_null() {
                object.shift_remove(key);
            } else {
                object.insert(key.clone(), value.clone());
            }
        }
        card
    }

    /// `{...card, ...patch}`, keeping each key where the card put it.
    fn with(mut card: Value, patch: Value) -> Value {
        let object = card.as_object_mut().expect("a card");
        for (key, value) in patch.as_object().expect("a patch object") {
            object.insert(key.clone(), value.clone());
        }
        card
    }

    fn curriculum_file(host: &FakeHost) -> std::path::PathBuf {
        host.data_root().join("curriculum.json")
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("read the file")).expect("valid JSON")
    }

    #[tokio::test]
    async fn rejects_a_slug_other_than_the_session_course() {
        let host = FakeHost::new(plan_config());
        let out = host
            .call(
                &TOOL,
                with(cur(json!([{"title": "A"}])), json!({"slug": "altro"})),
            )
            .await;
        assert_eq!(
            out,
            ToolOutcome::error("slug \"altro\" must be this session's course \"demo\".")
        );
        assert!(host.requests().is_empty());
    }

    #[tokio::test]
    async fn fails_when_a_taught_lesson_would_disappear_and_keeps_legacy_slugs() {
        let host = FakeHost::new(plan_config());
        let file = curriculum_file(&host);
        let mut curriculum = read_json(&file);
        curriculum["chapters"][0]["lessons"][0] = json!({
            "title": "Un titolo molto lungo che non produce lo slug",
            "slug": "legacy",
            "plannedNodes": 2,
        });
        fs::write(&file, serde_json::to_string(&curriculum).unwrap()).expect("rewrite the plan");
        fs::write(
            host.data_root()
                .join("courses/demo/lessons/0001-legacy.html"),
            "<html></html>",
        )
        .expect("write the taught lesson");

        let bad = host.call(&TOOL, cur(json!([{"title": "Altro"}]))).await;
        assert!(bad.is_error, "{}", bad.text);
        assert_eq!(
            bad.text,
            "The new plan drops already taught lessons: \"Un titolo molto lungo che non produce lo slug\". Keep them with the exact same title (or their slug) in the same plan, then call again."
        );
        assert!(host.requests().is_empty(), "nothing asked before the check");

        host.queue_answer(json!({"approved": true}));
        let ok = host
            .call(
                &TOOL,
                cur(json!([
                    {"title": "Un titolo molto lungo che non produce lo slug"},
                    {"title": "Nuova"},
                ])),
            )
            .await;
        assert!(!ok.is_error, "{}", ok.text);
        let planned = read_json(&file);
        assert_eq!(
            planned["chapters"][0]["lessons"][0]["slug"],
            json!("legacy")
        );
        assert_eq!(planned["chapters"][0]["lessons"][1]["slug"], json!("nuova"));
    }

    #[tokio::test]
    async fn validates_before_asking() {
        let host = FakeHost::new(nuovo_corso_config());
        for bad in [
            corso(json!({"slug": "Bad Slug"})),
            corso(json!({"mission": null})),
            corso(json!({"lessons": []})),
            corso(json!({"lessons": [{"title": "Uno!"}, {"title": "uno"}]})),
        ] {
            let out = host.call(&TOOL, bad.clone()).await;
            assert!(out.is_error, "{}", serde_json::to_string(&bad).unwrap());
        }
        assert!(host.requests().is_empty());
    }

    #[tokio::test]
    async fn approval_creates_the_course_mission_and_the_plan() {
        let host = FakeHost::new(nuovo_corso_config());
        host.queue_answer(json!({"approved": true}));
        let out = host.call(&TOOL, corso(json!({}))).await;
        assert_eq!(
            out,
            ToolOutcome::ok(r#"{"approved":true,"course":"nuovo-corso","lessons":2}"#)
        );

        let dir = host.data_root().join("courses/nuovo-corso");
        for name in ["lessons", "reference", "learning-records"] {
            assert!(dir.join(name).is_dir(), "{name}");
        }
        assert!(
            fs::read_to_string(dir.join("MISSION.md"))
                .expect("the mission")
                .contains("## Why")
        );

        let curriculum = read_json(&curriculum_file(&host));
        let chapters = curriculum["chapters"].as_array().expect("chapters");
        assert_eq!(chapters.len(), 2);
        let chapter = chapters
            .iter()
            .find(|chapter| chapter["slug"] == json!("nuovo-corso"))
            .expect("the new chapter");
        assert_eq!(chapter["title"], json!("Nuovo corso"));
        assert_eq!(
            chapter["lessons"],
            json!([
                {"title": "Prima lezione", "slug": "prima-lezione", "plannedNodes": 3},
                {"title": "Seconda lezione", "slug": "seconda-lezione"},
            ])
        );
    }

    #[tokio::test]
    async fn never_overwrites_an_existing_mission() {
        let host = FakeHost::new(json!({
            "kind": "plan", "course": "demo", "model": "haiku", "chapterTitle": "Nuovo corso",
        }));
        host.queue_answer(json!({"approved": true}));
        let out = host
            .call(
                &TOOL,
                json!({
                    "chapter": "Demo",
                    "slug": "demo",
                    "destination": "Arrivare.",
                    "mission": "# Mission\n\n## Why\n\nx",
                    "lessons": [{"title": "Prima lezione", "plannedNodes": 3}, {"title": "Seconda lezione"}],
                }),
            )
            .await;
        assert_eq!(
            out,
            ToolOutcome::ok(
                r#"{"approved":true,"course":"demo","lessons":2,"notes":["MISSION.md already exists and was left untouched."]}"#
            )
        );
        assert!(
            fs::read_to_string(host.data_root().join("courses/demo/MISSION.md"))
                .expect("the mission")
                .contains("Test course")
        );
    }

    #[tokio::test]
    async fn rejection_writes_nothing() {
        let host = FakeHost::new(nuovo_corso_config());
        host.queue_answer(json!({"approved": false, "feedback": "no"}));
        let out = host.call(&TOOL, corso(json!({}))).await;
        assert_eq!(
            out,
            ToolOutcome::ok(r#"{"approved":false,"feedback":"no"}"#)
        );
        assert!(!host.data_root().join("courses/nuovo-corso").exists());
        assert!(
            !fs::read_to_string(curriculum_file(&host))
                .expect("the untouched plan")
                .contains("nuovo-corso")
        );

        // A rejection with no feedback answers the empty string, like `?? ""`.
        host.queue_answer(json!({"approved": false}));
        let again = host.call(&TOOL, corso(json!({}))).await;
        assert_eq!(
            again,
            ToolOutcome::ok(r#"{"approved":false,"feedback":""}"#)
        );
    }

    #[tokio::test]
    async fn a_cancelled_card_is_an_error_result() {
        let host = FakeHost::new(nuovo_corso_config());
        host.queue_cancel();
        let out = host.call(&TOOL, corso(json!({}))).await;
        assert_eq!(out, ToolOutcome::error(util::CANCELLED));
        assert!(!host.data_root().join("courses/nuovo-corso").exists());
    }

    #[tokio::test]
    async fn a_title_of_punctuation_produces_an_empty_slug() {
        let host = FakeHost::new(nuovo_corso_config());
        let out = host
            .call(&TOOL, corso(json!({"lessons": [{"title": "!!!"}]})))
            .await;
        assert_eq!(
            out,
            ToolOutcome::error("lesson title \"!!!\" produces an empty slug.")
        );
        assert!(host.requests().is_empty());
    }

    #[test]
    fn invalid_input_uses_zods_text() {
        assert_eq!(
            (TOOL.validate)(&json!({})),
            Err(vec![
                "chapter: Invalid input: expected string, received undefined".to_string(),
                "slug: Invalid input: expected string, received undefined".to_string(),
                "destination: Invalid input: expected string, received undefined".to_string(),
                "lessons: Invalid input: expected array, received undefined".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&cur(json!([]))),
            Err(vec![
                "lessons: Too small: expected array to have >=1 items".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&cur(json!([{"title": ""}]))),
            Err(vec![
                "lessons.0.title: Too small: expected string to have >=1 characters".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&cur(json!([{"title": "A", "plannedNodes": 0}]))),
            Err(vec![
                "lessons.0.plannedNodes: Too small: expected number to be >0".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&cur(json!([{"title": "A", "plannedNodes": 1.5}]))),
            Err(vec![
                "lessons.0.plannedNodes: Invalid input: expected int, received number".to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&cur(json!([{"title": "A", "slug": "Bad Slug"}]))),
            Err(vec![
                r#"lessons.0.slug: Invalid string: must match pattern /^[a-z0-9]+(?:-[a-z0-9]+)*$/"#
                    .to_string(),
            ])
        );
        assert_eq!(
            (TOOL.validate)(&cur(json!(["no"]))),
            Err(vec![
                "lessons.0: Invalid input: expected object, received string".to_string(),
            ])
        );
    }

    #[test]
    fn the_validator_drops_unknown_keys_and_keeps_schema_order() {
        let data = (TOOL.validate)(&json!({
            "lessons": [{"slug": "legacy", "plannedNodes": 2.0, "title": "A", "extra": true}],
            "extra": 1,
            "mission": "m",
            "destination": "x",
            "slug": "demo",
            "chapter": "Demo",
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"chapter":"Demo","slug":"demo","destination":"x","mission":"m","lessons":[{"title":"A","plannedNodes":2,"slug":"legacy"}]}"#
        );
    }

    #[test]
    fn the_schema_file_is_the_tool_schema() {
        let file: Value =
            serde_json::from_str(include_str!("schemas/propose_curriculum.json")).expect("parses");
        assert_eq!(
            serde_json::to_string(&(TOOL.schema)()).unwrap(),
            serde_json::to_string(&file).unwrap()
        );
        assert_eq!(
            file["required"],
            json!(["chapter", "slug", "destination", "lessons"])
        );
        assert_eq!(
            TOOL.description,
            "Show the learner the destination and ordered lesson list and wait for approval (blocks); on approval writes curriculum.json (and MISSION.md for a new course)."
        );
    }
}
