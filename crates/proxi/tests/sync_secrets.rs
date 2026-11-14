mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
#[cfg(windows)]
use proxi::sync::secrets::{ElectronDecryptor, KeychainStore, dpapi_protect};
use proxi::sync::secrets::{
    GDRIVE_ACCOUNT, GITHUB_ACCOUNT, GdriveSecret, GithubSecret, LegacyDecryptor, LegacyState,
    MAX_SECRET_BYTES, MemoryStore, SecretStore, UNREADABLE_ERROR, decrypt_v10, get_gdrive,
    get_github, legacy_file, migrate_legacy, set_gdrive, set_github,
};
use serde_json::{Value, json};

fn token() -> String {
    "t".repeat(12)
}

fn github(secret: &str) -> GithubSecret {
    GithubSecret {
        token: secret.to_string(),
        login: "davide".to_string(),
        repo: "davide/proxi-data".to_string(),
        branch: "main".to_string(),
    }
}

struct FailingDecryptor;

impl LegacyDecryptor for FailingDecryptor {
    fn decrypt(&self, _b64: &str) -> Result<String, String> {
        Err("decryptString failed".to_string())
    }
}

fn write_legacy(repo: &Path, value: Value) -> PathBuf {
    let file = legacy_file(repo);
    fs::create_dir_all(file.parent().unwrap()).expect("create .learner");
    fs::write(&file, serde_json::to_string(&value).unwrap()).expect("write secrets.json");
    file
}

fn plaintext_legacy(repo: &Path, secrets: Value) -> PathBuf {
    write_legacy(
        repo,
        json!({ "v": 1, "encrypted": false, "data": secrets.to_string() }),
    )
}

fn state_of(repo: &Path, store: &MemoryStore) -> LegacyState {
    migrate_legacy(repo, store, &FailingDecryptor)
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn memory_store_round_trip_and_delete_of_a_missing_entry_is_ok() {
    let store = MemoryStore::default();
    assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);
    assert!(!store.is_os_keychain());

    store.set(GITHUB_ACCOUNT, "{\"token\":\"t\"}").unwrap();
    assert_eq!(
        store.get(GITHUB_ACCOUNT).unwrap(),
        Some("{\"token\":\"t\"}".to_string())
    );

    store.delete(GITHUB_ACCOUNT).unwrap();
    assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);
    // Deleting an entry that is not there is not an error.
    store.delete(GITHUB_ACCOUNT).unwrap();
}

#[test]
fn a_value_over_2560_bytes_is_refused() {
    let store = MemoryStore::default();
    let long = "x".repeat(MAX_SECRET_BYTES + 1);
    let err = store.set(GITHUB_ACCOUNT, &long).unwrap_err();
    assert!(err.contains("2560"), "{err}");
    assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);

    store
        .set(GITHUB_ACCOUNT, &"x".repeat(MAX_SECRET_BYTES))
        .unwrap();
}

#[test]
fn get_github_of_malformed_json_is_unreadable() {
    let store = MemoryStore::default();
    store.set(GITHUB_ACCOUNT, "not json").unwrap();
    assert_eq!(get_github(&store), Err(UNREADABLE_ERROR.to_string()));

    let good = github(&token());
    set_github(&store, &good).unwrap();
    assert_eq!(get_github(&store).unwrap(), Some(good));
}

