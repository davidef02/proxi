//! The diff/sync pass between the data folder and one remote provider (port of `app/server/sync/engine.ts`).
//!
//! Only files that changed since the manifest's last sync move. Change detection compares each side with the
//! manifest, never the two sides' mtimes: a stale git checkout must not overwrite newer work.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use indexmap::{IndexMap, IndexSet};

use crate::data_root::resolve_path;
use crate::protocol::PendingDeletions;
use crate::sync::types::{
    LocalStat, Manifest, ManifestEntry, ProviderError, RemoteStat, SyncProvider, SyncResult,
};

/// What is synced: the ordinary Proxi data (never `.learner/`, node_modules, builds).
pub const SYNC_ROOTS: [&str; 4] = ["courses", "graph", "decks", "curriculum.json"];
pub const SKIP_DIRS: [&str; 5] = ["node_modules", "dist", ".vite", "coverage", ".git"];
/// Retired files: deletions do not propagate, so without this a provider would hand them back forever.
pub const SKIP_FILES: [&str; 1] = ["graph/positions.json"];
pub const TMP: &str = ".ia-sync-tmp";
/// Above this share of the manifest, a pass that deletes stops and asks for confirmation.
const DELETE_GUARD: f64 = 0.3;

/// Guards remote names before they reach the disk.
pub fn in_sync_set(rel: &str) -> bool {
    if rel.is_empty() || rel.contains('\\') || rel.contains(':') {
        return false;
    }
    let segs: Vec<&str> = rel.split('/').collect();
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return false;
    }
    if segs[0] == "curriculum.json" {
        return segs.len() == 1;
    }
    if segs.len() == 1 || !SYNC_ROOTS.contains(&segs[0]) {
        return false;
    }
    if segs.iter().any(|s| SKIP_DIRS.contains(s)) {
        return false;
    }
    if SKIP_FILES.contains(&rel) {
        return false;
    }
    !rel.ends_with(TMP)
}

fn walk_local(root: &Path, rel: &str, out: &mut IndexMap<String, LocalStat>) {
    let full = root.join(rel);
    // fs::metadata follows links, like Node's stat.
    let meta = match fs::metadata(&full) {
        Ok(meta) => meta,
        Err(_) => return,
    };
    if meta.is_file() {
        if in_sync_set(rel) {
            out.insert(
                rel.to_string(),
                LocalStat {
                    size: meta.len(),
                    mtime_ms: file_mtime_ms(&meta),
                },
            );
        }
        return;
    }
    if !meta.is_dir() {
        return;
    }
    let mut names: Vec<String> = match fs::read_dir(&full) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => return,
    };
    // libuv's scandir returns names already sorted by bytes.
    names.sort();
    for name in names {
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        walk_local(root, &format!("{rel}/{name}"), out);
    }
}

/// Every syncable local file under dataRoot, keyed by forward-slash rel path, in walk order.
pub fn list_local(data_root: &Path) -> Vec<(String, LocalStat)> {
    let mut out = IndexMap::new();
    for root in SYNC_ROOTS {
        walk_local(data_root, root, &mut out);
    }
    out.into_iter().collect()
}

/// Node computes mtimeMs as `secs * 1000 + nsecs / 1e6`.
pub fn file_mtime_ms(meta: &fs::Metadata) -> f64 {
    match meta.modified() {
        Ok(time) => match time.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1e6,
            Err(e) => -(e.duration().as_secs_f64() * 1000.0),
        },
        Err(_) => 0.0,
    }
}

/// Manifest file for one provider target, under the code repo's `.learner/` (never synced).
pub fn manifest_path(repo_root: &Path, key: &str) -> PathBuf {
    let name: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    repo_root
        .join(".learner")
        .join("sync-manifests")
        .join(format!("{name}.json"))
}

/// Missing, invalid, for another provider/version/data folder: an empty manifest (a first sync).
pub fn read_manifest(file: &Path, key: &str, data_root: &Path) -> Manifest {
    let root = resolve_path(data_root).display().to_string();
    if let Ok(raw) = fs::read_to_string(file) {
        if let Ok(manifest) = serde_json::from_str::<Manifest>(&raw) {
            if manifest.v == 2
                && manifest.key == key
                && manifest.data_root.as_deref() == Some(root.as_str())
            {
                return manifest;
            }
        }
    }
    Manifest {
        v: 2,
        key: key.to_string(),
        data_root: Some(root),
        files: IndexMap::new(),
    }
}

