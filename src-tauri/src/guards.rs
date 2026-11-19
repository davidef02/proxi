//! The web guards of `app/desktop/main.mjs` (the `setWindowOpenHandler`, `on("will-navigate")`
//! and `setPermissionRequestHandler` plus the `MAX_WINDOWS` cap) as pure Rust functions, so the
//! unit tests exercise them without a webview. Tauri types here are only `Url` and the
//! `PermissionKind`/`PermissionResponse` enums, both re-exported by tauri.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

use regex::Regex;
use tauri::webview::{PermissionKind, PermissionResponse};
use tauri::Url;

/// The open-window cap of main.mjs (`const MAX_WINDOWS = 8`, counting the main window).
pub const MAX_WINDOWS: usize = 8;

/// Every popup label starts with this; run-loop releases only these.
pub const POPUP_LABEL_PREFIX: &str = "lesson-";

fn external_http_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^https?:").expect("external-http regex"))
}

fn localhost_origin_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^http://(127\.0\.0\.1|localhost)(:\d+)?$").expect("localhost origin regex")
    })
}

/// main.mjs `/^https?:/i` — the only schemes ever sent to the system browser.
pub fn is_external_http(url: &str) -> bool {
    external_http_re().is_match(url)
}

/// main.mjs `isAllowedOrigin` (main.mjs:231-247): parse and compare the origin strings; before
/// the backend origin is known, plain localhost http is allowed. Parse failure is false.
pub fn is_allowed_origin(url_str: &str, app_origin: Option<&str>) -> bool {
    let origin = match Url::parse(url_str) {
        Ok(url) => url.origin().ascii_serialization(),
        Err(_) => return false,
    };
    match app_origin {
        Some(o) => origin == o,
        None => localhost_origin_re().is_match(&origin),
    }
}

/// main.mjs setWindowOpenHandler (main.mjs:166-180): a pop-up may only ever be a `/files/` page
/// of our own server, and only once the origin is known (nothing is admitted before).
pub fn is_popup_url(url: &str, app_origin: Option<&str>) -> bool {
    match app_origin {
        Some(o) => url.starts_with(&format!("{o}/files/")),
        None => false,
    }
}

/// main.mjs will-navigate (main.mjs:177-184): the main webview stays on the app origin,
/// everything else is cancelled (an http(s) URL then goes to the system browser at the call site).
pub fn main_navigation_allowed(url: &Url, app_origin: Option<&str>) -> bool {
    match app_origin {
        Some(o) => url.origin().ascii_serialization() == o,
        None => false,
    }
}

/// The pop-up `on_navigation`: locked to the exact requested URL, everything else is cancelled.
pub fn popup_navigation_allowed(url: &Url, locked: &str) -> bool {
    url.as_str() == locked
}

/// main.mjs setPermissionRequestHandler: the microphone, for the allowed origin only. WebView2
/// splits a video+mic request into separate Camera and Microphone kinds, so denying Camera
/// makes a video request fail as a whole — audio-only falls out of the per-kind handler.
pub fn permission_decision(
    kind: PermissionKind,
    webview_url: &str,
    app_origin: Option<&str>,
) -> PermissionResponse {
    let allowed =
        matches!(kind, PermissionKind::Microphone) && is_allowed_origin(webview_url, app_origin);
    if allowed {
        PermissionResponse::Allow
    } else {
        PermissionResponse::Deny
    }
}

/// The bookkeeping behind the window cap; see [`admit_popup`]/[`release_popup`].
#[derive(Default)]
pub struct PopupGateInner {
    open: HashSet<String>,
    next: u64,
}

pub struct PopupGate {
    inner: Mutex<PopupGateInner>,
}

impl Default for PopupGate {
    fn default() -> Self {
        Self {
            inner: Mutex::new(PopupGateInner::default()),
        }
    }
}