#[test]
fn set_gdrive_round_trips_in_node_key_order() {
    let store = MemoryStore::default();
    let secret = GdriveSecret {
        refresh_token: "rt-1".to_string(),
        folder_id: "folder-1".to_string(),
        email: Some("me@example.com".to_string()),
    };
    set_gdrive(&store, &secret).unwrap();
    assert_eq!(
        store.get(GDRIVE_ACCOUNT).unwrap().as_deref(),
        Some(r#"{"refreshToken":"rt-1","folderId":"folder-1","email":"me@example.com"}"#)
    );
    assert_eq!(get_gdrive(&store).unwrap(), Some(secret));

    let without_email = GdriveSecret {
        refresh_token: "rt-2".to_string(),
        folder_id: "folder-2".to_string(),
        email: None,
    };
    set_gdrive(&store, &without_email).unwrap();
    assert_eq!(
        store.get(GDRIVE_ACCOUNT).unwrap().as_deref(),
        Some(r#"{"refreshToken":"rt-2","folderId":"folder-2"}"#)
    );
    assert_eq!(get_gdrive(&store).unwrap(), Some(without_email));
}

#[test]
fn get_gdrive_of_malformed_json_is_unreadable() {
    let store = MemoryStore::default();
    store.set(GDRIVE_ACCOUNT, "not json").unwrap();
    assert_eq!(get_gdrive(&store), Err(UNREADABLE_ERROR.to_string()));

    store
        .set(GDRIVE_ACCOUNT, r#"{"refreshToken":"rt-1"}"#)
        .unwrap();
    assert_eq!(get_gdrive(&store), Err(UNREADABLE_ERROR.to_string()));
}

#[test]
fn a_migrated_gdrive_secret_without_email_reads_with_none() {
    let repo = common::temp_dir("secrets-gdrive-no-email");
    let store = MemoryStore::default();
    let file = plaintext_legacy(
        &repo.0,
        json!({ "gdrive": { "refreshToken": "r".repeat(12), "folderId": "folder-1" } }),
    );

    assert_eq!(state_of(&repo.0, &store), LegacyState::Migrated);

    assert_eq!(
        get_gdrive(&store).unwrap(),
        Some(GdriveSecret {
            refresh_token: "r".repeat(12),
            folder_id: "folder-1".to_string(),
            email: None,
        })
    );
    assert!(!file.exists());
}

#[test]
fn plaintext_legacy_file_is_migrated_and_deleted() {
    let repo = common::temp_dir("secrets-plain");
    let store = MemoryStore::default();
    let gh = github(&token());
    let gdrive = json!({
        "refreshToken": "r".repeat(12),
        "folderId": "folder-1",
        "email": "me@example.com",
    });
    let file = plaintext_legacy(&repo.0, json!({ "github": gh, "gdrive": gdrive }));

    assert_eq!(state_of(&repo.0, &store), LegacyState::Migrated);

    assert!(!file.exists());
    assert_eq!(get_github(&store).unwrap(), Some(gh));
    assert_eq!(
        get_gdrive(&store).unwrap(),
        Some(GdriveSecret {
            refresh_token: "r".repeat(12),
            folder_id: "folder-1".to_string(),
            email: Some("me@example.com".to_string()),
        })
    );
    // Verbatim: the same bytes the object had in the file.
    assert_eq!(
        store.get(GDRIVE_ACCOUNT).unwrap(),
        Some(serde_json::to_string(&gdrive).unwrap())
    );
}

#[test]
fn legacy_empty_object_is_deleted() {
    let repo = common::temp_dir("secrets-empty");
    let store = MemoryStore::default();
    let file = plaintext_legacy(&repo.0, json!({}));

    assert_eq!(state_of(&repo.0, &store), LegacyState::Migrated);
    assert!(!file.exists());
}

#[test]
fn an_entry_already_in_the_store_is_not_overwritten() {
    let repo = common::temp_dir("secrets-existing");
    let store = MemoryStore::default();
    let old = github(&"o".repeat(12));
    set_github(&store, &old).unwrap();
    let gdrive = json!({ "refreshToken": "r".repeat(12), "folderId": "folder-1" });
    let file = plaintext_legacy(&repo.0, json!({ "github": github(&token()), "gdrive": gdrive }));

    assert_eq!(state_of(&repo.0, &store), LegacyState::Migrated);

    assert_eq!(get_github(&store).unwrap(), Some(old));
    assert_eq!(
        get_gdrive(&store).unwrap(),
        Some(GdriveSecret {
            refresh_token: "r".repeat(12),
            folder_id: "folder-1".to_string(),
            email: None,
        })
    );
    assert!(!file.exists());
}

#[test]
fn encrypted_file_with_a_failing_decryptor_is_locked_and_kept() {
    let repo = common::temp_dir("secrets-locked");
    let store = MemoryStore::default();
    let file = write_legacy(
        &repo.0,
        json!({ "v": 1, "encrypted": true, "data": "djEwAAAA" }),
    );
    let before = fs::read(&file).unwrap();

    match state_of(&repo.0, &store) {
        LegacyState::Locked(reason) => assert!(!reason.is_empty()),
        other => panic!("expected Locked, got {other:?}"),
    }

    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);
}

#[test]
fn store_write_failure_is_locked_and_keeps_the_file() {
    let repo = common::temp_dir("secrets-fail-set");
    let store = MemoryStore::default();
    store.fail_set.store(true, Ordering::Relaxed);
    let file = plaintext_legacy(
        &repo.0,
        json!({ "github": github(&token()), "gdrive": json!({ "folderId": "f" }) }),
    );
    let before = fs::read(&file).unwrap();

    assert!(matches!(state_of(&repo.0, &store), LegacyState::Locked(_)));

    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);
    assert_eq!(store.get(GDRIVE_ACCOUNT).unwrap(), None);
}

#[test]
fn an_oversize_github_value_is_locked_and_keeps_the_file() {
    let repo = common::temp_dir("secrets-oversize");
    let store = MemoryStore::default();
    let huge = GithubSecret {
        token: "t".repeat(MAX_SECRET_BYTES),
        ..github("")
    };
    let file = plaintext_legacy(&repo.0, json!({ "github": huge }));
    let before = fs::read(&file).unwrap();

    assert!(matches!(state_of(&repo.0, &store), LegacyState::Locked(_)));

    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);
}

