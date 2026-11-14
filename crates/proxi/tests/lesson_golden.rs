//! Replays the golden captured from the Node `lesson` CLI (app/test/capture-lesson-golden.ts).

mod common;

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

fn golden_file() -> PathBuf {
    common::repo_root()
        .join("crates")
        .join("proxi")
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("lesson-cli.json")
}

fn golden() -> Value {
    let file = golden_file();
    let raw = fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("parse {}: {error}", file.display()))
}

/// Mirrors `normaliseStream` of app/test/capture-lesson-golden.ts: every root form, then separators.
fn normalise_stream(text: &str, roots: &[String]) -> String {
    let mut out = text.to_string();
    for root in roots {
        // stdout is JSON, stderr is not: replace both the escaped and the raw spelling.
        let quoted = serde_json::to_string(root).expect("serialise root");
        let escaped = &quoted[1..quoted.len() - 1];
        out = out.replace(escaped, "<dataRoot>");
        out = out.replace(root.as_str(), "<dataRoot>");
    }
    out.replace("\\\\", "/").replace('\\', "/")
}

/// Mirrors `normaliseFile` of app/test/capture-lesson-golden.ts: CRLF, ISO stamps, generated date.
fn normalise_file(text: &str) -> String {
    replace_generated(&replace_iso(&text.replace("\r\n", "\n")))
}

fn replace_iso(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        if at + 24 <= bytes.len() && is_iso(&bytes[at..at + 24]) {
            out.push_str("<iso>");
            at += 24;
        } else {
            let ch = text[at..]
                .chars()
                .next()
                .expect("index sits on a char boundary");
            out.push(ch);
            at += ch.len_utf8();
        }
    }
    out
}

/// `\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z`, exactly 24 bytes (no regex crate here).
fn is_iso(window: &[u8]) -> bool {
    let digit = |at: usize| window[at].is_ascii_digit();
    (0..4).all(digit)
        && window[4] == b'-'
        && (5..7).all(digit)
        && window[7] == b'-'
        && (8..10).all(digit)
        && window[10] == b'T'
        && (11..13).all(digit)
        && window[13] == b':'
        && (14..16).all(digit)
        && window[16] == b':'
        && (17..19).all(digit)
        && window[19] == b'.'
        && (20..23).all(digit)
        && window[23] == b'Z'
}

fn replace_generated(text: &str) -> String {
    const NEEDLE: &str = "Generated: ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(NEEDLE) {
        out.push_str(&rest[..at]);
        let after = &rest[at + NEEDLE.len()..];
        let bytes = after.as_bytes();
        if bytes.len() >= 10 && is_date(&bytes[..10]) {
            out.push_str("Generated: <date>");
            rest = &after[10..];
        } else {
            out.push_str(NEEDLE);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `\d{4}-\d{2}-\d{2}`.
fn is_date(window: &[u8]) -> bool {
    window[..4].iter().all(u8::is_ascii_digit)
        && window[4] == b'-'
        && window[5..7].iter().all(u8::is_ascii_digit)
        && window[7] == b'-'
        && window[8..].iter().all(u8::is_ascii_digit)
}

/// Every file under `root` as `<dir>/<name>` → bytes, the board's `index.html` excluded like the capture.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    collect(root, "", &mut out);
    out
}

fn collect(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let metadata = entry
            .metadata()
            .unwrap_or_else(|error| panic!("stat {}: {error}", entry.path().display()));
        if metadata.is_dir() {
            collect(&entry.path(), &rel, out);
        } else if metadata.is_file() && !(prefix.is_empty() && name == "index.html") {
            let bytes = fs::read(entry.path())
                .unwrap_or_else(|error| panic!("read {}: {error}", entry.path().display()));
            out.insert(rel, bytes);
        }
    }
}

/// Created or changed files as normalised text, deleted ones as `null`, keys sorted.
fn changes(before: &BTreeMap<String, Vec<u8>>, after: &BTreeMap<String, Vec<u8>>) -> Value {
    let mut files: BTreeMap<String, Value> = BTreeMap::new();
    for (rel, content) in after {
        if before.get(rel) != Some(content) {
            let text = String::from_utf8_lossy(content);
            files.insert(rel.clone(), Value::String(normalise_file(&text)));
        }
    }
    for rel in before.keys() {
        if !after.contains_key(rel) {
            files.insert(rel.clone(), Value::Null);
        }
    }
    Value::Object(files.into_iter().collect::<Map<String, Value>>())
}

/// A failure message pointing at the first differing line instead of dumping a whole lesson file.
fn describe_files(expected: &Value, actual: &Value) -> Option<String> {
    let expected = expected.as_object()?;
    let actual = actual.as_object()?;
    let mut names: Vec<&String> = expected.keys().chain(actual.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let want = expected.get(name);
        let got = actual.get(name);
        match (want, got) {
            (Some(Value::String(want)), Some(Value::String(got))) if want != got => {
                return Some(describe_text(name, want, got));
            }
            (want, got) if want != got => {
                return Some(format!(
                    "{name}: expected {}, got {}",
                    describe_value(want),
                    describe_value(got)
                ));
            }
            _ => {}
        }
    }
    None
}

fn describe_value(value: Option<&Value>) -> String {
    match value {
        None => "no entry".to_string(),
        Some(Value::Null) => "deleted".to_string(),
        Some(Value::String(_)) => "content".to_string(),
        Some(other) => other.to_string(),
    }
}

