//! The courses, lesson-reading and /files routes over HTTP, against a real server.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use reqwest::header::{
    ACCESS_CONTROL_ALLOW_ORIGIN, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST,
    X_CONTENT_TYPE_OPTIONS,
};
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};

use proxi::server::{start, RunningServer, ServerOptions};

const MISSING_UI: &str =
    "Proxi: la UI non è stata compilata. Esegui `npm run app:build` e riapri questa pagina.";

/// A temp repo and data folder; the files on disk are the fixture.
struct Fixture {
    root: common::TempDir,
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
        Self { root, repo, data }
    }

    fn options(&self) -> ServerOptions {
        let mut options = ServerOptions::new(self.repo.clone());
        options.data_root = Some(self.data.clone());
        // No build: a read outside /files answers MISSING_UI, so a /files 200 proves the exemption.
        options.dist_dir = Some(self.root.0.join("missing-dist"));
        options
    }

    fn data_file(&self, rel: &str, content: &str) {
        write(&self.data, rel, content);
    }

    fn repo_file(&self, rel: &str, content: &str) {
        write(&self.repo, rel, content);
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("a parent")).expect("create parent dir");
    fs::write(path, content).expect("write fixture file");
}

fn url(server: &RunningServer, path: &str) -> String {
    format!("{}{}", server.url, path)
}

async fn body_text(response: Response) -> String {
    response.text().await.expect("read body")
}

async fn get_text(client: &Client, server: &RunningServer, path: &str) -> (StatusCode, String) {
    let response = client
        .get(url(server, path))
        .send()
        .await
        .expect("GET request");
    let status = response.status();
    (status, body_text(response).await)
}

fn header(response: &Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().expect("ascii header").to_string())
}

/// A lesson artifact the way the engine writes it.
fn lesson_html(num: i64, slug: &str, title: &str, nodes: usize, dag: &str) -> String {
    let doc = json!({
        "metadata": {
            "lessonNumber": num,
            "topic": title,
            "title": title,
            "slug": slug,
            "dateCreated": "",
            "dateModified": "",
            "version": "1",
            "initialDag": dag,
        },
        "nodes": (1..=nodes)
            .map(|i| json!({
                "nodeId": format!("n{i}"),
                "title": format!("Nodo {i}"),
                "tier": "axiom",
                "content": "x",
            }))
            .collect::<Vec<Value>>(),
    });
    format!(
        r#"<html><head><title>T</title></head><body><script type="application/json" id="lesson-data">{doc}</script></body></html>"#
    )
}

/// The demo course shared by the listing cases (same fixture as api-courses.test.ts).
fn seed_demo(data: &Path) {
    write(
        data,
        "curriculum.json",
        &json!({
            "chapters": [{
                "title": "Demo",
                "slug": "demo",
                "lessons": [
                    {"title": "Prima", "slug": "prima", "plannedNodes": 2},
                    {"title": "Seconda", "slug": "seconda"},
                ],
            }],
        })
        .to_string(),
    );
    write(
        data,
        "courses/demo/MISSION.md",
        "# M\n\n## Why\n\nImparare **Vue** `ref`\nin due righe.\n",
    );
    write(data, "courses/demo/exercises/package.json", "{}");
    write(
        data,
        "courses/demo/lessons/0001-prima.html",
        &lesson_html(
            1,
            "prima",
            "Prima",
            1,
            "graph TD\nN1[a] --> N2[b]\nN2 --> N3[c]",
        ),
    );
    write(data, "courses/demo/reference/prima.html", "<html></html>");
    write(
        data,
        "courses/demo/lessons/0002-extra.html",
        &lesson_html(2, "extra", "Extra", 1, ""),
    );
    write(
        data,
        "courses/loose/lessons/0001-solo.html",
        &lesson_html(1, "solo", "Solo", 1, ""),
    );
}

