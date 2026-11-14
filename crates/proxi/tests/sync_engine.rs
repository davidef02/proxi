mod common;

#[path = "common/mem_provider.rs"]
mod mem_provider;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use common::{TempDir, temp_dir};
use indexmap::IndexMap;
use mem_provider::MemProvider;
use proxi::data_root::resolve_path;
use proxi::events::iso_from_millis;
use proxi::protocol::PendingDeletions;
use proxi::sync::engine::{
    SyncPassOptions, file_mtime_ms, in_sync_set, manifest_path, read_manifest, sync_pass,
    write_manifest,
};
use proxi::sync::store::{list_conflicts, list_trash, purge_trash, TrashEntry};
use proxi::sync::types::{
    LocalStat, Manifest, ManifestEntry, ProviderError, RemoteStat, SyncProvider, SyncResult,
};

const KEY: &str = "mem:test";
const NOW: i64 = 1_700_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;

fn fixed_now() -> i64 {
    NOW
}

/// A distinct clock per call, for the retry tests: a duplicate conflict copy would get its own name.
static TICK: AtomicI64 = AtomicI64::new(0);

fn ticking_now() -> i64 {
    TICK.fetch_add(1, Ordering::SeqCst)
}

fn set_mtime(path: &Path, ms: i64) {
    let time = UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64);
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))
        .unwrap();
}

struct Setup {
    tmp: TempDir,
    data: PathBuf,
    conflicts_dir: PathBuf,
    trash_dir: PathBuf,
    mem: MemProvider,
    manifest: Manifest,
}

impl Setup {
    fn new(remote: &[(&str, &str, f64)]) -> Self {
        let tmp = temp_dir("sync-engine");
        let data = tmp.0.join("data");
        fs::create_dir_all(&data).unwrap();
        Self {
            data: data.clone(),
            conflicts_dir: tmp.0.join("conflicts"),
            trash_dir: tmp.0.join("trash"),
            mem: MemProvider::new(remote),
            manifest: Manifest {
                v: 2,
                key: KEY.to_string(),
                data_root: None,
                files: IndexMap::new(),
            },
            tmp,
        }
    }

    fn write(&self, rel: &str, body: &str, age_s: i64) {
        let path = self.data.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        set_mtime(&path, NOW - age_s * 1000);
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.data.join(rel)).unwrap()
    }
}

fn pass(setup: &mut Setup) -> SyncResult {
    pass_opts(setup, false, fixed_now).expect("sync pass")
}

fn pass_confirm(setup: &mut Setup) -> SyncResult {
    pass_opts(setup, true, fixed_now).expect("sync pass")
}

fn pass_opts(
    setup: &mut Setup,
    confirm_deletions: bool,
    now: fn() -> i64,
) -> Result<SyncResult, String> {
    let mut provider = setup.mem.clone();
    let opts = SyncPassOptions {
        data_root: &setup.data,
        provider: &mut provider,
        manifest: &setup.manifest,
        conflicts_dir: &setup.conflicts_dir,
        trash_dir: &setup.trash_dir,
        confirm_deletions,
        now,
        message: None,
    };
    let (result, manifest) = sync_pass(opts)?;
    setup.manifest = manifest;
    Ok(result)
}

fn pass_with_provider(
    setup: &mut Setup,
    provider: &mut dyn SyncProvider,
    now: fn() -> i64,
) -> Result<SyncResult, String> {
    let opts = SyncPassOptions {
        data_root: &setup.data,
        provider,
        manifest: &setup.manifest,
        conflicts_dir: &setup.conflicts_dir,
        trash_dir: &setup.trash_dir,
        confirm_deletions: false,
        now,
        message: None,
    };
    let (result, manifest) = sync_pass(opts)?;
    setup.manifest = manifest;
    Ok(result)
}

#[derive(Debug, PartialEq)]
struct Counts {
    pushed: u64,
    pulled: u64,
    deleted: u64,
    conflicts: u64,
    errors: Vec<String>,
}

fn counts(pushed: u64, pulled: u64, deleted: u64, conflicts: u64) -> Counts {
    Counts {
        pushed,
        pulled,
        deleted,
        conflicts,
        errors: Vec::new(),
    }
}

fn summary(result: &SyncResult) -> Counts {
    Counts {
        pushed: result.pushed,
        pulled: result.pulled,
        deleted: result.deleted,
        conflicts: result.conflicts,
        errors: result.errors.clone(),
    }
}

