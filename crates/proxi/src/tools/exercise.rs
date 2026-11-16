//! The `write_exercise` and `run_exercise` tools (port of app/server/tools/exercise.ts).

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::course_data::valid_course;
use crate::exercises::{self, HINTS_FILE, Hints, INSTRUCTIONS_FILE};
use crate::js;
use crate::lesson::node_path;
use crate::sessions::{invalid_type, issue, utf16_len};
use crate::tools::util::{self, fail};
use crate::tools::{ToolDef, ToolFuture, ToolHost, ToolOutcome};

/// Where the shas of the app-written files live, so a learner edit is never overwritten.
const TRACK: &str = ".app-written.json";

pub const WRITE: ToolDef = ToolDef {
    name: "write_exercise",
    description: "Write a coding exercise (files plus instructions) into courses/<course>/exercises/<name>/ and show it to the learner, who solves it in the app's built-in editor and runs the tests there (results reach you as \"Esito test\" messages).",
    schema: write_schema,
    validate: validate_write,
    run: run_write,
    writes: true,
};

pub const RUN: ToolDef = ToolDef {
    name: "run_exercise",
    description: "Run the tests of one exercise and return passed/failed with failure messages. The result is also shown to the learner.",
    schema: run_schema,
    validate: validate_run,
    run: run_run,
    writes: false,
};

fn write_schema() -> Value {
    serde_json::from_str(include_str!("schemas/write_exercise.json"))
        .expect("the write_exercise schema")
}

fn run_schema() -> Value {
    serde_json::from_str(include_str!("schemas/run_exercise.json"))
        .expect("the run_exercise schema")
}

/// `^\d{2}-[a-z0-9]+(?:-[a-z0-9]+)*$`: the `NN-` prefix, then a kebab tail.
fn is_exercise_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b'-'
        && valid_course(&name[3..])
}

#[derive(Default)]
struct Issues(Vec<String>);

impl Issues {
    fn push(&mut self, path: &str, message: &str) {
        self.0.push(issue(path, message));
    }

    fn type_error(&mut self, path: &str, expected: &str, value: Option<&Value>) {
        self.0.push(issue(path, &invalid_type(expected, value)));
    }
}

fn validate_write(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Issues::default();
    let mut out = Map::new();

    match object.get("name") {
        Some(Value::String(name)) => {
            if is_exercise_name(name) {
                out.insert("name".to_string(), Value::String(name.clone()));
            } else {
                issues.push("name", "must look like \"13-provide-inject\"");
            }
        }
        other => issues.type_error("name", "string", other),
    }

    match object.get("instructions") {
        Some(Value::String(text)) => {
            if text.is_empty() {
                issues.push(
                    "instructions",
                    "Too small: expected string to have >=1 characters",
                );
            }
            out.insert("instructions".to_string(), Value::String(text.clone()));
        }
        other => issues.type_error("instructions", "string", other),
    }

    match object.get("files") {
        Some(Value::Array(files)) => {
            // zod stops at the array's own length check: an out-of-range list reports no item issues.
            if files.is_empty() {
                issues.push("files", "Too small: expected array to have >=1 items");
            } else {
                let mut parsed = Vec::with_capacity(files.len());
                for (i, file) in files.iter().enumerate() {
                    let path = format!("files.{i}");
                    let Some(file) = file.as_object() else {
                        issues.type_error(&path, "object", Some(file));
                        continue;
                    };
                    let mut item = Map::new();
                    match file.get("path") {
                        Some(Value::String(plain)) => {
                            item.insert("path".to_string(), Value::String(plain.clone()));
                        }
                        other => issues.type_error(&format!("{path}.path"), "string", other),
                    }
                    match file.get("content") {
                        Some(Value::String(plain)) => {
                            item.insert("content".to_string(), Value::String(plain.clone()));
                        }
                        other => issues.type_error(&format!("{path}.content"), "string", other),
                    }
                    parsed.push(Value::Object(item));
                }
                out.insert("files".to_string(), Value::Array(parsed));
            }
        }
        other => issues.type_error("files", "array", other),
    }

    match object.get("hints") {
        None => {}
        Some(Value::Array(items)) => {
            // zod refuses a too-long list before reading its items, while a too-short one is still
            // parsed: its item issues come first, the length issue last.
            if items.len() > 4 {
                issues.push("hints", "Too big: expected array to have <=4 items");
            } else {
                let mut parsed = Vec::with_capacity(items.len());
                for (i, hint) in items.iter().enumerate() {
                    match hint {
                        Value::String(plain) if !plain.is_empty() => {
                            parsed.push(Value::String(plain.clone()));
                        }
                        Value::String(_) => issues.push(
                            &format!("hints.{i}"),
                            "Too small: expected string to have >=1 characters",
                        ),
                        other => issues.type_error(&format!("hints.{i}"), "string", Some(other)),
                    }
                }
                if items.len() < 2 {
                    issues.push("hints", "Too small: expected array to have >=2 items");
                }
                out.insert("hints".to_string(), Value::Array(parsed));
            }
        }
        other => issues.type_error("hints", "array", other),
    }

    match object.get("final") {
        None => {}
        Some(Value::Bool(final_)) => {
            out.insert("final".to_string(), Value::Bool(*final_));
        }
        other => issues.type_error("final", "boolean", other),
    }

    if issues.0.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(issues.0)
    }
}

