//! `SyncService`: status, runs, connections (GitHub device flow, Google PKCE), the data folder,
//! conflicts and trash (port of `app/server/sync/service.ts`).
//!
//! Blocking like the engine and the providers: the HTTP handlers call it through `spawn_blocking`,
//! and the device-flow poll, the Google callback's work and the runs they start live on plain
//! `std::thread`s.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::data_root::{data_root_problem, has_data, resolve_path, write_data_root_pointer};
use crate::events::{iso_from_millis, iso_now};
use crate::protocol::{
    GdriveStatus, GithubPending, GithubStatus, LearnerSettings, SyncLast, SyncStatus,
};
use crate::settings::{read_settings, write_settings};
use crate::sync::engine::{
    SyncPassOptions, manifest_path, now_ms, read_manifest, sync_pass, write_manifest,
};
use crate::sync::gdrive::{
    GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET, GdriveProvider, GoogleAuth, GoogleDeps, GoogleEndpoints,
    auth_url, drive_email, ensure_folder, exchange_code, pkce_pair, random_hex, revoke_token,
};
use crate::sync::github::{
    CancelToken, DeviceCode, GITHUB_CLIENT_ID, GithubDeps, GithubEndpoints, GithubProvider,
    Sleeper, poll_device_flow, setup_repo, start_device_flow,
};
use crate::sync::secrets::{
    GDRIVE_ACCOUNT, GITHUB_ACCOUNT, GdriveSecret, GithubSecret, LOCKED_ERROR, LegacyDecryptor,
    LegacyState, SecretStore, UNREADABLE_ERROR, get_gdrive, get_github, migrate_legacy, set_gdrive,
    set_github,
};
use crate::sync::store::{ConflictEntry, TrashEntry, list_conflicts, list_trash, purge_trash};
use crate::sync::types::{SyncProvider, SyncResult};

pub const NOT_CONNECTED: &str = "Non connesso: premi Accedi.";
pub const GOOGLE_PENDING_MS: i64 = 600_000;
pub const GOOGLE_BAD_STATE: &str = "Richiesta di accesso non valida o scaduta: riprova da Proxi.";
pub const GOOGLE_DENIED: &str = "Accesso a Google annullato.";
pub const GOOGLE_DONE: &str =
    "Accesso a Google completato. Puoi chiudere questa scheda e tornare a Proxi.";
pub const DATA_ROOT_FROM_ENV: &str = "La cartella dei dati è impostata da LEARNER_DATA_ROOT.";

/// Restarts the app after a data-folder change (the desktop shell at the #25 cutover).
pub type Relaunch = Arc<dyn Fn() + Send + Sync>;

/// The stored secret of the connected provider, as Node's `makeProvider(id, secrets)` sees it.
pub enum ProviderSecret {
    Github(GithubSecret),
    Gdrive(GdriveSecret),
}

/// Test seam: builds the provider for the connected remote (default: GitHub / Google Drive over HTTP).
pub type MakeProvider =
    Arc<dyn Fn(&ProviderSecret) -> Result<Box<dyn SyncProvider>, String> + Send + Sync>;

pub struct SyncConfig {
    pub repo_root: PathBuf,
    pub data_root: PathBuf,
    pub data_root_from_env: bool,
    pub github: GithubEndpoints,
    pub google: GoogleEndpoints,
    /// The bound port: the Google redirect URI points back at this server (Node's `getPort()`).
    pub port: u16,
    pub sleep: Sleeper,
    pub store: Arc<dyn SecretStore>,
    pub decryptor: Arc<dyn LegacyDecryptor>,
    pub make_provider: Option<MakeProvider>,
    pub relaunch: Option<Relaunch>,
    pub on_status: Arc<dyn Fn(&SyncStatus) + Send + Sync>,
    pub on_pulled: Arc<dyn Fn() + Send + Sync>,
}

struct GooglePending {
    verifier: String,
    redirect_uri: String,
    expires: i64,
}

struct State {
    last: SyncLast,
    github_pending: Option<GithubPending>,
    github_error: Option<String>,
    github_cancel: Option<Arc<CancelToken>>,
    google_pending: HashMap<String, GooglePending>,
    /// Node's `connectErrors.gdrive`: the last failed Google connect.
    gdrive_error: Option<String>,
    restart_required: bool,
}

