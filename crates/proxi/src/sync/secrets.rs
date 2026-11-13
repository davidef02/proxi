//! Provider secrets: the OS keychain, the in-memory test store and the legacy migration.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const KEYCHAIN_SERVICE: &str = "proxi-sync";
pub const GITHUB_ACCOUNT: &str = "github";
pub const GDRIVE_ACCOUNT: &str = "gdrive";
/// Windows Credential Manager caps one credential at 2,560 bytes.
pub const MAX_SECRET_BYTES: usize = 2560;
pub const LEGACY_APP_IDS: [&str; 2] = ["proxi", "ia-teacher"];
pub const LOCKED_ERROR: &str = "I token salvati dalla vecchia app non sono leggibili: il file .learner/secrets.json resta al suo posto. Premi Accedi per riconnetterti.";
pub const UNREADABLE_ERROR: &str = "Il file dei token non è leggibile: riconnetti.";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GithubSecret {
    pub token: String,
    pub login: String,
    pub repo: String,
    pub branch: String,
}

/// Where a provider secret lives. `Err` is a message safe to show the learner.
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
    fn is_os_keychain(&self) -> bool;
}

/// The OS keychain: one entry per provider account.
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_string(),
        }
    }

    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }
}

fn too_long(len: usize) -> String {
    format!("token troppo lungo per il portachiavi ({len} byte, massimo {MAX_SECRET_BYTES})")
}

impl SecretStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        let entry = keyring::Entry::new(&self.service, account).map_err(|e| e.to_string())?;
        match entry.get_secret() {
            Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|e| e.to_string()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        if value.len() > MAX_SECRET_BYTES {
            return Err(too_long(value.len()));
        }
        let entry = keyring::Entry::new(&self.service, account).map_err(|e| e.to_string())?;
        entry.set_secret(value.as_bytes()).map_err(|e| e.to_string())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(&self.service, account).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn is_os_keychain(&self) -> bool {
        true
    }
}

/// Test double: same contract, no keychain, and a switch to simulate a failing write.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, String>>,
    pub fail_set: AtomicBool,
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.entries.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        if value.len() > MAX_SECRET_BYTES {
            return Err(too_long(value.len()));
        }
        if self.fail_set.load(Ordering::Relaxed) {
            return Err("archivio non disponibile".to_string());
        }
        self.entries
            .lock()
            .unwrap()
            .insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.entries.lock().unwrap().remove(account);
        Ok(())
    }

    fn is_os_keychain(&self) -> bool {
        false
    }
}

pub fn get_github(store: &dyn SecretStore) -> Result<Option<GithubSecret>, String> {
    let Some(raw) = store.get(GITHUB_ACCOUNT)? else {
        return Ok(None);
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|_| UNREADABLE_ERROR.to_string())
}

pub fn set_github(store: &dyn SecretStore, s: &GithubSecret) -> Result<(), String> {
    let raw = serde_json::to_string(s).map_err(|e| e.to_string())?;
    store.set(GITHUB_ACCOUNT, &raw)
}

pub fn get_gdrive(store: &dyn SecretStore) -> Result<Option<Value>, String> {
    let Some(raw) = store.get(GDRIVE_ACCOUNT)? else {
        return Ok(None);
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|_| UNREADABLE_ERROR.to_string())
}

pub fn legacy_file(repo_root: &Path) -> PathBuf {
    repo_root.join(".learner").join("secrets.json")
}

/// Decrypts `data` of a legacy `secrets.json` written by the desktop app.
pub trait LegacyDecryptor: Send + Sync {
    fn decrypt(&self, b64: &str) -> Result<String, String>;
}

/// Electron `safeStorage` on Windows: the AES key is wrapped with DPAPI in each profile's `Local State`.
pub struct ElectronDecryptor {
    pub profiles: Vec<PathBuf>,
}

impl ElectronDecryptor {
    pub fn default_profiles() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(dir) = std::env::var_os("LEARNER_USER_DATA") {
            if !dir.is_empty() {
                out.push(PathBuf::from(dir));
            }
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            if !appdata.is_empty() {
                for id in LEGACY_APP_IDS {
                    out.push(PathBuf::from(&appdata).join(id));
                }
            }
        }
        out
    }
}

#[cfg(windows)]
impl LegacyDecryptor for ElectronDecryptor {
    fn decrypt(&self, b64: &str) -> Result<String, String> {
        let mut failures = Vec::new();
        for profile in &self.profiles {
            match decrypt_profile(profile, b64) {
                Ok(plain) => return Ok(plain),
                Err(e) => failures.push(format!("{}: {e}", profile.display())),
            }
        }
        if failures.is_empty() {
            return Err("nessun profilo dell'app desktop trovato".to_string());
        }
        Err(format!(
            "nessun profilo ha decifrato i token ({})",
            failures.join("; ")
        ))
    }
}

