//! `proxi anki` end to end: flags, ping against a fake AnkiConnect, export files.

mod common;

#[path = "common/fake_http.rs"]
mod fake_http;

use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use common::cli_golden;
use fake_http::{FakeHttp, Reply};
use serde_json::json;

fn run(data: &Path, cwd: &Path, args: &[&str], stdin: Option<&str>) -> cli_golden::Run {
    cli_golden::run(
        {
            let mut command = cli_golden::proxi_command(data, cwd);
            command.arg("anki").args(args);
            command
        },
        stdin,
    )
}

/// A data root and a cwd for one test; the CLI creates the data root itself.
fn dirs(tag: &str) -> (common::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let t = common::temp_dir(tag);
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");
    (t, data, cwd)
}

#[test]
fn ping_online_prints_version() {
    let fake = FakeHttp::start(|_| Reply::json(200, json!({ "result": 6, "error": null })));
    let (_t, data, cwd) = dirs("anki-cli-ping-online");

    let run = run(&data, &cwd, &["ping", "--anki-url", fake.url.as_str()], None);
    assert_eq!(run.code, 0);
    let prefix = format!("✅ AnkiConnect is ONLINE ({}) — Version 6 (", fake.url);
    assert!(run.stdout.starts_with(&prefix), "stdout: {}", run.stdout);
    assert!(run.stdout.ends_with("ms)\n"), "stdout: {}", run.stdout);
    assert_eq!(run.stderr, "");

    let requests = fake.requests();
    assert_eq!(requests.len(), 1, "one version probe");
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(requests[0].json(), json!({ "action": "version", "version": 6 }));
}

#[test]
fn ping_http_status() {
    let fake = FakeHttp::start(|_| Reply::text(500, "boom"));
    let (_t, data, cwd) = dirs("anki-cli-ping-status");

    let run = run(&data, &cwd, &["ping", "--anki-url", fake.url.as_str()], None);
    assert_eq!(run.code, 0);
    let offline = format!(
        "⚠️ AnkiConnect is OFFLINE ({}): HTTP error status 500 (",
        fake.url
    );
    assert!(run.stdout.starts_with(&offline), "stdout: {}", run.stdout);
    assert_eq!(run.stderr, "");
}

#[test]
fn ping_error_string() {
    let fake = FakeHttp::start(|_| Reply::json(200, json!({ "result": null, "error": "boom" })));
    let (_t, data, cwd) = dirs("anki-cli-ping-error");

    let run = run(&data, &cwd, &["ping", "--anki-url", fake.url.as_str()], None);
    assert_eq!(run.code, 0);
    assert!(run.stdout.contains(": boom ("), "stdout: {}", run.stdout);
}

#[test]
fn ping_timeout() {
    let fake = FakeHttp::start(|_| {
        thread::sleep(Duration::from_millis(1500));
        Reply::json(200, json!({ "result": 6, "error": null }))
    });
    let (_t, data, cwd) = dirs("anki-cli-ping-timeout");

    let started = Instant::now();
    let run = run(
        &data,
        &cwd,
        &["ping", "--anki-url", fake.url.as_str(), "--timeout", "200"],
        None,
    );
    let elapsed = started.elapsed();

    assert_eq!(run.code, 0);
    assert!(
        run.stdout.contains("Connection timed out"),
        "stdout: {}",
        run.stdout
    );
    assert!(
        elapsed < Duration::from_millis(1500),
        "the 200ms timeout must not wait for the slow server: {elapsed:?}"
    );
}

#[test]
fn ping_dead_port() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    drop(listener);
    let (_t, data, cwd) = dirs("anki-cli-ping-dead");

    let run = run(&data, &cwd, &["ping", "--anki-url", url.as_str()], None);
    assert_eq!(run.code, 0);
    // A closed port refuses fast on Linux and waits for the SYN retries on Windows.
    assert!(
        run.stdout.contains(": fetch failed (")
            || run.stdout.contains(": Connection timed out ("),
        "stdout: {}",
        run.stdout
    );
}

#[test]
fn ping_ignores_learner_anki_url() {
    let fake = FakeHttp::start(|_| Reply::json(200, json!({ "result": 6, "error": null })));
    let (_t, data, cwd) = dirs("anki-cli-ping-env");

    let run = cli_golden::run(
        {
            let mut command = cli_golden::proxi_command(&data, &cwd);
            command
                .arg("anki")
                .args(["ping", "--timeout", "200"])
                .env("LEARNER_ANKI_URL", &fake.url);
            command
        },
        None,
    );

    assert_eq!(run.code, 0);
    assert!(
        run.stdout.contains("(http://127.0.0.1:8765)"),
        "stdout: {}",
        run.stdout
    );
    assert_eq!(fake.requests().len(), 0, "LEARNER_ANKI_URL is not a ping flag");
}

const NODES: &str = r#"{"topic":"Algebra lineare","nodes":[{"nodeId":"n-eigen","tier":"axiom","ankiParams":{"targetQuestion":"Quanto vale $\\lambda$?","targetFact":"Lo spettro\tcompleto"}}]}"#;