/// Atomic write (tmp + rename), falling back to a direct write.
pub fn write_manifest(file: &Path, m: &Manifest) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(m).map_err(io::Error::other)?;
    let mut tmp = file.as_os_str().to_os_string();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    if fs::write(&tmp, &json).and_then(|_| fs::rename(&tmp, file)).is_ok() {
        return Ok(());
    }
    let _ = fs::remove_file(&tmp);
    fs::write(file, json)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

pub struct SyncPassOptions<'a> {
    pub data_root: &'a Path,
    pub provider: &'a mut dyn SyncProvider,
    pub manifest: &'a Manifest,
    pub conflicts_dir: &'a Path,
    pub trash_dir: &'a Path,
    pub confirm_deletions: bool,
    pub now: fn() -> i64,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Push,
    Pull,
    Drop,
    RemoveRemote,
    TrashLocal,
    Both,
}

/// A failed attempt: a moved remote is retried once, anything else ends the pass.
#[derive(Debug)]
enum PassError {
    Moved,
    Message(String),
}

impl From<ProviderError> for PassError {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::RemoteMoved => PassError::Moved,
            ProviderError::Other(message) => PassError::Message(message),
        }
    }
}

impl From<io::Error> for PassError {
    fn from(err: io::Error) -> Self {
        PassError::Message(err.to_string())
    }
}

/// Downloads one remote file into dataRoot, keeping the remote mtime, and records both sides in the manifest.
fn pull(
    provider: &mut dyn SyncProvider,
    data_root: &Path,
    m: &mut Manifest,
    rel: &str,
    rb: &RemoteStat,
) -> Result<(), PassError> {
    let data = provider.read(rel)?;
    let local = data_root.join(rel);
    if let Some(parent) = local.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = sibling(&local, TMP);
    fs::write(&tmp, &data)?;
    fs::rename(&tmp, &local)?;
    set_times_secs(&local, rb.mtime_ms / 1000.0)?;
    let meta = fs::metadata(&local)?;
    m.files.insert(
        rel.to_string(),
        ManifestEntry {
            l: LocalStat {
                size: meta.len(),
                mtime_ms: file_mtime_ms(&meta),
            },
            r: rb.clone(),
        },
    );
    Ok(())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

fn set_times_secs(path: &Path, secs: f64) -> io::Result<()> {
    if !secs.is_finite() || secs < 0.0 {
        return Ok(());
    }
    let time = UNIX_EPOCH + Duration::from_secs_f64(secs);
    let file = fs::OpenOptions::new().write(true).open(path)?;
    file.set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to)?;
    fs::remove_file(from)
}

/// Removes empty directories above `rel`, stopping before the top-level root (courses/, graph/, ...).
fn prune_empty_dirs(data_root: &Path, rel: &str) {
    let segs: Vec<&str> = rel.split('/').collect();
    if segs.len() < 3 {
        return;
    }
    for i in (1..=segs.len() - 2).rev() {
        if fs::remove_dir(data_root.join(segs[..=i].join("/"))).is_err() {
            return; // not empty (or gone): stop here
        }
    }
}

fn copy_to(from: &Path, to: &Path) -> Result<(), PassError> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to)?;
    Ok(())
}

