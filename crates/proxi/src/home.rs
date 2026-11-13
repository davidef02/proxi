//! Port of the home CLI of `extensions/homepage.ts`.

use std::env;
use std::io::Write;
use std::path::Path;

use serde_json::Value;

use crate::anki;
use crate::chapter;
use crate::data_root;
use crate::homepage::{self, GenerateOptions};
use crate::slug::slugify;

/// Verbatim from `extensions/homepage.ts` (same output, same help text).
pub const USAGE: &str = "Proxi homepage

  npm run home                      generate index.html (requires AnkiConnect)
  npm run home -- --no-anki         generate without card counts
  npm run home -- plan --chapter <title> [--slug <course-dir>] --lessons-json <json>
  npm run home -- chapters          print the plan as text (writes nothing)

  --lessons-json  [{\"title\":\"...\",\"plannedNodes\":4}, ...]
  --slug          course directory, when it differs from the chapter title
  --dir           courses directory (default ./courses)
  --anki-url      AnkiConnect endpoint (default http://127.0.0.1:8765)";

#[derive(Debug, Default)]
struct HomeArgs {
    chapter: Option<String>,
    slug: Option<String>,
    lessons_json: Option<String>,
    curriculum: Option<String>,
    dir: Option<String>,
    out: Option<String>,
    anki_url: Option<String>,
    no_anki: bool,
    help: bool,
    positionals: Vec<String>,
}

/// Replica of `node:util.parseArgs` (strict, `allowPositionals`) for the flags this CLI declares:
/// Node's messages are part of the contract, so clap is not used here.
fn parse_args(args: &[String]) -> Result<HomeArgs, String> {
    let mut parsed = HomeArgs::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;

        if arg == "--" {
            parsed.positionals.extend(args[index..].iter().cloned());
            break;
        }
        // A lone "-" is not option-like to Node: it is a positional.
        if arg == "-" || !arg.starts_with('-') {
            parsed.positionals.push(arg.to_string());
            continue;
        }

        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            match name {
                "no-anki" | "help" => {
                    if inline.is_some() {
                        // help also has a short alias, and Node prints both in its message.
                        let display = if name == "help" { "-h, --help" } else { "--no-anki" };
                        return Err(format!("Option '{display}' does not take an argument"));
                    }
                    if name == "help" {
                        parsed.help = true;
                    } else {
                        parsed.no_anki = true;
                    }
                }
                "chapter" | "slug" | "lessons-json" | "curriculum" | "dir" | "out" | "anki-url" => {
                    let value = match inline {
                        Some(value) => value.to_string(),
                        None => {
                            let Some(next) = args.get(index) else {
                                return Err(format!("Option '--{name} <value>' argument missing"));
                            };
                            // "--" itself counts as option-like here, "-" does not.
                            if next.len() > 1 && next.starts_with('-') {
                                return Err(format!(
                                    "Option '--{name}' argument is ambiguous.\nDid you forget to specify the option argument for '--{name}'?\nTo specify an option argument starting with a dash use '--{name}=-XYZ'."
                                ));
                            }
                            index += 1;
                            next.clone()
                        }
                    };
                    match name {
                        "chapter" => parsed.chapter = Some(value),
                        "slug" => parsed.slug = Some(value),
                        "lessons-json" => parsed.lessons_json = Some(value),
                        "curriculum" => parsed.curriculum = Some(value),
                        "dir" => parsed.dir = Some(value),
                        "out" => parsed.out = Some(value),
                        "anki-url" => parsed.anki_url = Some(value),
                        _ => unreachable!(),
                    }
                }
                _ => return Err(data_root::unknown_option(&format!("--{name}"))),
            }
            continue;
        }

        // Short group: every char but 'h' is unknown, reported one char at a time.
        for c in arg[1..].chars() {
            if c == 'h' {
                parsed.help = true;
            } else {
                return Err(data_root::unknown_option(&format!("-{c}")));
            }
        }
    }
    Ok(parsed)
}

pub fn run_cli_with(
    args: &[String],
    base_dir: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            let _ = writeln!(err, "Error: {message}");
            return 1;
        }
    };

    let command = parsed
        .positionals
        .first()
        .map(String::as_str)
        .filter(|command| !command.is_empty())
        .unwrap_or("generate");
    // Node checks help before dispatching, so `plan -h` shows the usage and writes nothing.
    if command == "help" || parsed.help {
        let _ = writeln!(out, "{USAGE}");
        return 0;
    }

    let result = match command {
        "plan" => run_plan(&parsed, base_dir, out),
        "chapters" => run_chapters(&parsed, base_dir, out, err),
        "generate" => run_generate(&parsed, base_dir, out, err),
        other => Err(format!("Unknown command: {other}")),
    };
    match result {
        Ok(()) => 0,
        Err(message) => {
            let _ = writeln!(err, "Error: {message}");
            1
        }
    }
}