fn counters(mem: &MemProvider) -> (usize, usize, usize) {
    let state = mem.0.lock().unwrap();
    (state.reads, state.writes, state.commits)
}

fn reset_counters(mem: &MemProvider) {
    let mut state = mem.0.lock().unwrap();
    state.reads = 0;
    state.writes = 0;
    state.commits = 0;
}

fn set_remote(mem: &MemProvider, rel: &str, body: &str, mtime_ms: f64) {
    mem.0.lock()
        .unwrap()
        .store
        .insert(rel.to_string(), (body.as_bytes().to_vec(), mtime_ms));
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn put(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn entry() -> ManifestEntry {
    ManifestEntry {
        l: LocalStat {
            size: 1,
            mtime_ms: 1.0,
        },
        r: RemoteStat {
            size: 1,
            mtime_ms: 1.0,
            token: "t".to_string(),
        },
    }
}

/// `path.relative` for the two absolute paths of one drive.
fn relative_path(base: &Path, target: &Path) -> PathBuf {
    let b: Vec<_> = base.components().collect();
    let t: Vec<_> = target.components().collect();
    if b.first() != t.first() {
        return target.to_path_buf();
    }
    let mut i = 0;
    while i < b.len() && i < t.len() && b[i] == t[i] {
        i += 1;
    }
    let mut out = PathBuf::new();
    for _ in i..b.len() {
        out.push("..");
    }
    for component in &t[i..] {
        out.push(component.as_os_str());
    }
    out
}

/// Test double: the first commit fails with `RemoteMoved`, as another PC pushing mid-pass does.
struct MovedBeforeCommit {
    inner: MemProvider,
    moved: bool,
}

impl SyncProvider for MovedBeforeCommit {
    fn key(&self) -> String {
        self.inner.key()
    }

    fn list(&mut self) -> Result<Vec<(String, RemoteStat)>, ProviderError> {
        self.inner.list()
    }

    fn read(&mut self, rel: &str) -> Result<Vec<u8>, ProviderError> {
        self.inner.read(rel)
    }

    fn write(&mut self, rel: &str, data: &[u8], mtime_ms: f64) -> Result<RemoteStat, ProviderError> {
        self.inner.write(rel, data, mtime_ms)
    }

    fn remove(&mut self, rel: &str) -> Result<(), ProviderError> {
        self.inner.remove(rel)
    }

    fn commit(&mut self, message: &str) -> Result<(), ProviderError> {
        if self.moved {
            self.moved = false;
            return Err(ProviderError::RemoteMoved);
        }
        self.inner.commit(message)
    }

    fn content_equals(&self, data: &[u8], r: &RemoteStat) -> Option<bool> {
        self.inner.content_equals(data, r)
    }
}

#[test]
fn sync_pass_one_sided_moves_skips_node_modules_learner_and_positions_and_the_second_pass_does_nothing()
{
    let mut setup = Setup::new(&[
        ("graph/nodes.jsonl", "{}\n", (NOW - 5_000) as f64),
        ("graph/positions.json", "{}", NOW as f64),
    ]);
    setup.write("courses/a/lessons/0001-x.html", "L1", 0);
    setup.write("courses/a/exercises/node_modules/x.js", "nope", 0);
    setup.write(".learner/settings.json", "{}", 0);
    assert_eq!(summary(&pass(&mut setup)), counts(1, 1, 0, 0));
    assert_eq!(
        setup.mem.file("courses/a/lessons/0001-x.html"),
        Some("L1".to_string())
    );
    assert!(!setup.mem.has("courses/a/exercises/node_modules/x.js"));
    assert_eq!(setup.read("graph/nodes.jsonl"), "{}\n");
    assert!(!setup.data.join("graph/positions.json").exists());

    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 0, 0));
    assert_eq!(counters(&setup.mem), (0, 0, 0));
}