#[cfg(not(windows))]
impl LegacyDecryptor for ElectronDecryptor {
    fn decrypt(&self, _b64: &str) -> Result<String, String> {
        Err("i token cifrati dall'app desktop si leggono solo su Windows".to_string())
    }
}

#[cfg(windows)]
fn decrypt_profile(profile: &Path, b64: &str) -> Result<String, String> {
    let state = fs::read_to_string(profile.join("Local State")).map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_str(&state).map_err(|e| e.to_string())?;
    let key_b64 = json
        .pointer("/os_crypt/encrypted_key")
        .and_then(Value::as_str)
        .ok_or("Local State senza os_crypt.encrypted_key")?;
    let wrapped = STANDARD.decode(key_b64).map_err(|e| e.to_string())?;
    let blob = wrapped
        .strip_prefix(b"DPAPI")
        .ok_or("chiave non protetta con DPAPI")?;
    let key = dpapi_unprotect(blob)?;
    if key.len() != 32 {
        return Err(format!("chiave di {} byte, attesi 32", key.len()));
    }
    let data = STANDARD.decode(b64).map_err(|e| e.to_string())?;
    let plain = decrypt_v10(&key, &data)?;
    String::from_utf8(plain).map_err(|e| e.to_string())
}

/// Electron's `v10` blob: 3-byte prefix, 12-byte nonce, AES-256-GCM ciphertext + tag.
pub fn decrypt_v10(key: &[u8], blob: &[u8]) -> Result<Vec<u8>, String> {
    if !blob.starts_with(b"v10") {
        return Err("formato dei token cifrati non riconosciuto".to_string());
    }
    if blob.len() < 3 + 12 + 16 {
        return Err("token cifrato troncato".to_string());
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| format!("chiave di {} byte, attesi 32", key.len()))?;
    cipher
        .decrypt(Nonce::from_slice(&blob[3..15]), &blob[15..])
        .map_err(|_| "decifratura fallita".to_string())
}

#[cfg(windows)]
fn dpapi_transform(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let out = if output.pbData.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() }
    };
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(out)
}

#[cfg(windows)]
pub fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    dpapi_transform(data, false)
}

#[cfg(windows)]
pub fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>, String> {
    dpapi_transform(data, true)
}

#[derive(Clone, Debug, PartialEq)]
pub enum LegacyState {
    Absent,
    Migrated,
    Locked(String),
    Unreadable,
}

fn unreadable(file: &Path) -> LegacyState {
    eprintln!("proxi: {} non è leggibile: riconnetti", file.display());
    LegacyState::Unreadable
}

fn locked(reason: String) -> LegacyState {
    eprintln!("proxi: secrets.json non migrato: {reason}");
    LegacyState::Locked(reason)
}

/// Moves `<repo>/.learner/secrets.json` into the store once, at service start. The file is deleted only
/// when every write succeeded, so a locked file survives for a later run with a working keychain.
pub fn migrate_legacy(
    repo_root: &Path,
    store: &dyn SecretStore,
    decryptor: &dyn LegacyDecryptor,
) -> LegacyState {
    let file = legacy_file(repo_root);
    let raw = match fs::read_to_string(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LegacyState::Absent,
        Err(e) => return locked(format!("{}: {e}", file.display())),
        Ok(raw) => raw,
    };
    let outer: Value = match serde_json::from_str(&raw) {
        Ok(v @ Value::Object(_)) => v,
        _ => return unreadable(&file),
    };
    let Some(data) = outer.get("data").and_then(Value::as_str) else {
        return unreadable(&file);
    };
    let plain = if outer.get("encrypted") == Some(&Value::Bool(true)) {
        match decryptor.decrypt(data) {
            Ok(plain) => plain,
            Err(e) => return locked(e),
        }
    } else {
        data.to_string()
    };
    let map: serde_json::Map<String, Value> = match serde_json::from_str(&plain) {
        Ok(Value::Object(map)) => map,
        _ => return unreadable(&file),
    };

    for (account, field) in [(GITHUB_ACCOUNT, "github"), (GDRIVE_ACCOUNT, "gdrive")] {
        let Some(value) = map.get(field) else {
            continue;
        };
        if !value.is_object() {
            continue;
        }
        if account == GITHUB_ACCOUNT
            && serde_json::from_value::<GithubSecret>(value.clone()).is_err()
        {
            return unreadable(&file);
        }
        match store.get(account) {
            Err(e) => return locked(e),
            // The keychain wins: the file may be older than a connect done from this binary.
            Ok(Some(_)) => continue,
            Ok(None) => {}
        }
        let Ok(text) = serde_json::to_string(value) else {
            return unreadable(&file);
        };
        if let Err(e) = store.set(account, &text) {
            return locked(e);
        }
    }

    if let Err(e) = fs::remove_file(&file) {
        eprintln!("proxi: secrets.json migrato ma non cancellato: {e}");
    }
    LegacyState::Migrated
}
