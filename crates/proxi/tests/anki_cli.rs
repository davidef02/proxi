//! `proxi anki` end to end: flags, ping against a fake AnkiConnect, export files.

mod common;

#[path = "common/fake_http.rs"]
mod fake_http;

#[path = "common/mock_anki.rs"]
mod mock_anki;

use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use common::cli_golden;
use fake_http::{FakeHttp, Reply};
use mock_anki::MockAnki;
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
fn export_base_dir_joins_like_node() {
    let (_t, data, cwd) = dirs("anki-cli-export-base-dir");

    // path.join drops the `./`: Node prints `out/decks/base.tsv`, not `./out/decks/base.tsv`.
    let run = run(
        &data,
        &cwd,
        &[
            "export",
            "--data",
            "{\"topic\":\"Base\"}",
            "--base-dir",
            "./out",
        ],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        run.stdout,
        format!("✅ Exported 0 cards to out{sep}decks{sep}base.tsv\n")
    );
    assert!(
        data.join("out").join("decks").join("base.tsv").is_file(),
        "the default target is created under the joined decks dir"
    );
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

const CURRICULUM: &str =
    r#"{"chapters":[{"title":"Vue.js & Vetrine","slug":"vue-vetrine","lessons":[]}]}"#;

const COURSE_LESSON: &str = "courses/vue-vetrine/lessons/0001-reattivita.html";
const LOOSE_LESSON: &str = "lessons-sciolte/0001-entropia.html";

fn write_file(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("seed file has a parent")).expect("create parents");
    fs::write(&path, content).expect("write seed file");
}

/// The document as the engine writes it: the JSON island inside the lesson shell.
fn island(doc: &serde_json::Value) -> String {
    format!(
        "<!doctype html>\n<html><body>\n  <script type=\"application/json\" id=\"lesson-data\">\n{}\n  </script>\n</body></html>\n",
        serde_json::to_string(doc).expect("serialise lesson")
    )
}

/// A course lesson with one basic and one cloze card, plus a loose lesson.
fn seed_lessons(data: &Path) {
    write_file(data, "curriculum.json", CURRICULUM);
    write_file(
        data,
        COURSE_LESSON,
        &island(&json!({
            "metadata": { "topic": "Vue.js & Vetrine", "title": "Reattività", "slug": "reattivita" },
            "nodes": [
                {
                    "nodeId": "n-basic",
                    "tier": "axiom",
                    "ankiParams": { "targetQuestion": "Q?", "targetFact": "A." }
                },
                {
                    "nodeId": "n-cloze",
                    "tier": "derivation",
                    "ankiParams": { "suggestedCloze": "Il ref {{c1::traccia}} chi lo legge." }
                }
            ]
        })),
    );
    write_file(
        data,
        LOOSE_LESSON,
        &island(&json!({
            "metadata": { "title": "Entropia", "slug": "entropia" },
            "nodes": [{
                "nodeId": "n1",
                "tier": "axiom",
                "ankiParams": { "targetQuestion": "Che cos'è l'entropia?", "targetFact": "Una misura del disordine." }
            }]
        })),
    );
}

fn deck_tsv(data: &Path) -> std::path::PathBuf {
    data.join("decks").join("reattivita.tsv")
}

fn actions_of(fake: &MockAnki) -> Vec<String> {
    fake.actions()
}

