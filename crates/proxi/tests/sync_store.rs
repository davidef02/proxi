//! Trash restore and conflict resolution over real files (port of `app/test/sync-store.test.ts`).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{TempDir, temp_dir};
use proxi::events::iso_from_millis;
use proxi::sync::store::{
    ConflictEntry, TrashEntry, list_conflicts, list_trash, resolve_conflict, restore_trash,
};

const NOW: i64 = 1_700_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;

fn put(root: &Path, rel: &str, body: &str) {
    let target = root.join(rel);
    fs::create_dir_all(target.parent().expect("seed file has a parent")).expect("create parents");
    fs::write(target, body).expect("write seed");
}

struct Fixture {
    tmp: TempDir,
    data: PathBuf,
    trash_dir: PathBuf,
    conflicts_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = temp_dir("sync-store");
        let data = tmp.0.join("data");
        fs::create_dir_all(&data).expect("create data folder");
        Self {
            data,
            trash_dir: tmp.0.join("trash"),
            conflicts_dir: tmp.0.join("conflicts"),
            tmp,
        }
    }

    fn read_data(&self, rel: &str) -> String {
        fs::read_to_string(self.data.join(rel)).expect("read data file")
    }
}

#[test]
fn list_trash_is_newest_first_and_restore_moves_a_file_back_and_refuses_to_overwrite() {
    let f = Fixture::new();
    put(
        &f.trash_dir,
        &format!("{}/courses/a/x.md", NOW - DAY),
        "restore me",
    );
    put(&f.trash_dir, &format!("{NOW}/graph/nodes.jsonl"), "{}");
    let list = list_trash(&f.trash_dir);
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

    restore_trash(
        &f.trash_dir,
        &f.data,
        &format!("{}/courses/a/x.md", NOW - DAY),
    )
    .expect("restore the trashed file");
    assert_eq!(f.read_data("courses/a/x.md"), "restore me");
    assert!(!f.trash_dir.join((NOW - DAY).to_string()).exists());
    assert!(f.trash_dir.exists());

    put(&f.trash_dir, &format!("{NOW}/courses/a/x.md"), "again");
    let err = restore_trash(&f.trash_dir, &f.data, &format!("{NOW}/courses/a/x.md"));
    assert_eq!(
        err,
        Err("Esiste già un file con questo nome: courses/a/x.md".to_string())
    );
    assert_eq!(f.read_data("courses/a/x.md"), "restore me");
}

#[test]
fn resolve_local_or_remote_writes_the_copy_back_only_when_it_is_the_chosen_side() {
    let f = Fixture::new();
    // copy = local side (locale): "local" keeps it, "remote" discards it
    put(&f.data, "courses/a/x.md", "current");
    put(
        &f.conflicts_dir,
        &format!("courses/a/x.md.locale-{NOW}"),
        "local copy",
    );
    resolve_conflict(
        &f.conflicts_dir,
        &f.data,
        &format!("courses/a/x.md.locale-{NOW}"),
        "local",
    )
    .expect("keep the local copy");
    assert_eq!(f.read_data("courses/a/x.md"), "local copy");
    assert!(!f.conflicts_dir.join("courses").exists());
    assert!(f.conflicts_dir.exists());

    put(&f.data, "courses/a/y.md", "current");
    put(
        &f.conflicts_dir,
        &format!("courses/a/y.md.locale-{NOW}"),
        "discard me",
    );
    resolve_conflict(
        &f.conflicts_dir,
        &f.data,
        &format!("courses/a/y.md.locale-{NOW}"),
        "remote",
    )
    .expect("discard the local copy");
    assert_eq!(f.read_data("courses/a/y.md"), "current");

    // copy = remote side (remoto): "remote" keeps it, "local" discards it
    put(&f.data, "decks/d.tsv", "current");
    put(
        &f.conflicts_dir,
        &format!("decks/d.tsv.remoto-{NOW}"),
        "remote copy",
    );
    resolve_conflict(
        &f.conflicts_dir,
        &f.data,
        &format!("decks/d.tsv.remoto-{NOW}"),
        "remote",
    )
    .expect("keep the remote copy");
    assert_eq!(f.read_data("decks/d.tsv"), "remote copy");

    put(&f.data, "decks/e.tsv", "current");
    put(
        &f.conflicts_dir,
        &format!("decks/e.tsv.remoto-{NOW}"),
        "discard me",
    );
    resolve_conflict(
        &f.conflicts_dir,
        &f.data,
        &format!("decks/e.tsv.remoto-{NOW}"),
        "local",
    )
    .expect("discard the remote copy");
    assert_eq!(f.read_data("decks/e.tsv"), "current");
    assert!(list_conflicts(&f.conflicts_dir).is_empty());
    assert!(f.conflicts_dir.exists());
}

#[test]
fn resolve_both_moves_the_copy_into_conflitti() {
    let f = Fixture::new();
    let id = format!("courses/a/x.md.remoto-{NOW}");
    put(&f.data, "courses/a/x.md", "current");
    put(&f.conflicts_dir, &id, "kept copy");
    resolve_conflict(&f.conflicts_dir, &f.data, &id, "both").expect("move the copy into conflitti");
    assert_eq!(
        fs::read_to_string(f.data.join("conflitti").join(&id)).expect("read kept copy"),
        "kept copy"
    );
    assert_eq!(f.read_data("courses/a/x.md"), "current");
    assert!(list_conflicts(&f.conflicts_dir).is_empty());
}

