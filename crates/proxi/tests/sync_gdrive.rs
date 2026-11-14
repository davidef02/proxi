//! The Google Drive provider and PKCE flow against a stateful fake (port of
//! `app/test/sync-gdrive.test.ts`).

#[path = "common/fake_http.rs"]
mod fake_http;
#[path = "common/fake_drive.rs"]
mod fake_drive;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fake_drive::{FakeDrive, FakeDriveOpts};
use fake_http::{FakeHttp, Recorded, Reply};
use indexmap::IndexMap;
use serde_json::{Value, json};

use proxi::sync::engine::{SyncPassOptions, now_ms, sync_pass};
use proxi::sync::gdrive::{
    Clock, DRIVE_APP_PROPERTY, DRIVE_FOLDER, FOLDER_MIME, GdriveProvider, GoogleAuth, GoogleDeps,
    GoogleEndpoints, NO_REFRESH, REVOKED, auth_url, challenge_for, drive_email, ensure_folder,
    exchange_code, md5_hex, pkce_pair, revoke_token,
};
use proxi::sync::types::{Manifest, ProviderError, RemoteStat, SyncProvider};

fn drive_deps(d: &FakeDrive) -> GoogleDeps {
    GoogleDeps::new(d.endpoints())
}

fn clock_ms(ms: i64) -> Clock {
    Arc::new(move || ms)
}

fn form(recorded: &Recorded) -> BTreeMap<String, String> {
    form_urlencoded::parse(&recorded.bytes)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn count(d: &FakeDrive, method: &str, needle: &str) -> usize {
    d.requests()
        .iter()
        .filter(|request| request.method == method && request.path.contains(needle))
        .count()
}

/// The JSON metadata and the bytes of a `multipart/related` upload.
fn upload_parts(recorded: &Recorded) -> (Value, Vec<u8>) {
    let boundary = recorded
        .header("content-type")
        .and_then(|value| value.split("boundary=").nth(1))
        .expect("multipart boundary")
        .trim()
        .to_string();
    let parts = fake_drive::multipart_parts(&recorded.bytes, &boundary);
    let meta = serde_json::from_slice(parts[0]).expect("upload metadata");
    (meta, parts[1].to_vec())
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("proxi-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path).expect("create temp dir");
    TempDir(path)
}

#[test]
fn pkce_pair_is_64_url_safe_chars_and_the_challenge_is_its_sha256() {
    let pair = pkce_pair();
    assert_eq!(pair.verifier.len(), 64);
    assert!(
        pair.verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{}",
        pair.verifier
    );
    assert_eq!(pair.challenge, challenge_for(&pair.verifier));
    // A known SHA-256 vector, not recomputed with the code under test.
    assert_eq!(
        challenge_for("abc"),
        "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0"
    );
    assert_ne!(pkce_pair().verifier, pair.verifier);
}

#[test]
fn auth_url_carries_pkce_offline_and_consent() {
    let url = auth_url(
        &GoogleEndpoints::default(),
        "cid",
        "http://127.0.0.1:5555/oauth/google/callback",
        "CH",
        "ST",
    )
    .unwrap();
    let (base, query) = url.split_once('?').expect("query string");
    assert_eq!(base, "https://accounts.google.com/o/oauth2/v2/auth");
    assert_eq!(
        query,
        "client_id=cid\
         &redirect_uri=http%3A%2F%2F127.0.0.1%3A5555%2Foauth%2Fgoogle%2Fcallback\
         &response_type=code\
         &scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file\
         &code_challenge=CH\
         &code_challenge_method=S256\
         &state=ST\
         &access_type=offline\
         &prompt=consent"
    );
}

#[test]
fn an_empty_client_id_is_refused_with_the_italian_message() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let deps = drive_deps(&d);
    let mut auth = GoogleAuth::new("", "", "rt-1");
    let messages = [
        auth_url(
            &GoogleEndpoints::default(),
            "",
            "http://127.0.0.1/x",
            "c",
            "s",
        )
        .unwrap_err(),
        exchange_code(&deps, "", "", "C", "V", "http://127.0.0.1/cb").unwrap_err(),
        auth.access_token(&deps, false).unwrap_err(),
    ];
    for message in messages {
        assert!(
            message.contains("Client Google non configurato"),
            "{message}"
        );
        assert!(
            message.contains("GOOGLE_CLIENT_ID in crates/proxi/src/sync/gdrive.rs"),
            "{message}"
        );
        assert!(
            message.contains("googleClientId in .learner/settings.json"),
            "{message}"
        );
        assert!(!message.contains("nelle impostazioni"), "{message}");
    }
}

#[test]
fn exchange_code_sends_the_verifier_and_omits_an_empty_secret() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let deps = GoogleDeps::with_clock(d.endpoints(), clock_ms(1_000_000));
    let tokens = exchange_code(
        &deps,
        "cid",
        "",
        "CODE",
        "VER",
        "http://127.0.0.1/cb",
    )
    .unwrap();
    assert_eq!(tokens.refresh_token, "rt-1");
    assert_eq!(tokens.expires_at, 1_000_000.0 + (3600.0 - 60.0) * 1000.0);

    let request = &d.requests()[0];
    let form = form(request);
    assert_eq!(form.get("code_verifier").map(String::as_str), Some("VER"));
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert_eq!(form.get("client_id").map(String::as_str), Some("cid"));
    assert_eq!(
        form.get("redirect_uri").map(String::as_str),
        Some("http://127.0.0.1/cb")
    );
    assert!(!form.contains_key("client_secret"));
}