#[test]
fn sync_pass_first_meeting_with_a_fresh_clone_remote_wins_and_the_local_copy_is_kept_in_conflicts() {
    let mut setup = Setup::new(&[(
        "curriculum.json",
        "drive-newer-work",
        (NOW - 3_600_000) as f64,
    )]);
    setup.write("curriculum.json", "git-version", 0);
    let result = pass(&mut setup);
    assert_eq!(result.conflicts, 1);
    assert_eq!(result.pulled, 1);
    assert_eq!(setup.read("curriculum.json"), "drive-newer-work");
    let kept = entries(&setup.conflicts_dir);
    assert_eq!(kept.len(), 1);
    assert!(kept[0].starts_with("curriculum.json.locale-"), "{kept:?}");
    assert_eq!(
        fs::read_to_string(setup.conflicts_dir.join(&kept[0])).unwrap(),
        "git-version"
    );
}

#[test]
fn sync_pass_edits_seconds_apart_are_synced_and_only_the_changed_side_moves() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/a/x.md", "v1", 10);
    pass(&mut setup);
    setup.write("courses/a/x.md", "v2", 9); // same size, 1 s later
    assert_eq!(summary(&pass(&mut setup)), counts(1, 0, 0, 0));
    assert_eq!(setup.mem.file("courses/a/x.md"), Some("v2".to_string()));
    set_remote(&setup.mem, "courses/a/x.md", "v3-from-other-pc", (NOW - 100_000) as f64);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 1, 0, 0));
    assert_eq!(setup.read("courses/a/x.md"), "v3-from-other-pc");
}

#[test]
fn sync_pass_both_changed_newer_wins_loser_kept_and_a_failing_file_does_not_stop_the_pass() {
    let mut setup = Setup::new(&[]);
    setup.write("decks/d.tsv", "base", 100);
    setup.write("decks/e.tsv", "e", 100);
    pass(&mut setup);
    setup.write("decks/d.tsv", "local-edit", 50);
    set_remote(&setup.mem, "decks/d.tsv", "remote-edit-newer", (NOW - 10_000) as f64);
    setup.write("decks/e.tsv", "e2", 5);
    setup
        .mem
        .0
        .lock()
        .unwrap()
        .fail_write
        .insert("decks/e.tsv".to_string());
    let result = pass(&mut setup);
    assert_eq!(result.conflicts, 1);
    assert_eq!(result.errors, vec!["decks/e.tsv: boom".to_string()]);
    assert_eq!(setup.read("decks/d.tsv"), "remote-edit-newer");
    let kept = entries(&setup.conflicts_dir.join("decks"));
    assert_eq!(kept.len(), 1);
    assert_eq!(
        fs::read_to_string(setup.conflicts_dir.join("decks").join(&kept[0])).unwrap(),
        "local-edit"
    );
}

#[test]
fn sync_pass_a_local_delete_removes_the_remote_file_in_one_commit() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/a/x.md", "x", 0);
    setup.write("courses/a/y.md", "y", 0);
    for i in 0..4 {
        setup.write(&format!("courses/keep/k{i}.md"), "keep", 0); // 1 of 6 stays under the 30% guard
    }
    pass(&mut setup);
    fs::remove_file(setup.data.join("courses/a/x.md")).unwrap();
    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 1, 0));
    assert!(!setup.mem.has("courses/a/x.md"));
    assert_eq!(counters(&setup.mem), (0, 0, 1));
}

#[test]
fn sync_pass_a_remote_delete_moves_the_local_file_to_trash_and_prunes_its_empty_course_dir() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/gone/a.md", "bye", 0);
    for i in 0..4 {
        setup.write(&format!("courses/keep/k{i}.md"), "keep", 0); // 1 of 5 stays under the 30% guard
    }
    pass(&mut setup);
    setup.mem.0.lock().unwrap().store.shift_remove("courses/gone/a.md");
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 1, 0));
    assert_eq!(
        fs::read_to_string(
            setup
                .trash_dir
                .join(NOW.to_string())
                .join("courses/gone/a.md")
        )
        .unwrap(),
        "bye"
    );
    assert!(!setup.data.join("courses/gone").exists());
    assert!(setup.data.join("courses").exists());
    assert!(setup.data.join("courses/keep/k0.md").exists());
}

