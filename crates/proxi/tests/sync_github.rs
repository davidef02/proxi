//! The GitHub provider and device flow against a stateful fake (port of `app/test/sync-github.test.ts`).

#[path = "common/fake_http.rs"]
mod fake_http;
#[path = "common/fake_github.rs"]
mod fake_github;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use fake_github::{FakeGithub, FakeGithubOpts, FULL, SEED_DATE};
use proxi::sync::github::{
    CancelToken, DEFAULT_API, DEFAULT_WEB, DeviceCode, GithubDeps, GithubEndpoints, GithubProvider,
    Sleeper, git_blob_sha, poll_device_flow, setup_repo, start_device_flow,
};
use proxi::sync::types::{ProviderError, SyncProvider};

fn recording_deps(gh: &FakeGithub) -> (GithubDeps, Arc<Mutex<Vec<u64>>>) {
    let sleeps = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&sleeps);
    let sleep: Sleeper = Arc::new(move |duration: Duration, _: &CancelToken| {
        log.lock().unwrap().push(duration.as_millis() as u64);
    });
    (GithubDeps::new(gh.endpoints(), sleep), sleeps)
}

fn code(expires_in: f64) -> DeviceCode {
    DeviceCode {
        device_code: "dev-1".to_string(),
        user_code: "X".to_string(),
        verification_uri: "u".to_string(),
        expires_in,
        interval: 5.0,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn git_blob_sha_matches_gits_blob_id() {
    assert_eq!(
        git_blob_sha(b"hello\n"),
        "ce013625030ba8dba906f756967f9e9ca394464a"
    );
}

#[test]
fn device_flow_pending_slow_down_then_token() {
    let gh = FakeGithub::start(FakeGithubOpts {
        device_replies: vec![
            json!({ "error": "authorization_pending" }),
            json!({ "error": "slow_down", "interval": 10 }),
            json!({ "access_token": "tok" }),
        ],
        ..Default::default()
    });
    let (deps, sleeps) = recording_deps(&gh);

    let device = start_device_flow(&deps, "cid").unwrap();
    assert_eq!(
        device,
        DeviceCode {
            device_code: "dev-1".to_string(),
            user_code: "WDJB-MJHT".to_string(),
            verification_uri: "https://github.com/login/device".to_string(),
            expires_in: 900.0,
            interval: 5.0,
        }
    );
    assert_eq!(poll_device_flow(&deps, "cid", &device, &CancelToken::new()).unwrap(), "tok");
    assert_eq!(*sleeps.lock().unwrap(), vec![5000, 5000, 10000]);
}

#[test]
fn device_flow_slow_down_without_interval_adds_5_seconds() {
    let gh = FakeGithub::start(FakeGithubOpts {
        device_replies: vec![json!({ "error": "slow_down" }), json!({ "access_token": "t2" })],
        ..Default::default()
    });
    let (deps, sleeps) = recording_deps(&gh);

    let device = start_device_flow(&deps, "cid").unwrap();
    assert_eq!(poll_device_flow(&deps, "cid", &device, &CancelToken::new()).unwrap(), "t2");
    assert_eq!(*sleeps.lock().unwrap(), vec![5000, 10000]);
}

#[test]
fn device_flow_expired_denied_and_local_expiry() {
    let expired = FakeGithub::start(FakeGithubOpts {
        device_replies: vec![json!({ "error": "expired_token" })],
        ..Default::default()
    });
    let (deps, _) = recording_deps(&expired);
    assert_eq!(
        poll_device_flow(&deps, "cid", &code(900.0), &CancelToken::new()).unwrap_err(),
        "Codice scaduto: riprova."
    );

    let denied = FakeGithub::start(FakeGithubOpts {
        device_replies: vec![json!({ "error": "access_denied" })],
        ..Default::default()
    });
    let (deps, _) = recording_deps(&denied);
    assert_eq!(
        poll_device_flow(&deps, "cid", &code(900.0), &CancelToken::new()).unwrap_err(),
        "Accesso a GitHub negato."
    );

    let pending = FakeGithub::start(FakeGithubOpts {
        device_replies: vec![json!({ "error": "authorization_pending" }); 5],
        ..Default::default()
    });
    let (deps, sleeps) = recording_deps(&pending);
    assert_eq!(
        poll_device_flow(&deps, "cid", &code(10.0), &CancelToken::new()).unwrap_err(),
        "Codice scaduto: riprova."
    );
    assert_eq!(sleeps.lock().unwrap().len(), 2);
}

#[test]
fn device_flow_aborts_before_and_during_wait() {
    let gh = FakeGithub::start(FakeGithubOpts::default());
    let (deps, _) = recording_deps(&gh);

    let before = CancelToken::new();
    before.cancel();
    assert_eq!(
        poll_device_flow(&deps, "cid", &code(900.0), &before).unwrap_err(),
        "Annullato."
    );

    let during = CancelToken::new();
    let sleep: Sleeper = Arc::new(|_: Duration, token: &CancelToken| token.cancel());
    let deps = GithubDeps::new(gh.endpoints(), sleep);
    assert_eq!(
        poll_device_flow(&deps, "cid", &code(900.0), &during).unwrap_err(),
        "Annullato."
    );
}

#[test]
fn device_flow_refuses_empty_client_id() {
    let gh = FakeGithub::start(FakeGithubOpts::default());
    let (deps, _) = recording_deps(&gh);

    for message in [
        start_device_flow(&deps, "").unwrap_err(),
        poll_device_flow(&deps, "", &code(10.0), &CancelToken::new()).unwrap_err(),
    ] {
        assert!(message.contains("Client GitHub non configurato"), "{message}");
        assert!(
            message.contains("GITHUB_CLIENT_ID in crates/proxi/src/sync/github.rs"),
            "{message}"
        );
        assert!(
            message.contains("githubClientId in .learner/settings.json"),
            "{message}"
        );
        assert!(!message.contains("nelle impostazioni"), "{message}");
    }
}

#[test]
fn setup_repo_creates_private_auto_initialised_repo() {
    let gh = FakeGithub::start(FakeGithubOpts {
        repo: "none",
        ..Default::default()
    });
    let (deps, _) = recording_deps(&gh);

    let repo = setup_repo(&deps, "tok").unwrap();
    assert_eq!(repo.login, "davide");
    assert_eq!(repo.repo, FULL);
    assert_eq!(repo.branch, "main");

    let requests = gh.http.requests();
    let lookup = requests
        .iter()
        .find(|request| request.method == "GET" && request.path.starts_with("/repos/"))
        .expect("lookup request");
    assert_eq!(lookup.path, "/repos/davide/proxi-data");
    let post = requests
        .iter()
        .find(|request| request.method == "POST" && request.path == "/user/repos")
        .expect("create request");
    assert_eq!(post.json()["name"], "proxi-data");
    assert_eq!(post.json()["private"], true);
    assert_eq!(post.json()["auto_init"], true);
    assert_eq!(requests[0].header("authorization"), Some("Bearer tok"));
}

#[test]
fn setup_repo_refuses_public_repository() {
    let gh = FakeGithub::start(FakeGithubOpts {
        repo: "public",
        ..Default::default()
    });
    let (deps, _) = recording_deps(&gh);

    assert_eq!(
        setup_repo(&deps, "tok").unwrap_err(),
        "Il repository proxi-data è pubblico: rendilo privato o rinominalo, poi riprova."
    );
}

#[test]
fn list_maps_blobs_skips_symlinks_and_read_returns_bytes() {
    let gh = FakeGithub::start(FakeGithubOpts {
        seed: vec![("courses/a.md".to_string(), "hi".to_string())],
        symlinks: vec!["courses/link.md".to_string()],
        ..Default::default()
    });
    let (deps, _) = recording_deps(&gh);
    let mut provider = GithubProvider::new(deps, "tok", FULL, "main");

    let remote = provider.list().unwrap();
    let paths: Vec<&str> = remote.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(paths, ["courses/a.md"]);
    let (_, stat) = &remote[0];
    assert_eq!(stat.size, 2);
    assert_eq!(stat.token, git_blob_sha(b"hi"));
    assert_eq!(stat.mtime_ms, 1772359200000.0);
    assert_eq!(provider.read("courses/a.md").unwrap(), b"hi");
    assert_eq!(
        provider.read("nope.md").unwrap_err(),
        ProviderError::Other("File remoto sconosciuto: nope.md".to_string())
    );

    // The seed date drives the mtime; the constant is the same one the fake writes.
    assert_eq!(SEED_DATE, "2026-03-01T10:00:00Z");
}

#[test]
fn list_on_empty_repo_seeds_readme() {
    let gh = FakeGithub::start(FakeGithubOpts {
        empty_repo: true,
        ..Default::default()
    });
    let (deps, _) = recording_deps(&gh);
    let mut provider = GithubProvider::new(deps, "tok", FULL, "main");

    let remote = provider.list().unwrap();
    assert!(remote.iter().any(|(path, _)| path == "README.md"));
    assert_eq!(gh.count("PUT", "/contents/README.md"), 1);
}

#[test]
fn commit_with_two_writes_and_one_removal_makes_one_commit() {
    let gh = FakeGithub::start(FakeGithubOpts {
        seed: vec![
            ("courses/a.md".to_string(), "a".to_string()),
            ("courses/old.md".to_string(), "old".to_string()),
        ],
        ..Default::default()
    });
    let (deps, _) = recording_deps(&gh);
    let mut provider = GithubProvider::new(deps, "tok", FULL, "main");

    provider.list().unwrap();
    provider.write("courses/x.md", b"x", 0.0).unwrap();
    provider.write("courses/y.md", b"y", 0.0).unwrap();
    provider.remove("courses/old.md").unwrap();
    let head_before = gh.state.lock().unwrap().ref_sha.clone();
    gh.http.clear();

    provider.commit("sync: test").unwrap();

    assert_eq!(gh.count("POST", "/git/blobs"), 2);
    assert_eq!(gh.count("POST", "/git/trees"), 1);
    assert_eq!(gh.count("POST", "/git/commits"), 1);
    assert_eq!(gh.count("PATCH", "/git/refs/heads/main"), 1);

    let requests = gh.http.requests();
    let tree = requests
        .iter()
        .find(|request| request.method == "POST" && request.path.ends_with("/git/trees"))
        .expect("tree request")
        .json();
    let deleted = tree["tree"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "courses/old.md")
        .expect("deletion entry");
    assert_eq!(deleted["sha"], serde_json::Value::Null);

    let commit = requests
        .iter()
        .find(|request| request.method == "POST" && request.path.ends_with("/git/commits"))
        .expect("commit request")
        .json();
    assert_eq!(commit["message"], "sync: test");
    assert_eq!(commit["parents"], json!([head_before.unwrap()]));
    assert_eq!(
        requests
            .iter()
            .find(|request| request.method == "PATCH")
            .expect("ref request")
            .json()["force"],
        false
    );

    let mut paths: Vec<String> = provider
        .list()
        .unwrap()
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    paths.sort();
    assert_eq!(paths, ["courses/a.md", "courses/x.md", "courses/y.md"]);
}

#[test]
fn commit_with_nothing_staged_makes_no_request() {
    let gh = FakeGithub::start(FakeGithubOpts {
        seed: vec![("courses/a.md".to_string(), "a".to_string())],
        ..Default::default()
    });
    let (deps, _) = recording_deps(&gh);
    let mut provider = GithubProvider::new(deps, "tok", FULL, "main");

    provider.list().unwrap();
    gh.http.clear();
    provider.commit("noop").unwrap();
    assert!(gh.http.requests().is_empty());
}

#[test]
fn a_422_on_ref_update_is_remote_moved() {
    let gh = FakeGithub::start(FakeGithubOpts {
        seed: vec![("courses/a.md".to_string(), "a".to_string())],
        ..Default::default()
    });
    let (deps, _) = recording_deps(&gh);
    let mut provider = GithubProvider::new(deps, "tok", FULL, "main");

    provider.list().unwrap();
    provider.write("courses/b.md", b"b", 0.0).unwrap();
    gh.state.lock().unwrap().ref_moved = true;
    assert_eq!(provider.commit("m").unwrap_err(), ProviderError::RemoteMoved);
}

#[test]
fn retry_after_waits_advertised_seconds() {
    let gh = FakeGithub::start(FakeGithubOpts::default());
    gh.state.lock().unwrap().rate_limit_once = true;
    let (deps, sleeps) = recording_deps(&gh);

    let repo = setup_repo(&deps, "tok").unwrap();
    assert_eq!(repo.login, "davide");
    assert_eq!(*sleeps.lock().unwrap(), vec![1000]);
}

#[test]
fn rate_limit_reset_waits_until_reset() {
    let gh = FakeGithub::start(FakeGithubOpts::default());
    gh.state.lock().unwrap().rate_limit_reset_once = Some(now_secs() + 3);
    let (deps, sleeps) = recording_deps(&gh);
    assert_eq!(setup_repo(&deps, "tok").unwrap().login, "davide");
    let waits = sleeps.lock().unwrap().clone();
    assert_eq!(waits.len(), 1, "{waits:?}");
    assert!(waits[0] == 2000 || waits[0] == 3000, "{waits:?}");

    let gh = FakeGithub::start(FakeGithubOpts::default());
    gh.state.lock().unwrap().rate_limit_reset_once = Some(now_secs() + 600);
    let (deps, sleeps) = recording_deps(&gh);
    assert_eq!(setup_repo(&deps, "tok").unwrap().login, "davide");
    assert_eq!(*sleeps.lock().unwrap(), vec![60000]);

    let gh = FakeGithub::start(FakeGithubOpts::default());
    gh.state.lock().unwrap().rate_limit_reset_once = Some(0);
    let (deps, sleeps) = recording_deps(&gh);
    assert_eq!(setup_repo(&deps, "tok").unwrap().login, "davide");
    assert_eq!(*sleeps.lock().unwrap(), vec![0]);
}

#[test]
fn endpoints_from_override_serve_api_and_oauth() {
    let gh = FakeGithub::start(FakeGithubOpts::default());
    assert_eq!(
        GithubEndpoints::from_override(None),
        GithubEndpoints {
            api: DEFAULT_API.to_string(),
            web: DEFAULT_WEB.to_string(),
        }
    );
    assert_eq!(
        gh.endpoints(),
        GithubEndpoints {
            api: gh.http.url.clone(),
            web: gh.http.url.clone(),
        }
    );

    let (deps, _) = recording_deps(&gh);
    let device = start_device_flow(&deps, "cid").unwrap();
    assert_eq!(device.user_code, "WDJB-MJHT");
    assert_eq!(setup_repo(&deps, "tok").unwrap().repo, FULL);
    assert_eq!(gh.count("POST", "/login/device/code"), 1);
    assert_eq!(gh.count("GET", "/user"), 1);
}

#[test]
fn blob_content_with_newlines_decodes() {
    let text = "x".repeat(200);
    let gh = FakeGithub::start(FakeGithubOpts {
        seed: vec![("courses/long.md".to_string(), text.clone())],
        ..Default::default()
    });
    let (deps, _) = recording_deps(&gh);
    let mut provider = GithubProvider::new(deps, "tok", FULL, "main");

    let remote = provider.list().unwrap();
    let token = &remote[0].1.token;
    let text_body = reqwest::blocking::get(format!("{}/repos/{FULL}/git/blobs/{token}", gh.http.url))
        .unwrap()
        .text()
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&text_body).unwrap();
    assert!(body["content"].as_str().unwrap().contains('\n'));
    assert_eq!(provider.read("courses/long.md").unwrap(), text.as_bytes());
}
