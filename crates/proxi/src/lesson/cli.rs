//! `proxi lesson` command line, Node getFlag semantics.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;

use serde_json::Value;

use crate::data_root::{self, resolve_path};
use crate::lesson::doc::{self, AppendOptions, InitOptions, RebuildOptions};
use crate::lesson::js;
use crate::lesson::reference::{self, CompileReferenceOptions};
use crate::lesson::refresh::{BoardRefresh, NoBoardRefresh};

/// Verbatim from `extensions/html-lesson.ts`: A5 keeps Node's text even when it says `html-lesson.ts`.
pub const USAGE: &str = "Usage: html-lesson.ts <command> [options]";

pub fn run_cli(args: &[String]) -> i32 {
    if let Err(e) = data_root::enter_data_root() {
        eprintln!("Error: {e}");
        return 1;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    run_in_cwd(
        args,
        &mut stdin.lock(),
        &mut stdout.lock(),
        &mut stderr.lock(),
        &mut NoBoardRefresh,
    )
}

pub fn run_in_cwd(
    args: &[String],
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    board: &mut dyn BoardRefresh,
) -> i32 {
    let command = args.first().map(String::as_str);
    let (result, refresh) = match command {
        Some("init") => (run_init(args), false),
        Some("append") => (run_append(args, stdin), false),
        Some("list") => (run_list(args), false),
        Some("rebuild") => (run_rebuild(args), false),
        Some("compile-ref" | "reference") => (run_compile_ref(args), true),
        _ => {
            let _ = writeln!(out, "{USAGE}");
            return 0;
        }
    };

    match result {
        Ok(json) => {
            let _ = writeln!(out, "{}", js::stringify_pretty(&json));
            // Only compile-ref/reference regenerate the board (D12), and after the JSON.
            if refresh && let Err(message) = board.refresh(err) {
                let _ = writeln!(err, "Error: {message}");
                return 1;
            }
            0
        }
        Err(message) => {
            let _ = writeln!(err, "Error: {message}");
            1
        }
    }
}

/// Node's `getFlag`: the next argument is the value even when it starts with `--`.
fn get_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let idx = args.iter().position(|arg| arg == flag)?;
    args.get(idx + 1).map(String::as_str)
}

/// JS `a || b`.
fn or<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    match a {
        Some(value) if !value.is_empty() => Some(value),
        _ => b,
    }
}

fn truthy(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

fn owned(value: Option<&str>) -> Option<String> {
    value.map(str::to_owned)
}

fn run_init(args: &[String]) -> Result<Value, String> {
    let Some(topic) = truthy(get_flag(args, "--topic")) else {
        return Err("Missing required parameter: --topic".to_string());
    };
    let options = InitOptions {
        topic: topic.to_string(),
        title: owned(get_flag(args, "--title")),
        slug: owned(get_flag(args, "--slug")),
        description: owned(get_flag(args, "--description")),
        dag: owned(get_flag(args, "--dag")),
        target_goal: owned(get_flag(args, "--goal")),
        base_dir: owned(get_flag(args, "--base-dir")),
        lessons_dir: owned(get_flag(args, "--out-dir")),
        repo_root: data_root::repo_root(),
    };
    doc::init_lesson(&options).map(|result| result.to_json())
}

fn run_append(args: &[String], stdin: &mut dyn Read) -> Result<Value, String> {
    let file = or(get_flag(args, "--lesson"), get_flag(args, "--file"));
    let slug = get_flag(args, "--slug");
    let data = or(get_flag(args, "--data"), get_flag(args, "--node-json"));
    let node_file = get_flag(args, "--node-file");
    let is_stdin = args.iter().any(|arg| arg == "--stdin");

    let payload = if let Some(data) = truthy(data) {
        data.to_string()
    } else if let Some(node_file) = truthy(node_file) {
        match fs::read(node_file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            // Node's fs prints `path.out()` after ToNamespacedPath: the resolved absolute path on
            // Windows only, the argument as typed on POSIX.
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let reported = if cfg!(windows) {
                    resolve_path(Path::new(node_file)).display().to_string()
                } else {
                    node_file.to_string()
                };
                return Err(format!(
                    "ENOENT: no such file or directory, open '{reported}'"
                ));
            }
            Err(e) => return Err(e.to_string()),
        }
    } else if is_stdin {
        let mut bytes = Vec::new();
        stdin.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        return Err(
            "Must provide --data, --node-json, --node-file, or --stdin for append payload"
                .to_string(),
        );
    };

    let node: Value =
        serde_json::from_str(&payload).map_err(|e| format!("Invalid JSON payload: {e}"))?;
    let result = doc::append_node(AppendOptions {
        lesson_path: owned(file),
        slug: owned(slug),
        node,
        base_dir: None,
    })?;
    Ok(result.to_json())
}