#[test]
fn sync_pass_edit_beats_delete_in_both_directions() {
    let mut setup = Setup::new(&[]);
    // local edit, remote delete -> the edit is pushed back
    setup.write("courses/e.md", "one", 100);
    pass(&mut setup);
    setup.write("courses/e.md", "two", 0);
    setup.mem.0.lock().unwrap().store.shift_remove("courses/e.md");
    assert_eq!(summary(&pass(&mut setup)), counts(1, 0, 0, 0));
    assert_eq!(setup.mem.file("courses/e.md"), Some("two".to_string()));

    // remote edit, local delete -> the remote copy is pulled back
    setup.write("courses/r.md", "one", 100);
    pass(&mut setup);
    set_remote(&setup.mem, "courses/r.md", "remote-two", NOW as f64);
    fs::remove_file(setup.data.join("courses/r.md")).unwrap();
    assert_eq!(summary(&pass(&mut setup)), counts(0, 1, 0, 0));
    assert_eq!(setup.read("courses/r.md"), "remote-two");
}

#[test]
fn sync_pass_deleting_over_30_percent_of_the_known_files_stops_for_confirmation_then_applies_it() {
    let mut setup = Setup::new(&[]);
    for n in 1..=4 {
        setup.write(&format!("courses/a/{n}.md"), &format!("f{n}"), 0);
    }
    pass(&mut setup);
    fs::remove_file(setup.data.join("courses/a/1.md")).unwrap();
    fs::remove_file(setup.data.join("courses/a/2.md")).unwrap();
    let before = serde_json::to_string(&setup.manifest).unwrap();
    reset_counters(&setup.mem);
    let held = pass(&mut setup);
    assert_eq!(
        held.pending_deletions,
        Some(PendingDeletions { count: 2, total: 4 })
    );
    assert_eq!(held.deleted, 0);
    assert_eq!(serde_json::to_string(&setup.manifest).unwrap(), before);
    assert_eq!(setup.mem.0.lock().unwrap().store.len(), 4);
    assert_eq!(counters(&setup.mem), (0, 0, 0));

    let applied = pass_confirm(&mut setup);
    assert_eq!(applied.pending_deletions, None);
    assert_eq!(applied.deleted, 2);
    let mut keys: Vec<String> = setup
        .mem
        .0
        .lock()
        .unwrap()
        .store
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(keys, ["courses/a/3.md", "courses/a/4.md"]);
}

#[test]
fn sync_pass_remote_moved_once_the_second_attempt_succeeds_and_reflects_the_moved_remote() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/n.md", "new", 0);
    setup.mem.0.lock().unwrap().moves_left = 1;
    let result = pass(&mut setup);
    assert_eq!(summary(&result), counts(1, 1, 0, 0));
    assert_eq!(setup.mem.file("courses/n.md"), Some("new".to_string()));
    assert_eq!(setup.read("curriculum.json"), "other pc");
}

#[test]
fn sync_pass_remote_moved_twice_errors_and_the_manifest_is_not_modified() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/n.md", "new", 0);
    let before = serde_json::to_string(&setup.manifest).unwrap();
    setup.mem.0.lock().unwrap().moves_left = 2;
    let error = pass_opts(&mut setup, false, fixed_now).unwrap_err();
    assert!(error.contains("riprovo alla prossima"), "{error}");
    assert_eq!(serde_json::to_string(&setup.manifest).unwrap(), before);
}

#[test]
fn sync_pass_remote_names_outside_the_sync_set_are_ignored() {
    let mut setup = Setup::new(&[
        ("courses/../../evil", "evil", NOW as f64),
        ("C:x", "drive", NOW as f64),
        (".learner/secrets.json", "{}", NOW as f64),
    ]);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 0, 0));
    assert_eq!(setup.mem.0.lock().unwrap().reads, 0);
    assert!(!setup.tmp.0.join("evil").exists());
}

#[test]
fn in_sync_set_accepts_only_the_synced_roots_and_rejects_traversal_and_skipped_names() {
    for ok in [
        "courses/a/x.md",
        "graph/nodes.jsonl",
        "decks/d.tsv",
        "curriculum.json",
    ] {
        assert!(in_sync_set(ok), "{ok}");
    }
    for bad in [
        "",
        "courses/../../evil",
        "courses//x",
        "./courses/x",
        "C:x",
        "courses\\x",
        ".learner/settings.json",
        "curriculum.json/x",
        "curriculum.json.bak",
        "courses",
        "courses/a/node_modules/x.js",
        "graph/positions.json",
        "courses/a/x.md.ia-sync-tmp",
        "viz/x.html",
    ] {
        assert!(!in_sync_set(bad), "{bad}");
    }
}