#[test]
fn traversal_ids_are_rejected_by_restore_and_resolve() {
    let f = Fixture::new();
    let ok_id = format!("ok.md.locale-{NOW}");
    put(&f.conflicts_dir, &ok_id, "x");
    for id in [
        format!("../ok.md.locale-{NOW}"),
        format!("courses/../../x.md.locale-{NOW}"),
    ] {
        assert_eq!(
            resolve_conflict(&f.conflicts_dir, &f.data, &id, "local"),
            Err("Conflitto non valido.".to_string()),
            "{id}"
        );
    }
    let err = resolve_conflict(&f.conflicts_dir, &f.data, &ok_id, "nope");
    assert_eq!(err, Err("Scelta non valida.".to_string()));
    for id in [
        format!("{NOW}/../../x"),
        format!("{NOW}/courses/../../x"),
        "../x".to_string(),
    ] {
        assert_eq!(
            restore_trash(&f.trash_dir, &f.data, &id),
            Err("Identificatore non valido.".to_string()),
            "{id}"
        );
    }
    assert!(!f.tmp.0.join("x").exists());
    assert!(f.conflicts_dir.join(&ok_id).exists());
}

#[test]
fn each_error_branch_has_its_node_message() {
    let f = Fixture::new();
    // restore: not a `<ms>/<rel>` id, then a rel outside SYNC_ROOTS, then an overwrite.
    assert_eq!(
        restore_trash(&f.trash_dir, &f.data, "12"),
        Err("Identificatore non valido.".to_string())
    );
    assert_eq!(
        restore_trash(&f.trash_dir, &f.data, &format!("{NOW}/notes/keep.txt")),
        Err("Identificatore non valido.".to_string())
    );
    // resolve: an unparsable id, then a known-side copy outside SYNC_ROOTS, then a missing copy.
    assert_eq!(
        resolve_conflict(&f.conflicts_dir, &f.data, "junk.txt", "local"),
        Err("Conflitto non valido.".to_string())
    );
    let outside = format!("notes/keep.txt.locale-{NOW}");
    put(&f.conflicts_dir, &outside, "x");
    assert_eq!(
        resolve_conflict(&f.conflicts_dir, &f.data, &outside, "local"),
        Err("Conflitto non valido.".to_string())
    );
    let missing = format!("courses/a/missing.md.locale-{NOW}");
    assert_eq!(
        resolve_conflict(&f.conflicts_dir, &f.data, &missing, "local"),
        Err("Conflitto non trovato.".to_string())
    );
}

#[test]
fn a_legacy_drive_copy_resolved_with_remote_writes_it_back() {
    let f = Fixture::new();
    let id = format!("curriculum.json.drive-{NOW}");
    put(&f.data, "curriculum.json", "current");
    put(
        &f.conflicts_dir,
        &format!("curriculum.json.remoto-{NOW}"),
        "newer",
    );
    assert_eq!(
        list_conflicts(&f.conflicts_dir)
            .iter()
            .map(|c| c.side.clone())
            .collect::<Vec<_>>(),
        ["remoto".to_string()]
    );
    put(&f.conflicts_dir, &id, "legacy newer");
    resolve_conflict(&f.conflicts_dir, &f.data, &id, "remote").expect("keep the legacy copy");
    assert_eq!(f.read_data("curriculum.json"), "legacy newer");
    // The older `remoto` copy of the same rel stays: only the chosen id is settled.
    assert!(
        f.conflicts_dir
            .join(format!("curriculum.json.remoto-{NOW}"))
            .exists()
    );
}

#[test]
fn an_unknown_choice_is_refused_before_touching_disk() {
    let f = Fixture::new();
    let id = format!("courses/a/x.md.locale-{NOW}");
    put(&f.conflicts_dir, &id, "copy");
    assert_eq!(
        resolve_conflict(&f.conflicts_dir, &f.data, &id, "mine"),
        Err("Scelta non valida.".to_string())
    );
    // The choice is validated before the id: even a bad id reports the choice.
    assert_eq!(
        resolve_conflict(&f.conflicts_dir, &f.data, "junk", ""),
        Err("Scelta non valida.".to_string())
    );
    assert_eq!(
        fs::read_to_string(f.conflicts_dir.join(&id)).expect("read copy"),
        "copy"
    );
    assert!(!f.data.join("courses").exists());
    assert!(!f.data.join("conflitti").exists());
}

#[test]
fn entries_serialise_in_node_key_order() {
    let conflict = ConflictEntry {
        id: format!("courses/a/x.md.locale-{NOW}"),
        rel: "courses/a/x.md".to_string(),
        side: "locale".to_string(),
        at: iso_from_millis(NOW),
    };
    assert_eq!(
        serde_json::to_string(&conflict).unwrap(),
        r#"{"id":"courses/a/x.md.locale-1700000000000","rel":"courses/a/x.md","side":"locale","at":"2023-11-14T22:13:20.000Z"}"#
    );
    let trash = TrashEntry {
        id: format!("{NOW}/courses/a/x.md"),
        rel: "courses/a/x.md".to_string(),
        deleted_at: iso_from_millis(NOW),
        size: 3,
    };
    assert_eq!(
        serde_json::to_string(&trash).unwrap(),
        r#"{"id":"1700000000000/courses/a/x.md","rel":"courses/a/x.md","deletedAt":"2023-11-14T22:13:20.000Z","size":3}"#
    );
}
