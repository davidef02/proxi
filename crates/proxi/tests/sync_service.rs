//! `SyncService` end to end with the in-memory provider and the fake GitHub (port of
//! `app/test/sync-service.test.ts`).

mod common;

#[path = "common/mem_provider.rs"]
mod mem_provider;
#[path = "common/fake_http.rs"]
mod fake_http;
#[path = "common/fake_github.rs"]
mod fake_github;
#[path = "common/fake_drive.rs"]
mod fake_drive;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use common::{TempDir, temp_dir};
use fake_drive::{FakeDrive, FakeDriveOpts};
use fake_github::{FakeGithub, FakeGithubOpts};
use fake_http::{FakeHttp, Reply};
use mem_provider::MemProvider;
use proxi::data_root::{DATA_ROOT_IN_REPO, DATA_ROOT_RELATIVE, read_data_root_pointer, resolve_path};
use proxi::protocol::{PendingDeletions, SyncStatus};
use proxi::settings::write_settings;
use proxi::sync::engine::now_ms;
use proxi::sync::gdrive::{GoogleEndpoints, challenge_for};
use proxi::sync::github::{CancelToken, GithubEndpoints, Sleeper};
use proxi::sync::secrets::{
    GDRIVE_ACCOUNT, GdriveSecret, GithubSecret, LOCKED_ERROR, LegacyDecryptor, LegacyState,
    MemoryStore, SecretStore, UNREADABLE_ERROR, get_gdrive, get_github, legacy_file, set_gdrive,
    set_github,
};
use proxi::sync::service::{
    DATA_ROOT_FROM_ENV, GOOGLE_DENIED, MakeProvider, NOT_CONNECTED, ProviderSecret, SyncConfig,
    SyncService, callback_page,
};
use proxi::sync::types::{ProviderError, RemoteStat, SyncProvider};
use serde_json::{Value, json};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const REMOTE_MTIME: f64 = 1_700_000_000_000.0;

fn token() -> String {
    "t".repeat(12)
}

fn github_secret() -> GithubSecret {
    GithubSecret {
        token: token(),
        login: fake_github::LOGIN.to_string(),
        repo: fake_github::FULL.to_string(),
        branch: "main".to_string(),
    }
}

fn connect_github(store: &dyn SecretStore, repo: &Path) {
    set_github(store, &github_secret()).expect("store the github secret");
    write_settings(repo, &json!({ "syncProvider": "github" })).expect("switch to github");
}

/// Stands in for Electron's safeStorage: it never reads the desktop app's blobs.
struct FailingDecryptor;

impl LegacyDecryptor for FailingDecryptor {
    fn decrypt(&self, _b64: &str) -> Result<String, String> {
        Err("decryptString failed".to_string())
    }
}

fn noop_sleeper() -> Sleeper {
    Arc::new(|_: Duration, _: &CancelToken| {})
}

#[derive(Clone)]
struct Counters {
    pulled: Arc<AtomicUsize>,
    relaunched: Arc<AtomicUsize>,
    statuses: Arc<Mutex<Vec<SyncStatus>>>,
}

