//! The Anki routes over HTTP against the in-process server: Node's exact texts and progress bytes.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};

use common::fake_anki::{card, FakeAnki, FakeCard, FakeState};
use proxi::course_data::progress_file;
use proxi::server::{start, RunningServer, ServerOptions};

const CURRICULUM: &str = r#"{"chapters":[{"title":"Demo","slug":"demo","lessons":[{"title":"Lesson T","slug":"t","plannedNodes":2},{"title":"Lesson U","slug":"u","plannedNodes":1}]},{"title":"Vuoto","slug":"vuoto","lessons":[]}]}"#;

const SEED_LINE: &str = "{\"at\":\"2023-11-14T00:00:00.000Z\",\"sessionId\":\"s1\",\"course\":\"demo\",\"lessonSlug\":\"t\",\"nodeRefs\":[\"demo/t#alpha\"],\"kind\":\"check\",\"correct\":true}\n";

/// Both a refused connection and Windows' slow refusal are acceptable.
const OFFLINE_ERRORS: [&str; 2] = ["fetch failed", "AnkiConnect request timed out"];

fn lesson_doc(lesson_number: i64, title: &str, slug: &str, node_ids: &[&str]) -> Value {
    json!({
        "metadata": {
            "lessonNumber": lesson_number,
            "topic": "Demo",
            "title": title,
            "slug": slug,
            "dateCreated": "",
            "dateModified": "",
            "version": "1",
        },
        "nodes": node_ids
            .iter()
            .map(|node_id| json!({ "nodeId": node_id, "title": node_id, "tier": "derivation", "content": "x" }))
            .collect::<Vec<_>>(),
    })
}

fn write_lesson(data_root: &Path, course: &str, filename: &str, doc: Value) {
    let dir = data_root.join("courses").join(course).join("lessons");
    fs::create_dir_all(&dir).expect("create lessons dir");
    fs::write(
        dir.join(filename),
        format!(r#"<script type="application/json" id="lesson-data">{doc}</script>"#),
    )
    .expect("write lesson");
}

/// The api-anki seed: Demo and Vuoto planned, orfano only on disk, one check already logged.
fn seed(data_root: &Path) {
    fs::write(data_root.join("curriculum.json"), CURRICULUM).expect("write curriculum");
    write_lesson(data_root, "demo", "0001-t.html", lesson_doc(1, "Lesson T", "t", &["alpha", "dup"]));
    write_lesson(data_root, "demo", "0002-u.html", lesson_doc(2, "Lesson U", "u", &["dup"]));
    write_lesson(data_root, "orfano", "0001-o.html", lesson_doc(1, "O", "o", &["x"]));
    fs::write(progress_file(data_root, "demo"), SEED_LINE).expect("seed progress log");
}

fn open_state() -> FakeState {
    FakeState {
        deck_names_and_ids: Some(json!({
            "Proxi": 1,
            "Proxi::Demo": 2,
            "Proxi::Altro": 3,
            "Default": 4,
        })),
        deck_stats: Some(json!({
            "1": { "deck_id": 1, "name": "Proxi", "new_count": 5, "learn_count": 2, "review_count": 4, "total_in_deck": 0 },
            "2": { "deck_id": 2, "name": "Demo", "new_count": 5, "learn_count": 2, "review_count": 4, "total_in_deck": 10 },
            "3": { "deck_id": 3, "name": "Altro", "new_count": 1, "learn_count": 0, "review_count": 0, "total_in_deck": 3 },
        })),
        ..FakeState::default()
    }
}

/// The six cards of the Node golden: two skipped, four reviews written.
fn golden_cards() -> Vec<FakeCard> {
    vec![
        card(
            1001,
            2001,
            "Proxi::Demo",
            &["node::alpha"],
            10,
            0,
            &[(1_700_000_002_000, 1), (1_700_000_001_000, 3)],
        ),
        card(
            1002,
            2002,
            "Proxi::Demo",
            &["NODE::dup", "lesson::u"],
            -600,
            2,
            &[(1_700_000_003_000, 4), (1_700_000_004_000, 0)],
        ),
        card(1003, 2003, "Proxi::Demo", &["node::dup"], 1, 0, &[(1_700_000_005_000, 3)]),
        card(1004, 2004, "Proxi::Demo", &[], 1, 0, &[(1_700_000_006_000, 3)]),
        card(
            1005,
            2005,
            "Proxi::Demo::Sub",
            &["node::alpha", "lesson::t"],
            1,
            0,
            &[(1_700_000_000_500, 2)],
        ),
        card(
            1006,
            2006,
            "Altro::Demo",
            &["node::alpha"],
            1,
            0,
            &[(1_700_000_007_000, 3)],
        ),
    ]
}

fn golden_fixture() -> PathBuf {
    common::repo_root()
        .join("app")
        .join("test")
        .join("fixtures")
        .join("anki")
        .join("import-progress.jsonl")
}

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