fn note_tags(note: &serde_json::Value) -> Vec<String> {
    note["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| tag.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn sync_sends_node_request_sequence() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-sequence");
    seed_lessons(&data);

    let run = run(
        &data,
        &cwd,
        &["sync", "--lesson", COURSE_LESSON, "--anki-url", fake.url()],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");

    let bodies: Vec<String> = fake
        .http
        .requests()
        .iter()
        .map(|request| request.body.clone())
        .collect();
    assert_eq!(bodies.len(), 4, "{bodies:#?}");
    assert_eq!(bodies[0], r#"{"action":"version","version":6}"#);
    assert_eq!(
        bodies[1],
        r#"{"action":"createDeck","version":6,"params":{"deck":"Proxi::Vue.js & Vetrine"}}"#
    );
    assert_eq!(bodies[2], r#"{"action":"modelNames","version":6,"params":{}}"#);
    assert_eq!(
        bodies[3],
        concat!(
            r#"{"action":"addNotes","version":6,"params":{"notes":["#,
            r#"{"deckName":"Proxi::Vue.js & Vetrine","modelName":"Basic","fields":{"Front":"Q?","Back":"A."},"tags":["learn-forme","vuejs-vetrine","axiom","tier::axiom","node::n-basic","course::vue-vetrine","lesson::reattivita"],"options":{"allowDuplicate":false,"duplicateScope":"deck"}},"#,
            r#"{"deckName":"Proxi::Vue.js & Vetrine","modelName":"Cloze","fields":{"Text":"Il ref {{c1::traccia}} chi lo legge.","Extra":""},"tags":["learn-forme","vuejs-vetrine","derivation","tier::derivation","node::n-cloze","course::vue-vetrine","lesson::reattivita"],"options":{"allowDuplicate":false,"duplicateScope":"deck"}}"#,
            "]}}"
        )
    );
}

#[test]
fn sync_course_lesson_lands_in_chapter_deck() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-course");
    seed_lessons(&data);

    let run = run(
        &data,
        &cwd,
        &["sync", "--lesson", COURSE_LESSON, "--anki-url", fake.url()],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let tsv = deck_tsv(&data);
    assert_eq!(
        run.stdout,
        format!(
            "✅ Synced 2 cards to deck 'Proxi::Vue.js & Vetrine' (0 duplicates skipped)\n📁 Backup TSV saved to {}\n",
            tsv.display()
        )
    );

    let notes = fake.added_notes();
    assert_eq!(notes.len(), 2);
    for note in &notes {
        assert_eq!(note["deckName"], json!("Proxi::Vue.js & Vetrine"));
        let tags = note_tags(note);
        for tag in ["course::vue-vetrine", "lesson::reattivita"] {
            assert!(tags.iter().any(|text| text == tag), "missing {tag}: {tags:?}");
        }
        assert!(
            tags.iter().any(|text| text.starts_with("node::")),
            "missing node tag: {tags:?}"
        );
    }

    let content = fs::read_to_string(&tsv).expect("read TSV");
    assert!(content.contains("#deck:Proxi::Vue.js & Vetrine\n"), "{content}");
}

#[test]
fn second_sync_adds_nothing() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-twice");
    seed_lessons(&data);
    let args = ["sync", "--lesson", COURSE_LESSON, "--anki-url", fake.url()];

    let first = run(&data, &cwd, &args, None);
    assert_eq!(first.code, 0, "stderr: {}", first.stderr);
    assert!(first.stdout.starts_with("✅ Synced 2 cards"), "{}", first.stdout);

    let second = run(&data, &cwd, &args, None);
    assert_eq!(second.code, 0, "stderr: {}", second.stderr);
    let tsv = deck_tsv(&data);
    assert_eq!(
        second.stdout,
        format!(
            "✅ Synced 0 cards to deck 'Proxi::Vue.js & Vetrine' (2 duplicates skipped)\n📁 Backup TSV saved to {}\n",
            tsv.display()
        )
    );
    assert_eq!(fake.added_notes().len(), 2, "the second run adds no note");
}

#[test]
fn allow_duplicates_adds_again() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-allow");
    seed_lessons(&data);

    for _ in 0..2 {
        let run = run(
            &data,
            &cwd,
            &["sync", "--lesson", COURSE_LESSON, "--anki-url", fake.url()],
            None,
        );
        assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    }

    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--lesson",
            COURSE_LESSON,
            "--anki-url",
            fake.url(),
            "--allow-duplicates",
        ],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.stdout.starts_with(
            "✅ Synced 2 cards to deck 'Proxi::Vue.js & Vetrine' (0 duplicates skipped)"
        ),
        "{}",
        run.stdout
    );
    assert_eq!(fake.added_notes().len(), 4);
    let bodies: Vec<String> = fake
        .http
        .requests()
        .iter()
        .map(|request| request.body.clone())
        .collect();
    let last = bodies.last().expect("an addNotes body");
    assert!(last.contains(r#""allowDuplicate":true"#), "{last}");
}

#[test]
fn deck_flag_overrides_notes_and_tsv() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-deck");
    seed_lessons(&data);

    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--lesson",
            COURSE_LESSON,
            "--deck",
            "Proxi::Personale",
            "--anki-url",
            fake.url(),
        ],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let tsv = deck_tsv(&data);
    assert_eq!(
        run.stdout,
        format!(
            "✅ Synced 2 cards to deck 'Proxi::Personale' (0 duplicates skipped)\n📁 Backup TSV saved to {}\n",
            tsv.display()
        )
    );

    let requests = fake.requests();
    assert_eq!(requests[1]["action"], json!("createDeck"));
    assert_eq!(requests[1]["params"], json!({ "deck": "Proxi::Personale" }));
    for note in fake.added_notes() {
        assert_eq!(note["deckName"], json!("Proxi::Personale"));
    }
    let content = fs::read_to_string(&tsv).expect("read TSV");
    assert!(content.contains("#deck:Proxi::Personale\n"), "{content}");
}