#[test]
fn sync_pass_n_pushes_and_d_removals_make_exactly_one_commit() {
    let mut setup = Setup::new(&[]);
    for i in 0..10 {
        setup.write(&format!("courses/a/f{i}.md"), &format!("f{i}"), 0);
    }
    pass(&mut setup);
    setup.write("courses/b/n1.md", "1", 0);
    setup.write("courses/b/n2.md", "2", 0);
    setup.write("courses/b/n3.md", "3", 0);
    fs::remove_file(setup.data.join("courses/a/f0.md")).unwrap();
    fs::remove_file(setup.data.join("courses/a/f1.md")).unwrap();
    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(3, 0, 2, 0));
    assert_eq!(counters(&setup.mem), (0, 3, 1));
}

#[test]
fn sync_pass_both_changed_local_newer_pushes_and_keeps_the_remote_bytes_as_a_remoto_copy() {
    let mut setup = Setup::new(&[]);
    setup.write("decks/d.tsv", "base", 1000);
    pass(&mut setup);
    set_remote(
        &setup.mem,
        "decks/d.tsv",
        "remote-edit-older",
        (NOW - 500_000) as f64,
    );
    setup.write("decks/d.tsv", "local-edit-newer", 0);
    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(1, 0, 0, 1));
    assert_eq!(
        setup.mem.file("decks/d.tsv"),
        Some("local-edit-newer".to_string())
    );
    assert_eq!(setup.read("decks/d.tsv"), "local-edit-newer");
    assert_eq!(counters(&setup.mem).2, 1);
    let kept = entries(&setup.conflicts_dir.join("decks"));
    assert_eq!(kept, [format!("d.tsv.remoto-{NOW}")]);
    assert_eq!(
        fs::read_to_string(setup.conflicts_dir.join("decks").join(&kept[0])).unwrap(),
        "remote-edit-older"
    );

    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 0, 0));
    assert_eq!(counters(&setup.mem), (0, 0, 0));
}

#[test]
fn sync_pass_a_remote_moved_retry_keeps_one_conflict_copy_and_counts_the_conflict_once() {
    let mut setup = Setup::new(&[]);
    setup.write("decks/d.tsv", "base", 1000);
    pass(&mut setup);
    set_remote(
        &setup.mem,
        "decks/d.tsv",
        "remote-edit-older",
        (NOW - 500_000) as f64,
    );
    setup.write("decks/d.tsv", "local-edit-newer", 0);
    TICK.store(NOW, Ordering::SeqCst);
    setup.mem.0.lock().unwrap().moves_left = 1;
    let result = pass_opts(&mut setup, false, ticking_now).expect("retried pass");
    assert_eq!(summary(&result), counts(1, 1, 0, 1));
    assert_eq!(
        setup.mem.file("decks/d.tsv"),
        Some("local-edit-newer".to_string())
    );
    let kept = entries(&setup.conflicts_dir.join("decks"));
    assert_eq!(kept.len(), 1);
    assert_eq!(
        fs::read_to_string(setup.conflicts_dir.join("decks").join(&kept[0])).unwrap(),
        "remote-edit-older"
    );
}

#[test]
fn sync_pass_both_changed_to_the_same_bytes_makes_no_conflict_no_transfer_and_a_no_op_next_pass() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/s.md", "base", 1000);
    pass(&mut setup);
    setup.write("courses/s.md", "same-edit", 0);
    set_remote(&setup.mem, "courses/s.md", "same-edit", NOW as f64);
    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 0, 0));
    assert_eq!(setup.mem.0.lock().unwrap().writes, 0);
    assert_eq!(setup.mem.0.lock().unwrap().commits, 0);
    assert!(!setup.conflicts_dir.exists());
    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 0, 0));
    assert_eq!(counters(&setup.mem), (0, 0, 0));
}

#[test]
fn sync_pass_remote_deletions_over_30_percent_also_stop_for_confirmation_and_keep_the_local_files() {
    let mut setup = Setup::new(&[]);
    for n in 1..=3 {
        setup.write(&format!("courses/a/{n}.md"), &format!("f{n}"), 0);
    }
    pass(&mut setup);
    setup.mem.0.lock().unwrap().store.shift_remove("courses/a/1.md");
    setup.mem.0.lock().unwrap().store.shift_remove("courses/a/2.md");
    let held = pass(&mut setup);
    assert_eq!(
        held.pending_deletions,
        Some(PendingDeletions { count: 2, total: 3 })
    );
    assert!(setup.data.join("courses/a/1.md").exists());
    assert!(!setup.trash_dir.exists());

    let applied = pass_confirm(&mut setup);
    assert_eq!(applied.deleted, 2);
    assert!(!setup.data.join("courses/a/1.md").exists());
    assert_eq!(
        fs::read_to_string(
            setup
                .trash_dir
                .join(NOW.to_string())
                .join("courses/a/1.md")
        )
        .unwrap(),
        "f1"
    );
    assert_eq!(setup.read("courses/a/3.md"), "f3");
}

