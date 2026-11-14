//! The /api/exercises data side: list, workspace, save, hints and runs (port of app/server/exercises.ts).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Map, Value};
use sha1::{Digest, Sha1};

use crate::js;

pub const LASTRUN_FILE: &str = ".app-lastrun.json";
/// `save_exercise_file` error when the file changed on disk since the version the editor loaded.
pub const CONFLICT: &str = "Il file è cambiato fuori dall'editor";
pub const HINTS_FILE: &str = "SUGGERIMENTI.json";
pub const INSTRUCTIONS_FILE: &str = "ISTRUZIONI.md";
pub const MAX_FILE: u64 = 200 * 1024;

const EXT: [&str; 6] = [".vue", ".js", ".ts", ".json", ".css", ".md"];

/// `NAME_SAFE`: a single safe path segment.
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `NAME_RE`: an exercise directory starts with two digits and a dash, or `zz-`.
pub fn is_exercise_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    name.starts_with("zz-")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_digit()
            && bytes[1].is_ascii_digit()
            && bytes[2] == b'-')
}

pub fn exercises_base(data_root: &Path, course: &str) -> PathBuf {
    data_root.join("courses").join(course).join("exercises")
}

/// Node `path.extname` on the last `/` segment.
fn extname(p: &str) -> &str {
    let name = match p.rfind('/') {
        Some(slash) => &p[slash + 1..],
        None => p,
    };
    if name == ".." {
        return "";
    }
    match name.rfind('.') {
        None => "",
        Some(0) => "",
        Some(dot) => &name[dot..],
    }
}

fn has_allowed_ext(name: &str) -> bool {
    EXT.contains(&extname(name).to_lowercase().as_str())
}

/// Validate a learner-exercise file path (relative, forward slashes, allowed extension).
pub fn bad_exercise_path(p: &str) -> Option<String> {
    if p.is_empty() || p.contains('\\') || p.contains('\0') {
        return Some(format!(
            "Il percorso \"{p}\" deve essere relativo, con barre dirette (/)"
        ));
    }
    let bytes = p.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if p.starts_with('/') || drive {
        return Some(format!(
            "Il percorso \"{p}\" deve essere relativo (niente percorsi assoluti o lettere di unità)"
        ));
    }
    if p.split('/')
        .any(|segment| segment == ".." || segment == "." || segment.is_empty())
    {
        return Some(format!(
            "Il percorso \"{p}\" non può contenere \"..\", \".\" o segmenti vuoti"
        ));
    }
    if !has_allowed_ext(p) {
        return Some(format!(
            "Il percorso \"{p}\": l'estensione deve essere una tra {}",
            EXT.join(" ")
        ));
    }
    None
}

/// `\.(test|spec)\.[^/]+$` case-insensitive: NTFS is case-insensitive, `Esercizio.TEST.ts` is a test.
pub fn is_test_file(rel: &str) -> bool {
    let bytes = rel.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] != b'.' {
            continue;
        }
        for marker in [b"test".as_slice(), b"spec".as_slice()] {
            let start = i + 1;
            let end = start + marker.len();
            if end >= bytes.len()
                || !bytes[start..end].eq_ignore_ascii_case(marker)
                || bytes[end] != b'.'
            {
                continue;
            }
            let tail = &rel[end + 1..];
            if !tail.is_empty() && !tail.contains('/') {
                return true;
            }
        }
    }
    false
}

