mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

/// AC1: identical homepage bytes and output, with and without the non-default wiring.
const GENERATE_NO_ANKI: &[&str] = &[
    "generate-no-anki",
    "generate-default-command",
    "generate-options",
    "generate-empty",
];
/// AC2: the Anki stamp is the only volatile byte; requests are recorded.
const GENERATE_ANKI: &[&str] = &["generate-anki"];
/// AC3: plan writes and every plan error.
const PLAN: &[&str] = &[
    "plan-new",
    "plan-replace",
    "plan-curriculum",
    "plan-empty-file",
    "plan-duplicate-slug",
    "plan-missing-chapter",
    "plan-missing-lessons",
    "plan-not-array",
    "plan-blank-title",
    "plan-malformed",
];
/// AC4: the listing and its option variants, all read-only.
const CHAPTERS: &[&str] = &[
    "chapters",
    "chapters-options",
    "chapters-empty",
    "chapters-malformed",
];
/// AC5: a closed port and an error reply.
const ANKI_UNREACHABLE: &[&str] = &["generate-anki-unreachable", "generate-anki-error"];
/// Help and `parseArgs` messages: the cases not tied to a data-root behaviour.
const CLI_ERRORS_AND_HELP: &[&str] = &[
    "help",
    "help-flag",
    "unknown-command",
    "unknown-option",
    "unknown-short",
    "missing-value",
    "ambiguous-value",
    "boolean-with-value",
    "help-with-value",
];

const STAMP: &str = "Dati Anki al ";
const IT_STAMP_FORMAT: &str = "dd/mm/yyyy, HH:MM:SS";

fn load_golden() -> Value {
    let file = common::repo_root()
        .join("crates")
        .join("proxi")
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("home.json");
    let raw = fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("parse {}: {error}", file.display()))
}

fn case<'a>(golden: &'a Value, name: &str) -> &'a Value {
    golden["cases"]
        .as_array()
        .expect("cases array")
        .iter()
        .find(|case| case["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("golden has no case {name}"))
}

fn replay(names: &[&str]) {
    let golden = load_golden();
    for name in names {
        replay_case(&golden, case(&golden, name));
    }
}

fn replay_case(golden: &Value, case: &Value) {
    let name = case["name"].as_str().expect("case name");
    let seed_name = case["seed"].as_str().expect("case seed");
    let seed = golden["seeds"][seed_name]
        .as_object()
        .unwrap_or_else(|| panic!("{name}: fixture has no seed {seed_name}"));

    let tmp = common::temp_dir("home-golden");
    let data = tmp.0.join("data");
    fs::create_dir_all(&data).unwrap_or_else(|error| panic!("{name}: create data root: {error}"));
    for (rel, content) in seed {
        let content = content
            .as_str()
            .unwrap_or_else(|| panic!("{name}: seed {rel} is not a string"));
        let target = data.join(rel.split('/').collect::<PathBuf>());
        fs::create_dir_all(target.parent().expect("seed file has a parent"))
            .unwrap_or_else(|error| panic!("{name}: create parents of {rel}: {error}"));
        fs::write(&target, content.as_bytes())
            .unwrap_or_else(|error| panic!("{name}: write seed {rel}: {error}"));
    }

    let anki = case
        .get("anki")
        .map(|responses| common::fake_anki_scripted(responses));
    let closed = common::closed_port_url();
    let data_root = proxi::data_root::resolve_path(&data);
    let args: Vec<String> = case["args"]
        .as_array()
        .expect("args array")
        .iter()
        .map(|arg| {
            arg.as_str()
                .expect("arg is a string")
                .replace("<dataRoot>", &data_root.display().to_string())
                .replace("<anki>", anki.as_ref().map(|anki| anki.url.as_str()).unwrap_or(""))
                .replace("<closedAnki>", &closed)
        })
        .collect();

    let output = Command::new(common::proxi_bin())
        .arg("home")
        .args(&args)
        .env(proxi::data_root::DATA_ROOT_ENV, &data)
        .env_remove("LEARNER_ANKI_URL")
        .current_dir(common::repo_root())
        .output()
        .unwrap_or_else(|error| panic!("{name}: run proxi home: {error}"));

    let roots = data_root_variants(&data_root);
    let expected = &case["result"];
    let expected_exit = expected["exitCode"].as_i64().expect("golden exit code") as i32;
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "{name}: exit code ({})",
        output.status
    );
    assert_eq!(
        normalise_text(&String::from_utf8_lossy(&output.stdout), &roots),
        expected["stdout"].as_str().expect("golden stdout"),
        "{name}: stdout"
    );
    assert_eq!(
        normalise_text(&String::from_utf8_lossy(&output.stderr), &roots),
        expected["stderr"].as_str().expect("golden stderr"),
        "{name}: stderr"
    );

    let files = changed_files(&data, seed, name);
    assert_eq!(
        Value::Object(files),
        case["files"],
        "{name}: changed files"
    );

    if let Some(expected_requests) = case.get("ankiRequests") {
        let anki = anki.as_ref().expect("ankiRequests without a fake server");
        assert_eq!(
            Value::Array(anki.requests()),
            *expected_requests,
            "{name}: AnkiConnect requests"
        );
    }
}