impl PopupGate {
    /// Admission, creation and (in the run loop) release all happen under this one lock:
    /// check + decision + write in the same critical section (docs/agents/rules.md).
    pub fn lock(&self) -> MutexGuard<'_, PopupGateInner> {
        self.inner.lock().expect("popup gate mutex")
    }
}

/// Under the gate lock: admit a new lesson pop-up and return its label, or None when the URL is
/// not a lesson page or `window_count` already reached [`MAX_WINDOWS`]. The label is registered
/// immediately so a caller that builds the window afterwards cannot race past the cap.
pub fn admit_popup(
    inner: &mut PopupGateInner,
    url: &str,
    app_origin: Option<&str>,
    window_count: usize,
) -> Option<String> {
    if window_count >= MAX_WINDOWS || !is_popup_url(url, app_origin) {
        return None;
    }
    let label = format!("{POPUP_LABEL_PREFIX}{}", inner.next);
    inner.next += 1;
    inner.open.insert(label.clone());
    Some(label)
}

/// Under the gate lock: forget a pop-up label once its window was destroyed (the cap counts live
/// windows, so a freed label must not keep blocking).
pub fn release_popup(inner: &mut PopupGateInner, label: &str) {
    inner.open.remove(label);
}

/// The guard state shared by every handler closure: the backend origin once known, plus the
/// pop-up gate. Read `app_origin()` in the handlers; write it once the backend prints its URL.
#[derive(Default)]
pub struct ShellGuards {
    app_origin: Mutex<Option<String>>,
    pub gate: PopupGate,
}

impl ShellGuards {
    pub fn set_app_origin(&self, origin: String) {
        *self.app_origin.lock().expect("app origin mutex") = Some(origin);
    }

    pub fn app_origin(&self) -> Option<String> {
        self.app_origin.lock().expect("app origin mutex").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const ORIGIN: &str = "http://127.0.0.1:4317";

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap_or_else(|e| panic!("test fixture is not a URL {s:?}: {e}"))
    }

    #[test]
    fn external_http_scheme_table() {
        for url in [
            "https://example.com",
            "http://example.com",
            "HTTPS://example.com",
            "Http://example.com",
        ] {
            assert!(is_external_http(url), "{url:?} is external");
        }
        for url in [
            "mailto:a@b.c",
            "ftp://example.com",
            "javascript:alert(1)",
            "file:///C:/x",
            "",
            "httpsfoo",
        ] {
            assert!(!is_external_http(url), "{url:?} is not external");
        }
    }

    #[test]
    fn allowed_origin_with_app_origin() {
        let app_origin = Some(ORIGIN);
        assert!(is_allowed_origin("http://127.0.0.1:4317/", app_origin));
        assert!(is_allowed_origin(
            "http://127.0.0.1:4317/files/x.html",
            app_origin
        ));
        // wrong port, https, and the localhost spelling: still foreign
        assert!(!is_allowed_origin("http://127.0.0.1:4318/", app_origin));
        assert!(!is_allowed_origin("https://127.0.0.1:4317/", app_origin));
        assert!(!is_allowed_origin("http://localhost:4317/", app_origin));
        assert!(!is_allowed_origin(
            "http://127.0.0.1.evil.com:4317/",
            app_origin
        ));
    }

    #[test]
    fn allowed_origin_before_the_app_origin_is_known() {
        let none = None;
        assert!(is_allowed_origin("http://127.0.0.1:4317/", none));
        assert!(is_allowed_origin("http://localhost:4317/", none));
        assert!(is_allowed_origin("http://localhost/", none));
        assert!(!is_allowed_origin("https://localhost:4317/", none));
        assert!(!is_allowed_origin("http://localhost.evil.com:4317/", none));
    }

    #[test]
    fn allowed_origin_rejects_malformed_input() {
        // the rules.md failure boundary: garbage must be false, never a panic
        assert!(!is_allowed_origin("", None));
        assert!(!is_allowed_origin("garbage", None));
        assert!(!is_allowed_origin("http://", None));
        assert!(!is_allowed_origin(":::", None));
    }

