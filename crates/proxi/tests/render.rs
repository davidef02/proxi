//! Headless rendering through the installed Edge/Chrome (AC2, AC3, AC4, AC7 of #22).

mod common;

use std::io;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proxi::jobs::render::{self, RenderError, Renderer};

fn is_png(bytes: &[u8]) -> bool {
    bytes.len() > 8 && &bytes[..4] == b"\x89PNG"
}

fn renderer() -> Renderer {
    Renderer::new(Some(render::require_test_browser()))
}

#[tokio::test]
async fn a_missing_override_names_the_browsers() {
    let dir = common::temp_dir("missing-browser");
    let missing = dir.0.join("missing-browser.exe");
    for error in [
        render::find_browser(Some(&missing)).expect_err("a missing override is no browser"),
        render::find_browser_in(None, &[]).expect_err("no candidates is no browser"),
    ] {
        let RenderError::NoBrowser(message) = error else {
            panic!("expected NoBrowser");
        };
        assert!(message.contains("Microsoft Edge"), "{message}");
        assert!(message.contains("Google Chrome"), "{message}");
    }

    let renderer = Renderer::new(Some(missing));
    let error = renderer
        .render_svg_png("j", "<svg/>")
        .await
        .expect_err("no browser, no png");
    assert!(matches!(error, RenderError::NoBrowser(_)), "{error:?}");
    assert_eq!(renderer.open_pages(), 0);
}

#[tokio::test]
async fn svg_render_returns_a_png_and_makes_no_request() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the counting listener");
    listener
        .set_nonblocking(true)
        .expect("a non-blocking listener");
    let port = listener.local_addr().expect("listener address").port();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><image href="http://127.0.0.1:{port}/x.png" width="20" height="20"/><rect width="10" height="10" style="fill:url(http://127.0.0.1:{port}/y)"/></svg>"#
    );

    let renderer = renderer();
    let png = renderer
        .render_svg_png("ac2", &svg)
        .await
        .expect("render the svg");
    assert!(is_png(&png), "not a png: {:?}", &png[..png.len().min(8)]);

    tokio::time::sleep(Duration::from_millis(300)).await;
    match listener.accept() {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        other => panic!("the svg made a network request: {other:?}"),
    }
    renderer.close().await;
}

#[tokio::test]
async fn demo_reports_page_errors_blocked_requests_and_returns_a_png() {
    let html = "<!doctype html><body><script>throw new Error('boom')</script>\
                <script>fetch('ht'+'tps://example.invalid/a').catch(()=>{})</script></body>";
    let renderer = renderer();

    let run = renderer
        .run_demo_page("ac3", html, render::DEFAULT_SETTLE)
        .await
        .expect("run the demo");
    assert!(
        run.errors
            .iter()
            .any(|error| error.contains("errore di pagina") && error.contains("boom")),
        "{:?}",
        run.errors
    );
    assert!(
        run.errors
            .iter()
            .any(|error| error.contains("bloccata") || error.contains("Content Security Policy")),
        "{:?}",
        run.errors
    );
    assert!(is_png(&run.png), "not a png");
    renderer.close().await;
}

#[tokio::test]
async fn obfuscated_network_attempts_are_all_blocked() {
    let bodies = [
        "<script>fetch('ht'+'tps://example.invalid/a').catch(()=>{})</script>",
        "<script>navigator.sendBeacon('ht'+'tps://example.invalid/b','x')</script>",
        "<script>new Image().src='ht'+'tps://example.invalid/c'</script>",
        "<script>try{new WebSocket('ws://127.0.0.1:9/d')}catch(e){console.error(String(e))}</script>",
        "<style>body{background:image-set('http://example.invalid/e' 1x)}</style><p>x</p>",
    ];
    let renderer = renderer();

    for body in bodies {
        let html = format!("<!doctype html><body>{body}</body>");
        let run = renderer
            .run_demo_page("hardening", &html, Duration::from_millis(400))
            .await
            .expect("run the demo");
        assert!(!run.errors.is_empty(), "{body}");
    }
    renderer.close().await;
}

#[tokio::test]
async fn a_clean_page_has_no_errors() {
    let html = "<!doctype html><body><input type=range min=0 max=10 value=5 \
                oninput=\"document.getElementById('v').textContent=this.value\">\
                <span id=v>5</span></body>";
    let renderer = renderer();

    let run = renderer
        .run_demo_page("clean", html, Duration::from_millis(400))
        .await
        .expect("run the demo");
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    assert!(is_png(&run.png), "not a png");
    renderer.close().await;
}

#[tokio::test]
async fn rendering_starts_no_node_process() {
    let renderer = renderer();
    renderer
        .render_svg_png(
            "ac4",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"#,
        )
        .await
        .expect("render the svg");
    let pid = renderer.browser_pid().expect("the browser has a pid");

    let all = common::process_tree::descendants(std::process::id());
    assert!(
        all.iter().any(|(child, _)| *child == pid),
        "the browser ({pid}) is not among the descendants: {all:?}"
    );
    for (_, name) in &all {
        let lower = name.to_ascii_lowercase();
        assert!(
            lower != "node" && lower != "node.exe",
            "a node process is a descendant: {name}"
        );
    }
    renderer.close().await;
}

#[tokio::test]
async fn close_owner_drops_open_pages_while_a_demo_runs() {
    let renderer = Arc::new(renderer());
    let run = tokio::spawn({
        let renderer = Arc::clone(&renderer);
        async move {
            renderer
                .run_demo_page("owner", "<!doctype html><p>x</p>", Duration::from_secs(5))
                .await
        }
    });

    let deadline = Instant::now() + Duration::from_secs(60);
    while renderer.open_pages() == 0 {
        assert!(Instant::now() < deadline, "the demo never opened a page");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    renderer.close_owner("owner").await;
    assert_eq!(renderer.open_pages(), 0);

    run.abort();
    let _ = run.await;
    renderer.close().await;
}
