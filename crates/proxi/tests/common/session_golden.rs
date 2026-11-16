//! Node golden helpers shared by the session and job parity tests: seeds, root variants, the `normaliseSession` port.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub fn golden_file(name: &str) -> PathBuf {
    super::repo_root()
        .join("crates")
        .join("proxi")
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join(name)
}

pub fn case<'a>(golden: &'a Value, name: &str) -> &'a Value {
    golden["cases"]
        .as_array()
        .expect("cases array")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("no {name} case in the golden"))
}

/// The golden `dataSeed`: raw text, written verbatim, never normalised.
pub fn write_data_seed(data: &Path, seed: &Value) {
    let Some(files) = seed.as_object() else {
        return;
    };
    for (rel, text) in files {
        let target = data.join(rel);
        fs::create_dir_all(target.parent().expect("data seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(&target, text.as_str().expect("data seed value is a string"))
            .unwrap_or_else(|error| panic!("write data seed {rel}: {error}"));
    }
}

pub fn write_seed(repo: &Path, seed: &Value) {
    let Some(files) = seed.as_object() else {
        return;
    };
    for (rel, value) in files {
        let target = repo.join(rel);
        fs::create_dir_all(target.parent().expect("seed file has a parent"))
            .unwrap_or_else(|error| panic!("create parents of {rel}: {error}"));
        fs::write(
            &target,
            serde_json::to_vec_pretty(value).expect("serialise seed"),
        )
        .unwrap_or_else(|error| panic!("write seed {rel}: {error}"));
    }
}

/// Every path form the server may report for `dir`: the resolved path and, when it exists, the
/// native realpath (Node's `rootVariants`).
pub fn root_variants(dir: &Path) -> Vec<String> {
    let mut out = vec![proxi::data_root::resolve_path(dir).display().to_string()];
    if let Ok(real) = fs::canonicalize(dir) {
        let real = real.display().to_string();
        if !out.contains(&real) {
            out.push(real);
        }
    }
    out
}

pub struct Roots {
    pub repo: Vec<String>,
    pub data: Vec<String>,
}

/// Compares a Rust output with the golden: values, then the compact serialisation, so key order
/// has to match Node's too.
pub fn assert_same(label: &str, rust: &Value, node: &Value, roots: &Roots, session_id: &str) {
    let got = normalise_session(rust.clone(), roots, session_id);
    assert_eq!(got, *node, "{label}");
    assert_eq!(
        serde_json::to_string(&got).expect("serialise Rust output"),
        serde_json::to_string(node).expect("serialise golden output"),
        "{label}: key order"
    );
}

/// Port of `normaliseSession` of app/test/capture-golden.ts.
pub fn normalise_session(value: Value, roots: &Roots, session_id: &str) -> Value {
    match value {
        Value::String(text) => Value::String(normalise_text(&text, roots, session_id)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| normalise_session(item, roots, session_id))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                out.insert(key, normalise_session(item, roots, session_id));
            }
            Value::Object(out)
        }
        other => other,
    }
}

/// In this order: the session id, every other UUID, the fake engine's pids, agy's and opencode's
/// invented ids, then the whole-string root and ISO rules of `normalise`.
pub fn normalise_text(text: &str, roots: &Roots, session_id: &str) -> String {
    let replaced = text.replace(session_id, "<session>");
    let replaced = replace_uuids(&replaced);
    let replaced = replace_pid_counter(&replaced, "msg_");
    let replaced = replace_pid_counter(&replaced, "toolu_fake_");
    let replaced = replace_engine_id(&replaced, "conv-");
    let replaced = replace_engine_id(&replaced, "ses-");
    let replaced = replace_step_ids(&replaced);
    if roots.repo.iter().any(|root| root == &replaced) {
        "<repoRoot>".to_string()
    } else if roots.data.iter().any(|root| root == &replaced) {
        "<dataRoot>".to_string()
    } else if is_iso(&replaced) {
        "<iso>".to_string()
    } else {
        replaced
    }
}

/// `[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}`, scanned left to right.
pub fn replace_uuids(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        if uuid_at(&bytes[at..]) {
            out.push_str("<uuid>");
            at += 36;
        } else {
            let ch = text[at..].chars().next().expect("a char at a boundary");
            out.push(ch);
            at += ch.len_utf8();
        }
    }
    out
}

pub fn uuid_at(bytes: &[u8]) -> bool {
    if bytes.len() < 36 {
        return false;
    }
    bytes[..36].iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_digit() || (b'a'..=b'f').contains(b),
    })
}

/// `/msg_\d+_/g` (and its `toolu_fake_` twin): the pid goes, the rest of the id stays.
pub fn replace_pid_counter(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(prefix) {
        out.push_str(&rest[..start]);
        let after = &rest[start + prefix.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after.as_bytes().get(digits) == Some(&b'_') {
            out.push_str(prefix);
            out.push_str("<pid>_");
            rest = &after[digits + 1..];
        } else {
            out.push_str(prefix);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `/conv-\d+/g` and `/ses-\d+/g`: the engine's conversation/session ids keep the prefix only.
pub fn replace_engine_id(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(prefix) {
        out.push_str(&rest[..start]);
        let after = &rest[start + prefix.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            out.push_str(prefix);
            out.push_str("<pid>");
            rest = &after[digits..];
        } else {
            // No digits: no match here, the regex retries one char later.
            out.push_str(&rest[start..start + 1]);
            rest = &rest[start + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// `/:a\d+_\d+_/g`: agy's `a<pid>_<run>_<step>` ids keep the step, losing the pid and the run.
pub fn replace_step_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(":a") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let pid = after.bytes().take_while(u8::is_ascii_digit).count();
        let run = after[pid..]
            .strip_prefix('_')
            .map(|tail| tail.bytes().take_while(u8::is_ascii_digit).count())
            .unwrap_or(0);
        if pid > 0 && run > 0 && after.as_bytes().get(pid + 1 + run) == Some(&b'_') {
            out.push_str(":a<pid>_<run>_");
            rest = &after[pid + 1 + run + 1..];
        } else {
            // No full id: no match here, the regex retries one char later.
            out.push_str(&rest[start..start + 1]);
            rest = &rest[start + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// `YYYY-MM-DDTHH:MM:SS` with an optional fraction, UTC only (no regex crate here).
pub fn is_iso(s: &str) -> bool {
    let b = s.as_bytes();
    let two_digits = |at: usize| b[at..at + 2].iter().all(u8::is_ascii_digit);
    if b.len() < 20
        || !b[..4].iter().all(u8::is_ascii_digit)
        || b[4] != b'-'
        || !two_digits(5)
        || b[7] != b'-'
        || !two_digits(8)
        || b[10] != b'T'
        || !two_digits(11)
        || b[13] != b':'
        || !two_digits(14)
        || b[16] != b':'
        || !two_digits(17)
    {
        return false;
    }
    match b[19] {
        b'Z' => b.len() == 20,
        b'.' => {
            let rest = &b[20..];
            rest.len() >= 2
                && rest[rest.len() - 1] == b'Z'
                && rest[..rest.len() - 1].iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}