#[test]
fn exchange_code_includes_a_set_client_secret() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let deps = drive_deps(&d);
    exchange_code(
        &deps,
        "cid",
        "sec",
        "C",
        "V",
        "http://127.0.0.1/cb",
    )
    .unwrap();
    assert_eq!(
        form(&d.requests()[0])
            .get("client_secret")
            .map(String::as_str),
        Some("sec")
    );
}

#[test]
fn exchange_code_without_refresh_token_fails() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    d.set_omit_refresh(true);
    let deps = drive_deps(&d);
    assert_eq!(
        exchange_code(
            &deps,
            "cid",
            "",
            "C",
            "V",
            "http://127.0.0.1/cb"
        )
        .unwrap_err(),
        NO_REFRESH
    );
}

#[test]
fn google_auth_caches_until_expiry_then_refreshes() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let ticks = Arc::new(AtomicI64::new(0));
    let deps = {
        let ticks = Arc::clone(&ticks);
        GoogleDeps::with_clock(
            d.endpoints(),
            Arc::new(move || ticks.load(Ordering::SeqCst)),
        )
    };
    let mut auth = GoogleAuth::new("cid", "", "rt-1");
    let first = auth.access_token(&deps, false).unwrap();
    assert_eq!(auth.access_token(&deps, false).unwrap(), first);
    assert_eq!(d.refresh_grants(), 1);

    ticks.store(3600 * 1000, Ordering::SeqCst); // past (expires_in - 60) s
    assert_ne!(auth.access_token(&deps, false).unwrap(), first);
    assert_eq!(d.refresh_grants(), 2);
}

#[test]
fn invalid_grant_maps_to_the_reconnect_message() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let deps = drive_deps(&d);
    let mut auth = GoogleAuth::new("cid", "", "revoked");
    assert_eq!(auth.access_token(&deps, false).unwrap_err(), REVOKED);
}

#[test]
fn drive_email_returns_the_address_and_none_on_error() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let deps = drive_deps(&d);
    let mut auth = GoogleAuth::new("cid", "", "rt-1");
    assert_eq!(
        drive_email(&deps, &mut auth).as_deref(),
        Some("davide@example.com")
    );
    d.set_about_fails(true);
    assert_eq!(drive_email(&deps, &mut auth), None);
}

#[test]
fn revoke_token_never_fails() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    revoke_token(&drive_deps(&d), "rt-1");
    assert_eq!(d.requests().last().unwrap().path, "/revoke");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind closed port");
    let addr = listener.local_addr().expect("closed port address");
    drop(listener);
    let deps = GoogleDeps::new(GoogleEndpoints::from_override(Some(&format!(
        "http://{addr}"
    ))));
    revoke_token(&deps, "rt-1");
}

