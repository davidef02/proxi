mod common;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn run_lesson(data: &Path, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Run {
    let mut command = Command::new(common::proxi_bin());
    command
        .arg("lesson")
        .args(args)
        .current_dir(cwd)
        .env("LEARNER_DATA_ROOT", data)
        .env_remove("LEARNER_REPO_ROOT")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = command.spawn().expect("spawn proxi");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    let output = child.wait_with_output().expect("wait proxi");
    Run {
        code: output.status.code().expect("exit code"),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    }
}

fn temp_cwd(tag: &str) -> common::TempDir {
    let dir = common::temp_dir(tag);
    fs::create_dir_all(&dir.0).expect("create cwd");
    dir
}

#[test]
fn unknown_and_missing_command_print_node_usage_and_create_the_data_folder() {
    let usage = "Usage: html-lesson.ts <command> [options]\n";
    for args in [&["frobnicate"][..], &[][..]] {
        let t = common::temp_dir("lesson-usage");
        let data = t.0.join("nested").join("data");
        let cwd = t.0.join("cwd");
        fs::create_dir_all(&cwd).expect("create cwd");

        let run = run_lesson(&data, &cwd, args, None);
        assert_eq!(run.code, 0, "{args:?}");
        assert_eq!(run.stdout, usage, "{args:?}");
        assert_eq!(run.stderr, "", "{args:?}");
        assert!(data.is_dir(), "{args:?}: data folder not created");
    }
}

#[test]
fn missing_topic_is_a_usage_error() {
    let t = common::temp_dir("lesson-no-topic");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    for args in [&["init", "--title", "X"][..], &["init", "--topic", ""][..]] {
        let run = run_lesson(&data, &cwd, args, None);
        assert_eq!(run.code, 1, "{args:?}");
        assert_eq!(run.stdout, "", "{args:?}");
        assert_eq!(
            run.stderr, "Error: Missing required parameter: --topic\n",
            "{args:?}"
        );
    }
}

#[test]
fn append_without_payload_is_a_usage_error() {
    let t = common::temp_dir("lesson-no-payload");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    let run = run_lesson(&data, &cwd, &["append", "--lesson", "x"], None);
    assert_eq!(run.code, 1);
    assert_eq!(run.stdout, "");
    assert_eq!(
        run.stderr,
        "Error: Must provide --data, --node-json, --node-file, or --stdin for append payload\n"
    );

    let run = run_lesson(&data, &cwd, &["append", "--data", "{}"], None);
    assert_eq!(run.code, 1);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "Error: Lesson file not found: undefined\n");
}

#[test]
fn relative_paths_resolve_against_data_root_not_cwd() {
    let t = common::temp_dir("lesson-relative");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    let run = run_lesson(
        &data,
        &cwd,
        &["init", "--topic", "A", "--out-dir", "courses/x/lessons"],
        None,
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(data.join("courses/x/lessons/0001-a.html").is_file());
    assert_eq!(
        fs::read_dir(&cwd).expect("read cwd").count(),
        0,
        "relative --out-dir must not touch the cwd"
    );

    let printed = if cfg!(windows) {
        "courses\\x\\lessons\\0001-a.html"
    } else {
        "courses/x/lessons/0001-a.html"
    };
    let expected = serde_json::json!({
        "filePath": printed,
        "lessonNumber": 1,
        "slug": "a",
    });
    assert_eq!(
        run.stdout,
        format!("{}\n", proxi::lesson::js::stringify_pretty(&expected))
    );

    fs::write(
        data.join("p.json"),
        r#"{"nodeId":"n","title":"N","tier":"axiom","content":"c"}"#,
    )
    .expect("write payload");
    let run = run_lesson(
        &data,
        &cwd,
        &[
            "append",
            "--lesson",
            "courses/x/lessons/0001-a.html",
            "--node-file",
            "p.json",
        ],
        None,
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("\"nodeCount\": 1"), "{}", run.stdout);

    let run = run_lesson(
        &data,
        &cwd,
        &[
            "append",
            "--lesson",
            "courses/x/lessons/0001-a.html",
            "--node-file",
            "missing.json",
        ],
        None,
    );
    assert_eq!(run.code, 1);
    assert_eq!(run.stdout, "");
    // Node's fs prints the resolved absolute path on Windows (ToNamespacedPath) and the argument as
    // typed on POSIX, so the binary must report the same text on each platform.
    let reported = if cfg!(windows) {
        proxi::data_root::resolve_path(&data)
            .join("missing.json")
            .display()
            .to_string()
    } else {
        "missing.json".to_string()
    };
    assert_eq!(
        run.stderr,
        format!("Error: ENOENT: no such file or directory, open '{reported}'\n")
    );
}

#[test]
fn stdin_payload_is_read() {
    let t = common::temp_dir("lesson-stdin");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    let run = run_lesson(
        &data,
        &cwd,
        &["init", "--topic", "A", "--out-dir", "courses/x/lessons"],
        None,
    );
    assert_eq!(run.code, 0, "{}", run.stderr);

    let run = run_lesson(
        &data,
        &cwd,
        &[
            "append",
            "--lesson",
            "courses/x/lessons/0001-a.html",
            "--stdin",
        ],
        Some(r#"{"nodeId":"n","title":"N","tier":"axiom","content":"c"}"#),
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("\"nodeCount\": 1"), "{}", run.stdout);
}

#[test]
fn init_and_append_do_not_refresh_the_board() {
    let t = common::temp_dir("lesson-no-board");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    let run = run_lesson(&data, &cwd, &["init", "--topic", "A"], None);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "", "init must not print the [home] line");
    assert!(
        !data.join("index.html").exists(),
        "init must not refresh index.html (#7)"
    );

    fs::write(
        data.join("p.json"),
        r#"{"nodeId":"n","title":"N","tier":"axiom","content":"c"}"#,
    )
    .expect("write payload");
    let run = run_lesson(
        &data,
        &cwd,
        &[
            "append",
            "--lesson",
            "lessons/0001-a.html",
            "--node-file",
            "p.json",
        ],
        None,
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stderr, "", "append must not print the [home] line");
    assert!(
        !data.join("index.html").exists(),
        "append must not refresh index.html (#7)"
    );
}

#[test]
fn list_keeps_directory_order() {
    let t = common::temp_dir("lesson-list");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");
    let lessons = data.join("lessons");
    fs::create_dir_all(&lessons).expect("create lessons");
    for name in ["b.html", "a.html", "C.html", "0002-z.html", "notes.txt"] {
        fs::write(lessons.join(name), "").expect("write lesson");
    }
    fs::create_dir_all(lessons.join("sub.html")).expect("create subdir");

    let names: Vec<String> = fs::read_dir(&lessons)
        .expect("read lessons")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".html"))
        .collect();
    let expected = format!(
        "[\n{}\n]\n",
        names
            .iter()
            .map(|name| format!("  \"{name}\""))
            .collect::<Vec<_>>()
            .join(",\n")
    );

    let run = run_lesson(&data, &cwd, &["list"], None);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, expected);

    let run = run_lesson(&data, &cwd, &["list", "--dir", "nope"], None);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "[]\n");
}