fn validate_run(input: &Value) -> Result<Value, Vec<String>> {
    let Some(object) = input.as_object() else {
        return Err(vec![issue("(input)", &invalid_type("object", Some(input)))]);
    };
    let mut issues = Issues::default();
    let mut out = Map::new();
    match object.get("name") {
        Some(Value::String(name)) => {
            if is_exercise_name(name) {
                out.insert("name".to_string(), Value::String(name.clone()));
            } else {
                issues.push("name", "must look like \"13-provide-inject\"");
            }
        }
        other => issues.type_error("name", "string", other),
    }
    if issues.0.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(issues.0)
    }
}

fn run_write<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(write_tool(input, host))
}

fn run_run<'a>(input: Value, host: &'a dyn ToolHost) -> ToolFuture<'a> {
    Box::pin(run_tool(input, host))
}

/// `sha256` hex of the LF-normalised UTF-8 text: an editor or git autocrlf must not look like a
/// learner edit.
fn sha(text: &str) -> String {
    let normalised = text.replace("\r\n", "\n");
    Sha256::digest(normalised.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `path.posix.normalize(p).replace(/^\.\//, "")`: the track's key for one file.
fn key(path: &str) -> String {
    let normalised = node_path::join_posix("", path);
    match normalised.strip_prefix("./") {
        Some(rest) => rest.to_string(),
        None => normalised,
    }
}

fn read_track(root: &Path) -> Vec<(String, String)> {
    let Ok(raw) = fs::read(root.join(TRACK)) else {
        return Vec::new();
    };
    let Ok(value) = js::parse(&String::from_utf8_lossy(&raw)) else {
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .filter_map(|(name, hash)| hash.as_str().map(|hash| (key(name), hash.to_string())))
        .collect()
}

fn track_set(track: &mut Vec<(String, String)>, key: String, value: String) {
    match track.iter_mut().find(|(candidate, _)| *candidate == key) {
        Some(entry) => entry.1 = value,
        None => track.push((key, value)),
    }
}

/// `JSON.stringify(track, null, 2) + "\n"`; V8 prints array-index keys first, ascending.
fn track_json(track: &[(String, String)]) -> String {
    let mut indexed: Vec<&(String, String)> = track
        .iter()
        .filter(|(key, _)| js::array_index(key).is_some())
        .collect();
    indexed.sort_by_key(|(key, _)| js::array_index(key).unwrap_or(0));
    let mut ordered: Vec<&(String, String)> = indexed;
    ordered.extend(
        track
            .iter()
            .filter(|(key, _)| js::array_index(key).is_none()),
    );

    let mut map = Map::new();
    for (key, value) in ordered {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(map)).expect("a track serialises");
    text.push('\n');
    text
}

/// Replace/insert `info` in the session exercise list.
fn upsert(host: &dyn ToolHost, info: Value) {
    let name = js::js_string(info.get("name"));
    let mut list: Vec<Value> = host
        .exercises()
        .into_iter()
        .filter(|entry| js::js_string(entry.get("name")) != name)
        .collect();
    list.push(info.clone());
    list.sort_by(|left, right| {
        js::locale_compare(
            &js::js_string(left.get("name")),
            &js::js_string(right.get("name")),
        )
    });
    host.set_exercises(list);
    host.emit(json!({
        "id": format!("exercise:{name}"),
        "kind": "exercise",
        "exercise": info,
    }));
}

async fn write_tool(input: Value, host: &dyn ToolHost) -> ToolOutcome {
    let course = match util::course_dir(host) {
        Ok(dir) => dir,
        Err(message) => return fail(message),
    };
    let course_name = js::js_string(host.config().get("course"));
    let name = js::js_string(input.get("name"));
    let root = course.join("exercises").join(&name);
    let files = input
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut errs: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for file in &files {
        let path = js::js_string(file.get("path"));
        // `bad_exercise_path` already rejects "..", ".", empty, absolute and drive paths, so Node's
        // separate `path.relative(root, …).startsWith("..")` check could never fire: not ported.
        if let Some(message) = exercises::bad_exercise_path(&path) {
            errs.push(message);
        } else if path == INSTRUCTIONS_FILE {
            errs.push(format!(
                "{INSTRUCTIONS_FILE} is written by the tool from `instructions`"
            ));
        } else if seen.contains(&path) {
            errs.push(format!("duplicate path \"{path}\""));
        }
        if !seen.contains(&path) {
            seen.push(path);
        }
    }
    if !errs.is_empty() {
        return fail(format!(
            "Invalid files: {}. Fix and call again.",
            errs.join("; ")
        ));
    }

    // Never destroy the learner's work: overwrite only files absent or still as the app last wrote them.
    let mut track = read_track(&root);
    for file in &files {
        let path = js::js_string(file.get("path"));
        let target = root.join(&path);
        if !target.is_file() {
            continue;
        }
        let bytes = match fs::read(&target) {
            Ok(bytes) => bytes,
            Err(error) => return fail(error.to_string()),
        };
        let current = sha(&String::from_utf8_lossy(&bytes));
        let content = sha(&js::js_string(file.get("content")));
        let tracked = track
            .iter()
            .find(|(candidate, _)| *candidate == key(&path))
            .map(|(_, hash)| hash.as_str());
        if tracked != Some(current.as_str()) && current != content {
            return fail(format!(
                "Il file {path} è stato modificato dal learner (o non è stato scritto da questa app): non sovrascriverlo, scegli un nuovo nome di esercizio (NN-...). Nothing was written."
            ));
        }
    }

    for file in &files {
        let path = js::js_string(file.get("path"));
        let content = js::js_string(file.get("content"));
        let target = root.join(&path);
        if let Some(parent) = target.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            return fail(error.to_string());
        }
        if let Err(error) = fs::write(&target, content.as_bytes()) {
            return fail(error.to_string());
        }
        track_set(&mut track, key(&path), sha(&content));
    }
    if let Err(error) = fs::write(root.join(TRACK), track_json(&track)) {
        return fail(error.to_string());
    }
    let instructions = js::js_string(input.get("instructions"));
    if let Err(error) = fs::write(
        root.join(INSTRUCTIONS_FILE),
        format!("{}\n", instructions.trim_end_matches(js::is_js_ws)),
    ) {
        return fail(error.to_string());
    }

    // The hints file is app-owned and mutated by the learner's clicks, so it is not sha-tracked;
    // `revealed` survives a rewrite unless the hints changed.
    let prev = exercises::read_hints(&root);
    let hints: Vec<String> = input
        .get("hints")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(|hint| js::js_string(Some(hint))).collect())
        .unwrap_or_default();
    let same = prev.as_ref().is_some_and(|prev| prev.hints == hints);
    let final_ = js::truthy(input.get("final"));
    let persist = !hints.is_empty() || final_;
    if persist {
        let revealed = match same {
            true => prev.as_ref().map(|prev| prev.revealed).unwrap_or(0.0),
            false => 0.0,
        };
        if let Err(error) = exercises::write_hints(
            &root,
            &Hints {
                hints: hints.clone(),
                final_,
                revealed,
            },
        ) {
            return fail(error.to_string());
        }
    } else if prev.is_some()
        && let Err(error) = fs::remove_file(root.join(HINTS_FILE))
    {
        return fail(error.to_string());
    }

    let listed = match exercises::list_exercises(host.data_root(), &course_name) {
        Ok(list) => list,
        Err(error) => return fail(error.to_string()),
    };
    let entry = listed
        .into_iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name.as_str()));
    let listed_files: Vec<String> = match &entry {
        Some(entry) => entry
            .get("files")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(|item| js::js_string(Some(item))).collect())
            .unwrap_or_default(),
        None => files.iter().map(|file| js::js_string(file.get("path"))).collect(),
    };
    let dir = util::to_rel(host.data_root(), &root);

    let mut info = Map::new();
    info.insert("course".to_string(), json!(course_name));
    info.insert("name".to_string(), json!(name));
    info.insert("dir".to_string(), json!(dir));
    info.insert(
        "files".to_string(),
        Value::Array(
            listed_files
                .into_iter()
                .filter(|file| file != TRACK)
                .map(Value::String)
                .collect(),
        ),
    );
    info.insert("instructions".to_string(), json!(instructions));
    if persist {
        info.insert(
            "hints".to_string(),
            Value::Array(hints.into_iter().map(Value::String).collect()),
        );
        info.insert(
            "hintsRevealed".to_string(),
            js::number(match same {
                true => prev.as_ref().map(|prev| prev.revealed).unwrap_or(0.0),
                false => 0.0,
            }),
        );
        info.insert("final".to_string(), Value::Bool(final_));
    }
    upsert(host, Value::Object(info));

    util::json(&json!({
        "dir": dir,
        "note": "Esercizio aperto nell'editor dell'app: il learner lavora lì e gli esiti arrivano come messaggi \"Esito test …\".",
    }))
}