#[test]
fn ensure_folder_finds_the_app_folder_else_creates_it() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let deps = drive_deps(&d);
    let mut auth = GoogleAuth::new("cid", "", "rt-1");
    let created = ensure_folder(&deps, &mut auth).unwrap();
    let create = d
        .requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path == "/drive/v3/files")
        .expect("creates the folder");
    let meta = create.json();
    assert_eq!(meta["name"], DRIVE_FOLDER);
    assert_eq!(meta["mimeType"], FOLDER_MIME);
    assert_eq!(meta["appProperties"], json!({ "proxi": "data" }));
    assert_eq!(
        d.file(&created)
            .unwrap()
            .app_properties
            .get("proxi")
            .map(String::as_str),
        Some("data")
    );

    let again = ensure_folder(&deps, &mut auth).unwrap();
    assert_eq!(again, created);
    assert_eq!(count(&d, "POST", "/drive/v3/files"), 1);

    let d2 = FakeDrive::start(FakeDriveOpts::default());
    let existing = d2.add_folder(DRIVE_FOLDER, Some("data"));
    let mut auth2 = GoogleAuth::new("cid", "", "rt-1");
    assert_eq!(ensure_folder(&drive_deps(&d2), &mut auth2).unwrap(), existing);
    assert_eq!(
        d2.requests()
            .iter()
            .filter(|request| request.method == "POST" && request.path.starts_with("/drive/v3/"))
            .count(),
        0
    );
}

#[test]
fn the_created_folder_is_proxi_data_with_app_property_proxi() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let deps = drive_deps(&d);
    let mut auth = GoogleAuth::new("cid", "", "rt-1");
    ensure_folder(&deps, &mut auth).unwrap();
    assert_eq!(DRIVE_FOLDER, "proxi-data");
    assert_eq!(DRIVE_APP_PROPERTY, "proxi");

    let create = d
        .requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path == "/drive/v3/files")
        .expect("create request");
    let meta = create.json();
    assert_eq!(meta["name"], "proxi-data");
    assert_eq!(meta["appProperties"], json!({ "proxi": "data" }));

    let lookup = d
        .requests()
        .into_iter()
        .find(|request| request.method == "GET" && request.path.contains("/drive/v3/files?q="))
        .expect("lookup request");
    let q = form_urlencoded::parse(lookup.path.split_once('?').unwrap().1.as_bytes())
        .find(|(key, _)| key == "q")
        .map(|(_, value)| value.into_owned())
        .unwrap();
    assert!(q.contains("key='proxi'"), "{q}");
}

#[test]
fn a_401_refreshes_the_access_token_once_and_retries() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let folder = d.add_folder("app", None);
    d.add_file(
        "a.txt",
        &[folder.as_str()],
        b"A",
        "2026-01-01T00:00:00.000Z",
    );
    let deps = drive_deps(&d);
    let mut auth = GoogleAuth::new("cid", "", "rt-1");
    auth.access_token(&deps, false).unwrap();
    d.invalidate_access();

    let mut provider = GdriveProvider::new(deps, auth, &folder);
    let remote = provider.list().unwrap();
    let (_, stat) = remote
        .iter()
        .find(|(name, _)| name == "a.txt")
        .expect("a.txt listed");
    assert_eq!(stat.size, 1);
    assert_eq!(count(&d, "GET", "/drive/v3/files?q="), 2);
    assert_eq!(d.refresh_grants(), 2);
}