/// One in-flight pass: callers that arrive while it runs wait for its status.
struct RunSlot {
    done: Mutex<Option<SyncStatus>>,
    ready: Condvar,
}

struct Inner {
    cfg: SyncConfig,
    conflicts_dir: PathBuf,
    trash_dir: PathBuf,
    state: Mutex<State>,
    legacy: LegacyState,
    slot: Mutex<Option<Arc<RunSlot>>>,
}

#[derive(Clone)]
pub struct SyncService {
    inner: Arc<Inner>,
}

/// JS strings in a query are truthy only when non-empty.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// Google client credentials: the settings override, else the embedded defaults.
fn google_client(settings: &LearnerSettings) -> (String, String) {
    let client_id = if settings.google_client_id.is_empty() {
        GOOGLE_CLIENT_ID.to_string()
    } else {
        settings.google_client_id.clone()
    };
    let client_secret = if settings.google_client_secret.is_empty() {
        GOOGLE_CLIENT_SECRET.to_string()
    } else {
        settings.google_client_secret.clone()
    };
    (client_id, client_secret)
}

/// Node's loopback page; `text` is escaped like `text.replace(/[&<>"']/g, ...)`.
pub fn callback_page(status: u16, text: &str) -> (u16, String) {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&#38;"),
            '<' => escaped.push_str("&#60;"),
            '>' => escaped.push_str("&#62;"),
            '"' => escaped.push_str("&#34;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    (
        status,
        format!(
            "<!doctype html><html lang=\"it\"><head><meta charset=\"utf-8\"><title>Proxi</title></head><body><h1>Proxi</h1><p>{escaped}</p></body></html>"
        ),
    )
}

impl SyncService {
    /// Runs the legacy-secret migration once, at server start.
    pub fn new(cfg: SyncConfig) -> Self {
        let conflicts_dir = cfg.repo_root.join(".learner").join("drive-conflicts");
        let trash_dir = cfg.repo_root.join(".learner").join("trash");
        let legacy = migrate_legacy(&cfg.repo_root, cfg.store.as_ref(), cfg.decryptor.as_ref());
        Self {
            inner: Arc::new(Inner {
                cfg,
                conflicts_dir,
                trash_dir,
                state: Mutex::new(State {
                    last: SyncLast::default(),
                    github_pending: None,
                    github_error: None,
                    github_cancel: None,
                    google_pending: HashMap::new(),
                    gdrive_error: None,
                    restart_required: false,
                }),
                legacy,
                slot: Mutex::new(None),
            }),
        }
    }