fn describe_text(name: &str, want: &str, got: &str) -> String {
    let wanted: Vec<&str> = want.split('\n').collect();
    let actual: Vec<&str> = got.split('\n').collect();
    for (index, (want_line, got_line)) in wanted.iter().zip(actual.iter()).enumerate() {
        if want_line != got_line {
            return format!(
                "{name}: first difference at line {}: expected {want_line:?}, got {got_line:?}",
                index + 1
            );
        }
    }
    format!(
        "{name}: line counts differ: expected {} lines, got {}",
        wanted.len(),
        actual.len()
    )
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_lesson(data: &Path, cwd: &Path, args: &[String], stdin: Option<&str>) -> Run {
    let mut command = Command::new(common::proxi_bin());
    command
        .arg("lesson")
        .args(args)
        .current_dir(cwd)
        .env("LEARNER_DATA_ROOT", data)
        .env_remove("LEARNER_REPO_ROOT")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped());
    let mut child = command.spawn().expect("spawn proxi");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.unwrap_or("").as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait proxi");
    Run {
        code: output.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn data_roots(data: &Path) -> Vec<String> {
    let mut roots = vec![proxi::data_root::resolve_path(data).display().to_string()];
    #[cfg(not(windows))]
    {
        let real = fs::canonicalize(data)
            .unwrap_or_else(|error| panic!("canonicalize {}: {error}", data.display()))
            .display()
            .to_string();
        if !roots.contains(&real) {
            roots.push(real);
        }
    }
    roots.sort_by_key(|root| Reverse(root.len()));
    roots
}

#[test]
fn lesson_cli_matches_the_node_golden() {
    let golden = golden();
    assert_eq!(golden["route"].as_str(), Some("lesson-cli"), "golden route");

    let t = common::temp_dir("lesson-golden");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&data).expect("create data");
    fs::create_dir_all(&cwd).expect("create cwd");

    for (rel, content) in golden["seed"].as_object().expect("seed object") {
        let content = content.as_str().expect("seed content is text");
        let target = data.join(rel);
        fs::create_dir_all(target.parent().expect("seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(&target, content.as_bytes())
            .unwrap_or_else(|error| panic!("write seed {rel}: {error}"));
    }

    let roots = data_roots(&data);

    for case in golden["cases"].as_array().expect("cases array") {
        let name = case["name"].as_str().expect("case name");
        let label = format!("lesson-cli/{name}");
        let args: Vec<String> = case["args"]
            .as_array()
            .expect("case args")
            .iter()
            .map(|arg| arg.as_str().expect("arg is a string").to_string())
            .collect();

        let before = snapshot(&data);
        let run = run_lesson(&data, &cwd, &args, case["stdin"].as_str());
        let after = snapshot(&data);

        let expected_code = case["exitCode"].as_i64().expect("golden exit code") as i32;
        assert_eq!(
            run.code,
            expected_code,
            "{label}: exit code\nstdout: {}\nstderr: {}",
            normalise_stream(&run.stdout, &roots),
            normalise_stream(&run.stderr, &roots)
        );

        assert_eq!(
            normalise_stream(&run.stdout, &roots),
            case["stdout"].as_str().expect("golden stdout"),
            "{label}: stdout"
        );

        match case["stderrPrefix"].as_str() {
            Some(prefix) => assert!(
                run.stderr.starts_with(prefix),
                "{label}: stderr does not start with {prefix:?}: {}",
                normalise_stream(&run.stderr, &roots)
            ),
            None => assert_eq!(
                normalise_stream(&run.stderr, &roots),
                case["stderr"].as_str().expect("golden stderr"),
                "{label}: stderr"
            ),
        }

        let actual_files = changes(&before, &after);
        let expected_files = case["files"].clone();
        if let Some(detail) = describe_files(&expected_files, &actual_files) {
            panic!("{label}: files differ: {detail}");
        }
        assert_eq!(actual_files, expected_files, "{label}: files");
    }
}

#[test]
fn golden_covers_every_acceptance_case() {
    let golden = golden();
    let cases = golden["cases"].as_array().expect("cases array");
    let names: Vec<String> = cases
        .iter()
        .map(|case| case["name"].as_str().expect("case name").to_string())
        .collect();
    let expected: Vec<&str> = vec![
        "init-default",
        "init-out-dir",
        "init-duplicate",
        "init-slug-flag",
        "init-base-dir",
        "init-flag-as-value",
        "init-missing-topic",
        "init-empty-topic",
        "append-data",
        "append-node-file",
        "append-stdin-slug",
        "append-replace",
        "append-empty-data-falls-through",
        "append-no-payload",
        "append-missing-lesson",
        "append-no-target",
        "append-slug-prefix",
        "append-missing-node-file",
        "append-invalid-json",
        "list-default",
        "list-dir",
        "list-missing",
        "usage-unknown",
        "usage-none",
    ];
    assert_eq!(names, expected, "golden case names");

    let init = cases
        .iter()
        .find(|case| case["name"].as_str() == Some("init-default"))
        .expect("init-default case");
    assert!(
        init["stdout"]
            .as_str()
            .expect("init-default stdout")
            .contains("\"slug\": \"probabilita-e-davvero-50-ca-va\""),
        "init-default must pin the slug: {}",
        init["stdout"]
    );
}

#[test]
fn normalise_stream_replaces_roots_and_separators() {
    assert_eq!(
        normalise_stream(
            r#"{"filePath": "C:\\d\\lessons\\a.html"}"#,
            &[r"C:\d".to_string()]
        ),
        r#"{"filePath": "<dataRoot>/lessons/a.html"}"#
    );
}

#[test]
fn normalise_file_replaces_iso_timestamps_and_dates() {
    assert_eq!(
        normalise_file("x 2026-11-13T10:00:00.000Z Generated: 2026-11-13\r\n"),
        "x <iso> Generated: <date>\n"
    );
}