/// Mirrors `rootVariants` (capture-golden.ts): the resolved path and the native realpath.
fn data_root_variants(data_root: &Path) -> Vec<String> {
    let mut out = vec![data_root.display().to_string()];
    if let Ok(real) = fs::canonicalize(data_root) {
        let real = real.display().to_string();
        // Rust canonicalisation returns the extended-length form; Node's realpath does not.
        let real = match real.strip_prefix("\\\\?\\") {
            Some(stripped) => stripped.to_string(),
            None => real,
        };
        if !out.contains(&real) {
            out.push(real);
        }
    }
    out.sort_by_key(|root| std::cmp::Reverse(root.len()));
    out
}

/// Mirrors `normaliseText` of capture-golden-home.ts: roots, separators, then the V8 JSON message.
fn normalise_text(text: &str, roots: &[String]) -> String {
    let mut out = text.to_string();
    for root in roots {
        out = out.replace(root, "<dataRoot>");
    }
    let out = out.replace('\\', "/");
    let marker = "Malformed #lesson-data island: ";
    out.split('\n')
        .map(|line| match line.rfind(marker) {
            Some(index) => format!("{}{}<json-error>", &line[..index], marker),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file the case wrote or changed, with the volatile stamp normalised (AC2).
fn changed_files(data: &Path, seed: &Map<String, Value>, case: &str) -> Map<String, Value> {
    let mut rels = Vec::new();
    walk(data, "", &mut rels);
    rels.sort();

    let mut files = Map::new();
    for rel in rels {
        let bytes = fs::read(data.join(rel.split('/').collect::<PathBuf>()))
            .unwrap_or_else(|error| panic!("{case}: read {rel}: {error}"));
        let content = String::from_utf8_lossy(&bytes);
        let unchanged = seed
            .get(&rel)
            .and_then(Value::as_str)
            .is_some_and(|seeded| seeded == content);
        if unchanged {
            continue;
        }
        assert_stamp_shape(&content, &format!("{case}: {rel}"));
        files.insert(rel, Value::String(normalise_file(&content)));
    }
    files
}

fn walk(root: &Path, rel: &str, out: &mut Vec<String>) {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel.split('/').collect::<PathBuf>())
    };
    for entry in fs::read_dir(&dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display())) {
        let entry = entry.unwrap_or_else(|error| panic!("read entry of {}: {error}", dir.display()));
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("type of {}: {error}", entry.path().display()));
        if file_type.is_dir() {
            walk(root, &child, out);
        } else {
            out.push(child);
        }
    }
}

fn is_it_stamp(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 {
        return false;
    }
    let digit = |index: usize| b[index].is_ascii_digit();
    digit(0)
        && digit(1)
        && b[2] == b'/'
        && digit(3)
        && digit(4)
        && b[5] == b'/'
        && digit(6)
        && digit(7)
        && digit(8)
        && digit(9)
        && b[10] == b','
        && b[11] == b' '
        && digit(12)
        && digit(13)
        && b[14] == b':'
        && digit(15)
        && digit(16)
        && b[17] == b':'
        && digit(18)
        && digit(19)
}