    #[test]
    fn popup_url_requires_the_files_prefix_on_the_app_origin() {
        let app_origin = Some(ORIGIN);
        assert!(is_popup_url(
            "http://127.0.0.1:4317/files/x.html",
            app_origin
        ));
        assert!(is_popup_url(
            "http://127.0.0.1:4317/files/a/b.html",
            app_origin
        ));
        assert!(
            !is_popup_url("http://127.0.0.1:4317/other", app_origin),
            "missing /files/"
        );
        assert!(
            !is_popup_url("http://127.0.0.1:4317/files", app_origin),
            "no trailing slash"
        );
        assert!(
            !is_popup_url("http://127.0.0.1:4318/files/x.html", app_origin),
            "other port"
        );
        assert!(
            !is_popup_url("https://127.0.0.1:4317/files/x.html", app_origin),
            "other scheme"
        );
        assert!(
            !is_popup_url("http://127.0.0.1:4317/files/x.html", None),
            "no origin known yet"
        );
    }

    #[test]
    fn main_navigation_only_on_the_app_origin() {
        let app_origin = Some(ORIGIN);
        assert!(main_navigation_allowed(
            &url("http://127.0.0.1:4317/#/c/demo"),
            app_origin
        ));
        assert!(main_navigation_allowed(
            &url("http://127.0.0.1:4317/x?y=1"),
            app_origin
        ));
        assert!(
            !main_navigation_allowed(&url("http://localhost:4317/"), app_origin),
            "spelling differs"
        );
        assert!(!main_navigation_allowed(
            &url("https://127.0.0.1:4317/"),
            app_origin
        ));
        // an opaque origin ("null") is never the app origin
        assert!(!main_navigation_allowed(
            &url("data:text/html,x"),
            app_origin
        ));
        assert!(!main_navigation_allowed(&url("about:blank"), None));
        assert!(!main_navigation_allowed(
            &url("http://127.0.0.1:4317/"),
            None
        ));
    }

    #[test]
    fn popup_navigation_is_locked_to_the_exact_url() {
        let locked = "http://127.0.0.1:4317/files/x.html";
        assert!(popup_navigation_allowed(&url(locked), locked));
        assert!(!popup_navigation_allowed(
            &url("http://127.0.0.1:4317/files/x.html?q=1"),
            locked
        ));
        assert!(!popup_navigation_allowed(
            &url("http://127.0.0.1:4317/files/x.html#f"),
            locked
        ));
        assert!(!popup_navigation_allowed(
            &url("http://127.0.0.1:4317/files/y.html"),
            locked
        ));
        assert!(!popup_navigation_allowed(
            &url("https://example.com/"),
            locked
        ));
    }

    #[test]
    fn permission_decision_allows_only_the_microphone_on_the_app_origin() {
        let app_origin = Some(ORIGIN);
        assert_eq!(
            permission_decision(
                PermissionKind::Microphone,
                "http://127.0.0.1:4317/",
                app_origin
            ),
            PermissionResponse::Allow
        );
        // camera is denied even on the app origin: audio+video then fails as a whole
        assert_eq!(
            permission_decision(PermissionKind::Camera, "http://127.0.0.1:4317/", app_origin),
            PermissionResponse::Deny
        );
        // foreign or malformed requesting URL
        assert_eq!(
            permission_decision(PermissionKind::Microphone, "https://evil.com/", app_origin),
            PermissionResponse::Deny
        );
        assert_eq!(
            permission_decision(PermissionKind::Microphone, "garbage", app_origin),
            PermissionResponse::Deny
        );
        assert_eq!(
            permission_decision(PermissionKind::Microphone, "", app_origin),
            PermissionResponse::Deny
        );
        // every other kind is denied
        for kind in [
            PermissionKind::Geolocation,
            PermissionKind::Notifications,
            PermissionKind::ClipboardRead,
        ] {
            assert_eq!(
                permission_decision(kind, "http://127.0.0.1:4317/", app_origin),
                PermissionResponse::Deny
            );
        }
        // before the backend origin is known, the localhost fallback still grants the mic
        assert_eq!(
            permission_decision(PermissionKind::Microphone, "http://localhost:4317/", None),
            PermissionResponse::Allow
        );
        assert_eq!(
            permission_decision(PermissionKind::Camera, "http://localhost:4317/", None),
            PermissionResponse::Deny
        );
    }

