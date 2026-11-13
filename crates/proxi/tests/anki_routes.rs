//! The Anki routes over HTTP against the in-process server: Node's exact texts and progress bytes.

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};

use common::anki_seed::{golden_cards, golden_fixture, open_state, seed, OFFLINE_ERRORS};
use common::fake_anki::{FakeAnki, FakeState};
use proxi::course_data::progress_file;
use proxi::server::{start, RunningServer, ServerOptions};

/// A fresh repo and data root; the server under test is started with an Anki URL per case.
struct Fixture {
    _root: common::TempDir,
    repo: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = common::temp_dir(tag);
        let repo = root.0.join("repo");
        let data = root.0.join("data");
        fs::create_dir_all(&repo).expect("create repo");
        fs::create_dir_all(&data).expect("create data");
        seed(&data);
        Self {
            _root: root,
            repo,
            data,
        }
    }

    async fn start(&self, anki_url: &str) -> RunningServer {
        let mut options = ServerOptions::new(self.repo.clone());
        options.data_root = Some(self.data.clone());
        options.endpoints.anki = Some(anki_url.to_string());
        start(options).await.expect("start server")
    }
}

async fn post(client: &Client, server: &RunningServer, path: &str, body: &str) -> Response {
    client
        .post(format!("{}{}", server.url, path))
        .header("x-learner-client", "1")
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .expect("POST request")
}

async fn text(response: Response) -> String {
    response.text().await.expect("read body")
}

