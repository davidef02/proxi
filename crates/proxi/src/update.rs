//! App update: signed release checks, installer download, minisign verification, relaunch.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::{self, EventBus};
use crate::protocol::{GlobalEvent, UpdateApplyResponse, UpdateState, UpdateStatus};

pub const SESSIONS_RUNNING: &str =
    "C'è una sessione in corso. Chiudila o conferma per interromperla prima di aggiornare.";

pub const DEFAULT_LATEST_JSON: &str =
    "https://github.com/davidef02/proxi/releases/latest/download/latest.json";

pub const UPDATER_PUBKEY: &str = include_str!("update_key.pub");

const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const APPLY_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const RELAUNCH_DELAY: Duration = Duration::from_millis(500);
const MAX_INSTALLER_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct UpdateError {
    pub status: u16,
    pub message: String,
    pub code: Option<&'static str>,
}

/// Seam for the sessions/jobs managers: the update refuses to run while work is live.
pub trait ActiveWork: Send + Sync {
    fn count(&self) -> usize;
    fn shutdown(&self) -> BoxFuture<'_, ()>;
}

pub struct NoActiveWork;

impl ActiveWork for NoActiveWork {
    fn count(&self) -> usize {
        0
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

pub fn update_pending(repo: &Path) -> PathBuf {
    repo.join("target")
        .join("release")
        .join("proxi.update-pending")
}

pub fn sanitize_version(version: &str) -> String {
    version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn installer_path(repo: &Path, version: &str) -> PathBuf {
    repo.join("target")
        .join("release")
        .join(format!("proxi-setup-{}.exe", sanitize_version(version)))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PendingUpdate {
    pub installer: String,
    pub version: String,
}

pub fn read_pending(repo: &Path) -> Option<PendingUpdate> {
    let bytes = fs::read(update_pending(repo)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write_pending(repo: &Path, pending: &PendingUpdate) -> std::io::Result<()> {
    let path = update_pending(repo);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(pending)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, bytes)
}

pub fn clear_pending(repo: &Path) {
    let _ = fs::remove_file(update_pending(repo));
}

/// JS truthiness of `body.force`: only an object body can ask for force.
pub fn force_from_body(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Value::Object(map) = value else {
        return false;
    };
    match map.get("force") {
        Some(Value::Bool(force)) => *force,
        Some(Value::Number(force)) => force.as_f64().is_some_and(|force| force != 0.0),
        Some(Value::String(force)) => !force.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
        Some(Value::Null) | None => false,
    }
}

pub fn is_newer(remote: &str, current: &str) -> bool {
    let parse_parts = |s: &str| -> Vec<u64> {
        let s = s.trim().strip_prefix('v').unwrap_or(s.trim());
        s.split('.')
            .map(|part| {
                part.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .collect()
    };
    let remote_parts = parse_parts(remote);
    let current_parts = parse_parts(current);
    let max_len = remote_parts.len().max(current_parts.len());
    for i in 0..max_len {
        let r = remote_parts.get(i).copied().unwrap_or(0);
        let c = current_parts.get(i).copied().unwrap_or(0);
        if r > c {
            return true;
        } else if r < c {
            return false;
        }
    }
    false
}

pub fn resolve_installer_url(releases_url: &str, installer: &str) -> String {
    if installer.starts_with("http://") || installer.starts_with("https://") {
        return installer.to_string();
    }
    if let Some((dir, _)) = releases_url.rsplit_once('/') {
        format!("{dir}/{installer}")
    } else {
        installer.to_string()
    }
}

pub fn verify_installer_signature(
    pubkey_str: &str,
    data: &[u8],
    sig_b64: &str,
) -> Result<(), &'static str> {
    let sig_text_string;
    let sig_text = match STANDARD.decode(sig_b64.trim()) {
        Ok(raw) => match String::from_utf8(raw) {
            Ok(s) => {
                sig_text_string = s;
                &sig_text_string
            }
            Err(_) => sig_b64,
        },
        Err(_) => sig_b64,
    };
    let sig = minisign_verify::Signature::decode(sig_text).map_err(|_| "decode_sig")?;
    let pubkey = minisign_verify::PublicKey::decode(pubkey_str)
        .or_else(|_| minisign_verify::PublicKey::from_base64(pubkey_str))
        .map_err(|_| "decode_pubkey")?;
    pubkey.verify(data, &sig, false).map_err(|_| "verify")?;
    Ok(())
}

pub struct UpdateConfig {
    pub repo_root: PathBuf,
    pub releases_url: String,
    pub pubkey: String,
    pub http: reqwest::Client,
    pub events: EventBus,
    pub active_work: Arc<dyn ActiveWork>,
    /// None = no relaunch signal (library and tests).
    pub relaunch: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub struct UpdateManager {
    cfg: UpdateConfig,
    status: Mutex<UpdateStatus>,
    check_lock: tokio::sync::Mutex<()>,
    check_gen: AtomicU64,
    last_check: Mutex<Option<UpdateStatus>>,
}

impl UpdateManager {
    pub fn new(cfg: UpdateConfig) -> Self {
        Self {
            cfg,
            status: Mutex::new(UpdateStatus::default()),
            check_lock: tokio::sync::Mutex::new(()),
            check_gen: AtomicU64::new(0),
            last_check: Mutex::new(None),
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.status
            .lock()
            .expect("update status poisoned")
            .clone()
    }

    pub fn active_sessions(&self) -> usize {
        self.cfg.active_work.count()
    }

    fn set(&self, status: UpdateStatus) {
        *self.status.lock().expect("update status poisoned") = status.clone();
        self.cfg.events.emit(&GlobalEvent::Update { status });
    }

    fn fail(&self, text: &str) {
        let mut status = self.status();
        status.state = UpdateState::Error;
        status.error = Some(text.to_string());
        self.set(status);
    }

    fn finish(&self, status: UpdateStatus) -> UpdateStatus {
        self.set(status.clone());
        status
    }

    /// Single flight per manager: a caller that waited for a running check gets its result.
    pub async fn check(&self) -> UpdateStatus {
        let generation = self.check_gen.load(Ordering::SeqCst);
        let _guard = self.check_lock.lock().await;
        if self.check_gen.load(Ordering::SeqCst) != generation {
            return self
                .last_check
                .lock()
                .expect("update check result poisoned")
                .clone()
                .unwrap_or_default();
        }
        let status = self.do_check().await;
        *self.last_check.lock().expect("update check result poisoned") = Some(status.clone());
        self.check_gen.fetch_add(1, Ordering::SeqCst);
        status
    }

    async fn do_check(&self) -> UpdateStatus {
        if let Some(pending) = read_pending(&self.cfg.repo_root) {
            return self.finish(UpdateStatus {
                state: UpdateState::Available,
                version: Some(pending.version),
                notes: None,
                checked_at: Some(events::iso_now()),
                error: None,
            });
        }

        let response = match self
            .cfg
            .http
            .get(&self.cfg.releases_url)
            .timeout(CHECK_TIMEOUT)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp,
            _ => return self.finish(idle_status()),
        };

        let json: Value = match response.json().await {
            Ok(val) => val,
            Err(_) => return self.finish(idle_status()),
        };

        let version = match json.get("version").and_then(Value::as_str) {
            Some(v) => v.to_string(),
            None => return self.finish(idle_status()),
        };

        let notes = json
            .get("notes")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);

        let current_version = env!("CARGO_PKG_VERSION");
        if is_newer(&version, current_version) {
            self.finish(UpdateStatus {
                state: UpdateState::Available,
                version: Some(version),
                notes,
                checked_at: Some(events::iso_now()),
                error: None,
            })
        } else {
            self.finish(UpdateStatus {
                state: UpdateState::UpToDate,
                version: None,
                notes: None,
                checked_at: Some(events::iso_now()),
                error: None,
            })
        }
    }

    pub async fn apply(&self, force: bool) -> Result<UpdateApplyResponse, UpdateError> {
        let active = self.cfg.active_work.count();
        if active > 0 {
            if !force {
                return Err(UpdateError {
                    status: 409,
                    message: SESSIONS_RUNNING.to_string(),
                    code: Some("sessions_running"),
                });
            }
            self.cfg.active_work.shutdown().await;
        }

        let mut updating = self.status();
        updating.state = UpdateState::Updating;
        self.set(updating);

        let response = match self
            .cfg
            .http
            .get(&self.cfg.releases_url)
            .timeout(APPLY_FETCH_TIMEOUT)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp,
            Ok(resp) => {
                let text = format!("Errore download release info: HTTP {}", resp.status());
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
            Err(e) => {
                let text = format!("Errore connessione release info: {e}");
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        };

        let json: Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                let text = format!("Errore parsing release info: {e}");
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        };

        let version = match json.get("version").and_then(Value::as_str) {
            Some(v) => v.to_string(),
            None => {
                let text = "Versione mancante nel manifest di release.".to_string();
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        };

        let platform = json
            .get("platforms")
            .and_then(Value::as_object)
            .and_then(|p| p.get("windows-x86_64").or_else(|| p.get("windows-x86_64-nsis")))
            .and_then(Value::as_object);

        let (installer_url_raw, signature_b64) = match platform {
            Some(p) => {
                let url = p.get("url").and_then(Value::as_str);
                let sig = p.get("signature").and_then(Value::as_str);
                match (url, sig) {
                    (Some(u), Some(s)) => (u, s),
                    _ => {
                        let text =
                            "Informazioni piattaforma non valide nel manifest di release.".to_string();
                        self.fail(&text);
                        return Err(UpdateError {
                            status: 500,
                            message: text,
                            code: None,
                        });
                    }
                }
            }
            None => {
                let text = "Piattaforma windows-x86_64 non supportata nel rilascio.".to_string();
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        };

        let installer_url = resolve_installer_url(&self.cfg.releases_url, installer_url_raw);

        let download_resp = match self
            .cfg
            .http
            .get(&installer_url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp,
            Ok(resp) => {
                let text = format!("Errore download installer: HTTP {}", resp.status());
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
            Err(e) => {
                let text = format!("Errore connessione installer: {e}");
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        };

        if let Some(len) = download_resp.content_length() {
            if len > MAX_INSTALLER_BYTES as u64 {
                let text = "Dimensione installer superiore al limite consentito.".to_string();
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        }

        let bytes = match download_resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                let text = format!("Errore lettura installer: {e}");
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        };

        if bytes.len() > MAX_INSTALLER_BYTES {
            let text = "Dimensione installer superiore al limite consentito.".to_string();
            self.fail(&text);
            return Err(UpdateError {
                status: 500,
                message: text,
                code: None,
            });
        }

        if let Err(_) = verify_installer_signature(&self.cfg.pubkey, &bytes, signature_b64) {
            let text = "Firma dell'installer non valida: aggiornamento annullato.";
            self.fail(text);
            return Err(UpdateError {
                status: 500,
                message: text.to_string(),
                code: None,
            });
        }

        let installer_file = installer_path(&self.cfg.repo_root, &version);
        if let Some(parent) = installer_file.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                let text = format!("Errore creazione directory installer: {e}");
                self.fail(&text);
                return Err(UpdateError {
                    status: 500,
                    message: text,
                    code: None,
                });
            }
        }
        if let Err(e) = fs::write(&installer_file, &bytes) {
            let text = format!("Errore salvataggio installer: {e}");
            self.fail(&text);
            return Err(UpdateError {
                status: 500,
                message: text,
                code: None,
            });
        }

        let pending = PendingUpdate {
            installer: installer_file.to_string_lossy().into_owned(),
            version: version.clone(),
        };
        if let Err(e) = write_pending(&self.cfg.repo_root, &pending) {
            let text = format!("Errore salvataggio marker di aggiornamento: {e}");
            self.fail(&text);
            return Err(UpdateError {
                status: 500,
                message: text,
                code: None,
            });
        }

        self.set(UpdateStatus {
            state: UpdateState::Relaunching,
            version: Some(version),
            notes: None,
            checked_at: Some(events::iso_now()),
            error: None,
        });

        if let Some(relaunch) = self.cfg.relaunch.clone() {
            tokio::spawn(async move {
                tokio::time::sleep(RELAUNCH_DELAY).await;
                relaunch();
            });
        }

        Ok(UpdateApplyResponse {
            ok: true,
            relaunching: true,
        })
    }
}

fn idle_status() -> UpdateStatus {
    UpdateStatus {
        checked_at: Some(events::iso_now()),
        ..UpdateStatus::default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    const TEST_PUBKEY: &str =
        "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n";

    const VALID_PREHASHED_SIG: &str = "untrusted comment: signature from minisign secret key\n\
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\n\
trusted comment: timestamp:1556193335\tfile:test\n\
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==\n";

    const LEGACY_SIG: &str = "untrusted comment: signature from minisign secret key\n\
RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\n\
trusted comment: timestamp:1555779966\tfile:test\n\
QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==\n";

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-update-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn is_newer_compares_versions() {
        assert!(is_newer("9.9.9", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));

        // Short vs long
        assert!(is_newer("1.0.1", "1.0"));
        assert!(!is_newer("1.0", "1.0.1"));
        assert!(is_newer("1.1", "1.0.5"));
        assert!(!is_newer("0.1.0", "0.1.0.1"));
    }

    #[test]
    fn sanitize_version_filters_disallowed_characters() {
        assert_eq!(sanitize_version("0.1.0"), "0.1.0");
        assert_eq!(sanitize_version("v1.2.3-alpha.1"), "v1.2.3-alpha.1");
        assert_eq!(sanitize_version("foo/bar\\baz?*123"), "foo_bar_baz__123");
    }

    #[test]
    fn resolve_installer_url_handles_relative_and_absolute() {
        assert_eq!(
            resolve_installer_url(
                "https://github.com/davidef02/proxi/releases/latest/download/latest.json",
                "proxi-setup.exe"
            ),
            "https://github.com/davidef02/proxi/releases/latest/download/proxi-setup.exe"
        );
        assert_eq!(
            resolve_installer_url(
                "https://example.com/latest.json",
                "https://cdn.example.com/installer.exe"
            ),
            "https://cdn.example.com/installer.exe"
        );
        assert_eq!(
            resolve_installer_url(
                "http://127.0.0.1:4000/latest.json",
                "proxi-setup.exe"
            ),
            "http://127.0.0.1:4000/proxi-setup.exe"
        );
    }

    #[test]
    fn marker_roundtrip_and_clear() {
        let repo = temp_dir("marker-roundtrip");
        assert_eq!(read_pending(&repo), None);

        let pending = PendingUpdate {
            installer: "C:/path/to/installer.exe".to_string(),
            version: "9.9.9".to_string(),
        };
        write_pending(&repo, &pending).unwrap();

        assert_eq!(read_pending(&repo), Some(pending));

        clear_pending(&repo);
        assert_eq!(read_pending(&repo), None);
    }

    #[test]
    fn minisign_vectors_verify_and_refuse() {
        let pubkey = minisign_verify::PublicKey::decode(TEST_PUBKEY).unwrap();
        let sig = minisign_verify::Signature::decode(VALID_PREHASHED_SIG).unwrap();

        // Valid prehashed signature over b"test" verifies with allow_legacy = false
        assert!(pubkey.verify(b"test", &sig, false).is_ok());

        // Tampered payload b"Test" is refused
        assert!(pubkey.verify(b"Test", &sig, false).is_err());

        // Legacy signature is refused with allow_legacy = false
        let legacy = minisign_verify::Signature::decode(LEGACY_SIG).unwrap();
        assert!(pubkey.verify(b"test", &legacy, false).is_err());

        // Flipped-byte signature is refused
        let mut flipped_sig_str = VALID_PREHASHED_SIG.to_string();
        let target = "RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=";
        let mut flipped = target.to_string();
        flipped.replace_range(10..11, "A");
        flipped_sig_str = flipped_sig_str.replace(target, &flipped);
        let bad_sig = minisign_verify::Signature::decode(&flipped_sig_str).unwrap();
        assert!(pubkey.verify(b"test", &bad_sig, false).is_err());

        // Test verify_installer_signature helper with base64 of signature
        let sig_b64 = STANDARD.encode(VALID_PREHASHED_SIG.as_bytes());
        assert!(verify_installer_signature(TEST_PUBKEY, b"test", &sig_b64).is_ok());
        assert!(verify_installer_signature(TEST_PUBKEY, b"Test", &sig_b64).is_err());
    }

    #[test]
    fn embedded_updater_pubkey_is_valid() {
        assert!(minisign_verify::PublicKey::decode(UPDATER_PUBKEY).is_ok());
    }

    #[test]
    fn force_follows_js_truthiness() {
        for body in [
            r#"{"force":true}"#,
            r#"{"force":1}"#,
            r#"{"force":-1}"#,
            r#"{"force":0.5}"#,
            r#"{"force":"yes"}"#,
            r#"{"force":[]}"#,
            r#"{"force":{}}"#,
        ] {
            assert!(force_from_body(body.as_bytes()), "{body}");
        }
        for body in [
            r#"{"force":false}"#,
            r#"{"force":0}"#,
            r#"{"force":0.0}"#,
            r#"{"force":""}"#,
            r#"{"force":null}"#,
            r#"{}"#,
            "not json",
            "[]",
            r#""true""#,
            "true",
        ] {
            assert!(!force_from_body(body.as_bytes()), "{body}");
        }
    }

    #[tokio::test]
    async fn no_active_work_is_zero() {
        let work = NoActiveWork;
        assert_eq!(work.count(), 0);
        work.shutdown().await;
    }

    #[test]
    fn update_rs_keeps_no_git_or_cargo_path() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let update_rs = fs::read_to_string(manifest_dir.join("src").join("update.rs")).unwrap();
        let forbidden = [
            ["fn", " git("].concat(),
            ["Run", "Error"].concat(),
            ["kill", "_tree"].concat(),
            ["parse", "_count"].concat(),
            ["parse", "_log"].concat(),
            ["release", "_exe"].concat(),
            ["aside", "_exe"].concat(),
            ["build", "_pending"].concat(),
            ["remove", "_leftover"].concat(),
            ["DIRTY", "_TREE"].concat(),
            ["NO", "_UPSTREAM"].concat(),
            ["DIVER", "GED"].concat(),
            ["cargo", " build"].concat(),
            ["git", " pull"].concat(),
            ["Update", "CommitInfo"].concat(),
        ];
        for term in forbidden {
            assert!(
                !update_rs.contains(&term),
                "src/update.rs contains forbidden string: {term}"
            );
        }
    }
}