    #[test]
    fn admit_popup_labels_and_caps() {
        let mut inner = PopupGateInner::default();
        let app_origin = Some(ORIGIN);
        // count 7 → admitted (7 popups + main = MAX_WINDOWS)
        assert_eq!(
            admit_popup(&mut inner, "http://127.0.0.1:4317/files/a", app_origin, 7),
            Some("lesson-0".into())
        );
        // everything at once in the set: 7 owned popups + main = 8 → denied
        assert_eq!(
            admit_popup(&mut inner, "http://127.0.0.1:4317/files/b", app_origin, 8),
            None
        );
        // non-popup URL, even with room
        assert_eq!(
            admit_popup(&mut inner, "https://example.com/", app_origin, 0),
            None
        );
        // no app origin known yet
        assert_eq!(
            admit_popup(&mut inner, "http://127.0.0.1:4317/files/c", None, 0),
            None
        );
    }

    #[test]
    fn release_popup_frees_the_cap() {
        let mut inner = PopupGateInner::default();
        let app_origin = Some(ORIGIN);
        for i in 0..MAX_WINDOWS - 1 {
            assert!(admit_popup(
                &mut inner,
                "http://127.0.0.1:4317/files/x",
                app_origin,
                1 + i
            )
            .is_some());
        }
        assert_eq!(
            admit_popup(
                &mut inner,
                "http://127.0.0.1:4317/files/x",
                app_origin,
                MAX_WINDOWS
            ),
            None
        );
        release_popup(&mut inner, "lesson-3");
        assert_eq!(
            admit_popup(
                &mut inner,
                "http://127.0.0.1:4317/files/x",
                app_origin,
                MAX_WINDOWS - 1
            ),
            Some(format!("{POPUP_LABEL_PREFIX}{}", MAX_WINDOWS - 1)),
            "the freed slot admits a new popup with the next counter value"
        );
    }

    #[test]
    fn nine_sequential_admits_admit_seven_then_deny() {
        let gate = PopupGate::default();
        let app_origin = Some(ORIGIN);
        let mut admitted = 0;
        for i in 0..9 {
            let mut inner = gate.lock();
            // each popup, once built, would be a live window: count = main + open popups
            let count = 1 + inner.open.len();
            match admit_popup(
                &mut inner,
                "http://127.0.0.1:4317/files/x",
                app_origin,
                count,
            ) {
                Some(label) => {
                    assert_eq!(label, format!("{POPUP_LABEL_PREFIX}{i}"));
                    admitted += 1;
                }
                None => {}
            }
        }
        assert_eq!(
            admitted,
            MAX_WINDOWS - 1,
            "exactly MAX_WINDOWS-1 popups fit next to the main window"
        );
    }

    #[test]
    fn sixteen_threads_racing_the_gate_stay_under_the_cap() {
        let gate = Arc::new(PopupGate::default());
        let app_origin = Some(ORIGIN);
        let admitted = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let gate = gate.clone();
                let admitted = admitted.clone();
                std::thread::spawn(move || {
                    for _ in 0..8 {
                        let mut inner = gate.lock();
                        let count = 1 + inner.open.len();
                        if admit_popup(
                            &mut inner,
                            "http://127.0.0.1:4317/files/x",
                            app_origin,
                            count,
                        )
                        .is_some()
                        {
                            admitted.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("gate thread must not panic");
        }
        assert_eq!(
            admitted.load(Ordering::SeqCst),
            MAX_WINDOWS - 1,
            "the cap holds under concurrency"
        );
    }
}