#[test]
fn list_paginates_skips_folders_and_native_docs_and_keeps_the_newest_duplicate() {
    let d = FakeDrive::start(FakeDriveOpts {
        page_size: 2,
        ..Default::default()
    });
    let folder = d.add_folder("app", None);
    d.add_folder_in("sub", &[folder.as_str()], None);
    d.add_native_doc("notes", &[folder.as_str()]);
    d.add_file(
        "a.json",
        &[folder.as_str()],
        b"aaa",
        "2026-01-01T00:00:00.000Z",
    );
    d.add_file(
        "b.md",
        &[folder.as_str()],
        b"b",
        "2026-01-02T00:00:00.000Z",
    );
    d.add_file(
        "dup",
        &[folder.as_str()],
        b"old",
        "2026-01-03T00:00:00.000Z",
    );
    d.add_file(
        "dup",
        &[folder.as_str()],
        b"new!",
        "2026-01-04T00:00:00.000Z",
    );

    let mut provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);
    let remote = provider.list().unwrap();

    let mut names: Vec<&str> = remote.iter().map(|(name, _)| name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a.json", "b.md", "dup"]);
    let (_, stat) = remote
        .iter()
        .find(|(name, _)| name == "a.json")
        .expect("a.json listed");
    assert_eq!(stat.size, 3);
    assert_eq!(stat.mtime_ms, 1_767_225_600_000.0);
    assert_eq!(stat.token, md5_hex(b"aaa"));
    let (_, dup) = remote
        .iter()
        .find(|(name, _)| name == "dup")
        .expect("dup listed");
    assert_eq!(dup.size, 4);
    assert!(count(&d, "GET", "pageToken=") >= 1, "second page requested");
}

#[test]
fn write_creates_with_multipart_post_and_updates_with_a_patch_of_modified_time_only() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let folder = d.add_folder("app", None);
    let mut provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);
    provider.list().unwrap();

    // Date.parse("2026-03-04T05:06:07.000Z")
    let mtime = 1_772_600_767_000.0;
    let created = provider
        .write("courses/demo/MISSION.md", b"hello", mtime)
        .unwrap();
    assert_eq!(created.size, 5);
    assert_eq!(created.token, md5_hex(b"hello"));
    assert_eq!(created.mtime_ms, mtime);

    let post = d.requests().last().cloned().unwrap();
    assert_eq!(post.method, "POST");
    assert!(
        post.path
            .starts_with("/upload/drive/v3/files?uploadType=multipart&fields="),
        "{}",
        post.path
    );
    assert!(
        post.header("content-type")
            .unwrap()
            .starts_with("multipart/related; boundary=")
    );
    let (meta, data) = upload_parts(&post);
    assert_eq!(
        meta,
        json!({
            "name": "courses/demo/MISSION.md",
            "parents": [&folder],
            "appProperties": { "proxi": "file" },
            "modifiedTime": "2026-03-04T05:06:07.000Z",
        })
    );
    assert_eq!(data, b"hello");

    provider.list().unwrap();
    let updated = provider
        .write("courses/demo/MISSION.md", b"bye", mtime + 1000.0)
        .unwrap();
    assert_eq!(updated.token, md5_hex(b"bye"));
    let patch = d.requests().last().cloned().unwrap();
    assert_eq!(patch.method, "PATCH");
    assert!(
        patch
            .path
            .starts_with("/upload/drive/v3/files/f") && patch.path.contains("?uploadType=multipart"),
        "{}",
        patch.path
    );
    let (patch_meta, patch_data) = upload_parts(&patch);
    assert_eq!(
        patch_meta,
        json!({ "modifiedTime": "2026-03-04T05:06:08.000Z" })
    );
    assert_eq!(patch_data, b"bye");
    assert_eq!(
        d.files_in(&folder)
            .get("courses/demo/MISSION.md")
            .unwrap()
            .as_slice(),
        b"bye"
    );

    // Binary files are uploaded byte for byte: the fake must never decode the body as text.
    let binary = [0xffu8, 0x00, 0xfe];
    let binary_stat = provider
        .write("courses/demo/bytes.bin", &binary, mtime)
        .unwrap();
    assert_eq!(binary_stat.size, 3);
    let upload = d.requests().last().cloned().unwrap();
    assert!(upload.bytes.windows(3).any(|window| window == binary));
    let (_, upload_data) = upload_parts(&upload);
    assert_eq!(upload_data, binary.to_vec());
    assert_eq!(
        d.files_in(&folder)
            .get("courses/demo/bytes.bin")
            .unwrap()
            .as_slice(),
        &binary[..]
    );
}

