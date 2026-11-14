//! `proxi anki` command line: the Node CLI's flags, output and exit codes.

use std::io::{Read, Write};

use serde_json::Value;

use crate::data_root;
use crate::js::parse_json;
use crate::lesson::js::stringify_pretty;

use super::cards::{self, ExportOptions};
use super::ping;

/// Verbatim from `extensions/anki-sync.ts:813-838`: A5 keeps Node's text even when it names tsx.
pub const HELP: &str = concat!(
    "\nProxi Anki Sync & Deck Exporter\n\n",
    "Usage:\n",
    "  npx tsx extensions/anki-sync.ts <command> [options]\n\n",
    "Commands:\n",
    "  export     Export cards to ./decks/<slug>.tsv (lesson slug with --lesson, else topic slug)\n",
    "  sync       Synchronize cards to local AnkiConnect (http://127.0.0.1:8765)\n",
    "  extract    Extract cards from an HTML lesson file as JSON\n",
    "  ping       Test connection to local AnkiConnect server\n\n",
    "Options:\n",
    "  --lesson <path>       Path to standalone HTML lesson file\n",
    "  --topic <name>        Topic or subject title\n",
    "  --slug <slug>         Topic slug\n",
    "  --deck <name>         Target Anki deck name (default: Proxi::<Topic>)\n",
    "  --data <json>         Direct JSON payload containing cards or lessonDoc\n",
    "  --stdin               Read JSON payload from stdin\n",
    "  --out <file>          Custom output TSV file path\n",
    "  --base-dir <dir>      Workspace root directory\n",
    "  --anki-url <url>      AnkiConnect URL (default: http://127.0.0.1:8765)\n",
    "  --timeout <ms>        Network timeout in milliseconds (default: 2000)\n",
    "  --allow-duplicates    Allow duplicate cards in AnkiConnect\n",
);

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
    let command = args
        .first()
        .map(String::as_str)
        .filter(|arg| !arg.is_empty())
        .unwrap_or("--help");

    match command {
        "--help" | "-h" | "help" => {
            let _ = writeln!(out, "{HELP}");
            0
        }
        "ping" => {
            // Node's ping reads only --anki-url: LEARNER_ANKI_URL is a server setting.
            let url = get_flag(args, "--anki-url")
                .filter(|text| !text.is_empty())
                .unwrap_or(ping::DEFAULT_URL);
            let timeout = ping::parse_timeout(get_flag(args, "--timeout"));
            let result = ping::ping_anki_connect(url, timeout);
            let _ = writeln!(out, "{}", ping::ping_line(url, &result));
            0
        }
        "extract" => match get_flag(args, "--lesson").filter(|text| !text.is_empty()) {
            None => {
                let _ = writeln!(err, "Error: --lesson <path> is required for extract command");
                1
            }
            Some(lesson) => match cards::extract_cards_from_lesson(lesson) {
                Ok(extracted) => {
                    let _ = writeln!(out, "{}", stringify_pretty(&extracted.to_json()));
                    0
                }
                Err(message) => {
                    let _ = writeln!(err, "Fatal error: {message}");
                    1
                }
            },
        },
        "export" => run_export(args, stdin, out, err),
        // The agents keep using `npm run anki` for sync until #17 ports it.
        "sync" => {
            let _ = writeln!(
                err,
                "Error: sync is not ported to proxi yet: run npm run anki -- sync"
            );
            1
        }
        unknown => {
            let _ = writeln!(err, "Unknown command: {unknown}");
            1
        }
    }
}

fn run_export(
    args: &[String],
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    // Node reads the payload before it looks at --lesson: bad JSON fails even with a lesson.
    let payload = match read_payload(args, stdin) {
        Ok(payload) => payload,
        Err(message) => {
            let _ = writeln!(err, "Fatal error: SyntaxError: {message}");
            return 1;
        }
    };
    let payload = payload.as_ref();

    let options = ExportOptions {
        cards: payload.and_then(|value| value.get("cards")).cloned(),
        nodes: payload.and_then(|value| value.get("nodes")).cloned(),
        lesson_path: get_flag(args, "--lesson").map(str::to_string),
        topic: flag_string(args, "--topic")
            .or_else(|| payload.and_then(|value| value.get("topic")).cloned()),
        slug: flag_string(args, "--slug")
            .or_else(|| payload.and_then(|value| value.get("slug")).cloned()),
        out_file: get_flag(args, "--out").map(str::to_string),
        base_dir: get_flag(args, "--base-dir").map(str::to_string),
    };

    match cards::export_deck_tsv(&options) {
        Ok(result) => {
            let _ = writeln!(
                out,
                "✅ Exported {} cards to {}",
                result.card_count, result.file_path
            );
            0
        }
        Err(message) => {
            let _ = writeln!(err, "Fatal error: {message}");
            1
        }
    }
}

/// Node's `readPayload`: `--data` first, then `--stdin`, otherwise nothing to read.
fn read_payload(args: &[String], stdin: &mut dyn Read) -> Result<Option<Value>, String> {
    if let Some(data) = get_flag(args, "--data").filter(|text| !text.is_empty()) {
        return parse_json(data).map(Some);
    }
    if has_flag(args, "--stdin") {
        let mut bytes = Vec::new();
        stdin.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        return parse_json(&String::from_utf8_lossy(&bytes)).map(Some);
    }
    Ok(None)
}

/// The next argument is the value even when it starts with `--`, as in Node's `getFlag`.
fn get_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let idx = args.iter().position(|arg| arg == flag)?;
    args.get(idx + 1).map(String::as_str)
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

/// `getArg(flag) || fallback`: an empty flag value falls through.
fn flag_string(args: &[String], flag: &str) -> Option<Value> {
    get_flag(args, flag)
        .filter(|text| !text.is_empty())
        .map(|text| Value::String(text.to_string()))
}