#[test]
fn offline_dead_port_writes_tsv_and_exits_0() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port to drop");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    drop(listener);
    let (_t, data, cwd) = dirs("anki-cli-sync-offline");
    seed_lessons(&data);

    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--lesson",
            COURSE_LESSON,
            "--anki-url",
            &url,
            "--timeout",
            "10000",
        ],
        None,
    );

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    let tsv = deck_tsv(&data);
    let expected = ["fetch failed", "Connection timed out"]
        .iter()
        .map(|error| {
            format!(
                "⚠️ Sync incomplete: [anki-sync] Note: AnkiConnect offline ({url} - {error}). Saved 2 cards to {}.\n📁 Deck TSV exported to {}\n",
                tsv.display(),
                tsv.display()
            )
        })
        .collect::<Vec<_>>();
    assert!(
        expected.iter().any(|line| line == &run.stdout),
        "stdout: {}",
        run.stdout
    );
    assert!(tsv.is_file(), "the offline run still writes the TSV");
}

#[test]
fn anki_error_sends_only_version() {
    let fake = FakeHttp::start(|_| {
        Reply::json(
            200,
            json!({ "result": null, "error": "collection is not available" }),
        )
    });
    let (_t, data, cwd) = dirs("anki-cli-sync-api-error");
    seed_lessons(&data);

    let run = run(
        &data,
        &cwd,
        &["sync", "--lesson", COURSE_LESSON, "--anki-url", &fake.url],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.stdout.contains(&format!(
            "AnkiConnect offline ({} - collection is not available)",
            fake.url
        )),
        "stdout: {}",
        run.stdout
    );
    let actions: Vec<String> = fake
        .requests()
        .iter()
        .map(|request| request.json()["action"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(actions, ["version"]);
    assert!(deck_tsv(&data).is_file());
}

#[test]
fn timeout_is_offline() {
    let fake = FakeHttp::start(|_| {
        thread::sleep(Duration::from_millis(1500));
        Reply::json(200, json!({ "result": 6, "error": null }))
    });
    let (_t, data, cwd) = dirs("anki-cli-sync-timeout");
    seed_lessons(&data);

    let started = Instant::now();
    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--lesson",
            COURSE_LESSON,
            "--anki-url",
            &fake.url,
            "--timeout",
            "200",
        ],
        None,
    );
    let elapsed = started.elapsed();

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.stdout.contains(&format!(
            "AnkiConnect offline ({} - Connection timed out). Saved 2 cards to ",
            fake.url
        )),
        "stdout: {}",
        run.stdout
    );
    assert!(
        elapsed < Duration::from_millis(1500),
        "the 200ms timeout must not wait for the slow server: {elapsed:?}"
    );
    assert!(deck_tsv(&data).is_file());
}

#[test]
fn localized_note_types() {
    let fake = MockAnki::start(
        true,
        Some(&[
            ("Basilare", ["Fronte", "Retro"]),
            ("Testo da completare", ["Testo", "Back Extra"]),
        ]),
    );
    let (_t, data, cwd) = dirs("anki-cli-sync-italian");
    seed_lessons(&data);

    let cards = r#"{"cards":[{"type":"basic","front":"F","back":"B","tags":[]},{"type":"cloze","text":"{{c1::x}}","extra":"E","tags":[]}]}"#;
    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--data",
            cards,
            "--deck",
            "Proxi::Vue",
            "--anki-url",
            fake.url(),
        ],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.stdout.starts_with("✅ Synced 2 cards to deck 'Proxi::Vue'"),
        "stdout: {}",
        run.stdout
    );

    assert_eq!(
        actions_of(&fake).join(","),
        "version,createDeck,modelNames,modelFieldNames,modelFieldNames,addNotes"
    );
    let notes = fake.added_notes();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0]["modelName"], json!("Basilare"));
    assert_eq!(
        notes[0]["fields"],
        json!({ "Fronte": "F", "Retro": "B" })
    );
    assert_eq!(notes[1]["modelName"], json!("Testo da completare"));
    assert_eq!(
        notes[1]["fields"],
        json!({ "Testo": "{{c1::x}}", "Back Extra": "E" })
    );
}

