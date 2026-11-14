//! `proxi lesson` command line, Node getFlag semantics.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;

use serde_json::Value;

use crate::data_root::{self, resolve_path};
use crate::lesson::doc::{self, AppendOptions, InitOptions};
use crate::lesson::js;

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
    )
}

pub fn run_in_cwd(
    args: &[String],
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let command = args.first().map(String::as_str);
    let result = match command {
        Some("init") => run_init(args),
        Some("append") => run_append(args, stdin),
        Some("list") => run_list(args),
        // The agents keep using `npm run lesson` for these (Scope Out).
        Some(cmd @ ("rebuild" | "compile-ref" | "reference")) => Err(format!(
            "{cmd} is not ported to proxi yet: run npm run lesson -- {cmd}"
        )),
        _ => {
            let _ = writeln!(out, "{USAGE}");
            return 0;
        }
    };

    match result {
        Ok(json) => {
            let _ = writeln!(out, "{}", js::stringify_pretty(&json));
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
            // Node's fs reports the resolved absolute path, never the argument as typed.
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(format!(
                    "ENOENT: no such file or directory, open '{}'",
                    resolve_path(Path::new(node_file)).display()
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
