//! Data folder resolution and the `data` command.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub const DATA_ROOT_ENV: &str = "LEARNER_DATA_ROOT";
pub const REPO_ROOT_ENV: &str = "LEARNER_REPO_ROOT";
pub const DATA_DIR_NAME: &str = "proxi-data";

/// Verbatim from `extensions/data-root.ts` (same output, same help text).
pub const USAGE: &str = "Usage: npm run data -- <command>

  where                  print the data folder and where it was resolved from
  help                   show this message

Resolution: LEARNER_DATA_ROOT, then .learner/config.json, then ~/Documents/proxi-data.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataRootSource {
    Env,
    Config,
    Default,
}

impl DataRootSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DataRootSource::Env => "env",
            DataRootSource::Config => "config",
            DataRootSource::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDataRoot {
    pub data_root: PathBuf,
    pub source: DataRootSource,
}

/// Node `path.resolve`: absolute paths stay absolute, everything else resolves against the cwd.
pub fn resolve_path(p: &Path) -> PathBuf {
    let has_prefix = matches!(p.components().next(), Some(Component::Prefix(_)));
    // Windows: a rooted path without a drive (`\foo`) still takes the cwd's drive.
    let source = if p.has_root() && has_prefix {
        p.to_path_buf()
    } else {
        env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::new();
    for component in source.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn is_node_absolute(p: &Path) -> bool {
    p.has_root()
}

pub fn default_data_root(home: &Path) -> PathBuf {
    home.join("Documents").join(DATA_DIR_NAME)
}

/// True when `dir` already holds learner data (courses/ or curriculum.json).
pub fn has_data(dir: &Path) -> bool {
    if fs::metadata(dir.join("courses"))
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
    {
        return true;
    }
    dir.join("curriculum.json").exists()
}

pub fn read_data_root_pointer(repo_root: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(repo_root.join(".learner").join("config.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let data_root = parsed.as_object()?.get("dataRoot")?.as_str()?;
    // The absolute check is on the untrimmed value, like Node's path.isAbsolute.
    if data_root.trim().is_empty() || !is_node_absolute(Path::new(data_root)) {
        return None;
    }
    Some(resolve_path(Path::new(data_root)))
}

pub fn resolve_data_root(
    repo_root: &Path,
    env_data_root: Option<&str>,
    home: &Path,
) -> ResolvedDataRoot {
    if let Some(value) = env_data_root.map(str::trim).filter(|v| !v.is_empty()) {
        return ResolvedDataRoot {
            data_root: resolve_path(Path::new(value)),
            source: DataRootSource::Env,
        };
    }
    if let Some(pointer) = read_data_root_pointer(repo_root) {
        return ResolvedDataRoot {
            data_root: pointer,
            source: DataRootSource::Config,
        };
    }
    ResolvedDataRoot {
        data_root: default_data_root(home),
        source: DataRootSource::Default,
    }
}

pub fn home_dir() -> PathBuf {
    env::home_dir().unwrap_or_default()
}

pub fn repo_root_from(env_repo: Option<&str>, exe: Option<&Path>, manifest_dir: &Path) -> PathBuf {
    if let Some(value) = env_repo.filter(|v| !v.is_empty()) {
        return resolve_path(Path::new(value));
    }
    if let Some(exe) = exe {
        // The binary lives in <repo>/target/{release,debug}: walk up to the first repo marker.
        for dir in exe.ancestors().skip(1) {
            if dir.join("app").join("shared").join("identity.mjs").is_file() {
                return resolve_path(dir);
            }
        }
    }
    resolve_path(&manifest_dir.join("..").join(".."))
}

pub fn repo_root() -> PathBuf {
    repo_root_from(
        env::var(REPO_ROOT_ENV).ok().as_deref(),
        env::current_exe().ok().as_deref(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )
}

pub fn run_cli_with(
    args: &[String],
    repo_root: &Path,
    env_data_root: Option<&str>,
    home: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut positionals: Vec<&str> = Vec::new();
    let mut positional_only = false;
    for arg in args {
        let token = arg.as_str();
        if positional_only {
            positionals.push(token);
            continue;
        }
        match token {
            "--" => positional_only = true,
            "-" => positionals.push(token),
            _ if token.starts_with("--") => {
                // Node reports the text before the first '=' for long options.
                let name = token.split('=').next().unwrap_or(token);
                let _ = writeln!(err, "Error: {}", unknown_option(name));
                return 1;
            }
            _ if token.starts_with('-') => {
                // Short groups report only the first char after the dash.
                let name = format!("-{}", token.chars().nth(1).unwrap_or('-'));
                let _ = writeln!(err, "Error: {}", unknown_option(&name));
                return 1;
            }
            _ => positionals.push(token),
        }
    }

    let command = positionals.first().copied().unwrap_or("help");
    match command {
        "where" => {
            let resolved = resolve_data_root(repo_root, env_data_root, home);
            let _ = writeln!(
                out,
                "{} ({})",
                resolved.data_root.display(),
                resolved.source.as_str()
            );
            0
        }
        "help" => {
            let _ = writeln!(out, "{USAGE}");
            0
        }
        other => {
            let _ = writeln!(err, "Unknown command: {other}");
            let _ = writeln!(err, "{USAGE}");
            1
        }
    }
}

fn unknown_option(name: &str) -> String {
    // Node's text really has no closing quote after the example: parity wins over prettiness.
    format!(
        "Unknown option '{name}'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"{name}\""
    )
}

pub fn run_cli(args: &[String]) -> i32 {
    let repo = repo_root();
    let env_data_root = env::var(DATA_ROOT_ENV).ok();
    let home = home_dir();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    run_cli_with(
        args,
        &repo,
        env_data_root.as_deref(),
        &home,
        &mut out,
        &mut err,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
        let path = env::temp_dir().join(format!(
            "proxi-data-root-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn write_config(repo: &Path, body: &str) {
        let dir = repo.join(".learner");
        fs::create_dir_all(&dir).expect("create .learner");
        fs::write(dir.join("config.json"), body).expect("write config");
    }

    fn write_pointer(repo: &Path, data_root: &Path) {
        let body = serde_json::json!({ "dataRoot": data_root }).to_string();
        write_config(repo, &body);
    }

    fn mark_repo(dir: &Path) {
        let marker = dir.join("app").join("shared").join("identity.mjs");
        fs::create_dir_all(marker.parent().unwrap()).expect("create marker dir");
        fs::write(marker, "").expect("write marker");
    }

    fn run(
        args: &[&str],
        repo: &Path,
        env_data_root: Option<&str>,
        home: &Path,
    ) -> (i32, String, String) {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_cli_with(&owned, repo, env_data_root, home, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    #[test]
    fn env_beats_pointer_and_default() {
        let t = temp_dir("env-wins");
        let repo = t.0.join("repo");
        let env_data = t.0.join("env-data");
        write_pointer(&repo, &t.0.join("pointer"));

        let resolved = resolve_data_root(
            &repo,
            Some(env_data.to_str().unwrap()),
            &t.0.join("home"),
        );
        assert_eq!(resolved.source, DataRootSource::Env);
        assert_eq!(resolved.data_root, resolve_path(&env_data));
    }

    #[test]
    fn whitespace_only_env_is_ignored() {
        let t = temp_dir("env-blank");
        let repo = t.0.join("repo");
        let pointer = t.0.join("pointer");
        write_pointer(&repo, &pointer);

        let resolved = resolve_data_root(&repo, Some(" \t "), &t.0.join("home"));
        assert_eq!(resolved.source, DataRootSource::Config);
        assert_eq!(resolved.data_root, resolve_path(&pointer));
    }

    #[test]
    fn env_is_trimmed() {
        let t = temp_dir("env-padded");
        let data = t.0.join("data");
        let padded = format!("  {}  ", data.display());

        let resolved = resolve_data_root(&t.0.join("repo"), Some(&padded), &t.0.join("home"));
        assert_eq!(resolved.source, DataRootSource::Env);
        assert_eq!(resolved.data_root, resolve_path(&data));
    }

    #[test]
    fn relative_env_resolves_against_cwd() {
        let t = temp_dir("env-relative");
        let resolved = resolve_data_root(&t.0.join("repo"), Some("rel/dir"), &t.0.join("home"));
        assert_eq!(resolved.source, DataRootSource::Env);
        assert_eq!(
            resolved.data_root,
            env::current_dir().expect("cwd").join("rel").join("dir")
        );
    }

    #[test]
    fn resolve_path_normalises_dots() {
        let cwd = env::current_dir().expect("cwd");
        assert_eq!(
            resolve_path(Path::new("a/./b/../c")),
            cwd.join("a").join("c")
        );
    }

    #[test]
    fn resolve_path_drops_trailing_separator() {
        let cwd = env::current_dir().expect("cwd");
        assert_eq!(resolve_path(Path::new("a/b/")), cwd.join("a").join("b"));
    }

    #[test]
    fn pointer_relative_falls_back_to_default() {
        let t = temp_dir("pointer-relative");
        let repo = t.0.join("repo");
        let home = t.0.join("home");
        write_config(&repo, r#"{"dataRoot":"rel/dir"}"#);

        let resolved = resolve_data_root(&repo, None, &home);
        assert_eq!(resolved.source, DataRootSource::Default);
        assert_eq!(resolved.data_root, default_data_root(&home));
    }

    #[test]
    fn pointer_empty_falls_back_to_default() {
        let t = temp_dir("pointer-empty");
        let repo = t.0.join("repo");
        let home = t.0.join("home");
        write_config(&repo, r#"{"dataRoot":"   "}"#);

        let resolved = resolve_data_root(&repo, None, &home);
        assert_eq!(resolved.source, DataRootSource::Default);
        assert_eq!(resolved.data_root, default_data_root(&home));
    }

    #[test]
    fn pointer_non_string_falls_back_to_default() {
        let t = temp_dir("pointer-non-string");
        let repo = t.0.join("repo");
        let home = t.0.join("home");

        for body in [r#"{"dataRoot":42}"#, r#"{"dataRoot":null}"#] {
            write_config(&repo, body);
            let resolved = resolve_data_root(&repo, None, &home);
            assert_eq!(resolved.source, DataRootSource::Default, "{body}");
            assert_eq!(resolved.data_root, default_data_root(&home), "{body}");
        }
    }

    #[test]
    fn pointer_array_falls_back_to_default() {
        let t = temp_dir("pointer-array");
        let repo = t.0.join("repo");
        let home = t.0.join("home");
        write_config(&repo, r#"["C:/somewhere"]"#);

        let resolved = resolve_data_root(&repo, None, &home);
        assert_eq!(resolved.source, DataRootSource::Default);
        assert_eq!(resolved.data_root, default_data_root(&home));
    }

    #[test]
    fn pointer_invalid_json_falls_back_to_default() {
        let t = temp_dir("pointer-invalid");
        let repo = t.0.join("repo");
        let home = t.0.join("home");
        write_config(&repo, "{not json");

        let resolved = resolve_data_root(&repo, None, &home);
        assert_eq!(resolved.source, DataRootSource::Default);
        assert_eq!(resolved.data_root, default_data_root(&home));
    }

    #[test]
    fn pointer_missing_falls_back_to_default() {
        let t = temp_dir("pointer-missing");
        let home = t.0.join("home");

        let resolved = resolve_data_root(&t.0.join("repo"), None, &home);
        assert_eq!(resolved.source, DataRootSource::Default);
        assert_eq!(resolved.data_root, default_data_root(&home));
    }

    #[test]
    fn pointer_parent_dirs_are_normalised() {
        let t = temp_dir("pointer-dots");
        let repo = t.0.join("repo");
        write_pointer(&repo, &t.0.join("a").join("b").join("..").join("data"));

        let resolved = resolve_data_root(&repo, None, &t.0.join("home"));
        assert_eq!(resolved.source, DataRootSource::Config);
        assert_eq!(resolved.data_root, t.0.join("a").join("data"));
    }

    #[test]
    fn default_data_root_is_documents_folder() {
        let t = temp_dir("default");
        assert_eq!(
            default_data_root(&t.0),
            t.0.join("Documents").join("proxi-data")
        );
    }

    #[test]
    fn has_data_false_on_empty_dir() {
        let t = temp_dir("has-data-empty");
        assert!(!has_data(&t.0));
    }

    #[test]
    fn has_data_true_with_courses_dir() {
        let t = temp_dir("has-data-courses");
        fs::create_dir_all(t.0.join("courses")).expect("create courses");
        assert!(has_data(&t.0));
    }

    #[test]
    fn has_data_false_when_courses_is_a_file() {
        let t = temp_dir("has-data-courses-file");
        fs::write(t.0.join("courses"), "").expect("write courses file");
        assert!(!has_data(&t.0));
    }

    #[test]
    fn has_data_true_with_curriculum_only() {
        let t = temp_dir("has-data-curriculum");
        fs::write(t.0.join("curriculum.json"), "{}").expect("write curriculum");
        assert!(has_data(&t.0));
    }

    #[test]
    fn repo_root_env_wins_and_resolves() {
        let t = temp_dir("repo-root-env");
        let marked = t.0.join("repo");
        mark_repo(&marked);
        let exe = marked.join("target").join("release").join("proxi.exe");
        let env_repo = t.0.join("env-repo");

        assert_eq!(
            repo_root_from(
                Some(env_repo.to_str().unwrap()),
                Some(&exe),
                &t.0.join("m")
            ),
            resolve_path(&env_repo)
        );
        assert_eq!(
            repo_root_from(Some("rel/r"), None, &t.0.join("m")),
            resolve_path(Path::new("rel/r"))
        );
    }

    #[test]
    fn repo_root_found_from_exe_ancestors() {
        let t = temp_dir("repo-root-exe");
        let repo = t.0.join("repo");
        mark_repo(&repo);
        let exe = repo.join("target").join("release").join("proxi.exe");
        let manifest = t.0.join("m").join("crates").join("proxi");

        assert_eq!(
            repo_root_from(None, Some(&exe), &manifest),
            resolve_path(&repo)
        );
        // JS `||`: an empty env value falls through.
        assert_eq!(
            repo_root_from(Some(""), Some(&exe), &manifest),
            resolve_path(&repo)
        );
    }

    #[test]
    fn repo_root_uses_nearest_marker() {
        let t = temp_dir("repo-root-nested");
        let outer = t.0.join("outer");
        let inner = outer.join("inner");
        mark_repo(&outer);
        mark_repo(&inner);
        let exe = inner.join("target").join("debug").join("proxi");

        assert_eq!(
            repo_root_from(None, Some(&exe), &t.0.join("m")),
            resolve_path(&inner)
        );
    }

    #[test]
    fn repo_root_ignores_marker_directory() {
        let t = temp_dir("repo-root-marker-dir");
        let repo = t.0.join("repo");
        fs::create_dir_all(repo.join("app").join("shared").join("identity.mjs"))
            .expect("create marker dir");
        let exe = repo.join("target").join("debug").join("proxi");
        let manifest = t.0.join("m").join("crates").join("proxi");

        assert_eq!(
            repo_root_from(None, Some(&exe), &manifest),
            resolve_path(&t.0.join("m"))
        );
    }

    #[test]
    fn repo_root_falls_back_to_manifest() {
        let t = temp_dir("repo-root-manifest");
        let manifest = t.0.join("m").join("crates").join("proxi");
        let exe = t.0.join("somewhere").join("bin").join("proxi");

        assert_eq!(
            repo_root_from(None, None, &manifest),
            resolve_path(&t.0.join("m"))
        );
        assert_eq!(
            repo_root_from(None, Some(&exe), &manifest),
            resolve_path(&t.0.join("m"))
        );
    }

    #[test]
    fn where_prints_source_and_creates_nothing() {
        let t = temp_dir("cli-where");
        let repo = t.0.join("repo");
        let data = t.0.join("data");
        let (code, out, err) = run(
            &["where"],
            &repo,
            Some(data.to_str().unwrap()),
            &t.0.join("home"),
        );

        assert_eq!(code, 0);
        assert_eq!(out, format!("{} (env)\n", resolve_path(&data).display()));
        assert!(err.is_empty());
        assert!(!data.exists());
    }

    #[test]
    fn help_and_no_args_print_usage() {
        let t = temp_dir("cli-help");
        for args in [&["help"][..], &[][..]] {
            let (code, out, err) = run(args, &t.0, None, &t.0);
            assert_eq!(code, 0, "{args:?}");
            assert_eq!(out, format!("{USAGE}\n"), "{args:?}");
            assert!(err.is_empty(), "{args:?}");
        }
    }

    #[test]
    fn unknown_command_prints_usage_to_stderr() {
        let t = temp_dir("cli-unknown");
        let (code, out, err) = run(&["nope"], &t.0, None, &t.0);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(err, format!("Unknown command: nope\n{USAGE}\n"));
    }

    #[test]
    fn unknown_long_option_is_reported() {
        let t = temp_dir("cli-long");
        let (code, out, err) = run(&["where", "--foo"], &t.0, None, &t.0);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(err, format!("Error: {}\n", unknown_option("--foo")));
        assert!(err.ends_with("as in '-- \"--foo\"\n"));
    }

    #[test]
    fn unknown_short_option_is_reported() {
        let t = temp_dir("cli-short");
        let (code, out, err) = run(&["-x"], &t.0, None, &t.0);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(err, format!("Error: {}\n", unknown_option("-x")));
    }

    #[test]
    fn unknown_long_option_with_value_reports_name_only() {
        let t = temp_dir("cli-long-value");
        let (code, out, err) = run(&["where", "--foo=bar"], &t.0, None, &t.0);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(err, format!("Error: {}\n", unknown_option("--foo")));
        assert!(!err.contains("bar"));
    }

    #[test]
    fn unknown_short_option_group_reports_first_char_only() {
        let t = temp_dir("cli-short-group");
        let (code, out, err) = run(&["-abc"], &t.0, None, &t.0);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(err, format!("Error: {}\n", unknown_option("-a")));
        assert!(!err.contains("-abc"));
        assert!(!err.contains("'-b'"));
    }

    #[test]
    fn options_after_double_dash_are_positional() {
        let t = temp_dir("cli-dd-unknown");
        let (code, out, err) = run(&["--", "-abc"], &t.0, None, &t.0);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(err, format!("Unknown command: -abc\n{USAGE}\n"));
    }

    #[test]
    fn double_dash_keeps_where_working() {
        let t = temp_dir("cli-dd-where");
        let home = t.0.join("home");
        let (code, out, err) = run(&["where", "--", "--zz"], &t.0.join("repo"), None, &home);
        assert_eq!(code, 0);
        assert_eq!(out, format!("{} (default)\n", default_data_root(&home).display()));
        assert!(err.is_empty());
    }

    #[test]
    fn dash_alone_is_a_positional() {
        let t = temp_dir("cli-dash");
        let (code, out, err) = run(&["-"], &t.0, None, &t.0);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(err, format!("Unknown command: -\n{USAGE}\n"));
    }
}
