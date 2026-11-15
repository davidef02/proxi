//! AC3: every `proxi data|lesson|anki|home` command the agent-facing docs quote has a concrete
//! run that exits 0 on a copy of the fixture data.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The command names the docs may tell an agent to run.
const COMMANDS: &[&str] = &["data", "lesson", "anki", "home", "--version"];

/// The docs an agent reads.
const DOC_FILES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "CONTEXT.md",
    "README.md",
    "app/README.md",
    "docs/desktop.md",
];

/// Every Markdown file below these folders is agent-facing too.
const DOC_DIRS: &[&str] = &["skills", "docs/agents", "agents"];

/// The lesson every lesson run works on, relative to the data folder.
const LESSON: &str = "courses/demo/lessons/0001-il-primo-passo.html";

const NODE_JSON: &str =
    r#"{"nodeId":"n1","title":"Nodo 1","tier":"axiom","content":"Il primo passo."}"#;
const NODE_FILE: &str =
    r#"{"nodeId":"n2","title":"Nodo 2","tier":"derivation","content":"Il secondo passo."}"#;

/// A command signature as the docs spell it: the flags are the part that must match a run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sig {
    file: String,
    cmd: String,
    sub: Option<String>,
    flags: Vec<String>,
}

/// One concrete invocation; `cmd`/`sub` name the signature it exercises.
struct Run {
    cmd: &'static str,
    sub: Option<&'static str>,
    argv: Vec<String>,
}

/// Files a run must have written: (index in `runs`, path under the data folder).
const CREATED: &[(usize, &str)] = &[
    (4, LESSON),
    (8, "courses/demo/reference/il-primo-passo.html"),
    (9, "index.html"),
];

fn run(cmd: &'static str, sub: Option<&'static str>, argv: &[&str]) -> Run {
    Run {
        cmd,
        sub,
        argv: argv.iter().map(|arg| arg.to_string()).collect(),
    }
}

/// Concrete runs for every signature `documented()` finds, in the order the task teaches a lesson.
fn runs(closed: &str) -> Vec<Run> {
    vec![
        run("--version", None, &["--version"]),
        run("data", Some("where"), &["data", "where"]),
        run("home", Some("chapters"), &["home", "chapters"]),
        run(
            "home",
            Some("plan"),
            &[
                "home",
                "plan",
                "--chapter",
                "Demo",
                "--slug",
                "demo",
                "--lessons-json",
                r#"[{"title":"Il primo passo","plannedNodes":2},{"title":"Il secondo passo","plannedNodes":3}]"#,
            ],
        ),
        run(
            "lesson",
            Some("init"),
            &[
                "lesson",
                "init",
                "--topic",
                "Demo",
                "--title",
                "Il primo passo",
                "--dag",
                "graph TD\n  N1[\"Nodo 1: a\"] --> N2[\"Nodo 2: b\"]",
                "--goal",
                "Capire il primo passo",
                "--out-dir",
                "courses/demo/lessons",
            ],
        ),
        run(
            "lesson",
            Some("append"),
            &[
                "lesson",
                "append",
                "--lesson",
                LESSON,
                "--node-json",
                NODE_JSON,
            ],
        ),
        run(
            "lesson",
            Some("append"),
            &[
                "lesson",
                "append",
                "--lesson",
                LESSON,
                "--node-file",
                "node.json",
            ],
        ),
        run(
            "anki",
            Some("sync"),
            &["anki", "sync", "--lesson", LESSON, "--anki-url", closed],
        ),
        run(
            "lesson",
            Some("compile-ref"),
            &[
                "lesson",
                "compile-ref",
                "--lesson",
                LESSON,
                "--out-dir",
                "courses/demo/reference",
            ],
        ),
        // `home --anki-url <closed>` is not here on purpose: a board without Anki exits 1
        // ("AnkiConnect non raggiungibile", golden generate-anki-unreachable), so `--no-anki` is
        // the only `home` that exits 0, and it covers the bare `proxi home` the docs quote.
        run("home", None, &["home", "--no-anki"]),
    ]
}

fn doc_files(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = DOC_FILES
        .iter()
        .map(|rel| root.join(rel))
        .filter(|path| path.is_file())
        .collect();
    for dir in DOC_DIRS {
        markdown_files(&root.join(dir), &mut files);
    }
    files
}

/// Recursive `*.md` scan; a missing folder contributes nothing.
fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            out.push(path);
        }
    }
}

fn documented() -> Vec<Sig> {
    let root = common::repo_root();
    let mut sigs = Vec::new();
    for file in doc_files(&root) {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        let name = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .display()
            .to_string()
            .replace('\\', "/");
        for line in quoted_lines(&text) {
            sigs.extend(sigs_in(&name, &line));
        }
    }
    sigs
}

/// Inside a fence: command lines, a trailing `\` joined with the next. Outside: the inline spans.
fn quoted_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut pending = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            pending.clear();
            continue;
        }
        if !in_fence {
            out.extend(inline_spans(line));
            continue;
        }
        let trimmed = line.trim_end();
        let continued = trimmed.ends_with('\\');
        let text = trimmed.strip_suffix('\\').unwrap_or(trimmed);
        if !pending.is_empty() {
            pending.push(' ');
        }
        pending.push_str(text.trim_start());
        if !continued {
            out.push(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        out.push(pending);
    }
    out
}