/// The raw Rust stamp must already be the it-IT shape before it is normalised (AC2).
fn assert_stamp_shape(text: &str, label: &str) {
    let mut rest = text;
    while let Some(index) = rest.find(STAMP) {
        let after = &rest[index + STAMP.len()..];
        assert!(
            is_it_stamp(after),
            "{label}: expected \"{STAMP}{IT_STAMP_FORMAT}\", got {:?}",
            after.chars().take(24).collect::<String>()
        );
        rest = after;
    }
}

/// Mirrors `normaliseFile`: only the date changes between two runs.
fn normalise_file(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(STAMP) {
        out.push_str(&rest[..index + STAMP.len()]);
        let after = &rest[index + STAMP.len()..];
        if is_it_stamp(after) {
            out.push_str("<it-IT>");
            rest = &after[20..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn sorted_keys(map: &Map<String, Value>) -> Vec<&str> {
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

#[test]
fn golden_generate_no_anki() {
    replay(GENERATE_NO_ANKI);
}

#[test]
fn golden_generate_anki() {
    replay(GENERATE_ANKI);
}

#[test]
fn golden_plan() {
    replay(PLAN);
}

#[test]
fn golden_chapters() {
    replay(CHAPTERS);
}

#[test]
fn golden_anki_unreachable() {
    replay(ANKI_UNREACHABLE);
}

#[test]
fn golden_cli_errors_and_help() {
    replay(CLI_ERRORS_AND_HELP);
}

#[test]
fn golden_covers_every_case() {
    let golden = load_golden();
    let cases = golden["cases"].as_array().expect("cases array");
    let groups = [
        GENERATE_NO_ANKI,
        GENERATE_ANKI,
        PLAN,
        CHAPTERS,
        ANKI_UNREACHABLE,
        CLI_ERRORS_AND_HELP,
    ];
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let owners = groups.iter().filter(|group| group.contains(&name)).count();
        assert_eq!(owners, 1, "case {name} is replayed by {owners} tests");
    }
    for name in groups.iter().flat_map(|group| group.iter()) {
        assert!(
            cases
                .iter()
                .any(|case| case["name"].as_str() == Some(name)),
            "group lists {name}, missing from the fixture"
        );
    }
}

/// A binary that ignored `--dir`/`--curriculum` would still pass the byte comparison against a golden
/// written from the same mistake: this checks the fixture really uses those flags.
#[test]
fn golden_non_default_wiring() {
    let golden = load_golden();
    let generate_options = case(&golden, "generate-options");
    let args: Vec<&str> = generate_options["args"]
        .as_array()
        .expect("args array")
        .iter()
        .map(|arg| arg.as_str().expect("arg is a string"))
        .collect();
    let dir = args
        .iter()
        .position(|arg| *arg == "--dir")
        .expect("generate-options passes --dir");
    assert!(
        args[dir + 1].ends_with("/corsi"),
        "generate-options --dir value: {}",
        args[dir + 1]
    );
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--curriculum" && pair[1] == "plan.json"),
        "generate-options passes --curriculum plan.json"
    );

    let option_files = generate_options["files"]
        .as_object()
        .expect("generate-options files");
    assert_eq!(sorted_keys(option_files), ["board.html"]);
    let no_anki_files = case(&golden, "generate-no-anki")["files"]
        .as_object()
        .expect("generate-no-anki files");
    assert_ne!(
        option_files["board.html"], no_anki_files["index.html"],
        "generate-options must render a different board than the default wiring"
    );

    let plan_curriculum_files = case(&golden, "plan-curriculum")["files"]
        .as_object()
        .expect("plan-curriculum files");
    assert_eq!(sorted_keys(plan_curriculum_files), ["plan.json"]);

    let chapters = case(&golden, "chapters")["result"]["stdout"]
        .as_str()
        .expect("chapters stdout");
    let chapters_options = case(&golden, "chapters-options")["result"]["stdout"]
        .as_str()
        .expect("chapters-options stdout");
    assert_ne!(
        chapters, chapters_options,
        "chapters-options must read a different plan"
    );
}