#[test]
fn sync_pass_deletions_at_exactly_30_percent_pass_without_confirmation() {
    let mut setup = Setup::new(&[]);
    for i in 0..10 {
        setup.write(&format!("courses/a/{i}.md"), &format!("f{i}"), 0);
    }
    pass(&mut setup);
    for i in 0..3 {
        fs::remove_file(setup.data.join(format!("courses/a/{i}.md"))).unwrap();
    }
    let result = pass(&mut setup);
    assert_eq!(result.pending_deletions, None);
    assert_eq!(result.deleted, 3);
}

#[test]
fn sync_pass_a_file_deleted_on_both_sides_only_drops_its_manifest_entry() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/x.md", "x", 0);
    for i in 0..4 {
        setup.write(&format!("courses/k{i}.md"), "k", 0);
    }
    pass(&mut setup);
    fs::remove_file(setup.data.join("courses/x.md")).unwrap();
    setup.mem.0.lock().unwrap().store.shift_remove("courses/x.md");
    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 0, 0));
    assert!(setup.manifest.files.get("courses/x.md").is_none());
    assert_eq!(setup.mem.0.lock().unwrap().commits, 0);
    assert!(!setup.trash_dir.exists());
}

#[test]
fn sync_pass_a_pulled_file_keeps_the_remote_mtime() {
    let mtime_ms = 1_600_000_000_000.0;
    let mut setup = Setup::new(&[("courses/old.md", "old", mtime_ms)]);
    pass(&mut setup);
    let meta = fs::metadata(setup.data.join("courses/old.md")).unwrap();
    assert_eq!(
        (file_mtime_ms(&meta) / 1000.0).round(),
        mtime_ms / 1000.0
    );
}

#[test]
fn read_manifest_a_manifest_is_only_reused_for_the_data_folder_it_was_written_for() {
    let tmp = temp_dir("sync-manifest");
    let file = tmp.0.join("m.json");
    let a = tmp.0.join("a");
    let b = tmp.0.join("b");
    let mut files = IndexMap::new();
    files.insert("courses/x.md".to_string(), entry());
    write_manifest(
        &file,
        &Manifest {
            v: 2,
            key: KEY.to_string(),
            data_root: Some(a.display().to_string()),
            files,
        },
    )
    .unwrap();
    assert_eq!(read_manifest(&file, KEY, &a).files.len(), 1);
    let other = read_manifest(&file, KEY, &b);
    assert!(other.files.is_empty());
    assert_eq!(
        other.data_root,
        Some(resolve_path(&b).display().to_string())
    );
    assert!(read_manifest(&file, "mem:other", &a).files.is_empty());
}

#[test]
fn read_manifest_a_manifest_without_a_data_folder_is_not_reused() {
    let tmp = temp_dir("sync-manifest");
    let file = tmp.0.join("m.json");
    let a = tmp.0.join("a");
    fs::write(
        &file,
        r#"{"v":2,"key":"mem:test","files":{"courses/x.md":{"l":{"size":1,"mtimeMs":1},"r":{"size":1,"mtimeMs":1,"token":"t"}}}}"#,
    )
    .unwrap();
    let manifest = read_manifest(&file, KEY, &a);
    assert!(manifest.files.is_empty());
    assert_eq!(
        manifest.data_root,
        Some(resolve_path(&a).display().to_string())
    );
    // a relative path and its absolute form are the same folder
    let mut with_file = manifest.clone();
    with_file.files.insert("courses/y.md".to_string(), entry());
    write_manifest(&file, &with_file).unwrap();
    let relative = relative_path(&std::env::current_dir().unwrap(), &a);
    assert_eq!(read_manifest(&file, KEY, &relative).files.len(), 1);
}