/// The contents of every `` `…` `` (or ``` ``…`` ```) span of one line.
fn inline_spans(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let mut fence = 1;
        while at + fence < bytes.len() && bytes[at + fence] == b'`' {
            fence += 1;
        }
        match closing_ticks(bytes, at + fence, fence) {
            Some(close) => {
                out.push(line[at + fence..close].to_string());
                at = close + fence;
            }
            None => at += fence,
        }
    }
    out
}

fn closing_ticks(bytes: &[u8], from: usize, fence: usize) -> Option<usize> {
    let mut at = from;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let mut ticks = 1;
        while at + ticks < bytes.len() && bytes[at + ticks] == b'`' {
            ticks += 1;
        }
        if ticks == fence {
            return Some(at);
        }
        at += ticks;
    }
    None
}

fn sigs_in(file: &str, line: &str) -> Vec<Sig> {
    // Everything after " #" is a shell comment.
    let line = match line.find(" #") {
        Some(at) => &line[..at],
        None => line,
    };
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(found) = line[search..].find("proxi ") {
        let at = search + found;
        search = at + "proxi ".len();
        // Start or whitespace before it: `my-proxi data` is not a command.
        if at > 0 && !line[..at].ends_with(char::is_whitespace) {
            continue;
        }
        let tokens: Vec<&str> = line[search..].split_whitespace().collect();
        let Some(&cmd) = tokens.first() else {
            continue;
        };
        if !COMMANDS.contains(&cmd) {
            continue;
        }
        let sub = tokens
            .get(1)
            .copied()
            .filter(|token| is_word(token))
            .map(str::to_string);
        let flags = tokens
            .iter()
            .skip(1)
            .filter(|token| is_flag(token))
            .map(|token| token.to_string())
            .collect();
        out.push(Sig {
            file: file.to_string(),
            cmd: cmd.to_string(),
            sub,
            flags,
        });
    }
    out
}

/// `^[a-z][a-z-]*$`: a subcommand, not a flag or a placeholder.
fn is_word(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c == '-')
}

/// `^--[a-z][a-z-]*$`: a flag, not a value.
fn is_flag(token: &str) -> bool {
    token.strip_prefix("--").is_some_and(is_word)
}

fn covers(sig: &Sig, run: &Run) -> bool {
    if sig.cmd != run.cmd {
        return false;
    }
    // `proxi home` says nothing about flags: any run of the command proves it.
    if sig.sub.is_none() && sig.flags.is_empty() {
        return true;
    }
    sig.sub.as_deref() == run.sub
        && sig
            .flags
            .iter()
            .all(|flag| run.argv.iter().any(|arg| arg == flag))
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap_or_else(|error| panic!("create {}: {error}", dst.display()));
    for entry in fs::read_dir(src).unwrap_or_else(|error| panic!("read {}: {error}", src.display()))
    {
        let entry = entry.expect("fixture entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target)
                .unwrap_or_else(|error| panic!("copy {}: {error}", entry.path().display()));
        }
    }
}

#[test]
fn every_documented_proxi_command_has_a_run() {
    let sigs = documented();
    for cmd in ["data", "lesson", "anki", "home"] {
        assert!(
            sigs.iter().any(|sig| sig.cmd == cmd),
            "the extractor found no `{cmd}` command: is it still reading the docs?"
        );
    }

    let runs = runs(&common::closed_port_url());
    let uncovered: Vec<String> = sigs
        .iter()
        .filter(|sig| !runs.iter().any(|run| covers(sig, run)))
        .map(|sig| {
            format!(
                "{}: {} {} {}",
                sig.file,
                sig.cmd,
                sig.sub.as_deref().unwrap_or(""),
                sig.flags.join(" ")
            )
        })
        .collect();
    assert!(
        uncovered.is_empty(),
        "documented commands with no run:\n{}",
        uncovered.join("\n")
    );
}

#[test]
fn documented_proxi_commands_exit_zero_on_the_fixture_data() {
    let tmp = common::temp_dir("docs-commands");
    let data = tmp.0.join("data");
    let cwd = tmp.0.join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");
    copy_dir(&common::repo_root().join("app/test/fixtures/repo"), &data);
    fs::write(data.join("node.json"), NODE_FILE).expect("write node.json");

    let closed = common::closed_port_url();
    for (index, run) in runs(&closed).iter().enumerate() {
        let output = Command::new(common::proxi_bin())
            .args(&run.argv)
            .current_dir(&cwd)
            .env("LEARNER_DATA_ROOT", &data)
            .env("LEARNER_ANKI_URL", &closed)
            .env_remove("LEARNER_REPO_ROOT")
            .output()
            .unwrap_or_else(|error| panic!("spawn proxi {}: {error}", run.argv.join(" ")));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let line = run.argv.join(" ");
        assert_eq!(
            output.status.code(),
            Some(0),
            "proxi {line}\nstdout: {stdout}\nstderr: {stderr}"
        );
        if index == 0 {
            assert!(
                stdout.starts_with("proxi "),
                "proxi --version printed {stdout:?}"
            );
        }
        for (_, path) in CREATED.iter().filter(|(created, _)| *created == index) {
            assert!(
                data.join(path).is_file(),
                "proxi {line} did not write {path}"
            );
        }
    }
}