impl Counters {
    fn new() -> Self {
        Self {
            pulled: Arc::new(AtomicUsize::new(0)),
            relaunched: Arc::new(AtomicUsize::new(0)),
            statuses: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn pulled(&self) -> usize {
        self.pulled.load(Ordering::SeqCst)
    }

    fn relaunched(&self) -> usize {
        self.relaunched.load(Ordering::SeqCst)
    }
}

#[allow(clippy::too_many_arguments)]
fn service_config(
    repo: &Path,
    data: &Path,
    store: Arc<dyn SecretStore>,
    endpoints: GithubEndpoints,
    google: GoogleEndpoints,
    make: Option<MakeProvider>,
    counters: &Counters,
) -> SyncService {
    SyncService::new(sync_config(
        repo, data, store, endpoints, google, make, counters,
    ))
}

#[allow(clippy::too_many_arguments)]
fn sync_config(
    repo: &Path,
    data: &Path,
    store: Arc<dyn SecretStore>,
    endpoints: GithubEndpoints,
    google: GoogleEndpoints,
    make: Option<MakeProvider>,
    counters: &Counters,
) -> SyncConfig {
    let pulled = Arc::clone(&counters.pulled);
    let relaunched = Arc::clone(&counters.relaunched);
    let statuses = Arc::clone(&counters.statuses);
    SyncConfig {
        repo_root: repo.to_path_buf(),
        data_root: data.to_path_buf(),
        data_root_from_env: false,
        github: endpoints,
        google,
        port: 4321,
        sleep: noop_sleeper(),
        store,
        decryptor: Arc::new(FailingDecryptor),
        make_provider: make,
        relaunch: Some(Arc::new(move || {
            relaunched.fetch_add(1, Ordering::SeqCst);
        })),
        on_status: Arc::new(move |status: &SyncStatus| {
            statuses
                .lock()
                .expect("status log")
                .push(status.clone());
        }),
        on_pulled: Arc::new(move || {
            pulled.fetch_add(1, Ordering::SeqCst);
        }),
    }
}

struct Harness {
    tmp: TempDir,
    repo: PathBuf,
    data: PathBuf,
    store: Arc<MemoryStore>,
    provider: MemProvider,
    counters: Counters,
}

impl Harness {
    fn new(files: &[(&str, &str)]) -> Self {
        let tmp = temp_dir("sync-service");
        let repo = tmp.0.join("repo");
        let data = tmp.0.join("data");
        fs::create_dir_all(repo.join(".learner")).expect("create .learner");
        fs::create_dir_all(&data).expect("create data folder");
        let files: Vec<(&str, &str, f64)> = files
            .iter()
            .map(|(rel, body)| (*rel, *body, REMOTE_MTIME))
            .collect();
        Self {
            tmp,
            repo,
            data,
            store: Arc::new(MemoryStore::default()),
            provider: MemProvider::new(&files),
            counters: Counters::new(),
        }
    }

    fn store(&self) -> Arc<dyn SecretStore> {
        self.store.clone()
    }

    fn service(&self) -> SyncService {
        let make = mem_make_provider(&self.provider);
        self.service_with(
            GithubEndpoints::default(),
            GoogleEndpoints::default(),
            Some(make),
        )
    }

    fn service_with(
        &self,
        endpoints: GithubEndpoints,
        google: GoogleEndpoints,
        make: Option<MakeProvider>,
    ) -> SyncService {
        service_config(
            &self.repo,
            &self.data,
            self.store(),
            endpoints,
            google,
            make,
            &self.counters,
        )
    }

    fn service_for(&self, data: &Path) -> SyncService {
        let make = mem_make_provider(&self.provider);
        service_config(
            &self.repo,
            data,
            self.store(),
            GithubEndpoints::default(),
            GoogleEndpoints::default(),
            Some(make),
            &self.counters,
        )
    }
}

fn mem_make_provider(provider: &MemProvider) -> MakeProvider {
    let provider = provider.clone();
    Arc::new(move |_secret: &ProviderSecret| {
        Ok(Box::new(provider.clone()) as Box<dyn SyncProvider>)
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Calls {
    lists: usize,
    reads: usize,
    writes: usize,
    removes: usize,
    commits: usize,
}

fn calls(provider: &MemProvider) -> Calls {
    let provider = provider.0.lock().expect("provider state");
    Calls {
        lists: provider.lists,
        reads: provider.reads,
        writes: provider.writes,
        removes: provider.removes,
        commits: provider.commits,
    }
}

fn write_legacy(repo: &Path, value: Value) -> PathBuf {
    let file = legacy_file(repo);
    fs::create_dir_all(file.parent().expect("legacy file parent")).expect("create .learner");
    fs::write(&file, value.to_string()).expect("write secrets.json");
    file
}

fn write_plaintext_legacy(repo: &Path, secrets: Value) -> PathBuf {
    write_legacy(
        repo,
        json!({ "v": 1, "encrypted": false, "data": secrets.to_string() }),
    )
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn set_mtime(path: &Path, ms: i64) {
    let time = UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for set_times");
    file.set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))
        .expect("set times");
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "timeout waiting for condition");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_fresh_data_folder_needs_data() {
    let t = Harness::new(&[]);
    let s = t.service().status();
    assert!(s.needs_data);
    assert_eq!(s.provider, "local");
}

#[test]
fn first_connect_pulls_the_remote_into_an_empty_data_folder_the_next_pass_moves_nothing() {
    let t = Harness::new(&[
        ("courses/demo/MISSION.md", "# demo\n"),
        ("curriculum.json", "{}\n"),
    ]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();

    let first = svc.run(false);
    assert_eq!(first.last.state, "idle");
    assert_eq!(first.last.pulled, Some(2));
    assert_eq!(
        fs::read_to_string(t.data.join("courses/demo/MISSION.md")).expect("pulled file"),
        "# demo\n"
    );
    assert!(!first.needs_data);
    assert_eq!(t.counters.pulled(), 1);

    let before = calls(&t.provider);
    let second = svc.run(false);
    assert_eq!(second.last.pulled, Some(0));
    assert_eq!(second.last.pushed, Some(0));
    let after = calls(&t.provider);
    assert_eq!(after.reads, before.reads);
    assert_eq!(after.writes, before.writes);
    assert_eq!(after.commits, before.commits);
    assert_eq!(t.counters.pulled(), 1);
}

#[test]
fn the_local_provider_makes_no_provider_calls() {
    let t = Harness::new(&[("curriculum.json", "{}\n")]);
    let s = t.service().run(false);
    assert_eq!(s.provider, "local");
    assert_eq!(s.last.state, "idle");
    assert_eq!(
        calls(&t.provider),
        Calls {
            lists: 0,
            reads: 0,
            writes: 0,
            removes: 0,
            commits: 0,
        }
    );
}

#[test]
fn a_remote_that_is_not_connected_reports_it_and_touches_nothing() {
    let t = Harness::new(&[("curriculum.json", "{}\n")]);
    write_settings(&t.repo, &json!({ "syncProvider": "github" })).expect("switch to github");
    let s = t.service().run(false);
    assert_eq!(s.last.state, "error");
    assert_eq!(s.last.error.as_deref(), Some(NOT_CONNECTED));
    assert_eq!(calls(&t.provider).lists, 0);
}

#[test]
fn deletions_over_30_percent_stop_the_pass_until_confirmed() {
    let t = Harness::new(&[
        ("courses/a.md", "a"),
        ("courses/b.md", "b"),
        ("courses/c.md", "c"),
        ("courses/d.md", "d"),
    ]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    assert_eq!(svc.run(false).last.pulled, Some(4));
    fs::remove_file(t.data.join("courses/a.md")).expect("remove a");
    fs::remove_file(t.data.join("courses/b.md")).expect("remove b");

    let held = svc.run(false);
    assert_eq!(held.last.state, "confirm");
    assert_eq!(
        held.last.pending_deletions,
        Some(PendingDeletions { count: 2, total: 4 })
    );
    assert_eq!(t.provider.0.lock().unwrap().store.len(), 4);

    let done = svc.run(true);
    assert_eq!(done.last.state, "idle");
    assert_eq!(done.last.deleted, Some(2));
    let mut keys: Vec<String> = t
        .provider
        .0
        .lock()
        .unwrap()
        .store
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(keys, ["courses/c.md", "courses/d.md"]);
}

#[test]
fn github_start_polls_the_device_flow_sets_up_the_repo_and_switches_the_provider_to_github() {
    let t = Harness::new(&[]);
    write_settings(&t.repo, &json!({ "githubClientId": "gh-client" })).expect("client id");
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&polls);
    let fake = FakeHttp::start(move |request| {
        let path = request.path.split('?').next().unwrap_or("");
        match (request.method.as_str(), path) {
            ("POST", "/login/device/code") => Reply::json(
                200,
                json!({
                    "device_code": "dc",
                    "user_code": "ABCD-1234",
                    "verification_uri": "https://github.com/login/device",
                    "expires_in": 900,
                    "interval": 1,
                }),
            ),
            ("POST", "/login/oauth/access_token") => {
                if counter.fetch_add(1, Ordering::SeqCst) < 1 {
                    Reply::json(200, json!({ "error": "authorization_pending" }))
                } else {
                    Reply::json(200, json!({ "access_token": "gho_x" }))
                }
            }
            ("GET", "/user") => Reply::json(200, json!({ "login": "octo" })),
            ("GET", "/repos/octo/proxi-data") => Reply::json(
                200,
                json!({ "private": true, "full_name": "octo/proxi-data", "default_branch": "main" }),
            ),
            _ => Reply::json(404, json!({ "message": "Not Found" })),
        }
    });
    let make = mem_make_provider(&t.provider);
    let svc = t.service_with(
        GithubEndpoints::from_override(Some(&fake.url)),
        GoogleEndpoints::default(),
        Some(make),
    );

    let started = svc.github_start();
    assert_eq!(
        started.github.pending.as_ref().map(|p| p.user_code.as_str()),
        Some("ABCD-1234")
    );
    wait_for(|| svc.status().provider == "github");
    let s = svc.status();
    assert!(s.github.connected);
    assert_eq!(s.github.login.as_deref(), Some("octo"));
    assert_eq!(s.github.repo.as_deref(), Some("octo/proxi-data"));
    assert_eq!(s.github.pending, None);
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|request| request.path == "/login/device/code")
            .count(),
        1
    );
}

#[test]
fn a_cancel_while_the_device_code_is_in_flight_leaves_no_pending_code() {
    // Holds the device-code request open so the cancel lands while `github_start` awaits it.
    #[derive(Default)]
    struct Gate {
        state: Mutex<(bool, bool)>, // (request arrived, reply released)
        ready: Condvar,
    }

    impl Gate {
        fn arrive(&self) {
            let mut state = self.state.lock().expect("gate");
            state.0 = true;
            self.ready.notify_all();
        }

        fn wait_until_arrived(&self) {
            let mut state = self.state.lock().expect("gate");
            while !state.0 {
                state = self.ready.wait(state).expect("gate wait");
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().expect("gate");
            state.1 = true;
            self.ready.notify_all();
        }

        fn wait_for_release(&self) {
            let mut state = self.state.lock().expect("gate");
            while !state.1 {
                state = self.ready.wait(state).expect("gate wait");
            }
        }
    }

    let t = Harness::new(&[]);
    let gate = Arc::new(Gate::default());
    let handler_gate = Arc::clone(&gate);
    let fake = FakeHttp::start(move |request| {
        if request.method == "POST" && request.path == "/login/device/code" {
            handler_gate.arrive();
            handler_gate.wait_for_release();
            Reply::json(
                200,
                json!({
                    "device_code": "dc",
                    "user_code": "ABCD-1234",
                    "verification_uri": "https://github.com/login/device",
                    "expires_in": 900,
                    "interval": 1,
                }),
            )
        } else {
            Reply::json(404, json!({ "message": "Not Found" }))
        }
    });
    let make = mem_make_provider(&t.provider);
    let svc = t.service_with(
        GithubEndpoints::from_override(Some(&fake.url)),
        GoogleEndpoints::default(),
        Some(make),
    );

    let starter = svc.clone();
    let start = thread::spawn(move || starter.github_start());
    gate.wait_until_arrived();
    svc.github_cancel();
    gate.release();

    let started = start.join().expect("start thread");
    assert_eq!(started.github.pending, None);
    assert_eq!(svc.status().github.pending, None);
    // No poller ran: only the device-code request reached the fake.
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn a_pass_retried_after_the_remote_moved_still_refreshes_the_board_for_the_files_attempt_1_pulled() {
    // `curriculum.json` is in the remote so the retry's "other pc" copy does not count as a new pull.
    let t = Harness::new(&[
        ("courses/a.md", "a"),
        ("curriculum.json", "other pc"),
    ]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    svc.run(false);
    let refreshed = t.counters.pulled();

    // Attempt 1 pulls b.md and pushes c.md; its commit finds the ref moved. Attempt 2 sees b.md already on disk.
    t.provider.0.lock().unwrap().store.insert(
        "courses/b.md".to_string(),
        (b"b".to_vec(), REMOTE_MTIME - 5_000.0),
    );
    fs::write(t.data.join("courses/c.md"), "c").expect("write c");
    t.provider.0.lock().unwrap().moves_left = 1;

    let r = svc.run(false);
    assert_eq!(r.last.state, "idle");
    assert_eq!(r.last.pulled, Some(1));
    assert_eq!(r.last.pushed, Some(1));
    assert_eq!(
        t.counters.pulled(),
        refreshed + 1,
        "on_pulled refreshes the board and index.html"
    );
    assert_eq!(t.provider.file("courses/c.md").as_deref(), Some("c"));
}

#[test]
fn disconnect_deletes_the_token_and_resets_the_provider_to_local() {
    let t = Harness::new(&[]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    assert!(svc.status().github.connected);

    let s = svc.disconnect("github").expect("disconnect");
    assert!(!s.github.connected);
    assert_eq!(s.provider, "local");
    assert_eq!(get_github(store.as_ref()).expect("store"), None);
    assert!(!legacy_file(&t.repo).exists());
}

#[test]
fn the_status_view_reports_the_last_run_and_the_conflict_and_trash_counts() {
    let t = Harness::new(&[("curriculum.json", "{}\n")]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    svc.run(false);

    let s = svc.status();
    assert_eq!(s.last.state, "idle");
    assert_eq!(s.last.pulled, Some(1));
    assert_eq!(s.conflict_count, 0);
    assert_eq!(s.trash_count, 0);
    assert_eq!(s.data_root, t.data.display().to_string());
    assert!(!s.data_root_from_env);
}

#[test]
fn each_run_purges_trash_older_than_30_days_and_keeps_newer_trash() {
    let t = Harness::new(&[]);
    let trash = t.repo.join(".learner").join("trash");
    let now = now_ms();
    let old = (now - 31 * DAY_MS).to_string();
    let fresh = (now - DAY_MS).to_string();
    for ms in [&old, &fresh] {
        let dir = trash.join(ms).join("courses");
        fs::create_dir_all(&dir).expect("create trash");
        fs::write(dir.join("f.md"), ms).expect("write trash file");
    }

    t.service().run(false); // the local provider still purges
    assert!(!trash.join(&old).exists());
    assert!(trash.join(&fresh).join("courses").join("f.md").exists());
}

#[test]
fn disconnecting_a_provider_that_is_not_the_active_one_keeps_the_active_provider() {
    let t = Harness::new(&[]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    store
        .set(
            GDRIVE_ACCOUNT,
            &json!({ "refreshToken": "rt-1", "folderId": "f" }).to_string(),
        )
        .expect("store gdrive");

    let s = t.service().disconnect("gdrive").expect("disconnect");
    assert_eq!(s.provider, "github");
    assert!(s.github.connected);
    assert!(!s.gdrive.connected);
}

/// A provider that holds the pass inside `list()` so a second caller is certainly waiting.
struct SlowList {
    inner: MemProvider,
    entered: Arc<Mutex<bool>>,
    delay: Duration,
}

impl SyncProvider for SlowList {
    fn key(&self) -> String {
        self.inner.key()
    }

    fn list(&mut self) -> Result<Vec<(String, RemoteStat)>, ProviderError> {
        *self.entered.lock().expect("entered flag") = true;
        thread::sleep(self.delay);
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
        self.inner.commit(message)
    }
}

#[test]
fn concurrent_run_calls_share_one_pass() {
    let t = Harness::new(&[("curriculum.json", "{}\n")]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let entered = Arc::new(Mutex::new(false));
    let inner = t.provider.clone();
    let flag = Arc::clone(&entered);
    let make: MakeProvider = Arc::new(move |_secret: &ProviderSecret| {
        Ok(Box::new(SlowList {
            inner: inner.clone(),
            entered: Arc::clone(&flag),
            delay: Duration::from_millis(300),
        }) as Box<dyn SyncProvider>)
    });
    let svc = t.service_with(
        GithubEndpoints::default(),
        GoogleEndpoints::default(),
        Some(make),
    );

    let first = svc.clone();
    let a = thread::spawn(move || first.run(false));
    while !*entered.lock().expect("entered flag") {
        thread::yield_now();
    }
    let second = svc.clone();
    let b = thread::spawn(move || second.run(false));

    let a = a.join().expect("first run");
    let b = b.join().expect("second run");
    assert_eq!(a, b);
    assert_eq!(calls(&t.provider).lists, 1);
}

#[test]
fn a_provider_error_lands_in_the_status_and_does_not_save_the_manifest() {
    let t = Harness::new(&[("curriculum.json", "{}\n")]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    t.provider.0.lock().unwrap().fail_list = Some("rete assente".to_string());

    let s = svc.run(false);
    assert_eq!(s.last.state, "error");
    assert_eq!(s.last.error.as_deref(), Some("rete assente"));
    assert!(!t.repo.join(".learner").join("sync-manifests").exists());
}

#[test]
fn switching_the_data_folder_never_pushes_an_older_copy_or_removes_remote_files() {
    let t = Harness::new(&[("courses/c.md", "remote")]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    t.service().run(false);

    let older = t.tmp.0.join("older");
    fs::create_dir_all(older.join("courses")).expect("create older");
    fs::write(older.join("courses/c.md"), "older-copy").expect("write older");
    set_mtime(&older.join("courses/c.md"), 1_700_000_000_000 - 3_600_000);

    let before = calls(&t.provider);
    let s1 = t.service_for(&older).run(false);
    assert_eq!(s1.last.state, "idle");
    assert_eq!(s1.last.pushed, Some(0));
    assert_eq!(s1.last.deleted, Some(0));
    let after = calls(&t.provider);
    assert_eq!(after.writes, before.writes);
    assert_eq!(after.removes, before.removes);
    assert_eq!(t.provider.file("courses/c.md").as_deref(), Some("remote"));
    assert_eq!(
        fs::read_to_string(older.join("courses/c.md")).expect("older copy"),
        "remote"
    );

    let empty = t.tmp.0.join("empty");
    fs::create_dir_all(&empty).expect("create empty");
    let s2 = t.service_for(&empty).run(false);
    assert_eq!(s2.last.state, "idle");
    assert_eq!(s2.last.deleted, Some(0));
    assert_eq!(s2.last.pending_deletions, None);
    assert_eq!(calls(&t.provider).removes, before.removes);
    assert!(t.provider.has("courses/c.md"));
    assert_eq!(
        fs::read_to_string(empty.join("courses/c.md")).expect("pulled into empty"),
        "remote"
    );
}

#[test]
fn a_pass_that_only_trashes_remote_deletions_still_refreshes_the_board() {
    let t = Harness::new(&[
        ("courses/a.md", "a"),
        ("courses/b.md", "b"),
        ("courses/c.md", "c"),
        ("courses/d.md", "d"),
    ]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    svc.run(false);
    let pulled = t.counters.pulled();

    t.provider
        .0
        .lock()
        .unwrap()
        .store
        .shift_remove("courses/a.md");
    let r = svc.run(false);
    assert_eq!(r.last.deleted, Some(1));
    assert_eq!(r.last.pulled, Some(0));
    assert!(!t.data.join("courses/a.md").exists());
    assert_eq!(t.counters.pulled(), pulled + 1);
}

#[test]
fn a_pass_that_moves_nothing_in_or_only_pushes_does_not_refresh_the_board() {
    let t = Harness::new(&[("courses/a.md", "a")]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    svc.run(false);
    let before = t.counters.pulled();
    svc.run(false); // no-op

    fs::write(t.data.join("courses/new.md"), "local only").expect("write new");
    let r = svc.run(false);
    assert_eq!(r.last.pushed, Some(1));
    assert_eq!(r.last.pulled, Some(0));
    assert_eq!(r.last.deleted, Some(0));
    assert_eq!(t.counters.pulled(), before);
}

#[test]
fn switching_back_to_the_original_data_folder_reuses_its_manifest_only_after_the_other_folders_pass_rewrote_it() {
    // The manifest file is per provider; a pass on another folder rebinds it, so returning is a first-meeting
    // pass again: remote wins, nothing local is pushed over it and nothing remote is removed.
    let t = Harness::new(&[("courses/c.md", "remote")]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    svc.run(false);

    let other = t.tmp.0.join("other");
    fs::create_dir_all(&other).expect("create other");
    t.service_for(&other).run(false);

    // Back on the original folder, with a local edit made meanwhile and an older mtime than the remote.
    fs::write(t.data.join("courses/c.md"), "edited-while-away").expect("write edit");
    set_mtime(&t.data.join("courses/c.md"), 1_600_000_000_000);
    let before = calls(&t.provider);
    let r = svc.run(false);
    assert_eq!(r.last.state, "idle");
    assert_eq!(calls(&t.provider).removes, before.removes);
    assert!(t.provider.has("courses/c.md"));

    // Whichever side wins, neither copy is lost.
    let local = fs::read_to_string(t.data.join("courses/c.md")).expect("local copy");
    let remote = t.provider.file("courses/c.md").expect("remote copy");
    assert_eq!(local, remote);
    let kept = files_under(&t.repo.join(".learner").join("drive-conflicts"));
    let copies: Vec<_> = kept
        .iter()
        .filter(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().contains("c.md."))
                .unwrap_or(false)
        })
        .collect();
    assert!(!copies.is_empty(), "a conflict copy is kept, got {kept:?}");
}

#[test]
fn tokens_never_touch_the_disk() {
    let t = Harness::new(&[]);
    let migrated = "t".repeat(12);
    let connected = "u".repeat(12);
    write_plaintext_legacy(
        &t.repo,
        json!({
            "github": {
                "token": migrated,
                "login": "davide",
                "repo": "davide/proxi-data",
                "branch": "main",
            }
        }),
    );
    let svc = t.service();
    assert_eq!(svc.legacy_state(), LegacyState::Migrated);
    write_settings(&t.repo, &json!({ "syncProvider": "github" })).expect("switch to github");
    let store = t.store();
    set_github(
        store.as_ref(),
        &GithubSecret {
            token: connected.clone(),
            ..github_secret()
        },
    )
    .expect("connect");

    svc.run(false);
    assert!(!legacy_file(&t.repo).exists());
    for file in files_under(&t.repo).into_iter().chain(files_under(&t.data)) {
        let body = fs::read_to_string(&file).unwrap_or_default();
        assert!(
            !body.contains(&migrated),
            "{} contains the migrated token",
            file.display()
        );
        assert!(
            !body.contains(&connected),
            "{} contains the connected token",
            file.display()
        );
    }
}

#[test]
fn encrypted_reflects_the_os_keychain() {
    let t = Harness::new(&[]);
    assert!(!t.service().status().encrypted);

    struct OsKeychain(MemoryStore);

    impl SecretStore for OsKeychain {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            self.0.get(account)
        }

        fn set(&self, account: &str, value: &str) -> Result<(), String> {
            self.0.set(account, value)
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            self.0.delete(account)
        }

        fn is_os_keychain(&self) -> bool {
            true
        }
    }

    let store: Arc<dyn SecretStore> = Arc::new(OsKeychain(MemoryStore::default()));
    let svc = service_config(
        &t.repo,
        &t.data,
        store,
        GithubEndpoints::default(),
        GoogleEndpoints::default(),
        None,
        &t.counters,
    );
    assert!(svc.status().encrypted);
}

#[test]
fn a_locked_legacy_file_reports_locked_error_in_status_and_run_and_a_connect_keeps_it_byte_identical() {
    let t = Harness::new(&[]);
    write_settings(&t.repo, &json!({ "syncProvider": "github" })).expect("switch to github");
    let file = write_legacy(&t.repo, json!({ "v": 1, "encrypted": true, "data": "AAAA" }));
    let before = fs::read(&file).expect("read legacy file");

    let svc = t.service();
    assert!(matches!(svc.legacy_state(), LegacyState::Locked(_)));
    let s = svc.status();
    assert!(!s.github.connected);
    assert_eq!(s.github.error.as_deref(), Some(LOCKED_ERROR));
    let r = svc.run(false);
    assert_eq!(r.last.state, "error");
    assert_eq!(r.last.error.as_deref(), Some(LOCKED_ERROR));

    // A connect writes the keychain only: the file it cannot read survives untouched.
    let store = t.store();
    set_github(store.as_ref(), &github_secret()).expect("connect");
    assert_eq!(fs::read(&file).expect("read legacy file"), before);
    let after = svc.status();
    assert!(after.github.connected);
    assert_eq!(after.github.error, None);
}

#[test]
fn an_unreadable_legacy_file_reports_unreadable_error_until_connected_and_the_file_stays() {
    let t = Harness::new(&[]);
    write_settings(&t.repo, &json!({ "syncProvider": "github" })).expect("switch to github");
    let file = write_legacy(&t.repo, json!({ "v": 1, "encrypted": false, "data": 42 }));

    let svc = t.service();
    assert_eq!(svc.legacy_state(), LegacyState::Unreadable);
    let s = svc.status();
    assert!(!s.github.connected);
    assert_eq!(s.github.error.as_deref(), Some(UNREADABLE_ERROR));
    let r = svc.run(false);
    assert_eq!(r.last.state, "error");
    assert_eq!(r.last.error.as_deref(), Some(NOT_CONNECTED));

    let store = t.store();
    set_github(store.as_ref(), &github_secret()).expect("connect");
    assert!(file.exists());
    assert_eq!(svc.status().github.error, None);
}

#[test]
fn a_plaintext_legacy_file_is_migrated_at_service_start() {
    let t = Harness::new(&[]);
    let file = write_plaintext_legacy(
        &t.repo,
        json!({
            "github": {
                "token": token(),
                "login": "davide",
                "repo": "davide/proxi-data",
                "branch": "main",
            },
            "gdrive": {
                "refreshToken": "rt-migrated",
                "folderId": "f1",
                "email": "a@example.com",
            }
        }),
    );

    let svc = t.service();
    assert_eq!(svc.legacy_state(), LegacyState::Migrated);
    let store = t.store();
    assert_eq!(get_github(store.as_ref()).expect("github"), Some(github_secret()));
    let gdrive = get_gdrive(store.as_ref())
        .expect("gdrive")
        .expect("gdrive migrated");
    assert_eq!(gdrive.email.as_deref(), Some("a@example.com"));
    assert!(!file.exists());
}

fn start_params(svc: &SyncService) -> BTreeMap<String, String> {
    let url = svc.google_start().expect("google start");
    let (_, query) = url.split_once('?').expect("consent URL has a query");
    form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn form_of(recorded: &fake_http::Recorded) -> BTreeMap<String, String> {
    form_urlencoded::parse(&recorded.bytes)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn google_service(t: &Harness, fake: &FakeDrive, make: Option<MakeProvider>) -> SyncService {
    t.service_with(GithubEndpoints::default(), fake.endpoints(), make)
}

#[test]
fn callback_page_escapes_the_five_html_characters() {
    assert_eq!(
        callback_page(400, "a & b < c > d \"e\" 'f'"),
        (
            400,
            String::from("<!doctype html><html lang=\"it\"><head><meta charset=\"utf-8\"><title>Proxi</title></head><body><h1>Proxi</h1><p>a &#38; b &#60; c &#62; d &#34;e&#34; &#39;f&#39;</p></body></html>")
        )
    );
    assert_eq!(callback_page(200, "x").0, 200);
}

#[test]
fn set_data_root_refuses_relative_and_in_repo_paths_needs_no_restart_for_the_same_path_and_relaunches_for_another()
 {
    let t = Harness::new(&[]);
    let svc = t.service();
    let dir = |path: &Path| path.display().to_string();

    assert_eq!(
        svc.set_data_root("relative/dir").unwrap_err(),
        (400, DATA_ROOT_RELATIVE.to_string())
    );
    assert_eq!(
        svc.set_data_root(&dir(&t.repo.join("inside"))).unwrap_err(),
        (400, DATA_ROOT_IN_REPO.to_string())
    );

    let same = svc.set_data_root(&dir(&t.data)).expect("same folder");
    assert_eq!(same.restart_required, None);
    assert!(t.data.join("courses").is_dir());
    assert_eq!(read_data_root_pointer(&t.repo), None);

    let other = t.tmp.0.join("other");
    let s = svc.set_data_root(&dir(&other)).expect("new folder");
    assert_eq!(s.restart_required, Some(true));
    assert_eq!(read_data_root_pointer(&t.repo), Some(resolve_path(&other)));
    assert!(other.join("courses").is_dir());
    assert_eq!(t.counters.relaunched(), 0, "the relaunch waits 500 ms");
    wait_for(|| t.counters.relaunched() == 1);
}

#[test]
fn google_start_builds_a_pkce_url_a_bad_state_is_refused_and_a_valid_callback_stores_the_secret() {
    let fake = FakeDrive::start(FakeDriveOpts::default());
    let folder = fake.add_folder("proxi-data", Some("data"));
    let t = Harness::new(&[]);
    write_settings(&t.repo, &json!({ "googleClientId": "test-client" })).expect("client id");
    let make = mem_make_provider(&t.provider);
    let svc = google_service(&t, &fake, Some(make));

    let params = start_params(&svc);
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some("test-client")
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some("http://127.0.0.1:4321/oauth/google/callback")
    );
    let challenge = params.get("code_challenge").expect("challenge").clone();
    let state = params.get("state").expect("state").clone();
    assert_eq!(state.len(), 32);
    assert!(svc.status().gdrive.pending, "the start shows as pending");

    let bad = svc.google_callback(Some("x"), Some("not-issued"), None);
    assert_eq!(bad.0, 400);
    assert!(bad.1.contains("scaduta"), "{}", bad.1);
    assert!(bad.1.contains("<title>Proxi</title>"));

    let ok = svc.google_callback(Some("abc"), Some(&state), None);
    assert_eq!(ok.0, 200, "{}", ok.1);
    assert!(ok.1.contains("completato"));
    assert!(ok.1.contains("<h1>Proxi</h1>"));
    assert!(ok.1.contains("tornare a Proxi"));

    let again = svc.google_callback(Some("abc"), Some(&state), None);
    assert_eq!(again.0, 400, "a state is one-time");

    // The fake received the verifier whose SHA-256 is the challenge.
    let token = fake
        .requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path == "/token")
        .expect("token request");
    let form = form_of(&token);
    assert_eq!(form.get("code").map(String::as_str), Some("abc"));
    assert_eq!(
        form.get("client_id").map(String::as_str),
        Some("test-client")
    );
    assert_eq!(
        form.get("redirect_uri").map(String::as_str),
        Some("http://127.0.0.1:4321/oauth/google/callback")
    );
    assert_eq!(
        challenge_for(form.get("code_verifier").expect("verifier")),
        challenge
    );

    let s = svc.status();
    assert!(s.gdrive.connected);
    assert_eq!(s.gdrive.email.as_deref(), Some("davide@example.com"));
    assert_eq!(s.provider, "gdrive");
    assert!(!s.gdrive.pending, "the pending entry was consumed");
    assert_eq!(
        get_gdrive(t.store.as_ref()).expect("store"),
        Some(GdriveSecret {
            refresh_token: "rt-1".to_string(),
            folder_id: folder,
            email: Some("davide@example.com".to_string()),
        })
    );
}

#[test]
fn google_callback_with_a_locked_legacy_file_connects_and_keeps_the_file_byte_identical() {
    let fake = FakeDrive::start(FakeDriveOpts::default());
    fake.add_folder("proxi-data", Some("data"));
    let t = Harness::new(&[]);
    let file = write_legacy(&t.repo, json!({ "v": 1, "encrypted": true, "data": "AAAA" }));
    let before = fs::read(&file).expect("read legacy file");
    write_settings(&t.repo, &json!({ "googleClientId": "test-client" })).expect("client id");
    let make = mem_make_provider(&t.provider);
    let svc = google_service(&t, &fake, Some(make));
    assert!(matches!(svc.legacy_state(), LegacyState::Locked(_)));

    let state = start_params(&svc)["state"].clone();
    let r = svc.google_callback(Some("abc"), Some(&state), None);
    assert_eq!(r.0, 200, "{}", r.1);
    assert!(r.1.contains("completato"));
    assert_eq!(
        fs::read(&file).expect("read legacy file"),
        before,
        "the locked file is untouched"
    );
    let s = svc.status();
    assert!(s.gdrive.connected);
    assert_eq!(s.provider, "gdrive");
}

#[test]
fn a_denied_google_login_answers_the_cancelled_page_and_stores_nothing() {
    let fake = FakeDrive::start(FakeDriveOpts::default());
    let t = Harness::new(&[]);
    write_settings(&t.repo, &json!({ "googleClientId": "test-client" })).expect("client id");
    let svc = google_service(&t, &fake, None);
    let state = start_params(&svc)["state"].clone();

    let denied = svc.google_callback(None, Some(&state), Some("access_denied"));
    assert_eq!(denied, callback_page(400, GOOGLE_DENIED));
    assert!(
        !fake
            .requests()
            .iter()
            .any(|request| request.method == "POST" && request.path == "/token"),
        "no token exchange"
    );
    let s = svc.status();
    assert!(!s.gdrive.connected);
    assert_eq!(s.provider, "local");
    assert_eq!(s.gdrive.error, None);
}

#[test]
fn a_failed_token_exchange_lands_in_gdrive_error_and_the_page() {
    let fake = FakeDrive::start(FakeDriveOpts::default());
    fake.set_omit_refresh(true);
    let t = Harness::new(&[]);
    write_settings(&t.repo, &json!({ "googleClientId": "test-client" })).expect("client id");
    let svc = google_service(&t, &fake, None);
    let state = start_params(&svc)["state"].clone();

    let failed = svc.google_callback(Some("abc"), Some(&state), None);
    let message = "Google non ha restituito un refresh token: riprova.";
    assert_eq!(
        failed,
        callback_page(400, &format!("Accesso non riuscito: {message}"))
    );
    let s = svc.status();
    assert_eq!(s.gdrive.error.as_deref(), Some(message));
    assert!(!s.gdrive.connected);
    assert_eq!(s.provider, "local");

    // Starting a new login clears the previous connect error, as Node does.
    start_params(&svc);
    assert_eq!(svc.status().gdrive.error, None);
}

#[test]
fn a_malformed_gdrive_secret_is_the_connect_error_of_the_gdrive_provider() {
    let t = Harness::new(&[]);
    write_settings(&t.repo, &json!({ "syncProvider": "gdrive" })).expect("switch to gdrive");
    t.store
        .set(GDRIVE_ACCOUNT, "not json")
        .expect("store a malformed gdrive secret");
    let svc = t.service();

    let s = svc.status();
    assert!(!s.gdrive.connected);
    assert_eq!(s.gdrive.error.as_deref(), Some(UNREADABLE_ERROR));

    // The error belongs to the active provider only: local keeps the status clean.
    write_settings(&t.repo, &json!({ "syncProvider": "local" })).expect("switch to local");
    assert_eq!(svc.status().gdrive.error, None);
}

#[test]
fn a_conflict_keeps_both_versions_and_the_resolved_choice_reaches_the_remote() {
    let t = Harness::new(&[("courses/c.md", "base")]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    svc.run(false);

    // Both sides edit the same file; the remote copy is newer, so it wins and the local bytes go to the conflicts.
    fs::write(t.data.join("courses/c.md"), "local-edit").expect("local edit");
    set_mtime(&t.data.join("courses/c.md"), now_ms() - 60_000);
    t.provider.0.lock().unwrap().store.insert(
        "courses/c.md".to_string(),
        (b"remote-edit".to_vec(), now_ms() as f64),
    );
    let r = svc.run(false);
    assert_eq!(r.last.conflicts, Some(1));
    assert_eq!(r.conflict_count, 1);
    assert_eq!(
        fs::read_to_string(t.data.join("courses/c.md")).expect("local copy"),
        "remote-edit"
    );

    let conflicts = svc.conflicts();
    let conflict = &conflicts[0];
    assert_eq!(conflict.rel, "courses/c.md");
    assert_eq!(conflict.side, "locale");
    assert_eq!(
        fs::read_to_string(
            t.repo
                .join(".learner")
                .join("drive-conflicts")
                .join(&conflict.id)
        )
        .expect("conflict copy"),
        "local-edit"
    );

    svc.resolve_conflict(&conflict.id, "local").expect("resolve");
    assert_eq!(
        fs::read_to_string(t.data.join("courses/c.md")).expect("resolved copy"),
        "local-edit"
    );
    assert!(svc.conflicts().is_empty());
    assert_eq!(svc.status().conflict_count, 0);

    let next = svc.run(false);
    assert_eq!(next.last.pushed, Some(1));
    assert_eq!(t.provider.file("courses/c.md").as_deref(), Some("local-edit"));
}

#[test]
fn resolve_and_restore_map_a_bad_id_or_a_missing_copy_to_an_error() {
    let t = Harness::new(&[]);
    let svc = t.service();

    assert_eq!(
        svc.resolve_conflict("../../x.locale-1", "local")
            .unwrap_err(),
        "Conflitto non valido."
    );
    assert_eq!(
        svc.resolve_conflict("courses/x.md.locale-1", "local")
            .unwrap_err(),
        "Conflitto non trovato."
    );
    assert_eq!(
        svc.restore("nope").unwrap_err(),
        "Identificatore non valido."
    );
}

#[test]
fn a_remote_deletion_trashes_the_local_copy_and_restore_brings_it_back_and_the_next_pass_uploads_it() {
    let t = Harness::new(&[
        ("courses/a/x.md", "x"),
        ("courses/a/k1.md", "k"),
        ("courses/a/k2.md", "k"),
        ("courses/a/k3.md", "k"),
    ]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    svc.run(false);

    t.provider
        .0
        .lock()
        .unwrap()
        .store
        .shift_remove("courses/a/x.md");
    let r = svc.run(false);
    assert_eq!(r.last.state, "idle");
    assert_eq!(r.last.deleted, Some(1));
    assert_eq!(r.trash_count, 1);
    assert!(!t.data.join("courses/a/x.md").exists());

    let trash = svc.trash();
    let entry = &trash[0];
    assert_eq!(entry.rel, "courses/a/x.md");
    svc.restore(&entry.id).expect("restore");
    assert_eq!(
        fs::read_to_string(t.data.join("courses/a/x.md")).expect("restored copy"),
        "x"
    );
    assert_eq!(svc.status().trash_count, 0);

    let again = svc.run(false);
    assert_eq!(again.last.pushed, Some(1));
    assert_eq!(t.provider.file("courses/a/x.md").as_deref(), Some("x"));
}

#[test]
fn disconnect_gdrive_revokes_the_refresh_token_and_keeps_github() {
    let fake = FakeDrive::start(FakeDriveOpts::default());
    let t = Harness::new(&[]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    set_gdrive(
        store.as_ref(),
        &GdriveSecret {
            refresh_token: "rt-1".to_string(),
            folder_id: "f".to_string(),
            email: Some("a@example.com".to_string()),
        },
    )
    .expect("store gdrive");
    write_settings(&t.repo, &json!({ "syncProvider": "gdrive" })).expect("switch to gdrive");
    let svc = google_service(&t, &fake, None);

    let s = svc.disconnect("gdrive").expect("disconnect");
    let revoke = fake
        .requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.path == "/revoke")
        .expect("revoke request");
    assert_eq!(revoke.body, "token=rt-1");
    assert!(!s.gdrive.connected);
    assert!(s.github.connected);
    assert_eq!(s.provider, "local");
}

#[test]
fn set_data_root_is_refused_with_409_under_learner_data_root() {
    let t = Harness::new(&[]);
    let mut cfg = sync_config(
        &t.repo,
        &t.data,
        t.store(),
        GithubEndpoints::default(),
        GoogleEndpoints::default(),
        None,
        &Counters::new(),
    );
    cfg.data_root_from_env = true;
    let svc = SyncService::new(cfg);

    let other = t.tmp.0.join("other");
    assert_eq!(
        svc.set_data_root(&other.display().to_string()).unwrap_err(),
        (409, DATA_ROOT_FROM_ENV.to_string())
    );
    assert_eq!(read_data_root_pointer(&t.repo), None);
}

#[test]
fn resolve_and_restore_refresh_the_board() {
    // resolveConflict
    let t = Harness::new(&[("courses/c.md", "base")]);
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service();
    svc.run(false);
    fs::write(t.data.join("courses/c.md"), "local-edit").expect("local edit");
    set_mtime(&t.data.join("courses/c.md"), now_ms() - 60_000);
    t.provider.0.lock().unwrap().store.insert(
        "courses/c.md".to_string(),
        (b"remote-edit".to_vec(), now_ms() as f64),
    );
    svc.run(false);
    let id = svc.conflicts()[0].id.clone();
    let before = t.counters.pulled();
    let emitted = t.counters.statuses.lock().unwrap().len();
    svc.resolve_conflict(&id, "local").expect("resolve");
    assert_eq!(t.counters.pulled(), before + 1);
    assert_eq!(
        t.counters.statuses.lock().unwrap().len(),
        emitted + 1,
        "a resolve emits the new status"
    );

    // restore
    let u = Harness::new(&[
        ("courses/a.md", "a"),
        ("courses/b.md", "b"),
        ("courses/c.md", "c"),
        ("courses/d.md", "d"),
    ]);
    let store = u.store();
    connect_github(store.as_ref(), &u.repo);
    let svc = u.service();
    svc.run(false);
    u.provider
        .0
        .lock()
        .unwrap()
        .store
        .shift_remove("courses/a.md");
    svc.run(false);
    let id = svc.trash()[0].id.clone();
    let before = u.counters.pulled();
    let emitted = u.counters.statuses.lock().unwrap().len();
    svc.restore(&id).expect("restore");
    assert_eq!(u.counters.pulled(), before + 1);
    assert_eq!(
        u.counters.statuses.lock().unwrap().len(),
        emitted + 1,
        "a restore emits the new status"
    );
    assert_eq!(
        fs::read_to_string(u.data.join("courses/a.md")).expect("restored"),
        "a"
    );
}

#[test]
fn a_gdrive_pass_against_the_fake_drive_pushes_then_is_a_no_op() {
    let fake = FakeDrive::start(FakeDriveOpts::default());
    let folder = fake.add_folder("proxi-data", Some("data"));
    let t = Harness::new(&[]);
    fs::create_dir_all(t.data.join("courses/demo")).expect("create course");
    fs::write(t.data.join("courses/demo/MISSION.md"), "# demo\n").expect("write mission");
    let store = t.store();
    set_gdrive(
        store.as_ref(),
        &GdriveSecret {
            refresh_token: "rt-1".to_string(),
            folder_id: folder.clone(),
            email: None,
        },
    )
    .expect("store gdrive");
    write_settings(&t.repo, &json!({ "syncProvider": "gdrive" })).expect("switch to gdrive");
    let svc = google_service(&t, &fake, None);

    let first = svc.run(false);
    assert_eq!(first.last.state, "idle");
    assert_eq!(first.last.pushed, Some(1));
    assert_eq!(first.last.error, None);
    assert_eq!(fake.writes_to("/upload/drive/v3/files"), 1);
    assert_eq!(
        fake.files_in(&folder)
            .get("courses/demo/MISSION.md")
            .map(Vec::as_slice),
        Some(b"# demo\n".as_slice())
    );

    fake.http.clear();
    let second = svc.run(false);
    assert_eq!(
        (
            second.last.pushed,
            second.last.pulled,
            second.last.deleted
        ),
        (Some(0), Some(0), Some(0))
    );
    assert_eq!(fake.writes_to("/upload/"), 0);
}

#[test]
fn an_invalid_grant_refresh_puts_the_reconnect_message_in_the_status() {
    let fake = FakeDrive::start(FakeDriveOpts::default());
    let t = Harness::new(&[]);
    let store = t.store();
    set_gdrive(
        store.as_ref(),
        &GdriveSecret {
            refresh_token: "rt-old".to_string(),
            folder_id: "f".to_string(),
            email: None,
        },
    )
    .expect("store gdrive");
    write_settings(&t.repo, &json!({ "syncProvider": "gdrive" })).expect("switch to gdrive");
    let svc = google_service(&t, &fake, None);

    let s = svc.run(false);
    assert_eq!(s.last.state, "error");
    assert_eq!(
        s.last.error.as_deref(),
        Some("Accesso a Google scaduto o revocato: riconnetti.")
    );
    assert_eq!(fake.refresh_grants(), 1);
}

#[test]
fn auto_enabled_is_true_for_a_connected_gdrive() {
    let t = Harness::new(&[]);
    let store = t.store();
    let svc = t.service();
    assert!(!svc.auto_enabled(), "local never syncs");

    set_gdrive(
        store.as_ref(),
        &GdriveSecret {
            refresh_token: "rt".to_string(),
            folder_id: "f".to_string(),
            email: None,
        },
    )
    .expect("store gdrive");
    assert!(
        !svc.auto_enabled(),
        "a stored secret alone does not enable a local provider"
    );
    write_settings(&t.repo, &json!({ "syncProvider": "gdrive" })).expect("switch to gdrive");
    assert!(svc.auto_enabled());

    store.delete(GDRIVE_ACCOUNT).expect("drop the secret");
    assert!(!svc.auto_enabled());
}

#[test]
fn two_github_passes_against_the_fake_the_second_makes_no_commit_and_reads_no_blob() {
    let gh = FakeGithub::start(FakeGithubOpts {
        seed: Vec::new(),
        ..Default::default()
    });
    let t = Harness::new(&[]);
    fs::create_dir_all(t.data.join("courses/demo")).expect("create course");
    fs::write(t.data.join("courses/demo/MISSION.md"), "# demo\n").expect("write mission");
    let store = t.store();
    connect_github(store.as_ref(), &t.repo);
    let svc = t.service_with(gh.endpoints(), GoogleEndpoints::default(), None);

    let first = svc.run(false);
    assert_eq!(first.last.state, "idle");
    assert_eq!(first.last.pushed, Some(1));
    assert_eq!(first.last.error, None);
    assert_eq!(gh.count("POST", "/git/commits"), 1);

    gh.http.clear();
    let second = svc.run(false);
    assert_eq!(
        (
            second.last.pushed,
            second.last.pulled,
            second.last.deleted
        ),
        (Some(0), Some(0), Some(0))
    );
    assert_eq!(gh.count("POST", "/git/commits"), 0);
    assert_eq!(gh.count("GET", "/git/blobs"), 0);
}