fn write_conflict(file: &Path, data: &[u8]) -> Result<(), PassError> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(file, data)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_action(
    opts: &mut SyncPassOptions,
    m: &mut Manifest,
    rel: &str,
    action: Action,
    la: Option<LocalStat>,
    rb: Option<RemoteStat>,
    copied: &mut HashMap<String, String>,
    res: &mut SyncResult,
    removed: &mut u64,
    trashed: &mut u64,
) -> Result<(), PassError> {
    let local_path = opts.data_root.join(rel);
    match action {
        Action::Push => {
            let l = la.expect("push needs a local stat");
            let data = fs::read(&local_path)?;
            let r = opts.provider.write(rel, &data, l.mtime_ms)?;
            m.files.insert(rel.to_string(), ManifestEntry { l, r });
            res.pushed += 1;
        }
        Action::Pull => {
            let r = rb.as_ref().expect("pull needs a remote stat");
            pull(&mut *opts.provider, opts.data_root, m, rel, r)?;
            res.pulled += 1;
        }
        Action::Drop => {
            m.files.shift_remove(rel);
        }
        Action::RemoveRemote => {
            opts.provider.remove(rel)?;
            m.files.shift_remove(rel);
            *removed += 1;
            res.deleted += 1;
        }
        Action::TrashLocal => {
            let dest = opts.trash_dir.join((opts.now)().to_string()).join(rel);
            move_file(&local_path, &dest)?;
            prune_empty_dirs(opts.data_root, rel);
            m.files.shift_remove(rel);
            res.deleted += 1;
            *trashed += 1;
        }
        Action::Both => {
            let l = la.expect("both needs a local stat");
            let r = rb.as_ref().expect("both needs a remote stat");
            let here = fs::read(&local_path)?;
            let equal = match opts.provider.content_equals(&here, r) {
                Some(equal) => equal,
                None => here == opts.provider.read(rel)?,
            };
            if equal {
                m.files.insert(
                    rel.to_string(),
                    ManifestEntry {
                        l,
                        r: r.clone(),
                    },
                );
            } else {
                // Remote wins when the head commit is newer than the local edit (GitHub) or on a first meeting.
                // The loser is always kept in conflictsDir, so neither side loses bytes.
                let remote_wins = m.files.get(rel).is_none() || r.mtime_ms >= l.mtime_ms;
                let stamp = (opts.now)();
                if remote_wins {
                    let dest = opts
                        .conflicts_dir
                        .join(format!("{rel}.locale-{stamp}"));
                    copy_to(&local_path, &dest)?;
                    pull(&mut *opts.provider, opts.data_root, m, rel, r)?;
                    res.pulled += 1;
                    res.conflicts += 1;
                } else {
                    // A retry re-plans the same conflict: its copy from the earlier attempt already exists, so
                    // count it once.
                    if copied.get(rel) != Some(&r.token) {
                        let data = opts.provider.read(rel)?;
                        let dest = opts
                            .conflicts_dir
                            .join(format!("{rel}.remoto-{stamp}"));
                        write_conflict(&dest, &data)?;
                        copied.insert(rel.to_string(), r.token.clone());
                        res.conflicts += 1;
                    }
                    let new_r = opts.provider.write(rel, &here, l.mtime_ms)?;
                    m.files.insert(rel.to_string(), ManifestEntry { l, r: new_r });
                    res.pushed += 1;
                }
            }
        }
    }
    Ok(())
}

