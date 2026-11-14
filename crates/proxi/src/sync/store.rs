//! Trash and conflict bookkeeping under `.learner/` (port of `app/server/sync/store.ts`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::data_root::resolve_path;
use crate::events::iso_from_millis;
use crate::sync::engine::{TMP, in_sync_set};

pub const TRASH_DAYS: i64 = 30;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// JS `\d` is ASCII-only.
fn is_digits(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

/// File paths below `dir`, forward-slash separated. Missing folders read as empty.
fn walk_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk_files_into(dir, "", &mut out);
    out
}

fn walk_files_into(dir: &Path, rel: &str, out: &mut Vec<String>) {
    let full = if rel.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rel)
    };
    let entries = match fs::read_dir(&full) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let child = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let meta = match fs::metadata(full.join(&name)) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk_files_into(dir, &child, out);
        } else {
            out.push(child);
        }
    }
}

/// Deletes trash folders (`<trashDir>/<ms>`) older than TRASH_DAYS. Non-numeric names are left alone.
pub fn purge_trash(trash_dir: &Path, now_ms: i64) {
    let entries = match fs::read_dir(trash_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let cutoff = now_ms as i128 - TRASH_DAYS as i128 * DAY_MS as i128;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_digits(&name) {
            continue;
        }
        // A name too large for u64 is in the future: keep it.
        let Ok(stamp) = name.parse::<u64>() else {
            continue;
        };
        if stamp as i128 >= cutoff {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
    }
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    pub id: String,
    pub rel: String,
    pub deleted_at: String,
    pub size: u64,
}

/// Trash files, newest first. The id is `<ms>/<rel>`: the folder the file was moved to.
pub fn list_trash(trash_dir: &Path) -> Vec<TrashEntry> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(trash_dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    let mut folders: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    folders.sort();
    for ms in folders {
        if !is_digits(&ms) {
            continue;
        }
        let Ok(stamp) = ms.parse::<i64>() else {
            continue;
        };
        let base = trash_dir.join(&ms);
        if !fs::metadata(&base).map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        for rel in walk_files(&base) {
            let size = fs::metadata(base.join(&rel)).map(|m| m.len()).unwrap_or(0);
            out.push(TrashEntry {
                id: format!("{ms}/{rel}"),
                rel,
                deleted_at: iso_from_millis(stamp),
                size,
            });
        }
    }
    out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at).then_with(|| a.id.cmp(&b.id)));
    out
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConflictEntry {
    pub id: String,
    pub rel: String,
    pub side: String,
    pub at: String,
}

/// `<rel>.<locale|remoto|drive>-<ms>`: a copy's last `-` ends the stamp, the side suffix precedes it.
fn parse_conflict(id: &str) -> Option<(String, String, i64)> {
    let (rest, digits) = id.rsplit_once('-')?;
    if !is_digits(digits) {
        return None;
    }
    let (rel, side) = rest.rsplit_once('.')?;
    if rel.is_empty() {
        return None;
    }
    let side = match side {
        "locale" => "locale",
        "remoto" | "drive" => "remoto",
        _ => return None,
    };
    let stamp = digits.parse::<i64>().ok()?;
    Some((rel.to_string(), side.to_string(), stamp))
}

/// Conflict copies, newest first. Legacy `drive` is read as `remoto`.
pub fn list_conflicts(conflicts_dir: &Path) -> Vec<ConflictEntry> {
    let mut out = Vec::new();
    for id in walk_files(conflicts_dir) {
        let Some((rel, side, stamp)) = parse_conflict(&id) else {
            continue;
        };
        out.push(ConflictEntry {
            id,
            rel,
            side,
            at: iso_from_millis(stamp),
        });
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Is `child` strictly inside `parent`? Both resolved first, like Node's `path.resolve`.
fn inside(parent: &Path, child: &Path) -> bool {
    let parent = resolve_path(parent);
    let child = resolve_path(child);
    child != parent && child.starts_with(&parent)
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::rename(from, to).is_err() {
        fs::copy(from, to)?;
        fs::remove_file(from)?;
    }
    Ok(())
}

/// Removes `dir` and its empty ancestors, stopping at (and never removing) `stop`.
fn prune_up(dir: &Path, stop: &Path) {
    let stop = resolve_path(stop);
    let mut dir = resolve_path(dir);
    while dir != stop && dir.starts_with(&stop) {
        if fs::remove_dir(&dir).is_err() {
            return;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => return,
        }
    }
}

/// `<ms>/<rel>`, the shape `list_trash` hands out.
fn parse_trash_id(id: &str) -> Option<&str> {
    let (stamp, rel) = id.split_once('/')?;
    if !is_digits(stamp) || rel.is_empty() {
        return None;
    }
    Some(rel)
}

/// Moves a trashed file back into dataRoot. Refuses to overwrite.
pub fn restore_trash(trash_dir: &Path, data_root: &Path, id: &str) -> Result<(), String> {
    let Some(rel) = parse_trash_id(id) else {
        return Err("Identificatore non valido.".to_string());
    };
    if !in_sync_set(rel) {
        return Err("Identificatore non valido.".to_string());
    }
    let src = trash_dir.join(id);
    if !inside(trash_dir, &src) {
        return Err("Identificatore non valido.".to_string());
    }
    let dst = data_root.join(rel);
    if dst.exists() {
        return Err(format!("Esiste già un file con questo nome: {rel}"));
    }
    move_file(&src, &dst).map_err(|e| e.to_string())?;
    if let Some(dir) = src.parent() {
        prune_up(dir, trash_dir);
    }
    Ok(())
}

/// Settles one conflict copy: `local`/`remote` keep the chosen side (the copy is written back into dataRoot when it
/// is that side, pushed by the next sync), `both` moves the copy to `<dataRoot>/conflitti/<id>`, outside SYNC_ROOTS.
pub fn resolve_conflict(
    conflicts_dir: &Path,
    data_root: &Path,
    id: &str,
    choice: &str,
) -> Result<(), String> {
    if choice != "local" && choice != "remote" && choice != "both" {
        return Err("Scelta non valida.".to_string());
    }
    let Some((rel, side, _)) = parse_conflict(id) else {
        return Err("Conflitto non valido.".to_string());
    };
    let copy = conflicts_dir.join(id);
    if !inside(conflicts_dir, &copy) {
        return Err("Conflitto non valido.".to_string());
    }
    if !in_sync_set(&rel) {
        return Err("Conflitto non valido.".to_string());
    }
    if !copy.exists() {
        return Err("Conflitto non trovato.".to_string());
    }
    if choice == "both" {
        move_file(&copy, &data_root.join("conflitti").join(id)).map_err(|e| e.to_string())?;
    } else if (choice == "local" && side == "locale") || (choice == "remote" && side == "remoto") {
        let dst = data_root.join(&rel);
        if let Some(dir) = dst.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        // `dst + TMP`: a sibling that a rename can put in place atomically.
        let mut os = dst.clone().into_os_string();
        os.push(TMP);
        let tmp = PathBuf::from(os);
        fs::copy(&copy, &tmp).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
        fs::remove_file(&copy).map_err(|e| e.to_string())?;
    } else {
        fs::remove_file(&copy).map_err(|e| e.to_string())?;
    }
    if let Some(dir) = copy.parent() {
        prune_up(dir, conflicts_dir);
    }
    Ok(())
}