const CALCULUS_CARDS: &str = r#"{"cards":[{"id":"card-1","nodeId":"node-1","type":"basic","deck":"Proxi::Calculus","front":"What is derivative of \\(x^2\\)?","back":"\\(2x\\)","tags":["learn-forme","calculus","tier::derivation"]},{"id":"card-2","nodeId":"node-2","type":"cloze","deck":"Proxi::Calculus","text":"The derivative of \\(e^x\\) is {{c1:\\(e^x\\) }}.","extra":"Rate of change equals function value.","tags":["learn-forme","calculus","tier::unconditional-truth"]}]}"#;

#[test]
fn data_cards_sync_without_lesson() {
    let fake = MockAnki::start(true, None);
    let (t, data, cwd) = dirs("anki-cli-sync-data");
    let base = t.0.join("base");
    let base_arg = base.display().to_string();

    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--data",
            CALCULUS_CARDS,
            "--topic",
            "Calculus",
            "--deck",
            "Proxi::Calculus",
            "--anki-url",
            fake.url(),
            "--base-dir",
            &base_arg,
        ],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let tsv = base.join("decks").join("calculus.tsv");
    assert_eq!(
        run.stdout,
        format!(
            "✅ Synced 2 cards to deck 'Proxi::Calculus' (0 duplicates skipped)\n📁 Backup TSV saved to {}\n",
            tsv.display()
        )
    );
    assert!(tsv.is_file(), "the TSV lands under --base-dir");

    let notes = fake.added_notes();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0]["modelName"], json!("Basic"));
    assert_eq!(
        notes[0]["fields"],
        json!({
            "Front": "What is derivative of \\(x^2\\)?",
            "Back": "\\(2x\\)"
        })
    );
    assert_eq!(notes[1]["modelName"], json!("Cloze"));
    assert_eq!(
        notes[1]["fields"],
        json!({
            "Text": "The derivative of \\(e^x\\) is {{c1:\\(e^x\\) }}.",
            "Extra": "Rate of change equals function value."
        })
    );
}

const ALGEBRA_NODES: &str = r#"{"topic":"Algebra","nodes":[{"nodeId":"n-det","tier":"axiom","ankiParams":{"suggestedCloze":"Il determinante {{c1::e nullo}}"}}]}"#;

#[test]
fn nodes_payload_sends_no_notes() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-nodes");

    let run = run(
        &data,
        &cwd,
        &["sync", "--data", ALGEBRA_NODES, "--anki-url", fake.url()],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let tsv = data.join("decks").join("algebra.tsv");
    assert_eq!(
        run.stdout,
        format!(
            "✅ Synced 0 cards to deck 'Proxi::Algebra' (0 duplicates skipped)\n📁 Backup TSV saved to {}\n",
            tsv.display()
        )
    );

    let add_notes = fake
        .requests()
        .into_iter()
        .find(|request| request["action"] == json!("addNotes"))
        .expect("addNotes sent");
    assert_eq!(add_notes["params"], json!({ "notes": [] }));

    let content = fs::read_to_string(&tsv).expect("read TSV");
    let rows: Vec<&str> = content
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .collect();
    assert_eq!(rows.len(), 1, "the node's card is in the TSV: {content}");
}

#[test]
fn missing_lesson_warns_then_fails() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-missing");

    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--lesson",
            "broken/nope.html",
            "--anki-url",
            fake.url(),
        ],
        None,
    );
    assert_eq!(run.code, 1);
    assert_eq!(run.stdout, "");
    assert_eq!(
        run.stderr,
        "[anki-sync] TSV export skipped: Lesson file not found at path: broken/nope.html\nFatal error: Error: Lesson file not found at path: broken/nope.html\n"
    );
    assert!(fake.requests().is_empty(), "no request reaches Anki");
}

#[test]
fn invalid_data_fails_before_anything() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-bad-data");

    let run = run(
        &data,
        &cwd,
        &["sync", "--data", "{", "--anki-url", fake.url()],
        None,
    );
    assert_eq!(run.code, 1);
    assert!(
        run.stderr.starts_with("Fatal error: SyntaxError: "),
        "stderr: {}",
        run.stderr
    );
    assert!(!data.join("decks").exists(), "no decks dir on a bad payload");
    assert!(fake.requests().is_empty());
}