/// One pass against a fresh copy of the manifest. Mutates `res` as it goes so a retry keeps the counts.
/// `trashed` counts the local trashes of this attempt: they are already on disk, so a retry cannot see them again.
fn attempt_pass(
    opts: &mut SyncPassOptions,
    res: &mut SyncResult,
    copied: &mut HashMap<String, String>,
    trashed: &mut u64,
) -> Result<Manifest, PassError> {
    let mut m = opts.manifest.clone();
    let mut remote: IndexMap<String, RemoteStat> = IndexMap::new();
    for (rel, stat) in opts.provider.list()? {
        if in_sync_set(&rel) {
            remote.insert(rel, stat);
        }
    }
    let local: IndexMap<String, LocalStat> = list_local(opts.data_root).into_iter().collect();

    // Plan over the first-seen union of local, remote and known files.
    let mut order: IndexSet<String> = IndexSet::new();
    for rel in local.keys() {
        order.insert(rel.clone());
    }
    for rel in remote.keys() {
        order.insert(rel.clone());
    }
    for rel in m.files.keys() {
        order.insert(rel.clone());
    }

    let mut actions: Vec<(String, Action)> = Vec::new();
    for rel in &order {
        let la = local.get(rel);
        let rb = remote.get(rel);
        let e = m.files.get(rel);
        let l_changed = match la {
            Some(l) => !matches!(e, Some(e) if l.size == e.l.size && l.mtime_ms == e.l.mtime_ms),
            None => false,
        };
        let r_changed = match rb {
            Some(r) => !matches!(e, Some(e) if r.token == e.r.token),
            None => false,
        };
        let action = if la.is_none() && rb.is_none() {
            e.map(|_| Action::Drop)
        } else if la.is_none() {
            Some(if e.is_some() && !r_changed {
                Action::RemoveRemote
            } else {
                Action::Pull
            })
        } else if rb.is_none() {
            Some(if e.is_some() && !l_changed {
                Action::TrashLocal
            } else {
                Action::Push
            })
        } else if l_changed && r_changed {
            Some(Action::Both)
        } else if l_changed {
            Some(Action::Push)
        } else if r_changed {
            Some(Action::Pull)
        } else {
            None
        };
        if let Some(action) = action {
            actions.push((rel.clone(), action));
        }
    }

    // Guard: a pass that would delete a large share of the known files waits for confirmation.
    let deletions = actions
        .iter()
        .filter(|(_, a)| matches!(a, Action::RemoveRemote | Action::TrashLocal))
        .count();
    let total = m.files.len();
    if deletions > 0 && (deletions as f64) > DELETE_GUARD * total as f64 && !opts.confirm_deletions {
        *res = SyncResult {
            pending_deletions: Some(PendingDeletions {
                count: deletions as u64,
                total: total as u64,
            }),
            ..SyncResult::default()
        };
        return Ok(opts.manifest.clone());
    }

    // Apply, one file at a time: a failure is recorded and the pass goes on.
    let mut removed = 0u64;
    for (rel, action) in &actions {
        let la = local.get(rel).copied();
        let rb = remote.get(rel).cloned();
        match apply_action(
            &mut *opts,
            &mut m,
            rel,
            *action,
            la,
            rb,
            copied,
            &mut *res,
            &mut removed,
            trashed,
        ) {
            Ok(()) => {}
            // A moved remote aborts the pass so the caller retries it from a fresh listing.
            Err(PassError::Moved) => return Err(PassError::Moved),
            Err(PassError::Message(message)) => res.errors.push(format!("{rel}: {message}")),
        }
    }

    // Commit: one remote commit for every staged write and removal.
    if res.pushed + removed > 0 {
        let message = opts.message.clone().unwrap_or_else(|| {
            format!(
                "sync: {} sent, {} deleted ({})",
                res.pushed,
                removed,
                hostname()
            )
        });
        opts.provider.commit(&message)?;
    }
    Ok(m)
}

/// Runs one two-way pass. Retries once from a fresh manifest when the remote moved during the commit.
pub fn sync_pass(mut opts: SyncPassOptions) -> Result<(SyncResult, Manifest), String> {
    // Local side effects of a failed attempt are already on disk (pulled files, trashed files) and the retry
    // re-plans them as no-ops, so their counts are carried over. Remote removals were only staged: the retry
    // counts them again.
    let mut carry_conflicts = 0u64;
    let mut carry_errors: Vec<String> = Vec::new();
    let mut carry_pulled = 0u64;
    let mut carry_trashed = 0u64;
    let mut copied: HashMap<String, String> = HashMap::new();
    for attempt in 1..=2 {
        let mut res = SyncResult {
            pushed: 0,
            pulled: carry_pulled,
            deleted: carry_trashed,
            conflicts: carry_conflicts,
            errors: carry_errors.clone(),
            pending_deletions: None,
        };
        let mut trashed = 0u64;
        match attempt_pass(&mut opts, &mut res, &mut copied, &mut trashed) {
            Ok(manifest) => return Ok((res, manifest)),
            Err(PassError::Message(message)) => return Err(message),
            Err(PassError::Moved) => {
                if attempt == 2 {
                    return Err(
                        "Il remoto è cambiato durante la sincronizzazione: riprovo alla prossima."
                            .to_string(),
                    );
                }
                // Conflict copies from attempt 1 stay on disk, so their counts and errors must survive.
                carry_conflicts = res.conflicts;
                carry_errors = res.errors;
                carry_pulled = res.pulled;
                carry_trashed += trashed;
            }
        }
    }
    Err("unreachable".to_string())
}