#[tokio::test(flavor = "multi_thread")]
async fn status_all_answers_the_node_text_and_calls() {
    let fixture = Fixture::new("anki-routes-status");
    let fake = FakeAnki::start(open_state()).await;
    let server = fixture.start(&fake.url).await;
    let client = Client::new();

    let response = client
        .get(format!("{}/api/anki", server.url))
        .send()
        .await
        .expect("GET /api/anki");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        text(response).await,
        r#"{"online":true,"due":4,"newCards":5,"learning":2,"total":13,"byCourse":{"demo":{"due":4,"learning":2,"newCards":5},"vuoto":{"due":0,"learning":0,"newCards":0},"orfano":{"due":0,"learning":0,"newCards":0}}}"#
    );
    // The parsed first body has no `params` key at all, as in Node.
    assert_eq!(
        fake.requests(),
        vec![
            json!({"action": "deckNamesAndIds", "version": 6}),
            json!({
                "action": "getDeckStats",
                "version": 6,
                "params": {"decks": ["Proxi", "Proxi::Demo", "Proxi::Altro"]},
            }),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn status_for_course_reads_the_first_query_and_rejects_a_bad_slug() {
    let fixture = Fixture::new("anki-routes-course");
    let fake = FakeAnki::start(open_state()).await;
    let server = fixture.start(&fake.url).await;
    let client = Client::new();
    let get = async |path: &str| {
        client
            .get(format!("{}{path}", server.url))
            .send()
            .await
            .expect("GET /api/anki")
    };

    let demo = get("/api/anki?course=demo").await;
    assert_eq!(demo.status(), StatusCode::OK);
    assert_eq!(
        text(demo).await,
        r#"{"online":true,"due":4,"newCards":5,"learning":2,"total":10,"deck":"Proxi::Demo","deckExists":true}"#
    );

    let vuoto = get("/api/anki?course=vuoto").await;
    assert_eq!(vuoto.status(), StatusCode::OK);
    assert_eq!(
        text(vuoto).await,
        r#"{"online":true,"due":0,"newCards":0,"learning":0,"total":0,"deck":"Proxi::Vuoto","deckExists":false}"#
    );

    let bad = get("/api/anki?course=Bad").await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text(bad).await, r#"{"error":"Corso non valido"}"#);

    // Hono's c.req.query reads the first occurrence.
    let first = get("/api/anki?course=demo&course=Bad").await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        text(first).await,
        r#"{"online":true,"due":4,"newCards":5,"learning":2,"total":10,"deck":"Proxi::Demo","deckExists":true}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn review_opens_the_deck_and_maps_the_errors() {
    let fixture = Fixture::new("anki-routes-review");
    let fake = FakeAnki::start(open_state()).await;
    let server = fixture.start(&fake.url).await;
    let client = Client::new();

    let started = post(&client, &server, "/api/anki/review", r#"{"course":"demo"}"#).await;
    assert_eq!(started.status(), StatusCode::OK);
    assert_eq!(text(started).await, r#"{"ok":true}"#);
    assert_eq!(
        fake.requests(),
        vec![
            json!({"action": "deckNamesAndIds", "version": 6}),
            json!({"action": "getDeckStats", "version": 6, "params": {"decks": ["Proxi::Demo"]}}),
            json!({"action": "guiDeckReview", "version": 6, "params": {"name": "Proxi::Demo"}}),
        ]
    );

    let missing = post(&client, &server, "/api/anki/review", r#"{"course":"vuoto"}"#).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        text(missing).await,
        r#"{"error":"Nessun mazzo Anki per questo corso"}"#
    );
    // Only the deck lookup ran: no guiDeckReview for a course without a deck.
    assert_eq!(
        fake.actions(),
        [
            "deckNamesAndIds",
            "getDeckStats",
            "guiDeckReview",
            "deckNamesAndIds"
        ]
    );

    fake.update(|state| state.gui_deck_review_error = Some("boom".to_string()));
    let failed = post(&client, &server, "/api/anki/review", r#"{"course":"demo"}"#).await;
    assert_eq!(failed.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(text(failed).await, r#"{"error":"boom"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn import_writes_the_node_golden_and_the_second_run_adds_nothing() {
    let fixture = Fixture::new("anki-routes-import");
    let mut state = FakeState::default();
    state.cards = golden_cards();
    let fake = FakeAnki::start(state).await;
    let server = fixture.start(&fake.url).await;
    let client = Client::new();

    let first = post(&client, &server, "/api/anki/import", r#"{"course":"demo"}"#).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        text(first).await,
        r#"{"online":true,"imported":4,"skipped":2}"#
    );
    let log = progress_file(&fixture.data, "demo");
    let written = fs::read(&log).expect("read progress log");
    assert_eq!(
        written,
        fs::read(golden_fixture()).expect("read the Node golden"),
        "the route must append exactly the Node golden, byte for byte"
    );
    let query = fake
        .requests()
        .into_iter()
        .find(|request| request["action"] == "findCards")
        .expect("findCards request")["params"]["query"]
        .as_str()
        .expect("query string")
        .to_string();
    assert_eq!(query, r#"deck:"Proxi::Demo""#);

    let second = post(&client, &server, "/api/anki/import", r#"{"course":"demo"}"#).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        text(second).await,
        r#"{"online":true,"imported":0,"skipped":2}"#
    );
    assert_eq!(
        fs::read(&log).expect("read progress log"),
        written,
        "a second import must not add a line"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_imports_write_each_review_once() {
    let fixture = Fixture::new("anki-routes-import-concurrent");
    let mut state = FakeState::default();
    state.cards = golden_cards();
    // Slow enough that the second POST arrives while the first run is still going.
    state.find_cards_delay = Duration::from_millis(300);
    let fake = FakeAnki::start(state).await;
    let server = fixture.start(&fake.url).await;
    let client = Client::new();

    let (a, b) = tokio::join!(
        post(&client, &server, "/api/anki/import", r#"{"course":"demo"}"#),
        post(&client, &server, "/api/anki/import", r#"{"course":"demo"}"#),
    );
    let mut imported = Vec::new();
    for response in [a, b] {
        assert_eq!(response.status(), StatusCode::OK);
        let raw = text(response).await;
        let body: Value = serde_json::from_str(&raw).expect("import JSON");
        assert_eq!(body["online"], json!(true), "{raw}");
        assert_eq!(body["skipped"], json!(2), "{raw}");
        imported.push(body["imported"].as_u64().expect("imported count"));
    }
    imported.sort_unstable();
    // Overlapping requests share one run ({4,4}); a late second one just finds nothing new ({0,4}).
    assert!(imported == [0, 4] || imported == [4, 4], "{imported:?}");

    let written = fs::read(progress_file(&fixture.data, "demo")).expect("read progress log");
    assert_eq!(
        String::from_utf8_lossy(&written).lines().count(),
        5,
        "the seed line and the four imported reviews, once each"
    );
    assert_eq!(
        written,
        fs::read(golden_fixture()).expect("read the Node golden")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn guards_reject_the_client_header_and_unknown_methods() {
    let fixture = Fixture::new("anki-routes-guards");
    let fake = FakeAnki::start(open_state()).await;
    let server = fixture.start(&fake.url).await;
    let client = Client::new();

    let no_header = client
        .post(format!("{}/api/anki/import", server.url))
        .header(CONTENT_TYPE, "application/json")
        .body(r#"{"course":"demo"}"#)
        .send()
        .await
        .expect("POST without the header");
    assert_eq!(no_header.status(), StatusCode::FORBIDDEN);

    let wrong_method = client
        .get(format!("{}/api/anki/import", server.url))
        .send()
        .await
        .expect("GET /api/anki/import");
    assert_eq!(wrong_method.status(), StatusCode::NOT_FOUND);
    assert_eq!(text(wrong_method).await, r#"{"error":"Not found"}"#);

    let broken = post(&client, &server, "/api/anki/import", "{").await;
    assert_eq!(broken.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text(broken).await, r#"{"error":"Invalid JSON body"}"#);

    // None of the refused requests reached Anki.
    assert_eq!(fake.actions(), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_anki_reports_online_false() {
    let fixture = Fixture::new("anki-routes-closed");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a port to drop");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let anki_url = format!("http://127.0.0.1:{port}");
    let server = fixture.start(&anki_url).await;
    let client = Client::new();

    let response = client
        .get(format!("{}/api/anki", server.url))
        .send()
        .await
        .expect("GET /api/anki");
    assert_eq!(response.status(), StatusCode::OK);
    let raw = text(response).await;
    let body: Value = serde_json::from_str(&raw).expect("status JSON");
    let object = body.as_object().expect("status object");
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        ["online", "error"],
        "{raw}"
    );
    assert_eq!(object["online"], json!(false), "{raw}");
    let error = object["error"].as_str().expect("error string");
    assert!(OFFLINE_ERRORS.contains(&error), "{error}");
}