fn run_plan(parsed: &HomeArgs, base_dir: &Path, out: &mut dyn Write) -> Result<(), String> {
    let chapter_title = parsed
        .chapter
        .as_deref()
        .filter(|chapter| !chapter.is_empty())
        .ok_or_else(|| "plan requires --chapter <title>".to_string())?;
    let raw = parsed
        .lessons_json
        .as_deref()
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| "plan requires --lessons-json '[{\"title\":\"...\"}]'".to_string())?;

    let lessons: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let lessons = lessons
        .as_array()
        .ok_or_else(|| "--lessons-json must be a JSON array".to_string())?;

    let curriculum = chapter::plan_chapter(
        &chapter::curriculum_file(base_dir, parsed.curriculum.as_deref()),
        chapter_title,
        parsed.slug.as_deref(),
        lessons,
    )?;

    let wanted = slugify(
        parsed
            .slug
            .as_deref()
            .filter(|slug| !slug.is_empty())
            .unwrap_or(chapter_title),
    );
    let saved = curriculum
        .chapters
        .iter()
        .find(|chapter| chapter::chapter_slug(chapter) == wanted)
        .ok_or_else(|| format!("chapter {wanted} missing after planning"))?;
    let slugs: Vec<String> = chapter::planned_lessons(saved)
        .into_iter()
        .map(|lesson| lesson.slug)
        .collect();
    let _ = writeln!(
        out,
        "Planned {} (courses/{}): {} lessons ({})",
        chapter::chapter_title(saved),
        chapter::chapter_slug(saved),
        slugs.len(),
        slugs.join(", ")
    );
    Ok(())
}

fn run_chapters(
    parsed: &HomeArgs,
    base_dir: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), String> {
    let curriculum = chapter::read_curriculum(&chapter::curriculum_file(
        base_dir,
        parsed.curriculum.as_deref(),
    ))?;
    let courses_dir = homepage::courses_dir_for(base_dir, parsed.dir.as_deref());
    let matched = chapter::match_curriculum(&curriculum, homepage::scan_lessons(&courses_dir, err));
    let _ = writeln!(out, "{}", chapter::format_curriculum(&matched));
    Ok(())
}

