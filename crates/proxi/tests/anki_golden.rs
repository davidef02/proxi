//! Replays the golden captured from the Node anki CLI (app/test/capture-anki-golden.ts).

mod common;

#[path = "common/fake_http.rs"]
mod fake_http;

use std::cmp::Reverse;
use std::fs;
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use common::cli_golden;
use fake_http::{FakeHttp, Reply};
use serde_json::{json, Value};

/// Real URLs first (longest first: one port can spell the start of another), then the data
/// root and every response time, so a live run reads like the capture.
fn normalise_anki_stream(text: &str, urls: &[(String, String)], roots: &[String]) -> String {
    let mut pairs: Vec<&(String, String)> = urls.iter().collect();
    pairs.sort_by_key(|(url, _)| Reverse(url.len()));
    let mut out = text.to_string();
    for (url, placeholder) in pairs {
        out = out.replace(url.as_str(), placeholder.as_str());
    }
    replace_ms(&cli_golden::normalise_stream(&out, roots))
}

/// `(123ms)` → `(<ms>ms)`, hand-scanned: no regex crate in this binary.
fn replace_ms(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'(' {
            let mut end = at + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > at + 1 && bytes[end..].starts_with(b"ms)") {
                out.push_str("(<ms>ms)");
                at = end + 3;
                continue;
            }
        }
        let ch = text[at..]
            .chars()
            .next()
            .expect("index sits on a char boundary");
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// The error text of a normalised OFFLINE line, asserting the rest of the line is intact.
fn offline_error(text: &str, label: &str) -> String {
    const PREFIX: &str = "⚠️ AnkiConnect is OFFLINE (<deadUrl>): ";
    const SUFFIX: &str = " (<ms>ms)\n";
    let rest = text
        .strip_prefix(PREFIX)
        .unwrap_or_else(|| panic!("{label}: stdout does not start with {PREFIX:?}: {text:?}"));
    let error = rest
        .strip_suffix(SUFFIX)
        .unwrap_or_else(|| panic!("{label}: stdout does not end with {SUFFIX:?}: {text:?}"));
    assert!(!error.is_empty(), "{label}: empty error text: {text:?}");
    error.to_string()
}

fn find_case<'a>(golden: &'a Value, name: &str) -> &'a Value {
    golden["cases"]
        .as_array()
        .expect("cases array")
        .iter()
        .find(|case| case["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("case {name}"))
}

#[test]
fn anki_cli_matches_the_node_golden() {
    let golden = cli_golden::golden("anki-cli");
    assert_eq!(golden["route"].as_str(), Some("anki-cli"), "golden route");

    let t = common::temp_dir("anki-golden");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&data).expect("create data");
    fs::create_dir_all(&cwd).expect("create cwd");

    cli_golden::write_seed(&data, &golden["seed"]);

    let roots = cli_golden::data_roots(&data);

    let anki_error = golden["ankiError"].as_str().expect("ankiError").to_string();
    let online = FakeHttp::start(|_| Reply::json(200, json!({ "result": 6, "error": null })));
    let error_server =
        FakeHttp::start(move |_| Reply::json(200, json!({ "result": null, "error": anki_error })));
    let slow = FakeHttp::start(|_| {
        thread::sleep(Duration::from_millis(1500));
        Reply::json(200, json!({ "result": 6, "error": null }))
    });
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a dead port");
        let url = format!("http://{}", listener.local_addr().expect("dead addr"));
        drop(listener);
        url
    };
    let urls = vec![
        (online.url.clone(), "<ankiUrl>".to_string()),
        (error_server.url.clone(), "<errorUrl>".to_string()),
        (slow.url.clone(), "<slowUrl>".to_string()),
        (dead, "<deadUrl>".to_string()),
    ];

    // Node calls the server timeout a request timeout; proxi prints reqwest's wording.
    let accepted = [common::anki_seed::OFFLINE_ERRORS[0], "Connection timed out"];

    for case in golden["cases"].as_array().expect("cases array") {
        let name = case["name"].as_str().expect("case name");
        let label = format!("anki-cli/{name}");
        let args: Vec<String> = case["args"]
            .as_array()
            .expect("case args")
            .iter()
            .map(|arg| {
                let mut arg = arg.as_str().expect("arg is a string").to_string();
                for (url, placeholder) in &urls {
                    arg = arg.replace(placeholder.as_str(), url.as_str());
                }
                arg
            })
            .collect();

        let before = cli_golden::snapshot(&data);
        let run = cli_golden::run(
            {
                let mut command = cli_golden::proxi_command(&data, &cwd);
                command.arg("anki").args(&args);
                command
            },
            case["stdin"].as_str(),
        );
        let after = cli_golden::snapshot(&data);

        let expected_code = case["exitCode"].as_i64().expect("golden exit code") as i32;
        assert_eq!(
            run.code,
            expected_code,
            "{label}: exit code\nstdout: {}\nstderr: {}",
            normalise_anki_stream(&run.stdout, &urls, &roots),
            normalise_anki_stream(&run.stderr, &urls, &roots)
        );

        if name == "ping-offline" {
            // A dead port refuses fast on Linux and slowly on Windows, so the two CLIs can
            // word the same failure differently: only the shape of the line is pinned.
            let captured = offline_error(case["stdout"].as_str().expect("golden stdout"), &label);
            assert!(
                accepted.contains(&captured.as_str()),
                "{label}: golden error {captured:?}"
            );
            let actual = normalise_anki_stream(&run.stdout, &urls, &roots);
            let actual = offline_error(&actual, &label);
            assert!(
                accepted.contains(&actual.as_str()),
                "{label}: error {actual:?}"
            );
        } else {
            assert_eq!(
                normalise_anki_stream(&run.stdout, &urls, &roots),
                case["stdout"].as_str().expect("golden stdout"),
                "{label}: stdout"
            );
        }

        match case["stderrPrefix"].as_str() {
            Some(prefix) => assert!(
                run.stderr.starts_with(prefix),
                "{label}: stderr does not start with {prefix:?}: {}",
                normalise_anki_stream(&run.stderr, &urls, &roots)
            ),
            None => assert_eq!(
                normalise_anki_stream(&run.stderr, &urls, &roots),
                case["stderr"].as_str().expect("golden stderr"),
                "{label}: stderr"
            ),
        }

        let actual_files = cli_golden::changes(&before, &after);
        let expected_files = case["files"].clone();
        if let Some(detail) = cli_golden::describe_files(&expected_files, &actual_files) {
            panic!("{label}: files differ: {detail}");
        }
        assert_eq!(actual_files, expected_files, "{label}: files");
    }
}