#[test]
fn sync_pass_a_remote_moved_retry_keeps_the_pulled_and_trashed_counts_of_the_failed_attempt() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/gone/a.md", "bye", 0);
    for i in 0..4 {
        setup.write(&format!("courses/keep/k{i}.md"), "keep", 0); // 1 of 5 stays under the 30% guard
    }
    pass(&mut setup);
    // Attempt 1 pulls a new remote file and trashes a remotely deleted one, then the commit finds the ref moved.
    setup.mem.0.lock().unwrap().store.shift_remove("courses/gone/a.md");
    set_remote(&setup.mem, "courses/new.md", "from pc2", (NOW - 5_000) as f64);
    setup.write("courses/keep/push.md", "push me", 0); // forces a commit, so the move hits this pass
    let mut provider = MovedBeforeCommit {
        inner: setup.mem.clone(),
        moved: true,
    };
    let result = pass_with_provider(&mut setup, &mut provider, fixed_now).expect("retried pass");
    assert_eq!(summary(&result), counts(1, 1, 1, 0));
    assert_eq!(setup.read("courses/new.md"), "from pc2");
    assert!(!setup.data.join("courses/gone/a.md").exists());
    assert_eq!(
        setup.mem.file("courses/keep/push.md"),
        Some("push me".to_string())
    );
}

#[test]
fn sync_pass_a_remote_moved_retry_counts_a_staged_remote_removal_once_and_adds_its_own_pulls() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/drop/d.md", "drop me", 0);
    for i in 0..4 {
        setup.write(&format!("courses/keep/k{i}.md"), "keep", 0); // 1 of 5 stays under the 30% guard
    }
    pass(&mut setup);
    // Attempt 1: pulls one new remote file and stages the removal of a file deleted here. The commit then finds
    // the ref moved (the hook adds curriculum.json), so attempt 2 re-plans: the pulled file is a no-op, the
    // removal is staged again, and curriculum.json is a new pull.
    fs::remove_file(setup.data.join("courses/drop/d.md")).unwrap();
    set_remote(&setup.mem, "courses/new.md", "from pc2", (NOW - 5_000) as f64);
    setup.mem.0.lock().unwrap().moves_left = 1;
    let commits_before = counters(&setup.mem).2;
    let result = pass(&mut setup);
    assert_eq!(summary(&result), counts(0, 2, 1, 0));
    assert!(
        !setup.mem.has("courses/drop/d.md"),
        "the retry's commit carries the removal"
    );
    assert_eq!(setup.read("courses/new.md"), "from pc2");
    assert_eq!(setup.read("curriculum.json"), "other pc");
    assert_eq!(counters(&setup.mem).2, commits_before + 1, "one commit lands for the whole pass");
}

#[test]
fn sync_pass_a_remote_wins_conflict_pulled_by_attempt_one_still_counts_as_pulled_after_the_retry() {
    let mut setup = Setup::new(&[]);
    setup.write("decks/d.tsv", "base", 1000);
    pass(&mut setup);
    set_remote(
        &setup.mem,
        "decks/d.tsv",
        "remote-edit-newer",
        (NOW + 500_000) as f64,
    );
    setup.write("decks/d.tsv", "local-edit-older", 0);
    setup.write("decks/other.tsv", "push me", 0); // forces a commit, so the move hits this pass
    setup.mem.0.lock().unwrap().moves_left = 1;
    let result = pass(&mut setup);
    // d.tsv (attempt 1) + curriculum.json (attempt 2); other.tsv is pushed once, by the commit that lands.
    assert_eq!(summary(&result), counts(1, 2, 0, 1));
}

#[test]
fn sync_pass_a_remote_moved_retry_after_a_remote_wins_conflict_counts_it_once_and_keeps_the_local_copy() {
    let mut setup = Setup::new(&[]);
    setup.write("decks/d.tsv", "base", 1000);
    pass(&mut setup);
    set_remote(
        &setup.mem,
        "decks/d.tsv",
        "remote-edit-newer",
        (NOW + 500_000) as f64,
    );
    setup.write("decks/d.tsv", "local-edit-older", 0);
    setup.write("decks/other.tsv", "push me", 0); // forces a commit, so the move hits this pass
    setup.mem.0.lock().unwrap().moves_left = 1;
    let result = pass(&mut setup);
    assert_eq!(result.conflicts, 1);
    assert_eq!(result.errors, Vec::<String>::new());
    assert_eq!(setup.read("decks/d.tsv"), "remote-edit-newer");
    assert_eq!(
        setup.mem.file("decks/other.tsv"),
        Some("push me".to_string())
    );
    assert_eq!(
        setup.read("curriculum.json"),
        "other pc",
        "the retry picks up the other PC's push"
    );
    let kept = entries(&setup.conflicts_dir.join("decks"));
    assert!(kept.iter().any(|f| fs::read_to_string(
        setup.conflicts_dir.join("decks").join(f)
    )
    .unwrap()
        == "local-edit-older"));
}