#[test]
fn invalid_json_keeps_prefix() {
    let t = common::temp_dir("lesson-invalid-json");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    let run = run_lesson(&data, &cwd, &["init", "--topic", "A"], None);
    assert_eq!(run.code, 0, "{}", run.stderr);

    let run = run_lesson(
        &data,
        &cwd,
        &["append", "--lesson", "lessons/0001-a.html", "--data", "{"],
        None,
    );
    assert_eq!(run.code, 1);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.starts_with("Error: Invalid JSON payload: "),
        "{}",
        run.stderr
    );
    assert!(run.stderr.ends_with('\n'), "{}", run.stderr);
}

#[test]
fn non_object_payload_is_rejected() {
    let t = common::temp_dir("lesson-non-object");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    let run = run_lesson(&data, &cwd, &["init", "--topic", "A"], None);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let lesson = data.join("lessons").join("0001-a.html");
    let before = fs::read(&lesson).expect("read lesson");

    for payload in ["5", r#""s""#, "[]", "null"] {
        let run = run_lesson(
            &data,
            &cwd,
            &[
                "append",
                "--lesson",
                "lessons/0001-a.html",
                "--data",
                payload,
            ],
            None,
        );
        assert_eq!(run.code, 1, "{payload}");
        assert_eq!(run.stdout, "", "{payload}");
        assert_eq!(
            run.stderr, "Error: Invalid node payload: expected a JSON object\n",
            "{payload}"
        );
        assert_eq!(
            fs::read(&lesson).expect("read lesson"),
            before,
            "{payload}: lesson file changed"
        );
    }
}

#[test]
fn rebuild_and_compile_ref_errors_match_node() {
    let t = common::temp_dir("lesson-errors");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");
    let lessons = data.join("courses/demo/lessons");
    fs::create_dir_all(&lessons).expect("create lessons");
    fs::write(lessons.join("0001-rotta.html"), "<html></html>").expect("write corrupted lesson");

    let cases: &[(&[&str], &str)] = &[
        (&["rebuild"], "Error: Lesson not found: undefined\n"),
        (
            &["rebuild", "--slug", "nope"],
            "Error: Lesson not found: nope\n",
        ),
        (
            &["rebuild", "--lesson", "courses/demo/lessons/9999-x.html"],
            "Error: Lesson not found: courses/demo/lessons/9999-x.html\n",
        ),
        (
            &[
                "rebuild",
                "--lesson",
                "courses/demo/lessons/0001-rotta.html",
            ],
            "Error: Corrupted lesson file: missing #lesson-data island in courses/demo/lessons/0001-rotta.html\n",
        ),
        (
            &["compile-ref", "--slug", "nope"],
            "Error: Cannot compile reference: lesson file not found (undefined)\n",
        ),
        (
            &["reference"],
            "Error: Cannot compile reference: lesson file not found (undefined)\n",
        ),
        (
            &[
                "compile-ref",
                "--lesson",
                "courses/demo/lessons/9999-x.html",
            ],
            "Error: Cannot compile reference: lesson file not found (courses/demo/lessons/9999-x.html)\n",
        ),
        (
            &[
                "reference",
                "--lesson",
                "courses/demo/lessons/0001-rotta.html",
            ],
            "Error: Corrupted lesson file: missing #lesson-data in courses/demo/lessons/0001-rotta.html\n",
        ),
    ];

    for (args, expected) in cases {
        let run = run_lesson(&data, &cwd, args, None);
        assert_eq!(run.code, 1, "{args:?}");
        assert_eq!(run.stdout, "", "{args:?}");
        assert_eq!(run.stderr, *expected, "{args:?}");
        assert!(
            !data.join("reference").exists(),
            "{args:?}: a failed command must not create the reference folder"
        );
    }
}

#[test]
fn help_lists_lesson() {
    let t = temp_cwd("lesson-help");
    let output = Command::new(common::proxi_bin())
        .arg("--help")
        .current_dir(&t.0)
        .output()
        .expect("run proxi --help");

    assert!(output.status.success());
    assert!(text(&output.stdout).contains("lesson"));
}