    pub fn legacy_state(&self) -> LegacyState {
        self.inner.legacy.clone()
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_last(&self, last: SyncLast) {
        self.lock_state().last = last;
    }

    /// The whole `/api/sync` view: settings, connections, the last run, and the counts behind the UI badges.
    pub fn status(&self) -> SyncStatus {
        let cfg = &self.inner.cfg;
        let settings = read_settings(&cfg.repo_root);
        let (last, pending, github_error, google_pending, gdrive_error, restart_required) = {
            let state = self.lock_state();
            let now = now_ms();
            (
                state.last.clone(),
                state.github_pending.clone(),
                state.github_error.clone(),
                state
                    .google_pending
                    .values()
                    .any(|entry| entry.expires > now),
                state.gdrive_error.clone(),
                state.restart_required,
            )
        };
        let legacy_error = match &self.inner.legacy {
            LegacyState::Locked(_) => Some(LOCKED_ERROR.to_string()),
            LegacyState::Unreadable => Some(UNREADABLE_ERROR.to_string()),
            LegacyState::Absent | LegacyState::Migrated => None,
        };

        let (github_secret, github_store_error) = match get_github(cfg.store.as_ref()) {
            Ok(secret) => (secret, None),
            Err(message) => (None, Some(message)),
        };
        let github_connected = github_secret.is_some();
        // Node shows the secrets error only for the active provider; an entry already in the store wins.
        let github_secrets_error = if settings.sync_provider == GITHUB_ACCOUNT && !github_connected {
            github_store_error.or_else(|| legacy_error.clone())
        } else {
            None
        };
        let github = GithubStatus {
            connected: github_connected,
            login: github_secret.as_ref().map(|secret| secret.login.clone()),
            repo: github_secret.as_ref().map(|secret| secret.repo.clone()),
            pending,
            error: github_error.or(github_secrets_error),
        };

        let (gdrive_secret, gdrive_store_error) = match get_gdrive(cfg.store.as_ref()) {
            Ok(secret) => (secret, None),
            Err(message) => (None, Some(message)),
        };
        let gdrive_connected = gdrive_secret.is_some();
        let gdrive_secrets_error = if settings.sync_provider == GDRIVE_ACCOUNT && !gdrive_connected {
            gdrive_store_error.or_else(|| legacy_error.clone())
        } else {
            None
        };
        let gdrive = GdriveStatus {
            connected: gdrive_connected,
            email: gdrive_secret.as_ref().and_then(|s| s.email.clone()),
            pending: google_pending,
            error: gdrive_error.or(gdrive_secrets_error),
        };

        SyncStatus {
            last,
            provider: settings.sync_provider.clone(),
            data_root: cfg.data_root.display().to_string(),
            data_root_from_env: cfg.data_root_from_env,
            needs_data: !has_data(&cfg.data_root),
            encrypted: cfg.store.is_os_keychain(),
            github,
            gdrive,
            restart_required: restart_required.then_some(true),
            conflict_count: list_conflicts(&self.inner.conflicts_dir).len() as u64,
            trash_count: list_trash(&self.inner.trash_dir).len() as u64,
        }
    }

    /// Runs one pass now. Calls while one runs share it; errors land in the status.
    pub fn run(&self, confirm_deletions: bool) -> SyncStatus {
        let (owner, waiter) = {
            let mut slot = self
                .inner
                .slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match slot.as_ref() {
                Some(running) => (None, Some(Arc::clone(running))),
                None => {
                    let running = Arc::new(RunSlot {
                        done: Mutex::new(None),
                        ready: Condvar::new(),
                    });
                    *slot = Some(Arc::clone(&running));
                    (Some(running), None)
                }
            }
        };
        if let Some(waiter) = waiter {
            let mut done = waiter
                .done
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while done.is_none() {
                done = waiter
                    .ready
                    .wait(done)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            return done.clone().expect("a finished run always has a status");
        }

        let owner = owner.expect("the run slot belongs to this call");
        let status = self.do_run(confirm_deletions);
        {
            let mut done = owner
                .done
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *done = Some(status.clone());
            owner.ready.notify_all();
        }
        {
            let mut slot = self
                .inner
                .slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = None;
        }
        status
    }

    fn do_run(&self, confirm_deletions: bool) -> SyncStatus {
        let cfg = &self.inner.cfg;
        purge_trash(&self.inner.trash_dir, now_ms());
        let settings = read_settings(&cfg.repo_root);
        if settings.sync_provider == "local" {
            self.set_last(SyncLast::default());
            return self.status();
        }
        let secret = match settings.sync_provider.as_str() {
            GITHUB_ACCOUNT => get_github(cfg.store.as_ref())
                .ok()
                .flatten()
                .map(ProviderSecret::Github),
            GDRIVE_ACCOUNT => get_gdrive(cfg.store.as_ref())
                .ok()
                .flatten()
                .map(ProviderSecret::Gdrive),
            _ => None,
        };
        let Some(secret) = secret else {
            let message = match &self.inner.legacy {
                LegacyState::Locked(_) => LOCKED_ERROR,
                _ => NOT_CONNECTED,
            };
            self.set_last(SyncLast {
                state: "error".to_string(),
                error: Some(message.to_string()),
                ..SyncLast::default()
            });
            return self.emit();
        };

        if let Err(message) = self.run_pass(&secret, confirm_deletions) {
            self.set_last(SyncLast {
                state: "error".to_string(),
                at: Some(iso_now()),
                error: Some(message),
                ..SyncLast::default()
            });
        }
        self.emit()
    }

    fn run_pass(&self, secret: &ProviderSecret, confirm_deletions: bool) -> Result<(), String> {
        let cfg = &self.inner.cfg;
        let mut provider: Box<dyn SyncProvider> = match &cfg.make_provider {
            Some(make) => make(secret)?,
            None => match secret {
                ProviderSecret::Github(secret) => Box::new(GithubProvider::new(
                    GithubDeps::new(cfg.github.clone(), Arc::clone(&cfg.sleep)),
                    &secret.token,
                    &secret.repo,
                    &secret.branch,
                )),
                ProviderSecret::Gdrive(secret) => {
                    let settings = read_settings(&cfg.repo_root);
                    let (client_id, client_secret) = google_client(&settings);
                    Box::new(GdriveProvider::new(
                        GoogleDeps::new(cfg.google.clone()),
                        GoogleAuth::new(&client_id, &client_secret, &secret.refresh_token),
                        &secret.folder_id,
                    ))
                }
            },
        };
        let key = provider.key();
        let file = manifest_path(&cfg.repo_root, &key);
        let manifest = read_manifest(&file, &key, &cfg.data_root);
        self.set_last(SyncLast {
            state: "syncing".to_string(),
            ..SyncLast::default()
        });
        (cfg.on_status)(&self.status());

        let (result, next) = sync_pass(SyncPassOptions {
            data_root: &cfg.data_root,
            provider: provider.as_mut(),
            manifest: &manifest,
            conflicts_dir: &self.inner.conflicts_dir,
            trash_dir: &self.inner.trash_dir,
            confirm_deletions,
            now: now_ms,
            message: None,
        })?;

        if let Some(pending) = result.pending_deletions.clone() {
            self.set_last(SyncLast {
                state: "confirm".to_string(),
                pending_deletions: Some(pending),
                ..SyncLast::default()
            });
            return Ok(());
        }
        write_manifest(&file, &next).map_err(|error| error.to_string())?;
        fs::create_dir_all(cfg.data_root.join("courses")).map_err(|error| error.to_string())?;
        self.set_last(last_after_run(&result));
        if result.pulled > 0 || result.deleted > 0 {
            (cfg.on_pulled)();
        }
        Ok(())
    }

    fn emit(&self) -> SyncStatus {
        let status = self.status();
        (self.inner.cfg.on_status)(&status);
        status
    }

    /// Auto sync runs only for a non-local provider with a stored secret.
    pub fn auto_enabled(&self) -> bool {
        let cfg = &self.inner.cfg;
        match read_settings(&cfg.repo_root).sync_provider.as_str() {
            GITHUB_ACCOUNT => matches!(get_github(cfg.store.as_ref()), Ok(Some(_))),
            GDRIVE_ACCOUNT => matches!(get_gdrive(cfg.store.as_ref()), Ok(Some(_))),
            _ => false,
        }
    }

    // ───────────── GitHub: device flow ─────────────

    pub fn github_start(&self) -> SyncStatus {
        let cfg = &self.inner.cfg;
        let cancel = CancelToken::new();
        {
            let mut state = self.lock_state();
            if let Some(previous) = state.github_cancel.replace(Arc::clone(&cancel)) {
                previous.cancel();
            }
            state.github_error = None;
            state.github_pending = None;
        }
        let settings = read_settings(&cfg.repo_root);
        let client_id = if settings.github_client_id.is_empty() {
            GITHUB_CLIENT_ID.to_string()
        } else {
            settings.github_client_id.clone()
        };
        let deps = GithubDeps::new(cfg.github.clone(), Arc::clone(&cfg.sleep));
        let code = match start_device_flow(&deps, &client_id) {
            Ok(code) => code,
            Err(message) => {
                self.lock_state().github_error = Some(message);
                return self.emit();
            }
        };
        let cancelled = {
            let mut state = self.lock_state();
            // A cancel (or a newer start) may have landed while the code was in flight.
            let cancelled = cancel.is_cancelled();
            if !cancelled {
                state.github_pending = Some(GithubPending {
                    user_code: code.user_code.clone(),
                    verification_uri: code.verification_uri.clone(),
                    expires_at: iso_from_millis(now_ms() + (code.expires_in * 1000.0) as i64),
                });
            }
            cancelled
        };
        if cancelled {
            return self.emit();
        }
        // Emit before the poller can clear the pending code: the response must show it.
        let status = self.emit();
        let service = self.clone();
        thread::spawn(move || service.finish_github(code, client_id, cancel));
        status
    }

    fn finish_github(&self, code: DeviceCode, client_id: String, cancel: Arc<CancelToken>) {
        let cfg = &self.inner.cfg;
        let deps = GithubDeps::new(cfg.github.clone(), Arc::clone(&cfg.sleep));
        let outcome = (|| -> Result<(), String> {
            let token = poll_device_flow(&deps, &client_id, &code, &cancel)?;
            let repo = setup_repo(&deps, &token)?;
            set_github(
                cfg.store.as_ref(),
                &GithubSecret {
                    token,
                    login: repo.login,
                    repo: repo.repo,
                    branch: repo.branch,
                },
            )?;
            write_settings(&cfg.repo_root, &json!({ "syncProvider": "github" }))
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        let mut ok = false;
        match outcome {
            Ok(()) => ok = true,
            Err(message) => {
                // Cancelled or replaced: githubCancel / githubStart already updated the state.
                if !cancel.is_cancelled() {
                    self.lock_state().github_error = Some(message);
                }
            }
        }
        if cancel.is_cancelled() {
            return;
        }
        self.lock_state().github_pending = None;
        if ok {
            let service = self.clone();
            thread::spawn(move || {
                service.run(false);
            });
        }
        self.emit();
    }

    pub fn github_cancel(&self) -> SyncStatus {
        {
            let mut state = self.lock_state();
            if let Some(cancel) = state.github_cancel.take() {
                cancel.cancel();
            }
            state.github_pending = None;
        }
        self.emit()
    }

    // ───────────── Google Drive: PKCE over loopback ─────────────

    /// Builds the consent URL and keeps its PKCE verifier under the one-time `state`.
    pub fn google_start(&self) -> Result<String, String> {
        let cfg = &self.inner.cfg;
        let settings = read_settings(&cfg.repo_root);
        let (client_id, _) = google_client(&settings);
        let redirect_uri = format!("http://127.0.0.1:{}/oauth/google/callback", cfg.port);
        let state = random_hex(16);
        let pair = pkce_pair();
        let url = auth_url(
            &cfg.google,
            &client_id,
            &redirect_uri,
            &pair.challenge,
            &state,
        )?;
        let now = now_ms();
        {
            let mut current = self.lock_state();
            current.google_pending.retain(|_, entry| entry.expires >= now);
            current.google_pending.insert(
                state,
                GooglePending {
                    verifier: pair.verifier,
                    redirect_uri,
                    expires: now + GOOGLE_PENDING_MS,
                },
            );
            current.gdrive_error = None;
        }
        self.emit();
        Ok(url)
    }

    /// Handles the loopback redirect: exchanges the code, stores the secret, starts the first pass.
    pub fn google_callback(
        &self,
        code: Option<&str>,
        state: Option<&str>,
        error: Option<&str>,
    ) -> (u16, String) {
        // Query values are JS strings: an empty one is falsy.
        let state = non_empty(state).map(str::to_string);
        let entry = {
            let mut current = self.lock_state();
            state
                .as_ref()
                .and_then(|state| current.google_pending.remove(state))
        };
        let Some(entry) = entry.filter(|entry| entry.expires >= now_ms()) else {
            return callback_page(400, GOOGLE_BAD_STATE);
        };
        let Some(code) = non_empty(code) else {
            return callback_page(400, GOOGLE_DENIED);
        };
        if non_empty(error).is_some() {
            return callback_page(400, GOOGLE_DENIED);
        }

        let cfg = &self.inner.cfg;
        let outcome = (|| -> Result<(), String> {
            let settings = read_settings(&cfg.repo_root);
            let (client_id, client_secret) = google_client(&settings);
            let deps = GoogleDeps::new(cfg.google.clone());
            let tokens = exchange_code(
                &deps,
                &client_id,
                &client_secret,
                code,
                &entry.verifier,
                &entry.redirect_uri,
            )?;
            let mut auth = GoogleAuth::new(&client_id, &client_secret, &tokens.refresh_token);
            let folder_id = ensure_folder(&deps, &mut auth)?;
            let email = drive_email(&deps, &mut auth);
            set_gdrive(
                cfg.store.as_ref(),
                &GdriveSecret {
                    refresh_token: tokens.refresh_token,
                    folder_id,
                    email,
                },
            )?;
            write_settings(&cfg.repo_root, &json!({ "syncProvider": "gdrive" }))
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                self.lock_state().gdrive_error = None;
                let service = self.clone();
                thread::spawn(move || {
                    service.run(false);
                });
                self.emit();
                callback_page(200, GOOGLE_DONE)
            }
            Err(message) => {
                self.lock_state().gdrive_error = Some(message.clone());
                self.emit();
                callback_page(400, &format!("Accesso non riuscito: {message}"))
            }
        }
    }

    pub fn disconnect(&self, provider: &str) -> Result<SyncStatus, String> {
        let cfg = &self.inner.cfg;
        let account = match provider {
            GITHUB_ACCOUNT => GITHUB_ACCOUNT,
            GDRIVE_ACCOUNT => GDRIVE_ACCOUNT,
            other => return Err(format!("provider sconosciuto: {other}")),
        };
        if provider == GDRIVE_ACCOUNT
            && let Ok(Some(secret)) = get_gdrive(cfg.store.as_ref())
        {
            revoke_token(&GoogleDeps::new(cfg.google.clone()), &secret.refresh_token);
        }
        cfg.store.delete(account)?;
        if provider == GITHUB_ACCOUNT {
            let mut state = self.lock_state();
            if let Some(cancel) = state.github_cancel.take() {
                cancel.cancel();
            }
            state.github_pending = None;
            state.github_error = None;
        } else {
            self.lock_state().gdrive_error = None;
        }
        if read_settings(&cfg.repo_root).sync_provider == provider {
            write_settings(&cfg.repo_root, &json!({ "syncProvider": "local" }))
                .map_err(|error| error.to_string())?;
        }
        Ok(self.emit())
    }

    // ───────────── data folder ─────────────

    /// The data folder is not hot-swapped: the pointer is written and the server restarts (when `relaunch` is set).
    pub fn set_data_root(&self, dir: &str) -> Result<SyncStatus, (u16, String)> {
        let cfg = &self.inner.cfg;
        if cfg.data_root_from_env {
            return Err((409, DATA_ROOT_FROM_ENV.to_string()));
        }
        if let Some(problem) = data_root_problem(&cfg.repo_root, dir) {
            return Err((400, problem.to_string()));
        }
        let target = resolve_path(Path::new(dir));
        fs::create_dir_all(target.join("courses")).map_err(|error| (500, error.to_string()))?;
        if target == resolve_path(&cfg.data_root) {
            return Ok(self.emit());
        }
        write_data_root_pointer(&cfg.repo_root, &target)
            .map_err(|error| (500, error.to_string()))?;
        self.lock_state().restart_required = true;
        let status = self.emit();
        if let Some(relaunch) = &cfg.relaunch {
            let relaunch = Arc::clone(relaunch);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                relaunch();
            });
        }
        Ok(status)
    }

    // ───────────── conflicts and trash ─────────────

    pub fn conflicts(&self) -> Vec<ConflictEntry> {
        list_conflicts(&self.inner.conflicts_dir)
    }

    pub fn resolve_conflict(&self, id: &str, choice: &str) -> Result<(), String> {
        crate::sync::store::resolve_conflict(
            &self.inner.conflicts_dir,
            &self.inner.cfg.data_root,
            id,
            choice,
        )?;
        (self.inner.cfg.on_pulled)();
        self.emit();
        Ok(())
    }

    pub fn trash(&self) -> Vec<TrashEntry> {
        list_trash(&self.inner.trash_dir)
    }

    pub fn restore(&self, id: &str) -> Result<(), String> {
        crate::sync::store::restore_trash(&self.inner.trash_dir, &self.inner.cfg.data_root, id)?;
        (self.inner.cfg.on_pulled)();
        self.emit();
        Ok(())
    }
}

/// Node's `this.last` after a completed pass: the counts plus the first per-file error, if any.
fn last_after_run(result: &SyncResult) -> SyncLast {
    let failed = result.errors.len();
    SyncLast {
        state: if failed > 0 { "error" } else { "idle" }.to_string(),
        at: Some(iso_now()),
        pushed: Some(result.pushed),
        pulled: Some(result.pulled),
        deleted: Some(result.deleted),
        conflicts: Some(result.conflicts),
        error: if failed > 0 {
            Some(format!(
                "{failed} file non sincronizzati (riprovo alla prossima): {}",
                result.errors[0]
            ))
        } else {
            None
        },
        pending_deletions: None,
    }
}