#[test]
fn write_refuses_a_file_changed_remotely_since_list() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let folder = d.add_folder("app", None);
    let id = d.add_file(
        "curriculum.json",
        &[folder.as_str()],
        b"v1",
        "2026-01-01T00:00:00.000Z",
    );
    let mut provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);
    provider.list().unwrap();
    d.set_remote(&id, b"other PC", "2026-01-02T00:00:00.000Z");
    assert_eq!(
        provider.write("curriculum.json", b"mine", 0.0).unwrap_err(),
        ProviderError::RemoteMoved
    );
    assert_eq!(d.file(&id).unwrap().data.as_slice(), b"other PC");
}

#[test]
fn remove_trashes_and_ignores_an_unknown_rel() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let folder = d.add_folder("app", None);
    let id = d.add_file(
        "decks/x.apkg",
        &[folder.as_str()],
        b"x",
        "2026-01-01T00:00:00.000Z",
    );
    let mut provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);
    provider.list().unwrap();
    provider.remove("decks/x.apkg").unwrap();

    let call = d.requests().last().cloned().unwrap();
    assert_eq!(call.method, "PATCH");
    assert_eq!(call.path, format!("/drive/v3/files/{id}"));
    assert_eq!(call.json(), json!({ "trashed": true }));
    assert!(d.file(&id).unwrap().trashed);

    let before = d.requests().len();
    provider.remove("never/seen.md").unwrap();
    assert_eq!(d.requests().len(), before);
    provider.commit("ignored").unwrap();
    assert_eq!(d.requests().len(), before);
}

#[test]
fn remove_trashes_every_duplicate() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let folder = d.add_folder("app", None);
    // Two PCs pushed the same new file before either listed it.
    let older = d.add_file(
        "courses/c/lessons/l.md",
        &[folder.as_str()],
        b"old",
        "2026-01-01T00:00:00.000Z",
    );
    let newer = d.add_file(
        "courses/c/lessons/l.md",
        &[folder.as_str()],
        b"new!",
        "2026-01-02T00:00:00.000Z",
    );
    let mut provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);

    let remote = provider.list().unwrap();
    assert_eq!(
        remote
            .iter()
            .find(|(name, _)| name == "courses/c/lessons/l.md")
            .unwrap()
            .1
            .size,
        4,
        "the newest copy is the file"
    );
    provider.remove("courses/c/lessons/l.md").unwrap();
    assert!(d.file(&newer).unwrap().trashed);
    assert!(d.file(&older).unwrap().trashed);

    let mut fresh = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);
    let listed = fresh.list().unwrap();
    assert!(!listed.iter().any(|(name, _)| name == "courses/c/lessons/l.md"));
}

#[test]
fn write_patches_the_newest_copy_and_keeps_the_others_for_remove() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let folder = d.add_folder("app", None);
    let older = d.add_file(
        "decks/x.apkg",
        &[folder.as_str()],
        b"a",
        "2026-01-01T00:00:00.000Z",
    );
    let newer = d.add_file(
        "decks/x.apkg",
        &[folder.as_str()],
        b"bb",
        "2026-01-02T00:00:00.000Z",
    );
    let mut provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);
    provider.list().unwrap();
    provider
        .write(
            "decks/x.apkg",
            b"ccc",
            1_767_225_600_000.0,
        )
        .unwrap();
    assert_eq!(d.file(&newer).unwrap().data.as_slice(), b"ccc");
    assert_eq!(
        d.file(&older).unwrap().data.as_slice(),
        b"a",
        "the older copy is not patched"
    );

    provider.remove("decks/x.apkg").unwrap();
    assert!(d.file(&older).unwrap().trashed);
    assert!(d.file(&newer).unwrap().trashed);
}

#[test]
fn content_equals_compares_md5() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), "folder");
    let token = md5_hex(b"same");
    assert_eq!(
        provider.content_equals(
            b"same",
            &RemoteStat {
                size: 4,
                mtime_ms: 0.0,
                token,
            }
        ),
        Some(true)
    );
    assert_eq!(
        provider.content_equals(
            b"diff",
            &RemoteStat {
                size: 4,
                mtime_ms: 0.0,
                token: md5_hex(b"same"),
            }
        ),
        Some(false)
    );
}