fn run_generate(
    parsed: &HomeArgs,
    base_dir: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), String> {
    let options = GenerateOptions {
        base_dir: base_dir.to_path_buf(),
        courses_dir: parsed.dir.clone(),
        curriculum: parsed.curriculum.clone(),
        out: parsed.out.clone(),
        no_anki: parsed.no_anki,
        anki_url: parsed.anki_url.clone(),
        timeout: anki::DEFAULT_TIMEOUT,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let result = runtime.block_on(homepage::generate_homepage(&options, err))?;

    let mut line = format!(
        "{} — {} chapters, {} lessons taught",
        result.file_path.display(),
        result.chapter_count,
        result.lesson_count
    );
    if result.orphan_count > 0 {
        line.push_str(&format!(", {} outside the plan", result.orphan_count));
    }
    if !result.anki_online {
        line.push_str(", without Anki");
    }
    let _ = writeln!(out, "{line}");
    Ok(())
}

/// CLI entrypoint: like `enterDataRoot` + `path.resolve(".")`, it chdirs into the data folder
/// before resolving any relative option (process-global, CLI only).
pub fn run_cli(args: &[String]) -> i32 {
    if let Err(error) = data_root::enter_data_root() {
        let _ = writeln!(std::io::stderr(), "Error: {error}");
        return 1;
    }
    let base_dir = env::current_dir().unwrap_or_default();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    run_cli_with(args, &base_dir, &mut out, &mut err)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;

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
            "proxi-home-cli-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn write(file: &Path, body: &str) {
        fs::create_dir_all(file.parent().expect("parent")).expect("create parent");
        fs::write(file, body).expect("write file");
    }

    fn run(base_dir: &Path, args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let code = run_cli_with(&args, base_dir, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("stdout utf-8"),
            String::from_utf8(err).expect("stderr utf-8"),
        )
    }

    /// Writes the lesson artifact shape the engine produces: the island is the last script tag.
    fn write_lesson(
        base_dir: &Path,
        courses: &str,
        course: &str,
        filename: &str,
        lesson_number: i64,
        slug: &str,
        title: &str,
    ) {
        let doc = json!({
            "metadata": { "lessonNumber": lesson_number, "slug": slug, "title": title },
            "nodes": [{ "tier": "axiom" }],
        });
        write(
            &base_dir
                .join(courses)
                .join(course)
                .join("lessons")
                .join(filename),
            &format!(
                "<html><body><h1>{title}</h1>\n<script type=\"application/json\" id=\"lesson-data\">\n{}\n</script>\n</body></html>",
                serde_json::to_string_pretty(&doc).expect("doc json")
            ),
        );
    }

    #[test]
    fn help_prints_usage() {
        let t = temp_dir("help");
        for args in [&["help"][..], &["-h"][..], &["plan", "-h"][..]] {
            let (code, out, err) = run(&t.0, args);
            assert_eq!(code, 0, "{args:?}");
            assert_eq!(out, format!("{USAGE}\n"), "{args:?}");
            assert_eq!(err, "", "{args:?}");
        }
    }

    #[test]
    fn help_with_a_value_is_rejected() {
        let t = temp_dir("help-value");
        let (code, out, err) = run(&t.0, &["--help=1"]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(err, "Error: Option '-h, --help' does not take an argument\n");
    }

    #[test]
    fn short_group_reports_the_unknown_char() {
        let t = temp_dir("short");
        let (code, out, err) = run(&t.0, &["-hx"]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "Error: Unknown option '-x'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"-x\"\n"
        );
    }

    #[test]
    fn unknown_long_option_reports_the_name_before_equals() {
        let t = temp_dir("long");
        let (code, out, err) = run(&t.0, &["--bogus=3"]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "Error: Unknown option '--bogus'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"--bogus\"\n"
        );
    }

    #[test]
    fn missing_option_value() {
        let t = temp_dir("missing");
        let (code, out, err) = run(&t.0, &["plan", "--chapter"]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(err, "Error: Option '--chapter <value>' argument missing\n");
    }

    #[test]
    fn ambiguous_option_value_spans_three_lines() {
        let t = temp_dir("ambiguous");
        let (code, out, err) = run(&t.0, &["plan", "--chapter", "--slug", "x"]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "Error: Option '--chapter' argument is ambiguous.\nDid you forget to specify the option argument for '--chapter'?\nTo specify an option argument starting with a dash use '--chapter=-XYZ'.\n"
        );
    }

    #[test]
    fn boolean_option_rejects_an_inline_value() {
        let t = temp_dir("boolean");
        let (code, out, err) = run(&t.0, &["--no-anki=1"]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(err, "Error: Option '--no-anki' does not take an argument\n");
    }

    #[test]
    fn unknown_command() {
        let t = temp_dir("unknown");
        let (code, out, err) = run(&t.0, &["frobnicate"]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(err, "Error: Unknown command: frobnicate\n");
    }

    #[test]
    fn plan_writes_curriculum_and_prints_summary() {
        let t = temp_dir("plan");
        let (code, out, err) = run(
            &t.0,
            &[
                "plan",
                "--chapter",
                "App State & Caching",
                "--lessons-json",
                "[{\"title\":\"Probabilità condizionata\",\"plannedNodes\":3}]",
            ],
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            "Planned App State & Caching (courses/app-state-and-caching): 1 lessons (probabilita-condizionata)\n"
        );
        assert_eq!(err, "");

        let saved = fs::read_to_string(t.0.join("curriculum.json")).expect("plan written");
        assert!(saved.contains("\"slug\": \"app-state-and-caching\""));
        assert!(saved.contains("\"plannedNodes\": 3"));
    }

    #[test]
    fn plan_empty_slug_falls_back_to_the_chapter_slug() {
        let t = temp_dir("plan-slug");
        let (code, out, err) = run(
            &t.0,
            &[
                "plan",
                "--chapter",
                "Docker",
                "--slug=",
                "--lessons-json",
                "[]",
            ],
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, "Planned Docker (courses/docker): 0 lessons ()\n");
        assert_eq!(err, "");
        assert!(t.0.join("curriculum.json").exists());
    }

    #[test]
    fn chapters_on_an_empty_base_prints_nothing_planned() {
        let t = temp_dir("chapters-empty");
        let (code, out, err) = run(&t.0, &["chapters"]);
        assert_eq!(code, 0);
        assert_eq!(out, "Nessun capitolo pianificato.\n");
        assert_eq!(err, "");
        assert!(!t.0.join("curriculum.json").exists());
    }

    #[test]
    fn generate_without_anki_on_an_empty_base() {
        let t = temp_dir("generate");
        let (code, out, err) = run(&t.0, &["--no-anki"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            format!(
                "{} — 0 chapters, 0 lessons taught, without Anki\n",
                t.0.join("index.html").display()
            )
        );
        assert_eq!(err, "");
        assert!(t.0.join("index.html").exists());
    }

    #[test]
    fn relative_out_resolves_against_the_base() {
        let t = temp_dir("generate-out");
        let (code, out, err) = run(&t.0, &["--no-anki", "--out", "board.html"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            format!(
                "{} — 0 chapters, 0 lessons taught, without Anki\n",
                t.0.join("board.html").display()
            )
        );
        assert_eq!(err, "");
        assert!(t.0.join("board.html").exists());
        assert!(!t.0.join("index.html").exists());
    }

    /// Node's falsy option values: `--dir=`, `--curriculum=` and `--out=` mean "not given".
    #[test]
    fn empty_option_values_mean_not_given() {
        let t = wiring_base("empty-values");
        let (code, out, err) = run(&t.0, &["--no-anki", "--dir=", "--curriculum=", "--out="]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            format!(
                "{} — 1 chapters, 1 lessons taught, without Anki\n",
                t.0.join("index.html").display()
            )
        );
        assert_eq!(err, "");
        assert!(t.0.join("index.html").exists());

        // `--chapter=` and `--lessons-json=` are empty too: the required-flag errors fire.
        let (code, out, err) = run(&t.0, &["plan", "--chapter=", "--lessons-json", "[]"]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(err, "Error: plan requires --chapter <title>\n");

        let (code, out, err) = run(&t.0, &["plan", "--chapter", "X", "--lessons-json="]);
        assert_eq!(code, 1);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "Error: plan requires --lessons-json '[{\"title\":\"...\"}]'\n"
        );
    }

    #[test]
    fn double_dash_and_lone_dash_are_positionals_and_the_last_option_wins() {
        let t = temp_dir("positionals");
        // Everything after `--` is positional: `-h` must not print the usage.
        let (code, out, err) = run(&t.0, &["chapters", "--", "-h"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, "Nessun capitolo pianificato.\n");
        assert_eq!(err, "");

        // A lone `-` is a positional too, not an option-like token.
        let (code, out, err) = run(&t.0, &["chapters", "-", "--no-anki"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, "Nessun capitolo pianificato.\n");
        assert_eq!(err, "");

        // Repeated options: the last value wins, like `node:util.parseArgs`.
        let (code, out, err) = run(
            &t.0,
            &["--no-anki", "--out", "first.html", "--out", "second.html"],
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            format!(
                "{} — 0 chapters, 0 lessons taught, without Anki\n",
                t.0.join("second.html").display()
            )
        );
        assert_eq!(err, "");
        assert!(t.0.join("second.html").exists());
        assert!(!t.0.join("first.html").exists());
    }

    /// Base with two plans and two course trees, so ignoring `--dir`/`--curriculum` is visible.
    fn wiring_base(tag: &str) -> TempDir {
        let t = temp_dir(tag);
        let curriculum = json!({
            "chapters": [{
                "title": "Docker",
                "slug": "docker",
                "lessons": [{ "title": "Image e container", "slug": "image-e-container" }],
            }],
        });
        write(
            &t.0.join("curriculum.json"),
            &(serde_json::to_string_pretty(&curriculum).expect("curriculum json") + "\n"),
        );
        write_lesson(
            &t.0,
            "courses",
            "docker",
            "0001-image-e-container.html",
            1,
            "image-e-container",
            "Image e container",
        );

        let plan = json!({
            "chapters": [{
                "title": "Rust",
                "slug": "rust",
                "lessons": [
                    { "title": "Ownership", "slug": "ownership" },
                    { "title": "Lifetimes", "slug": "lifetimes" },
                ],
            }],
        });
        write(
            &t.0.join("plan.json"),
            &(serde_json::to_string_pretty(&plan).expect("plan json") + "\n"),
        );
        write_lesson(&t.0, "corsi", "rust", "0001-ownership.html", 1, "ownership", "Ownership");
        t
    }

    #[test]
    fn chapters_honours_dir_and_curriculum() {
        let t = wiring_base("wiring-chapters");
        let (code, out, err) = run(
            &t.0,
            &["chapters", "--dir", "corsi", "--curriculum", "plan.json"],
        );
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("Rust"), "{out}");
        assert!(out.contains("Ownership"), "{out}");
        assert!(!out.contains("Docker"), "{out}");
        assert!(!out.contains("courses/docker"), "{out}");
        assert_eq!(err, "");
    }

    #[test]
    fn generate_honours_dir_and_curriculum() {
        let t = wiring_base("wiring-generate");
        let (code, out, err) = run(
            &t.0,
            &[
                "--no-anki",
                "--dir",
                "corsi",
                "--curriculum",
                "plan.json",
                "--out",
                "board.html",
            ],
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            format!(
                "{} — 1 chapters, 1 lessons taught, without Anki\n",
                t.0.join("board.html").display()
            )
        );
        assert_eq!(err, "");
        assert!(t.0.join("board.html").exists());
        assert!(!t.0.join("index.html").exists());

        // An absolute --dir resolves to the same directory without joining the base again.
        let (code, absolute_out, absolute_err) = run(
            &t.0,
            &[
                "--no-anki",
                "--dir",
                t.0.join("corsi").to_str().expect("utf-8 path"),
                "--curriculum",
                "plan.json",
                "--out",
                "board.html",
            ],
        );
        assert_eq!(code, 0, "{absolute_err}");
        assert_eq!(absolute_out, out);
    }

    #[test]
    fn plan_honours_curriculum_and_leaves_the_default_plan_alone() {
        let t = wiring_base("wiring-plan");
        let before = fs::read(t.0.join("curriculum.json")).expect("default plan");

        let (code, out, err) = run(
            &t.0,
            &[
                "plan",
                "--curriculum",
                "plan.json",
                "--chapter",
                "Rust",
                "--lessons-json",
                "[{\"title\":\"Traits\"}]",
            ],
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, "Planned Rust (courses/rust): 1 lessons (traits)\n");
        assert_eq!(err, "");

        let plan = fs::read_to_string(t.0.join("plan.json")).expect("plan written");
        assert!(plan.contains("\"traits\""), "{plan}");
        assert!(!plan.contains("\"ownership\""), "{plan}");
        assert_eq!(
            fs::read(t.0.join("curriculum.json")).expect("default plan"),
            before
        );
    }
}