#[test]
fn malformed_json_or_a_non_string_data_is_unreadable_and_kept() {
    let repo = common::temp_dir("secrets-unreadable-raw");
    let file = legacy_file(&repo.0);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "not json").unwrap();
    let before = fs::read(&file).unwrap();
    assert_eq!(state_of(&repo.0, &MemoryStore::default()), LegacyState::Unreadable);
    assert_eq!(fs::read(&file).unwrap(), before);

    let cases = [
        json!({ "v": 1, "encrypted": false, "data": 42 }),
        json!({ "v": 1, "encrypted": false, "data": "[]" }),
        json!({
            "v": 1,
            "encrypted": false,
            "data": json!({ "github": { "token": 5 } }).to_string(),
        }),
    ];
    for outer in cases {
        let repo = common::temp_dir("secrets-unreadable");
        let store = MemoryStore::default();
        let file = write_legacy(&repo.0, outer);
        let before = fs::read(&file).unwrap();

        assert_eq!(state_of(&repo.0, &store), LegacyState::Unreadable);

        assert_eq!(fs::read(&file).unwrap(), before);
        assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);
    }
}

#[test]
fn no_file_under_repo_contains_the_token_after_migration() {
    let repo = common::temp_dir("secrets-ac1");
    let store = MemoryStore::default();
    let secret = token();
    let file = plaintext_legacy(&repo.0, json!({ "github": github(&secret) }));
    assert!(fs::read_to_string(&file).unwrap().contains(&secret));

    assert_eq!(state_of(&repo.0, &store), LegacyState::Migrated);

    assert!(!legacy_file(&repo.0).exists());
    let needle = secret.as_bytes();
    for path in files_under(&repo.0) {
        let bytes = fs::read(&path).unwrap();
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "{} contains the token",
            path.display()
        );
    }
}

#[test]
fn decrypt_v10_round_trips_and_rejects_bad_prefix_or_key() {
    let key = [7u8; 32];
    let nonce = [3u8; 12];
    let plain = br#"{"github":{"token":"t"}}"#;
    let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
    let ciphertext = cipher.encrypt(Nonce::from_slice(&nonce), plain.as_ref()).unwrap();
    let mut blob = b"v10".to_vec();
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);

    assert_eq!(decrypt_v10(&key, &blob).unwrap(), plain);
    assert!(decrypt_v10(&key, &blob[3..]).is_err());
    assert!(decrypt_v10(&[9u8; 32], &blob).is_err());
    assert!(decrypt_v10(&key, &blob[..20]).is_err());
}

#[cfg(windows)]
fn random_key() -> [u8; 32] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        | 1;
    let mut key = [0u8; 32];
    for byte in &mut key {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *byte = x as u8;
    }
    key
}

#[cfg(windows)]
#[test]
fn electron_encrypted_file_is_migrated_through_dpapi_and_local_state() {
    let repo = common::temp_dir("secrets-electron");
    let store = MemoryStore::default();
    let key = random_key();

    let profile = repo.0.join("profile");
    fs::create_dir_all(&profile).unwrap();
    let mut wrapped = b"DPAPI".to_vec();
    wrapped.extend_from_slice(&dpapi_protect(&key).unwrap());
    fs::write(
        profile.join("Local State"),
        serde_json::to_string(&json!({
            "os_crypt": { "encrypted_key": STANDARD.encode(&wrapped) },
        }))
        .unwrap(),
    )
    .unwrap();

    let gh = github(&token());
    let nonce = [1u8; 12];
    let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            json!({ "github": gh }).to_string().as_bytes(),
        )
        .unwrap();
    let mut blob = b"v10".to_vec();
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&encrypted);
    let file = write_legacy(
        &repo.0,
        json!({ "v": 1, "encrypted": true, "data": STANDARD.encode(&blob) }),
    );

    let decryptor = ElectronDecryptor {
        profiles: vec![repo.0.join("missing-profile"), profile],
    };
    assert_eq!(
        migrate_legacy(&repo.0, &store, &decryptor),
        LegacyState::Migrated
    );

    assert_eq!(get_github(&store).unwrap(), Some(gh));
    assert!(!file.exists());
}

#[cfg(windows)]
#[test]
fn keychain_round_trip_on_windows() {
    let store = KeychainStore::with_service(&format!("proxi-sync-test-{}", std::process::id()));
    let _ = store.delete(GITHUB_ACCOUNT);

    assert!(store.is_os_keychain());
    assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);

    store.set(GITHUB_ACCOUNT, "{\"token\":\"t\"}").unwrap();
    assert_eq!(
        store.get(GITHUB_ACCOUNT).unwrap(),
        Some("{\"token\":\"t\"}".to_string())
    );

    store.delete(GITHUB_ACCOUNT).unwrap();
    assert_eq!(store.get(GITHUB_ACCOUNT).unwrap(), None);
    store.delete(GITHUB_ACCOUNT).unwrap();

    let long = "x".repeat(MAX_SECRET_BYTES + 1);
    assert!(store.set(GITHUB_ACCOUNT, &long).is_err());
}