#[test]
fn sync_pass_end_to_end_pushes_then_is_a_no_op() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let folder = d.add_folder(DRIVE_FOLDER, Some("data"));
    let mut provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);

    let tmp = temp_dir("gdrive-sync");
    let data_root = tmp.0.join("data");
    fs::create_dir_all(data_root.join("courses").join("demo")).expect("data folders");
    fs::write(data_root.join("courses").join("demo").join("MISSION.md"), "# demo\n").unwrap();
    fs::write(data_root.join("curriculum.json"), "{}\n").unwrap();
    let conflicts_dir = tmp.0.join("conflicts");
    let trash_dir = tmp.0.join("trash");
    let manifest = Manifest {
        v: 2,
        key: provider.key(),
        data_root: None,
        files: IndexMap::new(),
    };

    let (first, first_manifest) = sync_pass(SyncPassOptions {
        data_root: &data_root,
        provider: &mut provider,
        manifest: &manifest,
        conflicts_dir: &conflicts_dir,
        trash_dir: &trash_dir,
        confirm_deletions: false,
        now: now_ms,
        message: None,
    })
    .unwrap();
    assert_eq!(first.pushed, 2);
    assert_eq!(first.errors, Vec::<String>::new());

    let writes = d.writes_to("/upload/drive/v3/files");
    let reads = d
        .requests()
        .iter()
        .filter(|request| request.method == "GET" && request.path.contains("alt=media"))
        .count();
    let (second, _) = sync_pass(SyncPassOptions {
        data_root: &data_root,
        provider: &mut provider,
        manifest: &first_manifest,
        conflicts_dir: &conflicts_dir,
        trash_dir: &trash_dir,
        confirm_deletions: false,
        now: now_ms,
        message: None,
    })
    .unwrap();
    assert_eq!(second.pushed + second.pulled + second.deleted, 0);
    assert_eq!(d.writes_to("/upload/drive/v3/files"), writes);
    assert_eq!(
        d.requests()
            .iter()
            .filter(|request| request.method == "GET" && request.path.contains("alt=media"))
            .count(),
        reads
    );
}

#[test]
fn drive_errors_use_the_node_message() {
    fn message(status: u16, body: &'static [u8]) -> String {
        let fake = FakeHttp::start(move |request| {
            if request.method == "POST" && request.path == "/token" {
                Reply::json(200, json!({ "access_token": "at-1", "expires_in": 3600 }))
            } else {
                Reply::bytes(status, body.to_vec())
            }
        });
        let deps = GoogleDeps::new(GoogleEndpoints::from_override(Some(&fake.url)));
        let mut auth = GoogleAuth::new("cid", "", "rt-1");
        ensure_folder(&deps, &mut auth).unwrap_err()
    }

    assert_eq!(
        message(500, br#"{"error":{"message":"boom"}}"#),
        "Google Drive 500: boom"
    );
    assert_eq!(
        message(500, br#"{"error_description":"desc"}"#),
        "Google Drive 500: desc"
    );
    assert_eq!(
        message(500, br#"{"error":"invalid_scope"}"#),
        "Google Drive 500: invalid_scope"
    );
    assert_eq!(message(500, b"oops"), "Google Drive 500: oops");
    assert_eq!(
        message(500, b""),
        "Google Drive 500: Internal Server Error"
    );
    assert_eq!(
        message(500, br#"{"error":{"message":""}}"#),
        "Google Drive 500: [object Object]"
    );
    assert_eq!(message(500, b"null"), "Google Drive 500: null");
}

#[test]
fn modified_time_truncates_fractional_ms() {
    let d = FakeDrive::start(FakeDriveOpts::default());
    let folder = d.add_folder("app", None);
    let mut provider = GdriveProvider::new(drive_deps(&d), GoogleAuth::new("cid", "", "rt-1"), &folder);
    provider.list().unwrap();
    provider.write("x.md", b"a", 1_700_000_000_123.9).unwrap();
    let (meta, _) = upload_parts(&d.requests().last().cloned().unwrap());
    assert_eq!(meta["modifiedTime"], "2023-11-14T22:13:20.123Z");
}
