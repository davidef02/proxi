//! Replays the golden captured from the Node `lesson` CLI (app/test/capture-lesson-golden.ts).

mod common;

use std::fs;

use common::cli_golden;

#[test]
fn lesson_cli_matches_the_node_golden() {
    let golden = cli_golden::golden("lesson-cli");
    assert_eq!(golden["route"].as_str(), Some("lesson-cli"), "golden route");

    let t = common::temp_dir("lesson-golden");
    let data = t.0.join("data");
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&data).expect("create data");
    fs::create_dir_all(&cwd).expect("create cwd");

    cli_golden::write_seed(&data, &golden["seed"]);

    let roots = cli_golden::data_roots(&data);

    for case in golden["cases"].as_array().expect("cases array") {
        let name = case["name"].as_str().expect("case name");
        let label = format!("lesson-cli/{name}");
        let args: Vec<String> = case["args"]
            .as_array()
            .expect("case args")
            .iter()
            .map(|arg| arg.as_str().expect("arg is a string").to_string())
            .collect();

        let before = cli_golden::snapshot(&data);
        let run = cli_golden::run(
            {
                let mut command = cli_golden::proxi_command(&data, &cwd);
                command.arg("lesson").args(&args);
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
            cli_golden::normalise_stream(&run.stdout, &roots),
            cli_golden::normalise_stream(&run.stderr, &roots)
        );

        assert_eq!(
            cli_golden::normalise_stream(&run.stdout, &roots),
            case["stdout"].as_str().expect("golden stdout"),
            "{label}: stdout"
        );

        match case["stderrPrefix"].as_str() {
            Some(prefix) => assert!(
                run.stderr.starts_with(prefix),
                "{label}: stderr does not start with {prefix:?}: {}",
                cli_golden::normalise_stream(&run.stderr, &roots)
            ),
            None => assert_eq!(
                cli_golden::normalise_stream(&run.stderr, &roots),
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
fn golden_covers_every_acceptance_case() {
    let golden = cli_golden::golden("lesson-cli");
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
        "append-for-reference",
        "rebuild-lesson",
        "rebuild-slug-default-dir",
        "rebuild-dir-slug",
        "rebuild-out-dir-wins",
        "rebuild-missing-lesson",
        "rebuild-slug-miss",
        "rebuild-no-target",
        "rebuild-corrupted",
        "compile-ref-default",
        "reference-alias-record",
        "compile-ref-base-dir-record-fallback",
        "compile-ref-empty-lesson",
        "compile-ref-missing-lesson",
        "compile-ref-slug-miss",
        "compile-ref-corrupted",
        "rebuild-base-dir-slug",
        "rebuild-file-alias",
    ];
    assert_eq!(names, expected, "golden case names");

    let compile_ref = cases
        .iter()
        .find(|case| case["name"].as_str() == Some("compile-ref-default"))
        .expect("compile-ref-default case");
    assert!(
        compile_ref["files"]
            .as_object()
            .expect("compile-ref-default files")
            .contains_key("reference/il-modello-osi.html"),
        "compile-ref-default must write the reference file: {}",
        compile_ref["files"]
    );

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
        cli_golden::normalise_stream(
            r#"{"filePath": "C:\\d\\lessons\\a.html"}"#,
            &[r"C:\d".to_string()]
        ),
        r#"{"filePath": "<dataRoot>/lessons/a.html"}"#
    );
}

#[test]
fn normalise_file_replaces_iso_timestamps_and_dates() {
    assert_eq!(
        cli_golden::normalise_file("x 2026-11-13T10:00:00.000Z Generated: 2026-11-13\r\n"),
        "x <iso> Generated: <date>\n"
    );
}
