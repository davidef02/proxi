mod common;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use proxi::exercises::{
    MAX_FILE, SaveError, editor_command_line, get_workspace, list_exercises, reveal_hint,
    save_exercise_file,
};

/// `tempEx` of app/test/exercises.test.ts.
fn temp_ex() -> (common::TempDir, PathBuf) {
    let root = common::temp_dir("exercises");
    let dir = root
        .0
        .join("courses")
        .join("c")
        .join("exercises")
        .join("01-a");
    fs::create_dir_all(dir.join("node_modules")).expect("create fixture");
    fs::write(dir.join("Esercizio.vue"), "stub").expect("write Esercizio.vue");
    fs::write(dir.join("Esercizio.test.js"), "// t").expect("write Esercizio.test.js");
    fs::write(dir.join("ISTRUZIONI.md"), "testo").expect("write ISTRUZIONI.md");
    fs::write(dir.join("node_modules").join("x.js"), "x").expect("write node_modules/x.js");
    (root, dir)
}

fn version_of(root: &Path, path: &str) -> String {
    let workspace = get_workspace(root, "c", "01-a")
        .expect("workspace")
        .expect("exercise exists");
    workspace["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|file| file["path"].as_str() == Some(path))
        .unwrap_or_else(|| panic!("{path} in the workspace"))["version"]
        .as_str()
        .expect("version")
        .to_string()
}

#[test]
fn list_filters_dirs_lists_files_reads_instructions() {
    let root = common::temp_dir("exercises-list");
    let ex = root.0.join("courses").join("c").join("exercises");
    for dir in ["01-a", "zz-verifica", "node_modules", "misc"] {
        fs::create_dir_all(ex.join(dir)).expect("create exercise dir");
    }
    fs::create_dir_all(ex.join("01-a").join("sub")).expect("create sub");
    fs::write(ex.join("01-a").join("Esercizio.vue"), "x").expect("write Esercizio.vue");
    fs::write(ex.join("01-a").join("ISTRUZIONI.md"), "fai questo")
        .expect("write ISTRUZIONI.md");
    fs::write(ex.join("package.json"), "{}").expect("write package.json");

    let list = list_exercises(&root.0, "c").expect("list");
    let names = list
        .iter()
        .map(|entry| entry["name"].as_str().expect("name"))
        .collect::<Vec<_>>();
    assert_eq!(names, ["01-a", "zz-verifica"]);
    assert_eq!(
        list[0]["files"],
        json!(["Esercizio.vue", "ISTRUZIONI.md"])
    );
    assert_eq!(list[0]["instructions"], "fai questo");
    assert_eq!(list[0]["dir"], "courses/c/exercises/01-a");
    assert_eq!(
        serde_json::to_string(&list[0]).expect("serialise"),
        r#"{"course":"c","name":"01-a","dir":"courses/c/exercises/01-a","files":["Esercizio.vue","ISTRUZIONI.md"],"instructions":"fai questo"}"#
    );
    assert!(list_exercises(&root.0, "none").expect("list").is_empty());
}

#[test]
fn workspace_tests_read_only_hidden_and_node_modules_skipped() {
    let (root, dir) = temp_ex();
    fs::write(
        dir.join("SUGGERIMENTI.json"),
        r#"{"hints":["a","b"],"final":true,"revealed":1}"#,
    )
    .expect("write hints");

    let workspace = get_workspace(&root.0, "c", "01-a")
        .expect("workspace")
        .expect("exercise exists");
    let files = workspace["files"].as_array().expect("files");
    let pairs = files
        .iter()
        .map(|file| {
            (
                file["path"].as_str().expect("path").to_string(),
                file["readOnly"].as_bool().expect("readOnly"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        [
            ("Esercizio.test.js".to_string(), true),
            ("Esercizio.vue".to_string(), false),
        ]
    );

    let info = &workspace["info"];
    assert_eq!(info["hints"], json!(["a", "b"]));
    assert_eq!(info["hintsRevealed"], json!(1));
    assert_eq!(info["final"], json!(true));
    assert!(
        !info["files"]
            .as_array()
            .expect("info files")
            .iter()
            .any(|file| file.as_str() == Some("SUGGERIMENTI.json"))
    );
    assert!(get_workspace(&root.0, "c", "../x").expect("workspace").is_none());
    assert!(get_workspace(&root.0, "c", "09-none").expect("workspace").is_none());
}

#[test]
fn save_validates_paths_refuses_tests_and_escapes() {
    let (root, dir) = temp_ex();
    let save = |p: &str, content: &str| {
        save_exercise_file(&root.0, "c", "01-a", p, content, None)
    };

    assert!(save("Esercizio.vue", "nuovo").is_ok());
    assert_eq!(
        fs::read_to_string(dir.join("Esercizio.vue")).expect("read"),
        "nuovo"
    );
    assert!(save("helpers/uno.js", "x").is_ok());
    assert!(save("stile.css", "x").is_ok());

    for bad in [
        "../x.js",
        "a/../../x.js",
        "/abs.js",
        "C:/x.js",
        "C:x.js",
        "a\\b.js",
        "node_modules/x.js",
        "Esercizio.test.js",
        "x.spec.ts",
        "ISTRUZIONI.md",
        "SUGGERIMENTI.json",
        "run.exe",
        ".app-written.json",
        "",
    ] {
        assert!(save(bad, "x").is_err(), "{bad}");
    }
    assert!(save("Esercizio.vue", &"x".repeat(MAX_FILE as usize + 1)).is_err());
    assert!(
        save_exercise_file(&root.0, "c", "../01-a", "Esercizio.vue", "x", None).is_err()
    );
    assert_eq!(
        fs::read_to_string(dir.join("Esercizio.test.js")).expect("read"),
        "// t"
    );
}

#[test]
fn reveal_hint_increments_up_to_the_total() {
    let (root, dir) = temp_ex();
    assert!(reveal_hint(&root.0, "c", "01-a").expect("reveal").is_none());

    fs::write(
        dir.join("SUGGERIMENTI.json"),
        r#"{"hints":["a","b"],"final":false,"revealed":0}"#,
    )
    .expect("write hints");

    let first = reveal_hint(&root.0, "c", "01-a")
        .expect("reveal")
        .expect("exercise exists");
    assert_eq!(first["hintsRevealed"], json!(1));
    assert_eq!(
        fs::read_to_string(dir.join("SUGGERIMENTI.json")).expect("read hints"),
        "{\n  \"hints\": [\n    \"a\",\n    \"b\"\n  ],\n  \"final\": false,\n  \"revealed\": 1\n}\n"
    );

    let second = reveal_hint(&root.0, "c", "01-a")
        .expect("reveal")
        .expect("exercise exists");
    assert_eq!(second["hintsRevealed"], json!(2));
    let third = reveal_hint(&root.0, "c", "01-a")
        .expect("reveal")
        .expect("exercise exists");
    assert_eq!(third["hintsRevealed"], json!(2));
}

#[test]
fn stale_base_version_is_a_conflict_and_leaves_the_file_alone() {
    let (root, dir) = temp_ex();
    let stale = version_of(&root.0, "Esercizio.vue");
    fs::write(dir.join("Esercizio.vue"), "da VS Code").expect("external write");

    match save_exercise_file(&root.0, "c", "01-a", "Esercizio.vue", "mio", Some(&stale)) {
        Err(SaveError::Conflict) => {}
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(dir.join("Esercizio.vue")).expect("read"),
        "da VS Code"
    );

    let fresh = version_of(&root.0, "Esercizio.vue");
    assert!(
        save_exercise_file(&root.0, "c", "01-a", "Esercizio.vue", "mio", Some(&fresh)).is_ok()
    );
    assert_eq!(
        fs::read_to_string(dir.join("Esercizio.vue")).expect("read"),
        "mio"
    );
    // A new file has no version: the editor sends an empty base.
    assert!(save_exercise_file(&root.0, "c", "01-a", "nuovo.js", "x", Some("")).is_ok());
}

#[test]
fn read_only_checks_are_case_insensitive() {
    let (root, dir) = temp_ex();
    for bad in [
        "Esercizio.TEST.ts",
        "x.Spec.js",
        "istruzioni.md",
        "suggerimenti.json",
    ] {
        assert!(
            save_exercise_file(&root.0, "c", "01-a", bad, "x", None).is_err(),
            "{bad}"
        );
    }

    fs::write(dir.join("Istruzioni.MD"), "x").expect("write Istruzioni.MD");
    fs::write(dir.join("Foo.TEST.ts"), "x").expect("write Foo.TEST.ts");
    let workspace = get_workspace(&root.0, "c", "01-a")
        .expect("workspace")
        .expect("exercise exists");
    let files = workspace["files"].as_array().expect("files");
    assert!(
        !files.iter().any(|file| file["path"]
            .as_str()
            .expect("path")
            .to_ascii_lowercase()
            .contains("istruzioni"))
    );
    let test_file = files
        .iter()
        .find(|file| file["path"].as_str() == Some("Foo.TEST.ts"))
        .expect("Foo.TEST.ts in the workspace");
    assert_eq!(test_file["readOnly"], json!(true));
}

#[test]
fn hints_follow_js_coercion() {
    let (root, dir) = temp_ex();

    fs::write(
        dir.join("SUGGERIMENTI.json"),
        r#"{"hints":["uno",2],"final":"si","revealed":"x"}"#,
    )
    .expect("write hints");
    let list = list_exercises(&root.0, "c").expect("list");
    let entry = &list[0];
    assert_eq!(entry["hints"], json!(["uno", "2"]));
    assert_eq!(entry["hintsRevealed"], json!(0));
    assert_eq!(entry["final"], json!(true));

    fs::write(
        dir.join("SUGGERIMENTI.json"),
        r#"{"hints":["a","b","c"],"revealed":1.5}"#,
    )
    .expect("write hints");
    let list = list_exercises(&root.0, "c").expect("list");
    assert_eq!(
        serde_json::to_string(&list[0]["hintsRevealed"]).expect("serialise"),
        "1.5"
    );

    for raw in [r#"{"hints":"no"}"#, "{nope"] {
        fs::write(dir.join("SUGGERIMENTI.json"), raw).expect("write hints");
        let list = list_exercises(&root.0, "c").expect("list");
        assert!(list[0].get("hints").is_none(), "{raw}");
    }
}

#[test]
fn last_run_is_raw_and_truthy_only() {
    let (root, dir) = temp_ex();
    for raw in ["0", "{nope"] {
        fs::write(dir.join(".app-lastrun.json"), raw).expect("write last run");
        let list = list_exercises(&root.0, "c").expect("list");
        assert!(list[0].get("lastRun").is_none(), "{raw}");
    }

    fs::write(dir.join(".app-lastrun.json"), r#"{"b":1,"a":2}"#).expect("write last run");
    let list = list_exercises(&root.0, "c").expect("list");
    assert_eq!(
        serde_json::to_string(&list[0]["lastRun"]).expect("serialise"),
        r#"{"b":1,"a":2}"#
    );
}

#[test]
fn walk_stops_below_depth_three_and_orders_like_locale_compare() {
    let (root, dir) = temp_ex();
    fs::create_dir_all(dir.join("sub/a/b/c")).expect("create depth");
    fs::write(dir.join("sub/a/b/ok.js"), "// depth 3").expect("write ok.js");
    fs::write(dir.join("sub/a/b/c/no.js"), "// too deep").expect("write no.js");
    fs::write(dir.join("app.js"), "// app").expect("write app.js");
    fs::write(dir.join("big.js"), vec![b'x'; MAX_FILE as usize + 1]).expect("write big.js");

    let workspace = get_workspace(&root.0, "c", "01-a")
        .expect("workspace")
        .expect("exercise exists");
    let files = workspace["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|file| file["path"].as_str().expect("path"))
        .collect::<Vec<_>>();
    assert_eq!(
        files,
        [
            "app.js",
            "Esercizio.test.js",
            "Esercizio.vue",
            "sub/a/b/ok.js"
        ]
    );
}

#[test]
fn editor_command_line_quotes_the_dir_and_refuses_quotes() {
    assert_eq!(
        editor_command_line(Path::new("dir con spazi")),
        Some("code \"dir con spazi\"".to_string())
    );
    assert_eq!(editor_command_line(Path::new("a\"b")), None);
}
