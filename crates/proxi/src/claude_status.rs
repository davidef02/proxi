//! Claude CLI version probe (port of app/server/claude.ts:158-240).

use std::cmp::Ordering;
use std::process::Stdio;
use std::time::Duration;

use crate::child;
use crate::claude;
use crate::engine::spawn_error_message;
use crate::js;
use crate::protocol::ClaudeStatusResponse;

pub const CLAUDE_MIN_VERSION: &str = "2.1.259";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The three groups as written (`2.01.3` stays `2.01.3`): Node reports them verbatim.
    pub raw: String,
}

/// `output.trim().match(/^(\d+)\.(\d+)\.(\d+)/)`: anything after the third group is ignored.
pub fn parse_claude_version(output: &str) -> Option<Semver> {
    let text = js::js_trim(output);
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut numbers = [0u64; 3];
    let mut written: [&str; 3] = [""; 3];
    for (index, number) in numbers.iter_mut().enumerate() {
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return None;
        }
        *number = text[start..at].parse().unwrap_or(u64::MAX);
        written[index] = &text[start..at];
        if index < 2 {
            if bytes.get(at) != Some(&b'.') {
                return None;
            }
            at += 1;
        }
    }
    Some(Semver {
        major: numbers[0],
        minor: numbers[1],
        patch: numbers[2],
        raw: format!("{}.{}.{}", written[0], written[1], written[2]),
    })
}

pub fn compare_semver(a: &Semver, b: &Semver) -> Ordering {
    a.major
        .cmp(&b.major)
        .then_with(|| a.minor.cmp(&b.minor))
        .then_with(|| a.patch.cmp(&b.patch))
}

/// Node's `execFile` rejection text: `Command failed: <cmd> <args>`, then the stderr when there is one.
fn command_failed(program: &str, args: &[String], stderr: &str) -> String {
    let mut message = format!("Command failed: {program} {}", args.join(" "));
    if !stderr.is_empty() {
        message.push('\n');
        message.push_str(stderr);
    }
    message
}

fn unchecked(min_version: &str, bin_path: &str, error: String) -> ClaudeStatusResponse {
    ClaudeStatusResponse {
        status: "version-unchecked".to_string(),
        version: None,
        min_version: min_version.to_string(),
        bin_path: Some(bin_path.to_string()),
        error: Some(error),
    }
}

/// Runs `<bin> --version` and classifies the answer (claude.ts:194-240).
pub async fn probe_claude_cli(bin_path: &str, timeout: Duration) -> ClaudeStatusResponse {
    let min_version = CLAUDE_MIN_VERSION.to_string();
    let min_parsed = parse_claude_version(CLAUDE_MIN_VERSION).expect("the minimum version parses");
    if bin_path.is_empty() {
        return ClaudeStatusResponse {
            status: "not-found".to_string(),
            version: None,
            min_version,
            bin_path: None,
            error: None,
        };
    }

    let mut bin_path = bin_path.to_string();
    // `binPath === "claude"` is the unlocated default: try the usual install dirs once more.
    if bin_path == "claude"
        && let Some(resolved) = claude::locate_from_process()
    {
        bin_path = resolved;
    }

    let is_js_script = bin_path.ends_with(".js") || bin_path.ends_with(".mjs");
    let program = if is_js_script {
        child::node_on_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "node".to_string())
    } else {
        bin_path.clone()
    };
    let args: Vec<String> = if is_js_script {
        vec![bin_path.clone(), "--version".to_string()]
    } else {
        vec!["--version".to_string()]
    };

    let mut command = tokio::process::Command::new(&program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return ClaudeStatusResponse {
                status: "not-found".to_string(),
                version: None,
                min_version,
                bin_path: Some(bin_path),
                error: Some(spawn_error_message(&program, &err)),
            };
        }
        Err(err) => return unchecked(&min_version, &bin_path, err.to_string()),
    };

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return unchecked(&min_version, &bin_path, err.to_string()),
        // The child is dropped and killed: Node's timeout is a `Command failed` too.
        Err(_) => return unchecked(&min_version, &bin_path, command_failed(&program, &args, "")),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return unchecked(
            &min_version,
            &bin_path,
            command_failed(&program, &args, &stderr),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let Some(parsed) = parse_claude_version(&stdout) else {
        return unchecked(
            &min_version,
            &bin_path,
            format!("Output non riconosciuto: {}", js::js_trim(&stdout)),
        );
    };

    let status = if compare_semver(&parsed, &min_parsed) == Ordering::Less {
        "update-required"
    } else {
        "meets-minimum"
    };
    ClaudeStatusResponse {
        status: status.to_string(),
        version: Some(parsed.raw),
        min_version,
        bin_path: Some(bin_path),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(major: u64, minor: u64, patch: u64) -> Semver {
        Semver {
            major,
            minor,
            patch,
            raw: format!("{major}.{minor}.{patch}"),
        }
    }

    #[test]
    fn parse_claude_version_matches_node() {
        assert_eq!(
            parse_claude_version("2.1.314 (Claude Code)\r\n"),
            Some(version(2, 1, 314))
        );
        assert_eq!(
            parse_claude_version("0.0.0 (fake-claude)\n"),
            Some(version(0, 0, 0))
        );
        assert_eq!(parse_claude_version("2.1.259"), Some(version(2, 1, 259)));
        assert_eq!(
            parse_claude_version("  2.1.314  "),
            Some(version(2, 1, 314))
        );
        // `raw` is what was printed, the numbers what it means.
        assert_eq!(
            parse_claude_version("02.01.314"),
            Some(Semver {
                raw: "02.01.314".to_string(),
                ..version(2, 1, 314)
            })
        );

        assert_eq!(parse_claude_version("v2.1.0"), None);
        assert_eq!(parse_claude_version("unknown command"), None);
        assert_eq!(parse_claude_version(""), None);
        assert_eq!(parse_claude_version("claude version error"), None);
        assert_eq!(parse_claude_version("garbage"), None);
        assert_eq!(parse_claude_version("2.1"), None);
    }

    #[test]
    fn compare_semver_orders_each_part() {
        let min = version(2, 1, 259);
        for greater in [version(3, 0, 0), version(2, 2, 0), version(2, 1, 300)] {
            assert_eq!(compare_semver(&greater, &min), Ordering::Greater);
            assert_eq!(compare_semver(&min, &greater), Ordering::Less);
        }
        assert_eq!(compare_semver(&version(2, 1, 259), &min), Ordering::Equal);
        for lower in [version(2, 1, 258), version(2, 0, 999), version(1, 9, 999)] {
            assert_eq!(compare_semver(&lower, &min), Ordering::Less);
        }
    }
}