async fn run_tool(input: Value, host: &dyn ToolHost) -> ToolOutcome {
    let course_dir = match util::course_dir(host) {
        Ok(dir) => dir,
        Err(message) => return fail(message),
    };
    let course = js::js_string(host.config().get("course"));
    let name = js::js_string(input.get("name"));
    let dir = course_dir.join("exercises").join(&name);

    let res = exercises::run_exercise(
        host.data_root(),
        &course,
        &name,
        &host.run_config(),
        &host.installs(),
    )
    .await;

    let mut info = match host
        .exercises()
        .into_iter()
        .find(|entry| js::js_string(entry.get("name")) == name)
    {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let instructions = info
        .get("instructions")
        .filter(|value| !value.is_null())
        .cloned()
        .or_else(|| {
            fs::read(dir.join(INSTRUCTIONS_FILE))
                .ok()
                .map(|raw| Value::String(String::from_utf8_lossy(&raw).into_owned()))
        });
    let files = match info.get("files") {
        Some(Value::Array(items)) => Value::Array(items.clone()),
        _ => json!([]),
    };
    info.insert("course".to_string(), json!(course));
    info.insert("name".to_string(), json!(name));
    info.insert("dir".to_string(), json!(util::to_rel(host.data_root(), &dir)));
    info.insert("files".to_string(), files);
    match instructions {
        Some(instructions) => {
            info.insert("instructions".to_string(), instructions);
        }
        // Node sets `instructions: undefined`, which JSON drops: remove it, keep the key order.
        None => {
            info.shift_remove("instructions");
        }
    }
    info.insert("lastRun".to_string(), res.clone());
    upsert(host, Value::Object(info));

    let (text, is_error) = run_text(&res);
    ToolOutcome { text, is_error }
}

/// Node's result text, cut at 4000 UTF-16 units like `text.slice(0, 4000)` (no ellipsis).
pub(crate) fn run_text(res: &Value) -> (String, bool) {
    if js::truthy(res.get("error")) {
        let text = format!("Could not run the tests: {}", js::display(res.get("error")));
        return (truncate_run(&text), true);
    }
    let mut text = format!(
        "{}: {}/{} passed, {} failed ({} ms).",
        match js::truthy(res.get("ok")) {
            true => "PASS",
            false => "FAIL",
        },
        js::display(res.get("passed")),
        js::display(res.get("total")),
        js::display(res.get("failed")),
        js::display(res.get("durationMs")),
    );
    for failure in res
        .get("failures")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        text.push_str(&format!(
            "\n- {}: {}",
            js::display(failure.get("name")),
            js::display(failure.get("message")),
        ));
    }
    (truncate_run(&text), false)
}