fn is_hidden(rel: &str) -> bool {
    rel.eq_ignore_ascii_case(INSTRUCTIONS_FILE) || rel.eq_ignore_ascii_case(HINTS_FILE)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hints {
    pub hints: Vec<String>,
    pub final_: bool,
    pub revealed: f64,
}

pub fn read_hints(dir: &Path) -> Option<Hints> {
    let raw = fs::read(dir.join(HINTS_FILE)).ok()?;
    let value = js::parse(&String::from_utf8_lossy(&raw)).ok()?;
    let object = value.as_object()?;
    let hints = object
        .get("hints")?
        .as_array()?
        .iter()
        .map(|hint| js::js_string(Some(hint)))
        .collect::<Vec<_>>();
    let len = hints.len() as f64;
    let mut revealed = js::to_number(object.get("revealed"));
    if revealed.is_nan() {
        revealed = 0.0;
    }
    Some(Hints {
        hints,
        final_: js::truthy(object.get("final")),
        revealed: revealed.max(0.0).min(len),
    })
}

pub fn write_hints(dir: &Path, h: &Hints) -> io::Result<()> {
    let mut map = Map::new();
    map.insert(
        "hints".into(),
        Value::Array(h.hints.iter().map(|hint| Value::from(hint.as_str())).collect()),
    );
    map.insert("final".into(), Value::Bool(h.final_));
    map.insert("revealed".into(), js::number(h.revealed));
    let mut text =
        serde_json::to_string_pretty(&Value::Object(map)).map_err(io::Error::other)?;
    text.push('\n');
    fs::write(dir.join(HINTS_FILE), text)
}

/// Absolute dir of an existing exercise, or `None` (bad name, missing).
pub fn exercise_dir_of(data_root: &Path, course: &str, name: &str) -> Option<PathBuf> {
    if !safe_name(name) || matches!(name, "." | ".." | "node_modules") {
        return None;
    }
    let dir = exercises_base(data_root, course).join(name);
    dir.is_dir().then_some(dir)
}

/// Content hash of a file ("" when missing): the editor sends it back as baseVersion on save.
pub fn file_version(abs: &Path) -> String {
    let Ok(data) = fs::read(abs) else {
        return String::new();
    };
    let mut hasher = Sha1::new();
    hasher.update(&data);
    format!("{:x}", hasher.finalize())
}

fn walk(dir: &Path, rel: &str, depth: u32) -> io::Result<Vec<String>> {
    let mut entries = fs::read_dir(dir)?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| js::locale_compare(&a.0, &b.0));

    let mut out = Vec::new();
    for (name, entry) in entries {
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let r = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if depth < 3 {
                out.extend(walk(&entry.path(), &r, depth + 1)?);
            }
        } else if file_type.is_file() && has_allowed_ext(&name) && !is_hidden(&r) {
            out.push(r);
        }
    }
    Ok(out)
}

pub fn list_exercises(data_root: &Path, course: &str) -> io::Result<Vec<Value>> {
    let base = exercises_base(data_root, course);
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(&base)?
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| js::locale_compare(&a.0, &b.0));

    let mut out = Vec::new();
    for (name, entry) in entries {
        if !entry.file_type()?.is_dir() || !is_exercise_name(&name) {
            continue;
        }
        let dir = base.join(&name);

        let mut files = fs::read_dir(&dir)?
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|file| {
                let file_name = file.file_name().to_string_lossy().into_owned();
                let is_file = file.file_type().map(|t| t.is_file()).unwrap_or(false);
                (is_file
                    && !file_name.starts_with('.')
                    && !file_name.eq_ignore_ascii_case(HINTS_FILE))
                .then_some(file_name)
            })
            .collect::<Vec<_>>();
        // `.sort()`: UTF-16 code units, not locale order.
        files.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));

        let mut info = Map::new();
        info.insert("course".into(), Value::from(course));
        info.insert("name".into(), Value::from(name.as_str()));
        info.insert(
            "dir".into(),
            Value::from(format!("courses/{course}/exercises/{name}")),
        );
        info.insert(
            "files".into(),
            Value::Array(files.into_iter().map(Value::from).collect()),
        );

        if let Ok(raw) = fs::read(dir.join(LASTRUN_FILE)) {
            if let Ok(value) = js::parse(&String::from_utf8_lossy(&raw)) {
                if js::truthy(Some(&value)) {
                    info.insert("lastRun".into(), value);
                }
            }
        }

        if let Some(hints) = read_hints(&dir) {
            info.insert(
                "hints".into(),
                Value::Array(hints.hints.iter().map(|hint| Value::from(hint.as_str())).collect()),
            );
            info.insert("hintsRevealed".into(), js::number(hints.revealed));
            info.insert("final".into(), Value::Bool(hints.final_));
        }

        let instructions = dir.join(INSTRUCTIONS_FILE);
        if instructions.exists() {
            info.insert(
                "instructions".into(),
                Value::from(String::from_utf8_lossy(&fs::read(&instructions)?)),
            );
        }
        out.push(Value::Object(info));
    }
    Ok(out)
}