#[test]
fn golden_covers_every_case() {
    let golden = cli_golden::golden("anki-cli");
    let names: Vec<String> = golden["cases"]
        .as_array()
        .expect("cases array")
        .iter()
        .map(|case| case["name"].as_str().expect("case name").to_string())
        .collect();
    let expected: Vec<&str> = vec![
        "help",
        "usage-none",
        "unknown",
        "ping-online",
        "ping-error",
        "ping-timeout",
        "ping-offline",
        "extract-course",
        "extract-loose",
        "extract-standalone",
        "extract-upper",
        "extract-missing-flag",
        "extract-not-found",
        "extract-no-island",
        "extract-empty-island",
        "extract-blank-island",
        "extract-bad-json",
        "export-lesson",
        "export-lesson-wins",
        "export-data-nodes",
        "export-stdin-nodes",
        "export-data-cards",
        "export-cloze-only",
        "export-nodes-win",
        "export-empty-data-falls-through",
        "export-base-dir",
        "export-no-payload",
        "export-invalid-data",
        "export-invalid-stdin-with-lesson",
    ];
    assert_eq!(names, expected, "golden case names");
}

#[test]
fn golden_pins_data_stdin_parity() {
    let golden = cli_golden::golden("anki-cli");
    let from_data = find_case(&golden, "export-data-nodes")["files"]["decks/from-data.tsv"].clone();
    let from_stdin =
        find_case(&golden, "export-stdin-nodes")["files"]["decks/from-stdin.tsv"].clone();
    assert!(
        from_data.is_string(),
        "export-data-nodes wrote no tsv: {from_data}"
    );
    assert_eq!(
        from_data, from_stdin,
        "--data and --stdin must pin the same tsv"
    );
}

#[test]
fn golden_pins_latex_and_escaping() {
    let golden = cli_golden::golden("anki-cli");
    let tsv = find_case(&golden, "export-lesson")["files"]["decks/reattivita.tsv"]
        .as_str()
        .expect("export-lesson tsv");
    for needle in ["\\(", "\\[2x\\]", "<br>", "#deck:Proxi::Vue.js & Vetrine"] {
        assert!(
            tsv.contains(needle),
            "export-lesson must contain {needle:?}: {tsv}"
        );
    }
    for line in tsv.lines().filter(|line| !line.starts_with('#')) {
        let columns: Vec<&str> = line.split('\t').collect();
        assert_eq!(columns.len(), 3, "a raw tab leaked into a field: {line:?}");
        assert!(!columns[0].is_empty(), "empty first column: {line:?}");
    }
}

#[test]
fn normalise_anki_stream_replaces_urls_and_ms() {
    let urls = vec![
        ("http://h:12345".to_string(), "<long>".to_string()),
        ("http://h:1234".to_string(), "<short>".to_string()),
    ];
    let roots = vec![r"C:\data".to_string()];
    assert_eq!(
        normalise_anki_stream(
            r"C:\data\decks\a.tsv http://h:12345 http://h:1234 (12ms) (7ms) (ms) (12 ms)",
            &urls,
            &roots
        ),
        "<dataRoot>/decks/a.tsv <long> <short> (<ms>ms) (<ms>ms) (ms) (12 ms)"
    );
}
