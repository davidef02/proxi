//! /files/*: the sandboxed lesson and asset file server (main.ts:127-147,518-534).

use std::fs;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::data_root::resolve_path;
use crate::js;

/// Sandbox for LLM-authored lesson files; twin of `app/shared/csp.ts::LESSON_FILE_CSP`.
pub const LESSON_FILE_CSP: &str =
    "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox; frame-ancestors 'self'";

/// An opaque origin has no localStorage (it throws): lessons use it for the theme toggle.
pub const STORAGE_SHIM: &str = "<script>try{localStorage.getItem('_')}catch(e){var m={},s={getItem:function(k){return k in m?m[k]:null},setItem:function(k,v){m[k]=String(v)},removeItem:function(k){delete m[k]},clear:function(){m={}}};['localStorage','sessionStorage'].forEach(function(n){try{Object.defineProperty(window,n,{value:s,configurable:true})}catch(e){}})}</script>";

/// `path.extname` + the `MIME` table of main.ts:83-89.
pub fn mime_for(path: &Path) -> &'static str {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = match name.rfind('.') {
        // Node's extname ignores a leading dot: `.gitkeep` has no extension.
        Some(0) | None => "",
        Some(at) => &name[at..],
    };
    match ext.to_ascii_lowercase().as_str() {
        ".html" => "text/html; charset=utf-8",
        ".css" => "text/css; charset=utf-8",
        ".js" => "text/javascript; charset=utf-8",
        ".mjs" => "text/javascript; charset=utf-8",
        ".json" => "application/json; charset=utf-8",
        ".svg" => "image/svg+xml",
        ".png" => "image/png",
        ".jpg" => "image/jpeg",
        ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".ico" => "image/x-icon",
        ".woff" => "font/woff",
        ".woff2" => "font/woff2",
        ".ttf" => "font/ttf",
        ".md" => "text/markdown; charset=utf-8",
        ".txt" => "text/plain; charset=utf-8",
        ".map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Mirrors main.ts:141: after the first `<head…>` (even `<header>`), else after the leading
/// whitespace plus an optional doctype.
pub fn inject_storage_shim(html: &str) -> String {
    if let Some(start) = js::find_ci(html, "<head", 0) {
        if let Some(rel) = html[start..].find('>') {
            let at = start + rel + 1;
            return format!("{}{}{}", &html[..at], STORAGE_SHIM, &html[at..]);
        }
    }
    let mut at = 0;
    for (idx, ch) in html.char_indices() {
        if !js::is_js_ws(ch) {
            at = idx;
            break;
        }
        at = idx + ch.len_utf8();
    }
    if js::find_ci(html, "<!doctype", at) == Some(at) {
        if let Some(rel) = html[at..].find('>') {
            at += rel + 1;
        }
    }
    format!("{}{}{}", &html[..at], STORAGE_SHIM, &html[at..])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileError {
    BadRequest,
    NotFound,
}

/// `raw` is still percent-encoded: the path after `/files/`. Only `courses/` (data folder) and
/// `assets/` (repo) are served, and a symlink may not lead outside them either.
pub fn resolve_file(data_root: &Path, repo_root: &Path, raw: &str) -> Result<PathBuf, FileError> {
    let rel = js::decode_uri_component(raw).ok_or(FileError::BadRequest)?;
    if rel.contains('\0') {
        return Err(FileError::NotFound);
    }
    let base = if rel.starts_with("courses/") {
        data_root
    } else {
        repo_root
    };
    let target = resolve_path(&base.join(&rel));
    let roots = [
        resolve_path(&data_root.join("courses")),
        resolve_path(&repo_root.join("assets")),
    ];
    let inside = roots
        .iter()
        .any(|root| target.starts_with(root) && &target != root);
    if !inside || !target.is_file() {
        return Err(FileError::NotFound);
    }
    // The roots are canonicalised per request: courses/ may not exist yet on a fresh PC.
    let real = fs::canonicalize(&target).map_err(|_| FileError::NotFound)?;
    let real_inside = roots.iter().any(|root| {
        let canon = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        real.starts_with(&canon) && real != canon
    });
    if !real_inside {
        return Err(FileError::NotFound);
    }
    Ok(real)
}

pub fn serve(data_root: &Path, repo_root: &Path, raw: &str) -> Response {
    let real = match resolve_file(data_root, repo_root, raw) {
        Ok(real) => real,
        Err(FileError::BadRequest) => return json_error(StatusCode::BAD_REQUEST, "Bad request"),
        Err(FileError::NotFound) => return json_error(StatusCode::NOT_FOUND, "Not found"),
    };
    let mime = mime_for(&real);
    let bytes = match fs::read(&real) {
        Ok(bytes) => bytes,
        Err(error) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string());
        }
    };
    let html = mime.starts_with("text/html");
    let body = if html {
        inject_storage_shim(&String::from_utf8_lossy(&bytes)).into_bytes()
    } else {
        bytes
    };

    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if html {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(LESSON_FILE_CSP),
        );
    }
    // Fonts/modules requested from the opaque origin are cross-origin (static read-only assets).
    if mime.starts_with("font/")
        || mime.starts_with("text/javascript")
        || mime.starts_with("text/css")
    {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    response
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::body::to_bytes;

    use super::*;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "proxi-files-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    struct Fixture {
        root: TempDir,
        data: PathBuf,
        repo: PathBuf,
    }

    fn fixture(tag: &str) -> Fixture {
        let root = temp_dir(tag);
        let data = root.0.join("data");
        let repo = root.0.join("repo");
        fs::create_dir_all(data.join("courses").join("demo").join("lessons"))
            .expect("create data courses");
        fs::create_dir_all(repo.join("assets")).expect("create repo assets");
        fs::write(
            data.join("courses").join("demo").join("MISSION.md"),
            "# Demo\n",
        )
        .expect("write mission");
        fs::write(
            data.join("courses")
                .join("demo")
                .join("lessons")
                .join("0001-a.html"),
            "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>a</body></html>",
        )
        .expect("write lesson html");
        fs::write(data.join("curriculum.json"), "{}").expect("write curriculum");
        fs::write(data.join("secret.txt"), "TOPSECRET").expect("write data secret");
        fs::write(root.0.join("secret.txt"), "TOPSECRET").expect("write root secret");
        fs::write(repo.join("secret.txt"), "TOPSECRET").expect("write repo secret");
        fs::write(repo.join("assets").join("x.css"), "body{}").expect("write css");
        fs::write(
            repo.join("assets").join("p.png"),
            [
                0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0xFF, 0x80,
            ],
        )
        .expect("write png");
        Fixture { root, data, repo }
    }

    async fn body_bytes(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body")
            .to_vec()
    }

    fn header_str<'a>(response: &'a Response, name: header::HeaderName) -> &'a str {
        response
            .headers()
            .get(&name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .expect("ascii header")
    }

    #[test]
    fn mime_for_matches_the_node_table() {
        for (name, want) in [
            ("a.html", "text/html; charset=utf-8"),
            ("X.HTML", "text/html; charset=utf-8"),
            ("a.css", "text/css; charset=utf-8"),
            ("a.js", "text/javascript; charset=utf-8"),
            ("a.mjs", "text/javascript; charset=utf-8"),
            ("a.json", "application/json; charset=utf-8"),
            ("a.svg", "image/svg+xml"),
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.ico", "image/x-icon"),
            ("a.woff", "font/woff"),
            ("a.woff2", "font/woff2"),
            ("a.ttf", "font/ttf"),
            ("a.md", "text/markdown; charset=utf-8"),
            ("a.txt", "text/plain; charset=utf-8"),
            ("a.map", "application/json"),
            (".gitkeep", "application/octet-stream"),
            ("noext", "application/octet-stream"),
            ("a.unknown", "application/octet-stream"),
        ] {
            assert_eq!(mime_for(Path::new(name)), want, "{name}");
        }
    }

    #[test]
    fn inject_storage_shim_follows_head_then_doctype() {
        assert_eq!(
            inject_storage_shim("<head><body>"),
            format!("<head>{STORAGE_SHIM}<body>")
        );
        assert_eq!(
            inject_storage_shim("<HEAD lang=x>y"),
            format!("<HEAD lang=x>{STORAGE_SHIM}y")
        );
        assert_eq!(
            inject_storage_shim("  <!DOCTYPE html><p>"),
            format!("  <!DOCTYPE html>{STORAGE_SHIM}<p>")
        );
        assert_eq!(
            inject_storage_shim("<header>x</header>"),
            format!("<header>{STORAGE_SHIM}x</header>")
        );
        assert_eq!(inject_storage_shim("plain"), format!("{STORAGE_SHIM}plain"));
        assert_eq!(
            inject_storage_shim("  plain"),
            format!("  {STORAGE_SHIM}plain")
        );
        assert_eq!(inject_storage_shim(""), STORAGE_SHIM);
    }

    #[test]
    fn resolve_file_serves_only_inside_the_roots() {
        let f = fixture("resolve");
        assert_eq!(
            resolve_file(&f.data, &f.repo, "courses/demo/MISSION.md"),
            Ok(fs::canonicalize(f.data.join("courses/demo/MISSION.md"))
                .expect("canonical data file"))
        );
        assert_eq!(
            resolve_file(&f.data, &f.repo, "assets/x.css"),
            Ok(fs::canonicalize(f.repo.join("assets/x.css")).expect("canonical repo file"))
        );
        for bad in [
            "secret.txt",
            "curriculum.json",
            "..%2fsecret.txt",
            "courses/..%2f..%2fsecret.txt",
            "courses/..%5csecret.txt",
            "courses/%00",
            "courses/demo",
            "",
            "courses",
        ] {
            assert_eq!(
                resolve_file(&f.data, &f.repo, bad),
                Err(FileError::NotFound),
                "{bad}"
            );
        }
        assert_eq!(
            resolve_file(&f.data, &f.repo, "%E0%A4%A"),
            Err(FileError::BadRequest)
        );
    }

    #[test]
    fn resolve_file_without_the_roots_is_not_found() {
        let f = fixture("resolve-missing-roots");
        let fresh = f.root.0.join("fresh-data");
        fs::create_dir_all(&fresh).expect("create fresh data");
        assert_eq!(
            resolve_file(&fresh, &f.repo, "courses/demo/MISSION.md"),
            Err(FileError::NotFound)
        );
        assert_eq!(
            resolve_file(&f.data, &f.root.0.join("no-repo"), "assets/x.css"),
            Err(FileError::NotFound)
        );
    }

    #[test]
    fn resolve_file_rejects_symlinks_leading_outside() {
        let f = fixture("resolve-symlink");
        let outside = f.root.0.join("outside");
        fs::create_dir_all(&outside).expect("create outside dir");
        fs::write(outside.join("leak.txt"), "LEAKED").expect("write leak");
        let link = f.data.join("courses").join("demo").join("link");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        if !made {
            eprintln!("symlinks not allowed here: skipping the escape assertion");
            return;
        }
        assert_eq!(
            resolve_file(&f.data, &f.repo, "courses/demo/link/leak.txt"),
            Err(FileError::NotFound)
        );
    }

    #[tokio::test]
    async fn serve_html_adds_the_csp_nosniff_and_shim() {
        let f = fixture("serve-html");
        let response = serve(&f.data, &f.repo, "courses/demo/lessons/0001-a.html");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_str(&response, header::CONTENT_TYPE),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            header_str(&response, header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
        assert_eq!(
            header_str(&response, header::CONTENT_SECURITY_POLICY),
            LESSON_FILE_CSP
        );
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
        let html = String::from_utf8(body_bytes(response).await).expect("utf-8 html");
        let head = html.find("<head>").expect("head tag") + "<head>".len();
        assert_eq!(
            html.find("<script>try{localStorage").expect("storage shim"),
            head
        );
    }

    #[tokio::test]
    async fn serve_css_adds_cors_and_keeps_the_bytes() {
        let f = fixture("serve-css");
        let response = serve(&f.data, &f.repo, "assets/x.css");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header_str(&response, header::CONTENT_TYPE),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            header_str(&response, header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
        assert_eq!(
            header_str(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "*"
        );
        assert!(
            response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .is_none()
        );
        assert_eq!(
            body_bytes(response).await,
            fs::read(f.repo.join("assets/x.css")).expect("read css")
        );
    }

    #[tokio::test]
    async fn serve_png_keeps_the_bytes_and_adds_no_cors() {
        let f = fixture("serve-png");
        let response = serve(&f.data, &f.repo, "assets/p.png");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header_str(&response, header::CONTENT_TYPE), "image/png");
        assert_eq!(
            header_str(&response, header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
        assert!(
            response
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .is_none()
        );
        assert_eq!(
            body_bytes(response).await,
            fs::read(f.repo.join("assets/p.png")).expect("read png")
        );
    }

    #[tokio::test]
    async fn serve_answers_json_errors() {
        let f = fixture("serve-errors");
        let not_found = serve(&f.data, &f.repo, "");
        assert_eq!(not_found.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            String::from_utf8(body_bytes(not_found).await).expect("utf-8 body"),
            r#"{"error":"Not found"}"#
        );
        let bad = serve(&f.data, &f.repo, "%E0%A4%A");
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            String::from_utf8(body_bytes(bad).await).expect("utf-8 body"),
            r#"{"error":"Bad request"}"#
        );
    }
}
