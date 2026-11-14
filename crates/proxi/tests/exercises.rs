mod common;

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::task::JoinHandle;

use proxi::exercises::{
    Installs, MAX_FILE, NODE_MISSING, NpmCommand, RunConfig, SaveError, editor_command_line,
    get_workspace, list_exercises, npm_command, reveal_hint, run_exercise, save_exercise_file,
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

const FAKE_VITEST: &str = r#"import fs from "node:fs";const o=process.argv.find(a=>a.startsWith("--outputFile=")).slice(13);fs.writeFileSync(o,JSON.stringify({numTotalTests:1,numPassedTests:1,numFailedTests:0,testResults:[]}));"#;

fn node() -> PathBuf {
    proxi::child::node_on_path().expect("node on PATH")
}

fn exercises_base(root: &Path) -> PathBuf {
    root.join("courses").join("c").join("exercises")
}

fn fake_vitest(base: &Path, source: &str) -> PathBuf {
    let dir = base.join("node_modules").join("vitest");
    fs::create_dir_all(&dir).expect("create vitest dir");
    let file = dir.join("vitest.mjs");
    fs::write(&file, source).expect("write fake vitest");
    file
}

fn cfg() -> RunConfig {
    RunConfig::new(Some(node()), NpmCommand::Default)
}

async fn run_one(root: &Path, name: &str, cfg: &RunConfig) -> Value {
    run_exercise(root, "c", name, cfg, &Installs::default()).await
}

fn fake_npm() -> String {
    format!(
        r#"import fs from "node:fs";fs.appendFileSync("install.log", process.argv[2] + "\n");fs.mkdirSync("node_modules/vitest", {{ recursive: true }});fs.writeFileSync("node_modules/vitest/vitest.mjs", {});"#,
        serde_json::to_string(FAKE_VITEST).expect("serialise vitest source")
    )
}

#[cfg(windows)]
fn fake_npm_bin(base: &Path) -> PathBuf {
    let bin = base.join("fake-npm.cmd");
    fs::write(&bin, "@node \"%~dp0fake-npm.mjs\" %*\r\n").expect("write fake npm cmd");
    bin
}

#[cfg(unix)]
fn fake_npm_bin(base: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = base.join("fake-npm");
    fs::write(
        &bin,
        "#!/bin/sh\nexec node \"$(dirname \"$0\")/fake-npm.mjs\" \"$@\"\n",
    )
    .expect("write fake npm");
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("chmod fake npm");
    bin
}

const HEARTBEAT_VITEST: &str = r#"
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
spawn(process.execPath, [join(here, "child.mjs")], { stdio: "ignore" });
setInterval(() => {}, 1000);
"#;

const HEARTBEAT_CHILD: &str = r#"
import { appendFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
setInterval(() => appendFileSync(join(here, "beat.txt"), "."), 50);
"#;

/// A fake vitest that never exits and leaves a grandchild beating every 50 ms.
fn heartbeat_vitest(root: &Path) -> PathBuf {
    let dir = exercises_base(root).join("node_modules").join("vitest");
    fs::create_dir_all(&dir).expect("create vitest dir");
    fs::write(dir.join("vitest.mjs"), HEARTBEAT_VITEST).expect("write vitest");
    fs::write(dir.join("child.mjs"), HEARTBEAT_CHILD).expect("write child");
    dir.join("beat.txt")
}

fn beat_len(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

async fn wait_for_beat(path: &Path, handle: &JoinHandle<Value>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if beat_len(path) > 0 {
            return;
        }
        assert!(
            !handle.is_finished(),
            "the run finished before the heartbeat started"
        );
        assert!(Instant::now() < deadline, "heartbeat never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn assert_stopped(path: &Path) {
    for _ in 0..10 {
        let before = beat_len(path);
        tokio::time::sleep(Duration::from_millis(400)).await;
        if beat_len(path) == before {
            return;
        }
    }
    panic!("heartbeat still growing: {}", path.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn run_unknown_name_and_missing_package_json() {
    let (root, _dir) = temp_ex();
    for name in ["99-nope", "../x"] {
        let result = run_one(&root.0, name, &cfg()).await;
        let error = result["error"].as_str().expect("error");
        assert!(error.contains("sconosciuto"), "{name}: {error}");
    }

    // No package.json in the exercises dir: the install message says so.
    let result = run_one(&root.0, "01-a", &cfg()).await;
    assert_eq!(result["ok"], json!(false));
    let error = result["error"].as_str().expect("error");
    assert!(error.contains("package.json"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn last_run_is_persisted_and_listed() {
    let (root, dir) = temp_ex();
    fake_vitest(&exercises_base(&root.0), FAKE_VITEST);
    assert!(
        list_exercises(&root.0, "c").expect("list")[0]
            .get("lastRun")
            .is_none()
    );

    let result = run_one(&root.0, "01-a", &cfg()).await;
    assert_eq!(result["ok"], json!(true), "{}", result["error"]);

    let text = serde_json::to_string(&result).expect("serialise");
    assert_eq!(
        fs::read_to_string(dir.join(".app-lastrun.json")).expect("read last run"),
        text
    );
    assert!(text.starts_with("{\"ok\":"), "{text}");
    let keys: Vec<&str> = result
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "ok",
            "passed",
            "failed",
            "total",
            "failures",
            "tests",
            "durationMs",
            "at"
        ]
    );

    let list = list_exercises(&root.0, "c").expect("list");
    assert_eq!(list[0]["lastRun"], result);
    assert!(
        !list[0]["files"]
            .as_array()
            .expect("files")
            .iter()
            .any(|file| file.as_str() == Some(".app-lastrun.json"))
    );
    let workspace = get_workspace(&root.0, "c", "01-a")
        .expect("workspace")
        .expect("exercise exists");
    assert_eq!(workspace["info"]["lastRun"]["ok"], json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_the_dependencies_once_when_vitest_is_missing() {
    let (root, _dir) = temp_ex();
    let base = exercises_base(&root.0);
    fs::write(base.join("package.json"), "{}").expect("write package.json");
    let fake = root.0.join("fake-npm.mjs");
    fs::write(&fake, fake_npm()).expect("write fake npm");
    let cfg = RunConfig::new(Some(node()), NpmCommand::Script(fake));
    let installs = Installs::default();

    let (first, second) = tokio::join!(
        run_exercise(&root.0, "c", "01-a", &cfg, &installs),
        run_exercise(&root.0, "c", "01-a", &cfg, &installs),
    );
    assert_eq!(first["ok"], json!(true), "{}", first["error"]);
    assert_eq!(second["ok"], json!(true), "{}", second["error"]);
    assert_eq!(
        fs::read_to_string(base.join("install.log")).expect("install log"),
        "install\n"
    );

    // Vitest is already there: a later run must not install again.
    let third = run_exercise(&root.0, "c", "01-a", &cfg, &installs).await;
    assert_eq!(third["ok"], json!(true), "{}", third["error"]);
    assert_eq!(
        fs::read_to_string(base.join("install.log")).expect("install log"),
        "install\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn install_through_a_bin_shim() {
    let (root, _dir) = temp_ex();
    let base = exercises_base(&root.0);
    fs::write(base.join("package.json"), "{}").expect("write package.json");
    fs::write(base.join("fake-npm.mjs"), fake_npm()).expect("write fake npm");
    let cfg = RunConfig::new(Some(node()), NpmCommand::Bin(fake_npm_bin(&base)));

    let result = run_one(&root.0, "01-a", &cfg).await;
    assert_eq!(result["ok"], json!(true), "{}", result["error"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn node_missing_is_a_clear_error() {
    let (root, _dir) = temp_ex();
    let cfg = RunConfig::new(None, NpmCommand::Default);

    let result = run_exercise(&root.0, "c", "01-a", &cfg, &Installs::default()).await;
    assert_eq!(result["ok"], json!(false));
    assert_eq!(result["error"], json!(NODE_MISSING));
    assert!(result["error"].as_str().expect("error").contains("Node"));
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_report_shows_stderr_without_ansi() {
    let (root, _dir) = temp_ex();
    fake_vitest(
        &exercises_base(&root.0),
        r#"process.stderr.write("\u001b[31mboom\u001b[39m");"#,
    );

    let result = run_one(&root.0, "01-a", &cfg()).await;
    assert_eq!(result["ok"], json!(false));
    assert_eq!(
        result["error"],
        json!("vitest non ha prodotto un report. boom")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_report_is_an_error_and_not_persisted() {
    let (root, dir) = temp_ex();
    fake_vitest(
        &exercises_base(&root.0),
        r#"import fs from "node:fs";const o=process.argv.find(a=>a.startsWith("--outputFile=")).slice(13);fs.writeFileSync(o,"{nope");"#,
    );

    let result = run_one(&root.0, "01-a", &cfg()).await;
    assert_eq!(result["ok"], json!(false));
    let error = result["error"].as_str().expect("error");
    assert!(error.starts_with("Report di vitest non valido"), "{error}");
    assert!(!dir.join(".app-lastrun.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_kills_the_vitest_tree() {
    let (root, _dir) = temp_ex();
    let beat = heartbeat_vitest(&root.0);
    let mut cfg = cfg();
    cfg.run_timeout = Duration::from_secs(3);

    let data_root = root.0.clone();
    let handle = tokio::spawn(async move { run_one(&data_root, "01-a", &cfg).await });
    wait_for_beat(&beat, &handle).await;

    let result = handle.await.expect("run finished");
    assert_eq!(result["error"], json!("Timeout: i test hanno superato 3 s"));
    assert!(beat_len(&beat) > 0, "the heartbeat file vanished");
    assert_stopped(&beat).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_run_kills_the_vitest_tree() {
    let (root, _dir) = temp_ex();
    let beat = heartbeat_vitest(&root.0);
    let cfg = cfg();

    let data_root = root.0.clone();
    let handle = tokio::spawn(async move { run_one(&data_root, "01-a", &cfg).await });
    wait_for_beat(&beat, &handle).await;

    handle.abort();
    let _ = handle.await;
    assert_stopped(&beat).await;
}

#[test]
fn npm_command_prefers_the_flag_then_a_non_empty_env() {
    assert_eq!(
        npm_command(Some(Path::new("npm-flag")), Some(OsStr::new("env.mjs"))),
        NpmCommand::Bin(PathBuf::from("npm-flag"))
    );
    assert_eq!(
        npm_command(None, Some(OsStr::new("env.mjs"))),
        NpmCommand::Script(PathBuf::from("env.mjs"))
    );
    assert_eq!(npm_command(None, Some(OsStr::new(""))), NpmCommand::Default);
    assert_eq!(npm_command(None, None), NpmCommand::Default);
}

#[tokio::test(flavor = "multi_thread")]
async fn vitest_gets_the_node_arguments() {
    let (root, _dir) = temp_ex();
    let base = exercises_base(&root.0);
    fake_vitest(
        &base,
        r#"import fs from "node:fs";fs.appendFileSync("argv.json", JSON.stringify({ args: process.argv.slice(2), cwd: process.cwd() }));const o=process.argv.find(a=>a.startsWith("--outputFile=")).slice(13);fs.writeFileSync(o,JSON.stringify({numTotalTests:1,numPassedTests:1,numFailedTests:0,testResults:[]}));"#,
    );

    let result = run_one(&root.0, "01-a", &cfg()).await;
    assert_eq!(result["ok"], json!(true), "{}", result["error"]);

    let recorded: Value =
        serde_json::from_str(&fs::read_to_string(base.join("argv.json")).expect("read argv"))
            .expect("parse argv");
    let args = recorded["args"].as_array().expect("args");
    assert_eq!(args.len(), 4);
    assert_eq!(args[0], json!("run"));
    assert_eq!(args[1], json!("01-a/"));
    assert_eq!(args[2], json!("--reporter=json"));
    let output = args[3].as_str().expect("output file");
    assert!(output.starts_with("--outputFile="), "{output}");
    assert!(output.ends_with("report.json"), "{output}");
    assert_eq!(
        fs::canonicalize(recorded["cwd"].as_str().expect("cwd")).expect("canonical cwd"),
        fs::canonicalize(&base).expect("canonical base")
    );
}