#[test]
fn create_deck_failure_is_incomplete() {
    let fake = FakeHttp::start(|request| {
        let action = request.json()["action"].as_str().unwrap_or_default().to_string();
        match action.as_str() {
            "version" => Reply::json(200, json!({ "result": 6, "error": null })),
            "createDeck" => Reply::json(200, json!({ "result": null, "error": "deck boom" })),
            _ => Reply::json(200, json!({ "result": null, "error": "unexpected" })),
        }
    });
    let (_t, data, cwd) = dirs("anki-cli-sync-deck-fail");
    seed_lessons(&data);
    let base = data.display().to_string();

    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--lesson",
            COURSE_LESSON,
            "--anki-url",
            &fake.url,
            "--base-dir",
            &base,
        ],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    let tsv = deck_tsv(&data);
    assert_eq!(
        run.stdout,
        format!(
            "⚠️ Sync incomplete: [anki-sync] Failed to create deck 'Proxi::Vue.js & Vetrine': deck boom\n📁 Deck TSV exported to {}\n",
            tsv.display()
        )
    );
    let content = fs::read_to_string(&tsv).expect("read TSV");
    assert!(
        content.contains("#deck:Proxi::Vue.js & Vetrine\n"),
        "{content}"
    );
    let actions: Vec<String> = fake
        .requests()
        .iter()
        .map(|request| request.json()["action"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(actions, ["version", "createDeck"]);
}

#[test]
fn add_notes_failure_is_incomplete() {
    let fake = FakeHttp::start(|request| {
        let payload = request.json();
        let action = payload["action"].as_str().unwrap_or_default();
        match action {
            "version" => Reply::json(200, json!({ "result": 6, "error": null })),
            "createDeck" => Reply::json(200, json!({ "result": 1725128400000i64, "error": null })),
            "modelNames" => Reply::json(200, json!({ "result": ["Basic", "Cloze"], "error": null })),
            "modelFieldNames" => {
                let fields = if payload["params"]["modelName"] == json!("Basic") {
                    json!(["Front", "Back"])
                } else {
                    json!(["Text", "Extra"])
                };
                Reply::json(200, json!({ "result": fields, "error": null }))
            }
            "addNotes" => Reply::json(200, json!({ "result": null, "error": "batch boom" })),
            _ => Reply::json(200, json!({ "result": null, "error": "unexpected" })),
        }
    });
    let (_t, data, cwd) = dirs("anki-cli-sync-add-fail");
    seed_lessons(&data);
    let base = data.display().to_string();

    let run = run(
        &data,
        &cwd,
        &[
            "sync",
            "--lesson",
            COURSE_LESSON,
            "--anki-url",
            &fake.url,
            "--base-dir",
            &base,
        ],
        None,
    );
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    let tsv = deck_tsv(&data);
    assert_eq!(
        run.stdout,
        format!(
            "⚠️ Sync incomplete: [anki-sync] addNotes failed: batch boom\n📁 Deck TSV exported to {}\n",
            tsv.display()
        )
    );
    assert!(tsv.is_file());

    let requests = fake.requests();
    let actions: Vec<String> = requests
        .iter()
        .map(|request| request.json()["action"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        actions,
        [
            "version",
            "createDeck",
            "modelNames",
            "modelFieldNames",
            "modelFieldNames",
            "addNotes"
        ]
    );
    let last = requests.last().expect("addNotes sent");
    assert_eq!(
        last.json()["params"]["notes"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn cards_not_array_is_fatal_when_online() {
    let fake = MockAnki::start(true, None);
    let (_t, data, cwd) = dirs("anki-cli-sync-cards-number");

    let run = run(
        &data,
        &cwd,
        &["sync", "--data", r#"{"cards":5}"#, "--anki-url", fake.url()],
        None,
    );
    assert_eq!(run.code, 1);
    assert!(
        run.stderr.starts_with("[anki-sync] TSV export skipped: "),
        "stderr: {}",
        run.stderr
    );
    assert!(
        run.stderr
            .ends_with("Fatal error: TypeError: cards.map is not a function\n"),
        "stderr: {}",
        run.stderr
    );
    assert_eq!(
        actions_of(&fake).join(","),
        "version,createDeck,modelNames"
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