#[test]
fn purge_trash_keeps_trash_under_30_days_and_drops_older_ignoring_other_names() {
    let tmp = temp_dir("sync-store");
    let trash = tmp.0.join("trash");
    put(
        &trash,
        &format!("{}/courses/a/old.md", NOW - 31 * DAY),
        "old",
    );
    put(
        &trash,
        &format!("{}/courses/a/new.md", NOW - DAY),
        "new",
    );
    put(&trash, "notes/keep.txt", "keep");
    purge_trash(&trash, NOW);
    assert!(!trash.join((NOW - 31 * DAY).to_string()).exists());
    assert!(
        trash
            .join((NOW - DAY).to_string())
            .join("courses/a/new.md")
            .exists()
    );
    assert!(trash.join("notes/keep.txt").exists());
}

#[test]
fn list_conflicts_parses_locale_remoto_and_legacy_drive_copies() {
    let tmp = temp_dir("sync-store");
    let conflicts = tmp.0.join("conflicts");
    put(&conflicts, &format!("courses/a/x.md.locale-{NOW}"), "L");
    put(
        &conflicts,
        &format!("decks/d.tsv.remoto-{}", NOW + 1000),
        "R",
    );
    put(
        &conflicts,
        &format!("curriculum.json.drive-{}", NOW + 2000),
        "D",
    );
    put(&conflicts, "junk.txt", "ignored");
    let list = list_conflicts(&conflicts);
    let got: Vec<(String, String)> = list
        .iter()
        .map(|c| (c.rel.clone(), c.side.clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("curriculum.json".to_string(), "remoto".to_string()),
            ("decks/d.tsv".to_string(), "remoto".to_string()),
            ("courses/a/x.md".to_string(), "locale".to_string()),
        ]
    );
    assert_eq!(list[0].at, iso_from_millis(NOW + 2000));
}

#[test]
fn list_trash_lists_newest_first() {
    let tmp = temp_dir("sync-store");
    let trash = tmp.0.join("trash");
    put(
        &trash,
        &format!("{}/courses/a/x.md", NOW - DAY),
        "restore me",
    );
    put(&trash, &format!("{NOW}/graph/nodes.jsonl"), "{}");
    let list = list_trash(&trash);
    assert_eq!(
        list.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
        [
            format!("{NOW}/graph/nodes.jsonl"),
            format!("{}/courses/a/x.md", NOW - DAY),
        ]
    );
    assert_eq!(
        list[1],
        TrashEntry {
            id: format!("{}/courses/a/x.md", NOW - DAY),
            rel: "courses/a/x.md".to_string(),
            deleted_at: iso_from_millis(NOW - DAY),
            size: "restore me".len() as u64,
        }
    );
}

#[test]
fn a_manifest_written_with_integral_mtimes_is_reused_and_the_second_pass_moves_nothing() {
    let mut setup = Setup::new(&[]);
    setup.write("courses/a/x.md", "v1", 10);
    let file = manifest_path(&setup.tmp.0, KEY);
    // The service starts from the manifest read for this data folder, so it carries `dataRoot`.
    setup.manifest = read_manifest(&file, KEY, &setup.data);
    assert_eq!(summary(&pass(&mut setup)), counts(1, 0, 0, 0));
    write_manifest(&file, &setup.manifest).unwrap();
    let raw = fs::read_to_string(&file).unwrap();
    assert!(raw.contains(r#""mtimeMs":1699999990000"#), "{raw}");
    assert!(!raw.contains(".0"), "{raw}");
    let reread = read_manifest(&file, KEY, &setup.data);
    assert_eq!(reread, setup.manifest);
    setup.manifest = reread;
    reset_counters(&setup.mem);
    assert_eq!(summary(&pass(&mut setup)), counts(0, 0, 0, 0));
    assert_eq!(counters(&setup.mem), (0, 0, 0));
}