fn truncate_run(text: &str) -> String {
    if utf16_len(text) <= 4000 {
        return text.to_string();
    }
    format!("{}\n...(truncated)", slice_utf16(text, 4000))
}

/// The prefix of whole chars whose UTF-16 width sums to at most `n`. Node's `slice(0, n)` keeps a
/// lone high surrogate when the cut lands inside a pair; a Rust `str` cannot, so that char is
/// dropped whole.
fn slice_utf16(text: &str, n: usize) -> &str {
    let mut units = 0;
    for (at, c) in text.char_indices() {
        let width = c.len_utf16();
        if units + width > n {
            return &text[..at];
        }
        units += width;
    }
    text
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::tools::fake::FakeHost;

    fn teach() -> Value {
        json!({"kind": "teach", "course": "demo", "model": "haiku", "target": {"mode": "planned", "title": "Il primo passo"}, "ankiOptIn": false})
    }

    /// Node's `ex(o)`: the default exercise with `o`'s values merged in (a `null` drops the key).
    fn ex(over: Value) -> Value {
        let mut base = Map::from_iter([
            ("name".to_string(), json!("01-demo")),
            ("instructions".to_string(), json!("Fai **questo**.")),
            (
                "files".to_string(),
                json!([
                    {"path": "Esercizio.vue", "content": "<template/>"},
                    {"path": "Esercizio.test.js", "content": "// t"},
                ]),
            ),
        ]);
        if let Some(over) = over.as_object() {
            for (key, value) in over {
                if value.is_null() {
                    base.shift_remove(key);
                } else {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
        Value::Object(base)
    }

    fn dir_of(host: &FakeHost) -> std::path::PathBuf {
        host.data_root().join("courses/demo/exercises/01-demo")
    }

    #[tokio::test]
    async fn writes_files_and_instructions_and_updates_the_session_list() {
        let host = FakeHost::new(teach());
        let out = host.call(&WRITE, ex(json!({}))).await;
        assert!(!out.is_error, "{}", out.text);

        let value: Value = serde_json::from_str(&out.text).expect("the tool answers JSON");
        assert_eq!(value["dir"], json!("courses/demo/exercises/01-demo"));
        assert!(
            js::js_string(value.get("note")).contains("editor dell'app"),
            "{}",
            out.text
        );

        let dir = dir_of(&host);
        assert_eq!(
            fs::read_to_string(dir.join("Esercizio.vue")).expect("the exercise file"),
            "<template/>"
        );
        assert_eq!(
            fs::read_to_string(dir.join("ISTRUZIONI.md")).expect("the instructions"),
            "Fai **questo**.\n"
        );

        let list = host.exercises();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], json!("01-demo"));
        assert_eq!(list[0]["course"], json!("demo"));
        assert_eq!(list[0]["dir"], json!("courses/demo/exercises/01-demo"));
        assert_eq!(
            list[0]["files"],
            json!(["Esercizio.test.js", "Esercizio.vue", "ISTRUZIONI.md"])
        );
        assert!(
            js::js_string(list[0].get("instructions")).contains("questo"),
            "{:?}",
            list[0]
        );
        assert_eq!(host.items().last().expect("an item")["kind"], json!("exercise"));
        assert_eq!(host.items().last().expect("an item")["id"], json!("exercise:01-demo"));
        assert_eq!(host.courses_changed_count(), 1, "`writes: true` raises courses_changed");
        assert_eq!(
            host.events(),
            vec![json!({"type": "exercises", "exercises": list})]
        );
    }

    #[tokio::test]
    async fn tracks_the_files_it_wrote_by_the_lf_normalised_sha() {
        let host = FakeHost::new(teach());
        let out = host
            .call(
                &WRITE,
                ex(json!({
                    "instructions": "i",
                    "files": [{"path": "Esercizio.vue", "content": "v1"}],
                })),
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        let raw = fs::read_to_string(dir_of(&host).join(TRACK)).expect("the track file");
        assert_eq!(
            raw,
            "{\n  \"Esercizio.vue\": \"3bfc269594ef649228e9a74bab00f042efc91d5acc6fbee31a382e80d42388fe\"\n}\n"
        );
    }

    #[tokio::test]
    async fn prints_array_index_keys_first_in_the_track() {
        let host = FakeHost::new(teach());
        let dir = dir_of(&host);
        fs::create_dir_all(&dir).expect("create the exercise dir");
        fs::write(dir.join("a.js"), "A").expect("seed the app-written file");
        fs::write(dir.join(TRACK), "{\n  \"a.js\": \"x\",\n  \"2\": \"y\"\n}\n")
            .expect("seed the track");

        let out = host
            .call(
                &WRITE,
                ex(json!({
                    "instructions": "i",
                    "files": [
                        {"path": "a.js", "content": "A"},
                        {"path": "b.js", "content": "B"},
                    ],
                })),
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(
            fs::read_to_string(dir.join(TRACK)).expect("the track file"),
            "{\n  \"2\": \"y\",\n  \"a.js\": \"559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd\",\n  \"b.js\": \"df7e70e5021544f4834bbee64a9e3789febc4be81470df629cad6ddb03320a5c\"\n}\n"
        );
    }

    #[tokio::test]
    async fn rejects_bad_names_and_paths_and_writes_nothing() {
        let host = FakeHost::new(teach());
        let bads = [
            ex(json!({"name": "demo"})),
            ex(json!({"name": "1-x"})),
            ex(json!({"name": "../01-x"})),
            ex(json!({"files": [{"path": "../evil.js", "content": ""}]})),
            ex(json!({"files": [{"path": "/abs.js", "content": ""}]})),
            ex(json!({"files": [{"path": "C:/x.js", "content": ""}]})),
            ex(json!({"files": [{"path": "a\\b.js", "content": ""}]})),
            ex(json!({"files": [{"path": "sub/../../x.js", "content": ""}]})),
            ex(json!({"files": [{"path": "run.sh", "content": ""}]})),
            ex(json!({"files": [{"path": "ISTRUZIONI.md", "content": ""}]})),
            ex(json!({"files": []})),
        ];
        for bad in bads {
            let out = host.call(&WRITE, bad.clone()).await;
            assert!(out.is_error, "{}", serde_json::to_string(&bad).unwrap());
        }
        assert!(!dir_of(&host).exists());
        assert!(!host.data_root().join("courses/evil.js").exists());
        assert!(host.exercises().is_empty(), "a rejected write lists nothing");
    }

    #[tokio::test]
    async fn allows_nested_relative_paths() {
        let host = FakeHost::new(teach());
        let out = host
            .call(
                &WRITE,
                ex(json!({"files": [{"path": "sub/dir/x.ts", "content": "1"}]})),
            )
            .await;
        assert!(!out.is_error, "{}", out.text);
        assert!(dir_of(&host).join("sub/dir/x.ts").exists());
    }

    #[tokio::test]
    async fn run_exercise_validates_the_name() {
        let host = FakeHost::new(teach());
        let out = host.call(&RUN, json!({"name": "../x"})).await;
        assert!(out.is_error, "{}", out.text);
        assert!(host.items().is_empty());
    }

    #[tokio::test]
    async fn run_exercise_records_no_progress() {
        let host = FakeHost::new(teach());
        let progress_file = host.data_root().join("courses/demo/progress.jsonl");
        fs::create_dir_all(progress_file.parent().expect("a parent")).expect("create the dir");
        let seed = "{\"kind\":\"check\",\"nodeRefs\":[\"demo/seed\"],\"correct\":true}\n";
        fs::write(&progress_file, seed).expect("seed the ledger");
        let initial = fs::read(&progress_file).expect("read the ledger");

        let out = host.call(&RUN, json!({"name": "01-missing"})).await;
        assert!(out.is_error, "{}", out.text);
        assert!(
            out.text.contains("Could not run the tests: Esercizio sconosciuto: 01-missing"),
            "{}",
            out.text
        );
        assert!(host.progress().is_empty());
        assert_eq!(fs::read(&progress_file).expect("read the ledger"), initial);

        // The run still upserts the session entry, with the failed result as `lastRun`.
        let list = host.exercises();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], json!("01-missing"));
        assert_eq!(list[0]["lastRun"]["ok"], json!(false));
        assert_eq!(host.items().last().expect("an item")["kind"], json!("exercise"));
    }

    #[tokio::test]
    async fn rewrites_app_written_files_and_refuses_learner_edits() {
        let host = FakeHost::new(teach());
        let file = dir_of(&host).join("Esercizio.vue");
        let first = host
            .call(
                &WRITE,
                ex(json!({"instructions": "i", "files": [{"path": "Esercizio.vue", "content": "v1"}]})),
            )
            .await;
        assert!(!first.is_error, "{}", first.text);
        let second = host
            .call(
                &WRITE,
                ex(json!({"instructions": "i", "files": [{"path": "Esercizio.vue", "content": "v2"}]})),
            )
            .await;
        assert!(!second.is_error, "{}", second.text);
        assert_eq!(fs::read_to_string(&file).expect("the file"), "v2");

        fs::write(&file, "il lavoro del learner").expect("the learner edits it");
        let out = host
            .call(
                &WRITE,
                ex(json!({
                    "instructions": "i",
                    "files": [
                        {"path": "Esercizio.vue", "content": "v3"},
                        {"path": "Altro.js", "content": "x"},
                    ],
                })),
            )
            .await;
        assert!(out.is_error, "{}", out.text);
        assert_eq!(
            out.text,
            "Il file Esercizio.vue è stato modificato dal learner (o non è stato scritto da questa app): non sovrascriverlo, scegli un nuovo nome di esercizio (NN-...). Nothing was written."
        );
        assert_eq!(
            fs::read_to_string(&file).expect("the file"),
            "il lavoro del learner"
        );
        assert!(!dir_of(&host).join("Altro.js").exists());
    }

    #[tokio::test]
    async fn a_crlf_checkout_of_an_app_written_file_is_not_a_learner_edit() {
        let host = FakeHost::new(teach());
        let file = dir_of(&host).join("Esercizio.vue");
        let first = host
            .call(
                &WRITE,
                ex(json!({"instructions": "i", "files": [{"path": "Esercizio.vue", "content": "a\nb\n"}]})),
            )
            .await;
        assert!(!first.is_error, "{}", first.text);

        fs::write(&file, "a\r\nb\r\n").expect("a CRLF checkout");
        let second = host
            .call(
                &WRITE,
                ex(json!({"instructions": "i", "files": [{"path": "Esercizio.vue", "content": "c\n"}]})),
            )
            .await;
        assert!(!second.is_error, "{}", second.text);
        assert_eq!(fs::read_to_string(&file).expect("the file"), "c\n");

        fs::write(&file, "mio").expect("the learner edits it");
        let third = host.call(&WRITE, ex(json!({}))).await;
        assert!(
            third.text.contains("nuovo nome di esercizio (NN-"),
            "{}",
            third.text
        );
    }

    #[tokio::test]
    async fn does_not_clobber_a_file_it_never_wrote() {
        let host = FakeHost::new(teach());
        let dir = dir_of(&host);
        fs::create_dir_all(&dir).expect("create the exercise dir");
        fs::write(dir.join("Esercizio.vue"), "mio").expect("a pre-existing file");

        let out = host.call(&WRITE, ex(json!({}))).await;
        assert!(out.is_error, "{}", out.text);
        assert!(!dir.join(TRACK).exists());
        assert!(!dir.join(INSTRUCTIONS_FILE).exists());
        assert_eq!(
            fs::read_to_string(dir.join("Esercizio.vue")).expect("the file"),
            "mio"
        );
    }

    #[tokio::test]
    async fn persists_hints_and_keeps_revealed_unless_the_hints_change() {
        let host = FakeHost::new(teach());
        let hints_file = dir_of(&host).join(HINTS_FILE);
        let hints = |all: Value, final_: Value| {
            ex(json!({
                "instructions": "i",
                "files": [{"path": "Esercizio.vue", "content": "v1"}],
                "hints": all,
                "final": final_,
            }))
        };

        let first = host
            .call(&WRITE, hints(json!(["uno", "due"]), json!(true)))
            .await;
        assert!(!first.is_error, "{}", first.text);
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&hints_file).expect("the hints file"))
                .expect("JSON");
        assert_eq!(
            written,
            json!({"hints": ["uno", "due"], "final": true, "revealed": 0})
        );
        assert_eq!(host.exercises()[0]["final"], json!(true));
        assert_eq!(host.exercises()[0]["hintsRevealed"], json!(0));

        fs::write(
            &hints_file,
            "{\"hints\":[\"uno\",\"due\"],\"final\":true,\"revealed\":1}",
        )
        .expect("the learner revealed one");
        let again = host
            .call(&WRITE, hints(json!(["uno", "due"]), json!(true)))
            .await;
        assert!(!again.is_error, "{}", again.text);
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&hints_file).expect("the hints file"))
                .expect("JSON");
        assert_eq!(written["revealed"], json!(1));
        assert_eq!(host.exercises()[0]["hintsRevealed"], json!(1));

        let changed = host
            .call(&WRITE, hints(json!(["uno", "tre"]), json!(true)))
            .await;
        assert!(!changed.is_error, "{}", changed.text);
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&hints_file).expect("the hints file"))
                .expect("JSON");
        assert_eq!(written["revealed"], json!(0));

        let five = host
            .call(
                &WRITE,
                hints(json!(["1", "2", "3", "4", "5"]), json!(true)),
            )
            .await;
        assert!(five.is_error, "{}", five.text);

        // No hints and not the final exercise: the app-owned file goes away.
        let plain = host
            .call(
                &WRITE,
                ex(json!({
                    "instructions": "i",
                    "files": [{"path": "Esercizio.vue", "content": "v1"}],
                })),
            )
            .await;
        assert!(!plain.is_error, "{}", plain.text);
        assert!(!hints_file.exists());
        assert!(host.exercises()[0].get("hints").is_none());
    }

    #[test]
    fn validate_pins_zods_messages() {
        assert_eq!(
            validate_write(&json!({"name": "demo", "instructions": "", "files": []}))
                .expect_err("invalid"),
            vec![
                "name: must look like \"13-provide-inject\"".to_string(),
                "instructions: Too small: expected string to have >=1 characters".to_string(),
                "files: Too small: expected array to have >=1 items".to_string(),
            ]
        );
        assert_eq!(
            validate_write(&json!({"name": "01-a", "instructions": "i", "files": ["x"]}))
                .expect_err("invalid"),
            vec!["files.0: Invalid input: expected object, received string".to_string()]
        );
        assert_eq!(
            validate_write(&json!({
                "name": "01-a",
                "instructions": "i",
                "files": [{"path": "a.js", "content": ""}],
                "hints": ["1", "2", "3", "4", ""],
            }))
            .expect_err("invalid"),
            vec!["hints: Too big: expected array to have <=4 items".to_string()]
        );
        assert_eq!(
            validate_write(&json!({
                "name": "01-a",
                "instructions": "i",
                "files": [{"path": "a.js", "content": ""}],
                "hints": [2],
            }))
            .expect_err("invalid"),
            vec![
                "hints.0: Invalid input: expected string, received number".to_string(),
                "hints: Too small: expected array to have >=2 items".to_string(),
            ]
        );
        assert_eq!(
            validate_write(&json!({
                "name": "01-a",
                "instructions": "i",
                "files": [{"path": "a.js", "content": ""}],
                "hints": ["a", ""],
            }))
            .expect_err("invalid"),
            vec!["hints.1: Too small: expected string to have >=1 characters".to_string()]
        );
        assert_eq!(
            validate_write(&json!("x")).expect_err("not an object"),
            vec!["(input): Invalid input: expected object, received string".to_string()]
        );
        assert_eq!(
            validate_run(&json!({})).expect_err("invalid"),
            vec!["name: Invalid input: expected string, received undefined".to_string()]
        );

        // Unknown keys are dropped; the output keeps the schema order.
        let cleaned = validate_write(&json!({
            "final": true,
            "hints": ["a", "b"],
            "files": [{"path": "a.js", "content": "x", "extra": 1}],
            "instructions": "i",
            "name": "01-a",
            "extra": true,
        }))
        .expect("valid");
        assert_eq!(
            serde_json::to_string(&cleaned).unwrap(),
            r#"{"name":"01-a","instructions":"i","files":[{"path":"a.js","content":"x"}],"hints":["a","b"],"final":true}"#
        );
        assert_eq!(
            serde_json::to_string(&validate_run(&json!({"name": "01-a", "extra": 1})).expect("valid"))
                .unwrap(),
            r#"{"name":"01-a"}"#
        );
    }

    #[test]
    fn run_text_reports_pass_and_fail_with_the_failure_lines() {
        let pass = json!({
            "ok": true, "passed": 2, "failed": 0, "total": 2, "failures": [], "durationMs": 12,
            "at": "2026-11-15T00:00:00.000Z",
        });
        assert_eq!(
            run_text(&pass),
            ("PASS: 2/2 passed, 0 failed (12 ms).".to_string(), false)
        );

        let fail = json!({
            "ok": false, "passed": 1, "failed": 2, "total": 3, "durationMs": 8,
            "failures": [{"name": "a", "message": "boom"}, {"name": "b", "message": "bang"}],
        });
        assert_eq!(
            run_text(&fail),
            (
                "FAIL: 1/3 passed, 2 failed (8 ms).\n- a: boom\n- b: bang".to_string(),
                false
            )
        );
    }

    #[test]
    fn run_text_reports_an_error_as_an_error_result() {
        let res = json!({
            "ok": false, "passed": 0, "failed": 0, "total": 0, "failures": [], "durationMs": 0,
            "error": "Esercizio sconosciuto: 01-missing",
        });
        assert_eq!(
            run_text(&res),
            (
                "Could not run the tests: Esercizio sconosciuto: 01-missing".to_string(),
                true
            )
        );
    }

    #[test]
    fn run_text_truncates_past_4000_utf16_units_without_an_ellipsis() {
        let error = format!("{}x{}", "è".repeat(1000), "x".repeat(3100));
        let (text, is_error) = run_text(&json!({"error": error}));
        assert!(is_error);
        assert_eq!(
            text,
            format!(
                "Could not run the tests: {}{}\n...(truncated)",
                "è".repeat(1000),
                "x".repeat(2975)
            )
        );
        assert!(!text.contains('…'));

        // Exactly 4000 units: unchanged.
        let exact = json!({"error": "x".repeat(3975)});
        let (text, _) = run_text(&exact);
        assert_eq!(utf16_len(&text), 4000);
        assert_eq!(text, format!("Could not run the tests: {}", "x".repeat(3975)));

        // Cutting inside a surrogate pair drops the emoji whole.
        assert_eq!(slice_utf16("a😀", 2), "a");
        assert_eq!(slice_utf16("a😀", 3), "a😀");
    }
}