#[test]
fn export_data_and_stdin_write_identical_files() {
    let (_t, data, cwd) = dirs("anki-cli-export-parity");

    let from_data = run(
        &data,
        &cwd,
        &["export", "--data", NODES, "--out", "decks/a.tsv"],
        None,
    );
    assert_eq!(from_data.code, 0, "stderr: {}", from_data.stderr);
    assert_eq!(
        from_data.stdout,
        "✅ Exported 1 cards to decks/a.tsv\n"
    );

    let from_stdin = run(
        &data,
        &cwd,
        &["export", "--stdin", "--out", "decks/b.tsv"],
        Some(NODES),
    );
    assert_eq!(from_stdin.code, 0, "stderr: {}", from_stdin.stderr);
    assert_eq!(
        from_stdin.stdout,
        "✅ Exported 1 cards to decks/b.tsv\n"
    );

    let a = fs::read(data.join("decks").join("a.tsv")).expect("read a.tsv");
    let b = fs::read(data.join("decks").join("b.tsv")).expect("read b.tsv");
    assert!(!a.is_empty(), "a.tsv is empty");
    assert_eq!(a, b, "--data and --stdin must write the same TSV");
}

#[test]
fn export_invalid_stdin_fails_before_lesson() {
    let (_t, data, cwd) = dirs("anki-cli-export-bad-stdin");

    let run = run(
        &data,
        &cwd,
        &["export", "--stdin", "--lesson", "x.html"],
        Some("nope"),
    );
    assert_eq!(run.code, 1);
    assert!(
        run.stderr.starts_with("Fatal error: SyntaxError: "),
        "stderr: {}",
        run.stderr
    );
    assert_eq!(run.stdout, "");
    assert!(!data.join("decks").exists(), "no decks dir on a bad payload");
}

#[test]
fn extract_keeps_latex_backslashes() {
    let (_t, data, cwd) = dirs("anki-cli-extract-latex");
    let lesson = data.join("lessons-sciolte/0001-latex.html");
    fs::create_dir_all(lesson.parent().expect("lesson parent")).expect("create lessons dir");
    fs::write(
        &lesson,
        "<html><body><script type=\"application/json\" id=\"lesson-data\">\
{\"metadata\":{\"topic\":\"Latex\",\"slug\":\"latex\"},\"nodes\":[\
{\"nodeId\":\"n1\",\"tier\":\"axiom\",\"ankiParams\":{\
\"targetQuestion\":\"Quanto vale $x$?\",\"targetFact\":\"$2x$\"}}]}\
</script></body></html>",
    )
    .expect("write lesson");

    let run = run(
        &data,
        &cwd,
        &["extract", "--lesson", "lessons-sciolte/0001-latex.html"],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    // Raw stdout, not the golden: capture normalisation turns every backslash into `/`.
    let stdout: serde_json::Value = serde_json::from_str(&run.stdout).expect("extract prints JSON");
    assert_eq!(stdout["cards"][0]["front"], json!("Quanto vale \\(x\\)?"));
    assert_eq!(stdout["cards"][0]["back"], json!("\\(2x\\)"));
}

#[test]
fn extract_requires_lesson() {
    let (_t, data, cwd) = dirs("anki-cli-extract-no-flag");

    let run = run(&data, &cwd, &["extract"], None);
    assert_eq!(run.code, 1);
    assert_eq!(
        run.stderr,
        "Error: --lesson <path> is required for extract command\n"
    );
    assert_eq!(run.stdout, "");
}

#[test]
fn sync_is_not_ported() {
    let (_t, data, cwd) = dirs("anki-cli-sync");

    let run = run(&data, &cwd, &["sync"], None);
    assert_eq!(run.code, 1);
    assert_eq!(
        run.stderr,
        "Error: sync is not ported to proxi yet: run npm run anki -- sync\n"
    );
}

#[test]
fn unknown_command_exits_1() {
    let (_t, data, cwd) = dirs("anki-cli-unknown");

    let run = run(&data, &cwd, &["frobnicate"], None);
    assert_eq!(run.code, 1);
    assert_eq!(run.stderr, "Unknown command: frobnicate\n");
    assert_eq!(run.stdout, "");
}

#[test]
fn help_lists_commands() {
    let (_t, data, cwd) = dirs("anki-cli-help");

    for args in [&["--help"][..], &[][..]] {
        let run = run(&data, &cwd, args, None);
        assert_eq!(run.code, 0, "{args:?}");
        assert!(
            run.stdout.starts_with("\nProxi Anki Sync & Deck Exporter\n"),
            "{args:?} stdout: {}",
            run.stdout
        );
        assert_eq!(run.stderr, "", "{args:?}");
    }
}

#[test]
fn top_level_help_lists_anki() {
    let (_t, data, cwd) = dirs("anki-cli-top-help");

    let run = cli_golden::run(
        {
            let mut command = cli_golden::proxi_command(&data, &cwd);
            command.arg("--help");
            command
        },
        None,
    );
    assert_eq!(run.code, 0);
    assert!(run.stdout.contains("anki"), "stdout: {}", run.stdout);
}