/// A raw request line: reqwest (like fetch) would normalise the dot segments away.
async fn raw_get(port: u16, path: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read response");
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").expect("response headers");
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status code");
    (status, body.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn courses_and_standalone_routes_are_served() {
    let fixture = Fixture::new("courses-api-lists");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    // Nothing studied yet: [] and courses/ is not created (the first-run sync depends on it).
    let (status, body) = get_text(&client, &server, "/api/courses").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "[]");
    assert_eq!(fs::read_dir(&fixture.data).expect("read data").count(), 0);

    seed_demo(&fixture.data);
    let expected = json!([
        {
            "slug": "demo",
            "title": "Demo",
            "lessons": [
                {
                    "title": "Prima",
                    "slug": "prima",
                    "lessonPath": "courses/demo/lessons/0001-prima.html",
                    "lessonNumber": 1,
                    "nodeCount": 1,
                    "referencePath": "courses/demo/reference/prima.html",
                    "inProgress": true,
                    "plannedNodes": 2,
                },
                {"title": "Seconda", "slug": "seconda"},
            ],
            "orphans": [
                {
                    "title": "Extra",
                    "slug": "extra",
                    "lessonPath": "courses/demo/lessons/0002-extra.html",
                    "lessonNumber": 2,
                    "nodeCount": 1,
                    "inProgress": false,
                },
            ],
            "hasExercises": true,
            "hasMission": true,
            "destination": "Imparare Vue ref in due righe.",
        },
        {
            "slug": "loose",
            "title": "loose",
            "lessons": [],
            "orphans": [
                {
                    "title": "Solo",
                    "slug": "solo",
                    "lessonPath": "courses/loose/lessons/0001-solo.html",
                    "lessonNumber": 1,
                    "nodeCount": 1,
                    "inProgress": false,
                },
            ],
            "hasExercises": false,
            "hasMission": false,
        },
    ]);
    // Serialised string: key order is part of the response.
    let (status, body) = get_text(&client, &server, "/api/courses").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, expected.to_string());
    assert!(
        fixture
            .data
            .join("courses/lezioni-singole/MISSION.md")
            .is_file(),
        "the standalone container is provisioned"
    );

    // A learning record closes the lesson.
    fixture.data_file("courses/demo/learning-records/0007-y.md", "# y\n\n<!-- lesson: prima -->\n");
    let (_, body) = get_text(&client, &server, "/api/courses").await;
    let listed: Value = serde_json::from_str(&body).expect("courses JSON");
    assert_eq!(listed[0]["lessons"][0]["slug"], "prima");
    assert_eq!(listed[0]["lessons"][0]["inProgress"], false);

    // Standalone lessons: in lesson order, with the status and the reference sheet.
    fixture.data_file(
        "courses/lezioni-singole/lessons/0001-trasformata-di-fourier.html",
        &lesson_html(
            1,
            "trasformata-di-fourier",
            "La trasformata di Fourier scompone un segnale",
            2,
            "",
        ),
    );
    fixture.data_file(
        "courses/lezioni-singole/reference/trasformata-di-fourier.html",
        "<html></html>",
    );
    fixture.data_file(
        "courses/lezioni-singole/learning-records/0001-chiusa.md",
        "# chiusa\n\n<!-- lesson: trasformata-di-fourier -->\n",
    );
    fixture.data_file(
        "courses/lezioni-singole/lessons/0002-entropia.html",
        &lesson_html(
            2,
            "entropia",
            "L'entropia di Shannon misura la sorpresa media",
            0,
            "graph TD\nN1[a] --> N2[b]",
        ),
    );
    let expected = json!([
        {
            "title": "La trasformata di Fourier scompone un segnale",
            "slug": "trasformata-di-fourier",
            "lessonPath": "courses/lezioni-singole/lessons/0001-trasformata-di-fourier.html",
            "lessonNumber": 1,
            "nodeCount": 2,
            "referencePath": "courses/lezioni-singole/reference/trasformata-di-fourier.html",
            "inProgress": false,
        },
        {
            "title": "L'entropia di Shannon misura la sorpresa media",
            "slug": "entropia",
            "lessonPath": "courses/lezioni-singole/lessons/0002-entropia.html",
            "lessonNumber": 2,
            "nodeCount": 0,
            "inProgress": true,
        },
    ]);
    let (status, body) = get_text(&client, &server, "/api/standalone-lessons").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, expected.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_curriculum_is_400_invalid_json() {
    let fixture = Fixture::new("courses-api-bad-json");
    fixture.data_file("curriculum.json", "{");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let (status, body) = get_text(&client, &server, "/api/courses").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, r#"{"error":"Invalid JSON body"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn lesson_route_statuses_and_messages() {
    let fixture = Fixture::new("courses-api-lesson");
    // Keys in a non-alphabetical order and an unknown top-level key: the island is echoed as it is.
    let island = json!({
        "metadata": {
            "topic": "Prima",
            "title": "Prima",
            "slug": "prima",
            "lessonNumber": 1,
            "dateCreated": "",
            "dateModified": "",
            "version": "1",
            "initialDag": "",
        },
        "nodes": [{"nodeId": "n1", "title": "Nodo 1", "tier": "axiom", "content": "x"}],
        "zeta": {"b": 1, "a": 2},
    });
    fixture.data_file(
        "courses/demo/lessons/0001-prima.html",
        &format!(
            r#"<html><script type="application/json" id="lesson-data">{island}</script></html>"#
        ),
    );
    fixture.data_file("courses/demo/lessons/0002-prosa.html", "<html>prosa</html>");
    fixture.data_file(
        "courses/demo/lessons/0003-rotta.html",
        r#"<html><script type="application/json" id="lesson-data">{nope</script></html>"#,
    );
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let (status, body) = get_text(
        &client,
        &server,
        "/api/lesson?path=courses/demo/lessons/0001-prima.html",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, island.to_string());

    // The query is decoded: %2F is a separator.
    let (status, body) = get_text(
        &client,
        &server,
        "/api/lesson?path=courses%2Fdemo%2Flessons%2F0001-prima.html",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, island.to_string());

    // A `+` in a query is a space (Hono's getQueryParam), and a space breaks the lesson rule.
    let (status, body) = get_text(
        &client,
        &server,
        "/api/lesson?path=courses/demo/lessons/0001+prima.html",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, r#"{"error":"Percorso della lezione non valido"}"#);

    let missing = get_text(
        &client,
        &server,
        "/api/lesson?path=courses/demo/lessons/0099-no.html",
    )
    .await;
    assert_eq!(missing.0, StatusCode::NOT_FOUND);
    assert_eq!(missing.1, r#"{"error":"Lezione non trovata"}"#);

    let prose = get_text(
        &client,
        &server,
        "/api/lesson?path=courses/demo/lessons/0002-prosa.html",
    )
    .await;
    assert_eq!(prose.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(prose.1, r#"{"error":"La lezione non ha dati strutturati"}"#);

    let malformed = get_text(
        &client,
        &server,
        "/api/lesson?path=courses/demo/lessons/0003-rotta.html",
    )
    .await;
    assert_eq!(malformed.0, StatusCode::UNPROCESSABLE_ENTITY);
    let parsed: Value = serde_json::from_str(&malformed.1).expect("malformed JSON error");
    let message = parsed["error"].as_str().expect("a string error");
    assert!(message.starts_with("Malformed #lesson-data island:"), "{message}");

    // A directory named like a lesson: Node's readFileSync gives EISDIR -> 500.
    fs::create_dir_all(fixture.data.join("courses/demo/lessons/0004-dir.html"))
        .expect("directory lesson");
    let dir = get_text(
        &client,
        &server,
        "/api/lesson?path=courses/demo/lessons/0004-dir.html",
    )
    .await;
    assert_eq!(dir.0, StatusCode::INTERNAL_SERVER_ERROR);
    let parsed: Value = serde_json::from_str(&dir.1).expect("directory JSON error");
    assert!(parsed["error"].is_string(), "{}", dir.1);

    for path in [
        "../secret.txt",
        "courses/demo/MISSION.md",
        "courses/demo/lessons/..%2f..%2fMISSION.md",
        "courses/demo/lessons/../../../secret.html",
        "",
    ] {
        let (status, body) = get_text(&client, &server, &format!("/api/lesson?path={path}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body, r#"{"error":"Percorso della lezione non valido"}"#, "{path}");
    }
    let (status, body) = get_text(&client, &server, "/api/lesson").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, r#"{"error":"Percorso della lezione non valido"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn doc_and_reference_statuses_and_messages() {
    let fixture = Fixture::new("courses-api-doc");
    fixture.data_file(
        "courses/c/reference/x.html",
        r#"<html><head><title> T </title><style>a{}</style><link rel=x></head><body><script>1</script><main><h2>Ciao</h2></main></body></html>"#,
    );
    fixture.data_file(
        "courses/c/lessons/0001-a.html",
        "<html><head></head><body><p>x</p></body></html>",
    );
    let island = json!({
        "title": "Sintesi",
        "slug": "x",
        "nodes": [],
        "formulas": [],
        "misconceptions": [],
    });
    fixture.data_file(
        "courses/c/reference/isola.html",
        &format!(
            r#"<html><body><script type="application/json" id="reference-data">{island}</script></body></html>"#
        ),
    );
    let dag = "graph TD\nN1[a] --> N2[b]";
    fixture.data_file(
        "courses/c/lessons/0002-b.html",
        &lesson_html(2, "b", "b", 2, dag),
    );
    fixture.data_file(
        "courses/c/learning-records/0002-b.md",
        "# Rec\n\n## Evidence\n\n- Learner slip A\n\n<!-- lesson: b -->\n",
    );
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let (status, body) = get_text(
        &client,
        &server,
        "/api/doc?path=courses/c/reference/x.html",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"title":"T","html":"<h2>Ciao</h2>"}"#);

    let (status, body) = get_text(
        &client,
        &server,
        "/api/doc?path=courses/c/lessons/0001-a.html",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let doc: Value = serde_json::from_str(&body).expect("doc JSON");
    assert_eq!(doc.get("title"), None, "{body}");
    assert_eq!(doc["html"], "<p>x</p>");

    let missing = get_text(
        &client,
        &server,
        "/api/doc?path=courses/c/reference/nope.html",
    )
    .await;
    assert_eq!(missing.0, StatusCode::NOT_FOUND);
    assert_eq!(missing.1, r#"{"error":"Documento non trovato"}"#);

    let bad = get_text(
        &client,
        &server,
        "/api/doc?path=courses/c/exercises/x.html",
    )
    .await;
    assert_eq!(bad.0, StatusCode::BAD_REQUEST);
    assert_eq!(bad.1, r#"{"error":"Percorso del documento non valido"}"#);

    let (status, body) = get_text(
        &client,
        &server,
        "/api/reference?path=courses/c/reference/isola.html",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, island.to_string());

    let (status, body) = get_text(
        &client,
        &server,
        "/api/reference?path=courses/c/reference/b.html",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let reference: Value = serde_json::from_str(&body).expect("reference JSON");
    assert_eq!(reference["title"], "b");
    assert_eq!(reference["nodes"].as_array().expect("nodes").len(), 2);
    assert_eq!(reference["initialDag"], dag);
    assert!(
        reference["misconceptions"]
            .as_array()
            .expect("misconceptions")
            .iter()
            .any(|item| item["description"]
                .as_str()
                .is_some_and(|text| text.contains("Learner slip A"))),
        "{body}"
    );

    for path in ["courses/c/reference/nope.html", "courses/c/lessons/0002-b.html"] {
        let (status, body) =
            get_text(&client, &server, &format!("/api/reference?path={path}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, r#"{"error":"Sintesi non trovata"}"#, "{path}");
    }

    let (status, body) = get_text(
        &client,
        &server,
        "/api/reference?path=courses/c/exercises/x.html",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, r#"{"error":"Percorso del documento non valido"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn files_bypass_missing_ui_and_harden_headers() {
    let fixture = Fixture::new("courses-api-files");
    fixture.repo_file("assets/x.css", "body{}");
    fixture.data_file("courses/demo/MISSION.md", "# Demo\n\nCorpo è qui.\n");
    fixture.data_file(
        "courses/demo/lessons/0001-a.html",
        r#"<!doctype html><html><head><meta charset="utf-8"></head><body>a</body></html>"#,
    );
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    // No dist built: every other read answers the hint ...
    let (status, body) = get_text(&client, &server, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, MISSING_UI);

    // ... while /files still serves the data folder and the repo.
    let html = client
        .get(url(&server, "/files/courses/demo/lessons/0001-a.html"))
        .send()
        .await
        .expect("lesson file");
    assert_eq!(html.status(), StatusCode::OK);
    assert_eq!(
        header(&html, CONTENT_TYPE).as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(header(&html, X_CONTENT_TYPE_OPTIONS).as_deref(), Some("nosniff"));
    assert_eq!(
        header(&html, CONTENT_SECURITY_POLICY).as_deref(),
        Some(proxi::files::LESSON_FILE_CSP)
    );
    assert_eq!(header(&html, ACCESS_CONTROL_ALLOW_ORIGIN), None);
    let body = body_text(html).await;
    let head = body.find("<head>").expect("head tag") + "<head>".len();
    assert_eq!(
        body.find("<script>try{localStorage").expect("storage shim"),
        head,
        "{body}"
    );

    // The route answers HEAD too: same headers, no body.
    let head_response = client
        .head(url(&server, "/files/courses/demo/lessons/0001-a.html"))
        .send()
        .await
        .expect("lesson file HEAD");
    assert_eq!(head_response.status(), StatusCode::OK);
    assert_eq!(
        header(&head_response, CONTENT_SECURITY_POLICY).as_deref(),
        Some(proxi::files::LESSON_FILE_CSP)
    );
    assert_eq!(body_text(head_response).await, "");

    let css = client
        .get(url(&server, "/files/assets/x.css"))
        .send()
        .await
        .expect("css file");
    assert_eq!(css.status(), StatusCode::OK);
    assert_eq!(
        header(&css, CONTENT_TYPE).as_deref(),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(header(&css, ACCESS_CONTROL_ALLOW_ORIGIN).as_deref(), Some("*"));
    assert_eq!(header(&css, CONTENT_SECURITY_POLICY), None);
    assert_eq!(body_text(css).await, "body{}");

    let md = client
        .get(url(&server, "/files/courses/demo/MISSION.md"))
        .send()
        .await
        .expect("mission file");
    assert_eq!(md.status(), StatusCode::OK);
    assert_eq!(
        header(&md, CONTENT_TYPE).as_deref(),
        Some("text/markdown; charset=utf-8")
    );
    assert_eq!(header(&md, X_CONTENT_TYPE_OPTIONS).as_deref(), Some("nosniff"));
    assert_eq!(header(&md, CONTENT_SECURITY_POLICY), None);
    assert_eq!(header(&md, ACCESS_CONTROL_ALLOW_ORIGIN), None);
    assert_eq!(body_text(md).await, "# Demo\n\nCorpo è qui.\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn files_refuse_escapes() {
    let fixture = Fixture::new("courses-api-escapes");
    fixture.repo_file("assets/x.css", "body{}");
    fixture.repo_file("secret.txt", "TOPSECRET");
    fixture.repo_file("courses/repo-only/MISSION.md", "# repo\n");
    fixture.data_file("courses/demo/MISSION.md", "# Demo\n");
    fixture.data_file("curriculum.json", r#"{"chapters":[]}"#);
    fixture.data_file("secret.txt", "TOPSECRET");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    for path in [
        "/files/secret.txt",
        "/files/curriculum.json",
        "/files/..%2fsecret.txt",
        "/files/courses/..%2f..%2fsecret.txt",
        "/files/courses/..%5csecret.txt",
        "/files/courses/%00",
        "/files/courses/repo-only/MISSION.md",
        "/files/",
        "/files/courses/demo",
    ] {
        let (status, body) = get_text(&client, &server, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, r#"{"error":"Not found"}"#, "{path}");
    }

    let (status, body) = get_text(&client, &server, "/files/%E0%A4%A").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, r#"{"error":"Bad request"}"#);

    // Raw request lines: a normal client resolves the dot segments before sending.
    for path in ["/files/../secret.txt", "/files/%2e%2e/secret.txt"] {
        let (status, body) = raw_get(server.port, path).await;
        assert_eq!(status, 404, "{path}");
        assert!(!body.contains("TOPSECRET"), "{path}: {body}");
    }

    // A junction inside courses/ must not lead outside the served roots.
    let outside = fixture.root.0.join("outside");
    fs::create_dir_all(&outside).expect("create outside dir");
    fs::write(outside.join("leak.txt"), "LEAKED").expect("write leak");
    let link = fixture.data.join("courses").join("demo").join("link");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
    if made {
        let (status, body) = get_text(&client, &server, "/files/courses/demo/link/leak.txt").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.contains("LEAKED"), "{body}");
    } else {
        eprintln!("symlinks not allowed here: skipping the escape assertion");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn post_to_courses_is_404_json() {
    let fixture = Fixture::new("courses-api-post");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client
        .post(url(&server, "/api/courses"))
        .header("x-learner-client", "1")
        .body("{}")
        .send()
        .await
        .expect("POST request");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_text(response).await, r#"{"error":"Not found"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_host_is_403_on_files() {
    let fixture = Fixture::new("courses-api-host");
    fixture.data_file("courses/demo/MISSION.md", "# Demo\n");
    let server = start(fixture.options()).await.expect("start server");
    let client = Client::new();

    let response = client
        .get(url(&server, "/files/courses/demo/MISSION.md"))
        .header(HOST, format!("evil.example:{}", server.port))
        .send()
        .await
        .expect("GET request");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_text(response).await, r#"{"error":"Forbidden host"}"#);
}
