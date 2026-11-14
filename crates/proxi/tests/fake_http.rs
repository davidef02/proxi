#[path = "common/fake_http.rs"]
mod fake_http;

use fake_http::{FakeHttp, Reply};
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread")]
async fn records_requests_and_answers_through_the_handler() {
    let fake = FakeHttp::start(|request| {
        if request.method == "POST" && request.path == "/echo?a=1" {
            Reply::json(201, request.json())
        } else {
            Reply::json(404, json!({ "message": "Not Found" }))
        }
    });

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/echo?a=1", fake.url))
        .header("x-test", "yes")
        .header(CONTENT_TYPE, "application/json")
        .body("{\"hello\":\"world\"}")
        .send()
        .await
        .expect("POST /echo");

    assert_eq!(response.status().as_u16(), 201);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let text = response.text().await.expect("read body");
    let body: Value = serde_json::from_str(&text).expect("JSON body");
    assert_eq!(body, json!({ "hello": "world" }));

    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/echo?a=1");
    assert_eq!(requests[0].header("x-test"), Some("yes"));
    assert_eq!(requests[0].body, "{\"hello\":\"world\"}");
    assert_eq!(requests[0].json(), json!({ "hello": "world" }));

    fake.clear();
    assert!(fake.requests().is_empty());

    let response = client
        .get(format!("{}/elsewhere", fake.url))
        .send()
        .await
        .expect("GET /elsewhere");
    assert_eq!(response.status().as_u16(), 404);
    assert_eq!(fake.requests().len(), 1);
    assert_eq!(fake.requests()[0].path, "/elsewhere");
}
