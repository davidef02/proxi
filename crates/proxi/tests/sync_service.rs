//! `SyncService` end to end with the in-memory provider and the fake GitHub (port of
//! `app/test/sync-service.test.ts`).

mod common;

#[path = "common/mem_provider.rs"]
mod mem_provider;
#[path = "common/fake_http.rs"]
mod fake_http;
#[path = "common/fake_github.rs"]
mod fake_github;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use common::{TempDir, temp_dir};
use fake_github::{FakeGithub, FakeGithubOpts};
use fake_http::{FakeHttp, Reply};
use mem_provider::MemProvider;
use proxi::protocol::{PendingDeletions, SyncStatus};
use proxi::settings::write_settings;
use proxi::sync::engine::now_ms;
use proxi::sync::github::{CancelToken, GithubEndpoints, Sleeper};
use proxi::sync::secrets::{
    GDRIVE_ACCOUNT, GithubSecret, LOCKED_ERROR, LegacyDecryptor, LegacyState, MemoryStore,
    SecretStore, UNREADABLE_ERROR, get_gdrive, get_github, legacy_file, set_github,
};
use proxi::sync::service::{
    GDRIVE_UNAVAILABLE, MakeProvider, NOT_CONNECTED, SyncConfig, SyncService,
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
    statuses: Arc<Mutex<Vec<SyncStatus>>>,
}

impl Counters {
    fn new() -> Self {
        Self {
            pulled: Arc::new(AtomicUsize::new(0)),
            statuses: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn pulled(&self) -> usize {
        self.pulled.load(Ordering::SeqCst)
    }
}

fn service_config(
    repo: &Path,
    data: &Path,
    store: Arc<dyn SecretStore>,
    endpoints: GithubEndpoints,
    make: Option<MakeProvider>,
    counters: &Counters,
) -> SyncService {
    let pulled = Arc::clone(&counters.pulled);
    let statuses = Arc::clone(&counters.statuses);
    SyncService::new(SyncConfig {
        repo_root: repo.to_path_buf(),
        data_root: data.to_path_buf(),
        data_root_from_env: false,
        github: endpoints,
        sleep: noop_sleeper(),
        store,
        decryptor: Arc::new(FailingDecryptor),
        make_provider: make,
        on_status: Arc::new(move |status: &SyncStatus| {
            statuses
                .lock()
                .expect("status log")
                .push(status.clone());
        }),
        on_pulled: Arc::new(move || {
            pulled.fetch_add(1, Ordering::SeqCst);
        }),
    })
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
        self.service_with(GithubEndpoints::default(), Some(make))
    }

    fn service_with(&self, endpoints: GithubEndpoints, make: Option<MakeProvider>) -> SyncService {
        service_config(
            &self.repo,
            &self.data,
            self.store(),
            endpoints,
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
            Some(make),
            &self.counters,
        )
    }
}

fn mem_make_provider(provider: &MemProvider) -> MakeProvider {
    let provider = provider.clone();
    Arc::new(move |_secret: &GithubSecret| {
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
    let svc = t.service_with(GithubEndpoints::from_override(Some(&fake.url)), Some(make));

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
    let svc = t.service_with(GithubEndpoints::from_override(Some(&fake.url)), Some(make));

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
    let make: MakeProvider = Arc::new(move |_secret: &GithubSecret| {
        Ok(Box::new(SlowList {
            inner: inner.clone(),
            entered: Arc::clone(&flag),
            delay: Duration::from_millis(300),
        }) as Box<dyn SyncProvider>)
    });
    let svc = t.service_with(GithubEndpoints::default(), Some(make));

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

#[test]
fn gdrive_provider_reports_gdrive_unavailable() {
    let t = Harness::new(&[]);
    let store = t.store();
    store
        .set(
            GDRIVE_ACCOUNT,
            &json!({ "refreshToken": "rt", "folderId": "f" }).to_string(),
        )
        .expect("store gdrive");
    write_settings(&t.repo, &json!({ "syncProvider": "gdrive" })).expect("switch to gdrive");

    let s = t.service().run(false);
    assert_eq!(s.last.state, "error");
    assert_eq!(s.last.error.as_deref(), Some(GDRIVE_UNAVAILABLE));
    assert_eq!(calls(&t.provider).lists, 0);
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
    let svc = t.service_with(gh.endpoints(), None);

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
