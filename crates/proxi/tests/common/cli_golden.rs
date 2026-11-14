//! Shared plumbing for the CLI golden replays: the captured file, its normalisation and a run.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

/// `crates/proxi/tests/fixtures/golden/<name>.json`, captured from the Node CLIs.
pub fn golden(name: &str) -> Value {
    let file = super::repo_root()
        .join("crates")
        .join("proxi")
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join(format!("{name}.json"));
    let raw = fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("parse {}: {error}", file.display()))
}

/// Mirrors `normaliseStream` of app/test/capture-lesson-golden.ts: every root form, then separators.
pub fn normalise_stream(text: &str, roots: &[String]) -> String {
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
pub fn normalise_file(text: &str) -> String {
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
pub fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
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
pub fn changes(before: &BTreeMap<String, Vec<u8>>, after: &BTreeMap<String, Vec<u8>>) -> Value {
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
pub fn describe_files(expected: &Value, actual: &Value) -> Option<String> {
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

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Writes the golden seed as files under `data`, creating the folders they need.
pub fn write_seed(data: &Path, seed: &Value) {
    for (rel, content) in seed.as_object().expect("seed object") {
        let content = content.as_str().expect("seed content is text");
        let target = data.join(rel);
        fs::create_dir_all(target.parent().expect("seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(&target, content.as_bytes())
            .unwrap_or_else(|error| panic!("write seed {rel}: {error}"));
    }
}

/// A proxi command in `cwd` reading `data` as its data root; add the subcommand and its args.
pub fn proxi_command(data: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(super::proxi_bin());
    command
        .current_dir(cwd)
        .env("LEARNER_DATA_ROOT", data)
        .env_remove("LEARNER_REPO_ROOT")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped());
    command
}

pub fn run(mut command: Command, stdin: Option<&str>) -> Run {
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

pub fn data_roots(data: &Path) -> Vec<String> {
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