/// Files for the in-app editor: tests are readOnly, everything else is editable.
pub fn get_workspace(
    data_root: &Path,
    course: &str,
    name: &str,
) -> io::Result<Option<Value>> {
    let Some(dir) = exercise_dir_of(data_root, course, name) else {
        return Ok(None);
    };
    let Some(info) = list_exercises(data_root, course)?
        .into_iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
    else {
        return Ok(None);
    };

    let mut files = Vec::new();
    for rel in walk(&dir, "", 0)? {
        let abs = dir.join(&rel);
        if fs::metadata(&abs)?.len() > MAX_FILE {
            continue;
        }
        let mut file = Map::new();
        file.insert("path".into(), Value::from(rel.as_str()));
        file.insert(
            "content".into(),
            Value::from(String::from_utf8_lossy(&fs::read(&abs)?)),
        );
        file.insert("version".into(), Value::from(file_version(&abs)));
        file.insert("readOnly".into(), Value::Bool(is_test_file(&rel)));
        files.push(Value::Object(file));
    }

    let mut out = Map::new();
    out.insert("info".into(), info);
    out.insert("files".into(), Value::Array(files));
    Ok(Some(Value::Object(out)))
}

#[derive(Debug)]
pub enum SaveError {
    Invalid(String),
    Conflict,
    Io(io::Error),
}

/// Save one learner file. Editable files only, or new files with allowed extensions.
pub fn save_exercise_file(
    data_root: &Path,
    course: &str,
    name: &str,
    rel: &str,
    content: &str,
    base_version: Option<&str>,
) -> Result<(), SaveError> {
    let Some(dir) = exercise_dir_of(data_root, course, name) else {
        return Err(SaveError::Invalid("Esercizio sconosciuto".into()));
    };
    if let Some(message) = bad_exercise_path(rel) {
        return Err(SaveError::Invalid(message));
    }
    if rel
        .split('/')
        .any(|segment| segment == "node_modules" || segment.starts_with('.'))
    {
        return Err(SaveError::Invalid("Percorso non consentito".into()));
    }
    if is_test_file(rel) || is_hidden(rel) {
        return Err(SaveError::Invalid("File di sola lettura".into()));
    }
    if content.len() as u64 > MAX_FILE {
        return Err(SaveError::Invalid("File troppo grande".into()));
    }
    let target = dir.join(rel);
    if !target.starts_with(&dir) {
        return Err(SaveError::Invalid("Percorso non consentito".into()));
    }
    if let Some(base) = base_version {
        if file_version(&target) != base {
            return Err(SaveError::Conflict);
        }
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(SaveError::Io)?;
    }
    fs::write(&target, content).map_err(SaveError::Io)
}

/// Reveal the next hint (capped at hints.length); `None` when the exercise has none.
pub fn reveal_hint(data_root: &Path, course: &str, name: &str) -> io::Result<Option<Value>> {
    let Some(dir) = exercise_dir_of(data_root, course, name) else {
        return Ok(None);
    };
    let Some(hints) = read_hints(&dir) else {
        return Ok(None);
    };
    if hints.hints.is_empty() {
        return Ok(None);
    }
    if hints.revealed < hints.hints.len() as f64 {
        let next = Hints {
            revealed: hints.revealed + 1.0,
            ..hints
        };
        write_hints(&dir, &next)?;
    }
    Ok(list_exercises(data_root, course)?
        .into_iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name)))
}

/// `code "<abs dir>"`; `None` when the path contains a quote the shell would eat.
pub fn editor_command_line(abs_dir: &Path) -> Option<String> {
    let dir = abs_dir.to_string_lossy();
    if dir.contains('"') {
        return None;
    }
    Some(format!("code \"{dir}\""))
}

/// Open the exercise dir in VS Code: detached, through the shell, never awaited (tests avoid it).
pub fn open_in_editor(abs_dir: &Path) {
    let Some(line) = editor_command_line(abs_dir) else {
        return;
    };

    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        let mut cmd = std::process::Command::new("cmd.exe");
        cmd.raw_arg(format!("/d /s /c \"{line}\""))
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        cmd
    };
    #[cfg(unix)]
    let mut cmd = {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(&line).process_group(0);
        cmd
    };

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}