fn run_list(args: &[String]) -> Result<Value, String> {
    let dir = or(get_flag(args, "--dir"), Some("./lessons")).expect("defaulted above");
    if !Path::new(dir).exists() {
        return Ok(Value::Array(Vec::new()));
    }
    let mut files = Vec::new();
    // No sort and no stat: parity with readdirSync, directories included.
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let name = entry.map_err(|e| e.to_string())?.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".html") {
            files.push(Value::String(name.into_owned()));
        }
    }
    Ok(Value::Array(files))
}

fn run_rebuild(args: &[String]) -> Result<Value, String> {
    let options = RebuildOptions {
        lesson_path: owned(or(get_flag(args, "--lesson"), get_flag(args, "--file"))),
        slug: owned(get_flag(args, "--slug")),
        base_dir: owned(get_flag(args, "--base-dir")),
        lessons_dir: owned(or(get_flag(args, "--out-dir"), get_flag(args, "--dir"))),
        repo_root: data_root::repo_root(),
    };
    doc::rebuild_lesson(&options).map(|result| result.to_json())
}

fn run_compile_ref(args: &[String]) -> Result<Value, String> {
    let options = CompileReferenceOptions {
        lesson_path: owned(or(get_flag(args, "--lesson"), get_flag(args, "--file"))),
        slug: owned(get_flag(args, "--slug")),
        output_dir: owned(get_flag(args, "--out-dir")),
        base_dir: owned(get_flag(args, "--base-dir")),
        record_path: owned(get_flag(args, "--record")),
    };
    reference::compile_reference(&options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson::refresh::RecordingRefresh;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "proxi-lesson-cli-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn repo_root() -> PathBuf {
        resolve_path(&Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."))
    }

    /// Init plus one node with absolute paths: no chdir happens in-process.
    fn lesson(dir: &Path) -> String {
        let init = doc::init_lesson(&InitOptions {
            topic: "Reti".to_string(),
            title: Some("Il modello OSI".to_string()),
            slug: None,
            description: None,
            dag: None,
            target_goal: None,
            base_dir: Some(dir.display().to_string()),
            lessons_dir: None,
            repo_root: repo_root(),
        })
        .expect("init");
        doc::append_node(AppendOptions {
            lesson_path: Some(init.file_path.clone()),
            slug: None,
            node: serde_json::json!({
                "nodeId": "n1",
                "title": "Livelli",
                "tier": "axiom",
                "content": "Un testo."
            }),
            base_dir: None,
        })
        .expect("append");
        init.file_path
    }

    fn run(args: &[&str], board: &mut dyn BoardRefresh) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_in_cwd(&args, &mut std::io::empty(), &mut out, &mut err, board);
        (
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    }

    #[test]
    fn compile_ref_and_reference_refresh_the_board_once() {
        let tmp = temp_dir("compile-ref-refresh");
        let file = lesson(&tmp.0);
        let out_dir = tmp.0.join("ref").display().to_string();
        for command in ["compile-ref", "reference"] {
            let mut board = RecordingRefresh::default();
            let (code, out, err) = run(
                &[command, "--lesson", &file, "--out-dir", &out_dir],
                &mut board,
            );
            assert_eq!(code, 0, "{command}: {err}");
            assert_eq!(board.calls, 1, "{command}");
            let json: Value = serde_json::from_str(&out).expect("stdout is JSON");
            assert!(json.get("filePath").is_some(), "{command}: {out}");
            assert_eq!(err, "", "{command}");
        }
    }

    #[test]
    fn rebuild_never_refreshes_the_board() {
        let tmp = temp_dir("rebuild-no-refresh");
        let file = lesson(&tmp.0);
        let base = tmp.0.display().to_string();
        let mut board = RecordingRefresh::default();
        let (code, out, err) = run(
            &["rebuild", "--lesson", &file, "--base-dir", &base],
            &mut board,
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(board.calls, 0);
        assert!(out.contains("\"nodeCount\": 1"), "{out}");
    }

    #[test]
    fn failed_commands_never_refresh() {
        let tmp = temp_dir("failed-no-refresh");
        let missing = tmp.0.join("missing.html").display().to_string();
        let mut board = RecordingRefresh::default();
        let (code, out, err) = run(&["compile-ref", "--lesson", &missing], &mut board);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert!(
            err.starts_with("Error: Cannot compile reference: lesson file not found ("),
            "{err}"
        );
        assert_eq!(board.calls, 0);
    }

    #[test]
    fn board_failure_exits_one_after_printing() {
        let tmp = temp_dir("board-failure");
        let file = lesson(&tmp.0);
        let out_dir = tmp.0.join("ref").display().to_string();
        let mut board = RecordingRefresh {
            fail: Some("boom".to_string()),
            ..Default::default()
        };
        let (code, out, err) = run(
            &["compile-ref", "--lesson", &file, "--out-dir", &out_dir],
            &mut board,
        );
        assert_eq!(code, 1);
        assert!(out.contains("\"filePath\""), "{out}");
        assert_eq!(err, "Error: boom\n");
        assert!(Path::new(&out_dir).join("il-modello-osi.html").is_file());
    }
}
