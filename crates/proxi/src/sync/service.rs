//! `SyncService`: status, runs, device login and disconnect (port of `app/server/sync/service.ts`).
//!
//! Blocking like the engine and the providers: the HTTP handlers call it through `spawn_blocking`,
//! and the device-flow poll plus the run it starts live on plain `std::thread`s.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

use serde_json::json;

use crate::data_root::has_data;
use crate::events::{iso_from_millis, iso_now};
use crate::protocol::{GdriveStatus, GithubPending, GithubStatus, SyncLast, SyncStatus};
use crate::settings::{read_settings, write_settings};
use crate::sync::engine::{
    SyncPassOptions, manifest_path, now_ms, read_manifest, sync_pass, write_manifest,
};
use crate::sync::github::{
    CancelToken, DeviceCode, GITHUB_CLIENT_ID, GithubDeps, GithubEndpoints, GithubProvider,
    Sleeper, poll_device_flow, setup_repo, start_device_flow,
};
use crate::sync::secrets::{
    GDRIVE_ACCOUNT, GITHUB_ACCOUNT, GithubSecret, LOCKED_ERROR, LegacyDecryptor, LegacyState,
    SecretStore, UNREADABLE_ERROR, get_gdrive, get_github, migrate_legacy, set_github,
};
use crate::sync::store::{list_conflicts, list_trash, purge_trash};
use crate::sync::types::{SyncProvider, SyncResult};

pub const GDRIVE_UNAVAILABLE: &str = "Google Drive non è ancora disponibile in questa versione.";
pub const NOT_CONNECTED: &str = "Non connesso: premi Accedi.";

/// Test seam: builds the provider for the connected GitHub account.
pub type MakeProvider =
    Arc<dyn Fn(&GithubSecret) -> Result<Box<dyn SyncProvider>, String> + Send + Sync>;

pub struct SyncConfig {
    pub repo_root: PathBuf,
    pub data_root: PathBuf,
    pub data_root_from_env: bool,
    pub github: GithubEndpoints,
    pub sleep: Sleeper,
    pub store: Arc<dyn SecretStore>,
    pub decryptor: Arc<dyn LegacyDecryptor>,
    pub make_provider: Option<MakeProvider>,
    pub on_status: Arc<dyn Fn(&SyncStatus) + Send + Sync>,
    pub on_pulled: Arc<dyn Fn() + Send + Sync>,
}

struct State {
    last: SyncLast,
    github_pending: Option<GithubPending>,
    github_error: Option<String>,
    github_cancel: Option<Arc<CancelToken>>,
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
        let (last, pending, github_error) = {
            let state = self.lock_state();
            (
                state.last.clone(),
                state.github_pending.clone(),
                state.github_error.clone(),
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
            pending: false,
            error: gdrive_secrets_error,
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
            restart_required: None,
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
        if settings.sync_provider == GDRIVE_ACCOUNT {
            self.set_last(SyncLast {
                state: "error".to_string(),
                error: Some(GDRIVE_UNAVAILABLE.to_string()),
                ..SyncLast::default()
            });
            return self.emit();
        }
        let secret = match get_github(cfg.store.as_ref()) {
            Ok(Some(secret)) => secret,
            _ => {
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
            }
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

    fn run_pass(&self, secret: &GithubSecret, confirm_deletions: bool) -> Result<(), String> {
        let cfg = &self.inner.cfg;
        let mut provider: Box<dyn SyncProvider> = match &cfg.make_provider {
            Some(make) => make(secret)?,
            None => Box::new(GithubProvider::new(
                GithubDeps::new(cfg.github.clone(), Arc::clone(&cfg.sleep)),
                &secret.token,
                &secret.repo,
                &secret.branch,
            )),
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

    /// Auto sync runs only for a connected GitHub account.
    pub fn auto_enabled(&self) -> bool {
        let cfg = &self.inner.cfg;
        read_settings(&cfg.repo_root).sync_provider == GITHUB_ACCOUNT
            && matches!(get_github(cfg.store.as_ref()), Ok(Some(_)))
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

    pub fn disconnect(&self, provider: &str) -> Result<SyncStatus, String> {
        let cfg = &self.inner.cfg;
        let account = match provider {
            GITHUB_ACCOUNT => GITHUB_ACCOUNT,
            GDRIVE_ACCOUNT => GDRIVE_ACCOUNT,
            other => return Err(format!("provider sconosciuto: {other}")),
        };
        cfg.store.delete(account)?;
        if provider == GITHUB_ACCOUNT {
            let mut state = self.lock_state();
            if let Some(cancel) = state.github_cancel.take() {
                cancel.cancel();
            }
            state.github_pending = None;
            state.github_error = None;
        }
        if read_settings(&cfg.repo_root).sync_provider == provider {
            write_settings(&cfg.repo_root, &json!({ "syncProvider": "local" }))
                .map_err(|error| error.to_string())?;
        }
        Ok(self.emit())
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
