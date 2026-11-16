//! Headless rendering through the installed Edge/Chrome (port of app/server/jobs/render.ts).
//!
//! One lazy shared browser per server (Edge first, then Chrome) driven through CDP; a failed launch is
//! not cached, so the next render retries. No Node, no Playwright, no bundled browser.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use base64::Engine as _;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::browser::BrowserContextId;
use chromiumoxide::cdp::browser_protocol::emulation::SetScriptExecutionDisabledParams;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams, EventRequestPaused, FailRequestParams, RequestPattern,
};
use chromiumoxide::cdp::browser_protocol::log::{EventEntryAdded, LogEntryLevel, LogEntrySource};
use chromiumoxide::cdp::browser_protocol::network::{ErrorReason, EventWebSocketCreated};
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, EventLoadEventFired, SetDocumentContentParams,
};
use chromiumoxide::cdp::browser_protocol::target::CreateTargetParams;
use chromiumoxide::cdp::js_protocol::runtime::{
    ConsoleApiCalledType, EventConsoleApiCalled, EventExceptionThrown, ExceptionDetails,
    RemoteObject, RemoteObjectType,
};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::listeners::EventStream;
use chromiumoxide::page::{Page, ScreenshotParams};
use futures_util::StreamExt as _;
use regex::Regex;

/// The policy of a subagent demo, byte-identical to `app/shared/csp.ts::DEMO_CSP` (the UI iframe
/// applies the same one, so what was tested is what ships).
pub const DEMO_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; form-action 'none'; frame-src 'none'; worker-src 'none'";

/// Playwright's viewport in `render.ts`; the UI shows the figure at this size.
pub const VIEWPORT: (u32, u32) = (900, 600);
/// How long a demo runs before the screenshot, as `runDemoPage`'s default.
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(1500);

const SVG_SHELL_START: &str = r#"<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:"></head><body style="margin:0;padding:16px;background:#1c1c1c;display:inline-block">"#;
const SVG_SHELL_END: &str = "</body></html>";

/// The demo HTML with the CSP meta inserted right after a leading doctype (before it would trigger
/// quirks mode); a `<meta>` CSP applies to everything parsed after it.
pub fn with_demo_csp(html: &str) -> String {
    static DOCTYPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\s*<!doctype[^>]*>").expect("the doctype regex compiles")
    });
    let meta = format!(r#"<meta http-equiv="Content-Security-Policy" content="{DEMO_CSP}">"#);
    match DOCTYPE.find(html) {
        Some(found) => format!("{}{}{}", &html[..found.end()], meta, &html[found.end()..]),
        None => format!("{meta}{html}"),
    }
}

/// Why a render did not produce a PNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// No Edge/Chrome found, or it did not start; the message names what is missing.
    NoBrowser(String),
    /// The browser ran but the render failed.
    Failed(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::NoBrowser(message) | RenderError::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for RenderError {}

/// The browser to use: the override when it is a file, else the first candidate that is one.
pub fn find_browser_in(
    override_path: Option<&Path>,
    candidates: &[PathBuf],
) -> Result<PathBuf, RenderError> {
    if let Some(path) = override_path {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(RenderError::NoBrowser(format!(
                "nessun browser disponibile per il rendering (Microsoft Edge o Google Chrome non trovato in {})",
                path.display()
            )))
        };
    }
    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .ok_or_else(|| {
            RenderError::NoBrowser(
                "nessun browser disponibile per il rendering (Microsoft Edge o Google Chrome non trovato)"
                    .to_string(),
            )
        })
}

/// [`find_browser_in`] over this machine's Edge and Chrome install locations.
pub fn find_browser(override_path: Option<&Path>) -> Result<PathBuf, RenderError> {
    find_browser_in(override_path, &browser_candidates())
}

fn browser_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let roots: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
            .iter()
            .filter_map(|key| std::env::var_os(key).map(PathBuf::from))
            .collect();
        for relative in [
            r"Microsoft\Edge\Application\msedge.exe",
            r"Google\Chrome\Application\chrome.exe",
        ] {
            for root in &roots {
                out.push(root.join(relative));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from(
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ));
        out.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        const NAMES: [&str; 6] = [
            "microsoft-edge-stable",
            "microsoft-edge",
            "google-chrome-stable",
            "google-chrome",
            "chromium",
            "chromium-browser",
        ];
        if let Some(path) = std::env::var_os("PATH") {
            let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
            for name in NAMES {
                for dir in &dirs {
                    out.push(dir.join(name));
                }
            }
        }
    }
    out
}

/// A test-only browser: `PROXI_TEST_BROWSER` when set, else an installed Edge/Chrome.
///
/// For tests only; panic when there is none, so a CI run never silently skips the browser checks.
#[doc(hidden)]
pub fn require_test_browser() -> PathBuf {
    match test_browser_from(std::env::var_os("PROXI_TEST_BROWSER")) {
        Ok(path) => path,
        Err(message) => panic!("{message}"),
    }
}

fn test_browser_from(var: Option<OsString>) -> Result<PathBuf, String> {
    if let Some(raw) = var.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(raw);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!(
                "PROXI_TEST_BROWSER={} is not a file",
                path.display()
            ))
        };
    }
    find_browser(None).map_err(|_| {
        "render tests need Microsoft Edge or Google Chrome (install one, or set PROXI_TEST_BROWSER to a Chromium executable)"
            .to_string()
    })
}

/// What a demo run collected: the page errors and blocked requests, and the screenshot.
pub struct DemoRun {
    pub errors: Vec<String>,
    pub png: Vec<u8>,
}

struct Running {
    browser: Browser,
    handler: tokio::task::JoinHandle<()>,
    user_data_dir: PathBuf,
}

/// Shared with the per-render guards, which outlive the borrow of `Renderer` they were made from.
struct Inner {
    override_path: Option<PathBuf>,
    running: tokio::sync::Mutex<Option<Running>>,
    pages: Mutex<HashMap<String, Vec<BrowserContextId>>>,
    pid: Mutex<Option<u32>>,
}

/// One lazy headless browser, closed by `JobManager::shutdown` (one per server: tests share a process).
pub struct Renderer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer").finish_non_exhaustive()
    }
}

impl Renderer {
    pub fn new(browser: Option<PathBuf>) -> Self {
        Renderer {
            inner: Arc::new(Inner {
                override_path: browser,
                running: tokio::sync::Mutex::new(None),
                pages: Mutex::new(HashMap::new()),
                pid: Mutex::new(None),
            }),
        }
    }

    /// PNG of the SVG on a dark background: JavaScript off, every non-`data:` request refused.
    pub async fn render_svg_png(&self, owner: &str, svg: &str) -> Result<Vec<u8>, RenderError> {
        let open = self.inner.open_page(owner).await?;
        let result = render_svg(&open.page, svg).await;
        open.finish().await;
        result
    }

    /// Loads the demo under [`DEMO_CSP`] for `settle`, collecting console errors, page errors and
    /// blocked requests, then screenshots it.
    pub async fn run_demo_page(
        &self,
        owner: &str,
        html: &str,
        settle: Duration,
    ) -> Result<DemoRun, RenderError> {
        let open = self.inner.open_page(owner).await?;
        let result = run_demo(&open.page, html, settle).await;
        open.finish().await;
        result
    }

    /// Closes the pages of one job (settle/cancel), leaving the browser up for the next one.
    pub async fn close_owner(&self, owner: &str) {
        let ids = take_owner_pages(&self.inner.pages, owner);
        for id in ids {
            self.inner.dispose(id).await;
        }
    }

    pub fn open_pages(&self) -> usize {
        self.inner
            .pages
            .lock()
            .expect("pages lock")
            .values()
            .map(Vec::len)
            .sum()
    }

    pub fn browser_pid(&self) -> Option<u32> {
        *self.inner.pid.lock().expect("browser pid lock")
    }

    pub async fn close(&self) {
        let ids: Vec<BrowserContextId> = {
            let mut pages = self.inner.pages.lock().expect("pages lock");
            std::mem::take(&mut *pages)
                .into_values()
                .flatten()
                .collect()
        };
        let mut running = self.inner.running.lock().await;
        let Some(mut running) = running.take() else {
            return;
        };
        for id in ids {
            let _ = running.browser.dispose_browser_context(id).await;
        }
        if running.browser.close().await.is_err() {
            let _ = running.browser.kill().await;
        }
        let _ = running.browser.wait().await;
        running.handler.abort();
        *self.inner.pid.lock().expect("browser pid lock") = None;
        remove_user_data_dir(&running.user_data_dir).await;
    }
}

impl Drop for Renderer {
    /// A dropped renderer (a test that panicked) must not leave a browser behind: dropping the
    /// running child kills it (`kill_on_drop` in chromiumoxide).
    fn drop(&mut self) {
        if let Ok(mut running) = self.inner.running.try_lock() {
            let _ = running.take();
        }
    }
}

impl Inner {
    async fn ensure(&self) -> Result<(), RenderError> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Ok(());
        }
        let path = find_browser(self.override_path.as_deref())?;
        let viewport = Viewport {
            width: VIEWPORT.0,
            height: VIEWPORT.1,
            ..Viewport::default()
        };
        let user_data_dir = std::env::temp_dir().join(format!(
            "proxi-browser-{}-{}",
            std::process::id(),
            launch_counter()
        ));
        let config = BrowserConfig::builder()
            .chrome_executable(&path)
            .new_headless_mode()
            .user_data_dir(&user_data_dir)
            .window_size(VIEWPORT.0, VIEWPORT.1)
            .viewport(viewport)
            .no_sandbox()
            .build()
            .map_err(|error| browser_error(&error))?;
        let (mut browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|error| browser_error(&error.to_string()))?;
        let pid = browser
            .get_mut_child()
            .and_then(|child| child.as_mut_inner().id());
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
        *self.pid.lock().expect("browser pid lock") = pid;
        *running = Some(Running {
            browser,
            handler,
            user_data_dir,
        });
        Ok(())
    }

    async fn open_page(self: &Arc<Self>, owner: &str) -> Result<OpenPage, RenderError> {
        self.ensure().await?;
        let mut running = self.running.lock().await;
        let Some(running) = running.as_mut() else {
            return Err(RenderError::NoBrowser(
                "nessun browser disponibile per il rendering".to_string(),
            ));
        };
        let id = running
            .browser
            .create_browser_context(Default::default())
            .await
            .map_err(|error| RenderError::Failed(error.to_string()))?;
        self.pages
            .lock()
            .expect("pages lock")
            .entry(owner.to_string())
            .or_default()
            .push(id.clone());
        let guard = PageGuard {
            inner: Arc::clone(self),
            owner: owner.to_string(),
            id: id.clone(),
            done: false,
        };
        let target = CreateTargetParams {
            browser_context_id: Some(id),
            ..CreateTargetParams::new("about:blank")
        };
        let page = running
            .browser
            .new_page(target)
            .await
            .map_err(|error| RenderError::Failed(error.to_string()))?;
        Ok(OpenPage { guard, page })
    }

    async fn dispose(&self, id: BrowserContextId) {
        let mut running = self.running.lock().await;
        if let Some(running) = running.as_mut() {
            let _ = running.browser.dispose_browser_context(id).await;
        }
    }
}

/// A page and its browser context; dropping it closes the context, `finish` does it in place.
struct OpenPage {
    guard: PageGuard,
    page: Page,
}

impl OpenPage {
    async fn finish(mut self) {
        self.guard.close().await;
    }
}

struct PageGuard {
    inner: Arc<Inner>,
    owner: String,
    id: BrowserContextId,
    done: bool,
}

impl PageGuard {
    async fn close(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        unregister_page(&self.inner.pages, &self.owner, &self.id);
        self.inner.dispose(self.id.clone()).await;
    }
}

impl Drop for PageGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        unregister_page(&self.inner.pages, &self.owner, &self.id);
        // Dropped outside an await (a cancelled future): dispose in the background.
        let inner = Arc::clone(&self.inner);
        let id = self.id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move { inner.dispose(id).await });
        }
    }
}

fn unregister_page(
    pages: &Mutex<HashMap<String, Vec<BrowserContextId>>>,
    owner: &str,
    id: &BrowserContextId,
) {
    let mut pages = pages.lock().expect("pages lock");
    let empty = match pages.get_mut(owner) {
        Some(ids) => {
            ids.retain(|other| other != id);
            ids.is_empty()
        }
        None => false,
    };
    if empty {
        pages.remove(owner);
    }
}

fn take_owner_pages(
    pages: &Mutex<HashMap<String, Vec<BrowserContextId>>>,
    owner: &str,
) -> Vec<BrowserContextId> {
    pages
        .lock()
        .expect("pages lock")
        .remove(owner)
        .unwrap_or_default()
}

async fn render_svg(page: &Page, svg: &str) -> Result<Vec<u8>, RenderError> {
    page.execute(SetScriptExecutionDisabledParams::new(true))
        .await
        .map_err(failed)?;
    let paused = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(failed)?;
    page.execute(any_request()).await.map_err(failed)?;
    let blocking = tokio::spawn(refuse_requests(page.clone(), paused, None));

    let loaded = page.event_listener::<EventLoadEventFired>().await.ok();
    let html = format!("{SVG_SHELL_START}{svg}{SVG_SHELL_END}");
    match page.mainframe().await.map_err(failed)? {
        Some(frame) => match page
            .execute(SetDocumentContentParams::new(frame, html.clone()))
            .await
        {
            Ok(_) => {}
            Err(_) => navigate_to_html(page, &html).await?,
        },
        None => navigate_to_html(page, &html).await?,
    }
    wait_for_load(loaded).await;

    let png = page
        .screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(true)
                .build(),
        )
        .await
        .map_err(failed)?;
    blocking.abort();
    Ok(png)
}

async fn navigate_to_html(page: &Page, html: &str) -> Result<(), RenderError> {
    let url = format!(
        "data:text/html;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(html)
    );
    page.goto(url).await.map_err(failed)?;
    Ok(())
}

async fn run_demo(page: &Page, html: &str, settle: Duration) -> Result<DemoRun, RenderError> {
    page.enable_log().await.map_err(failed)?;
    page.enable_runtime().await.map_err(failed)?;
    let errors: DemoErrors = Arc::new(Mutex::new(Vec::new()));

    let paused = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(failed)?;
    let sockets = page
        .event_listener::<EventWebSocketCreated>()
        .await
        .map_err(failed)?;
    let console = page
        .event_listener::<EventConsoleApiCalled>()
        .await
        .map_err(failed)?;
    let entries = page
        .event_listener::<EventEntryAdded>()
        .await
        .map_err(failed)?;
    let exceptions = page
        .event_listener::<EventExceptionThrown>()
        .await
        .map_err(failed)?;
    page.execute(any_request()).await.map_err(failed)?;

    let mut watchers = vec![
        tokio::spawn(refuse_requests(
            page.clone(),
            paused,
            Some(Arc::clone(&errors)),
        )),
        tokio::spawn(watch_sockets(sockets, Arc::clone(&errors))),
        tokio::spawn(watch_console(console, Arc::clone(&errors))),
        tokio::spawn(watch_entries(entries, Arc::clone(&errors))),
        tokio::spawn(watch_exceptions(exceptions, Arc::clone(&errors))),
    ];

    page.set_content(with_demo_csp(html))
        .await
        .map_err(failed)?;
    tokio::time::sleep(settle).await;
    for watcher in &watchers {
        watcher.abort();
    }
    watchers.clear();
    let errors = dedupe(std::mem::take(
        &mut *errors.lock().expect("demo errors lock"),
    ));

    let png = page
        .screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(true)
                .build(),
        )
        .await
        .map_err(failed)?;
    Ok(DemoRun { errors, png })
}

type DemoErrors = Arc<Mutex<Vec<String>>>;

fn any_request() -> EnableParams {
    EnableParams {
        patterns: Some(vec![RequestPattern {
            url_pattern: Some("*".to_string()),
            ..RequestPattern::default()
        }]),
        handle_auth_requests: None,
    }
}

fn local_url(url: &str) -> bool {
    url.starts_with("about:") || url.starts_with("data:") || url.starts_with("blob:")
}

async fn refuse_requests(
    page: Page,
    mut events: EventStream<EventRequestPaused>,
    errors: Option<DemoErrors>,
) {
    while let Some(event) = events.next().await {
        let url = event.request.url.clone();
        let outcome = if local_url(&url) {
            page.execute(ContinueRequestParams::new(event.request_id.clone()))
                .await
                .map(|_| ())
        } else {
            if let Some(errors) = &errors {
                push_error(errors, format!("richiesta di rete bloccata: {url}"));
            }
            page.execute(FailRequestParams::new(
                event.request_id.clone(),
                ErrorReason::BlockedByClient,
            ))
            .await
            .map(|_| ())
        };
        if outcome.is_err() {
            break;
        }
    }
}

async fn watch_sockets(mut events: EventStream<EventWebSocketCreated>, errors: DemoErrors) {
    while let Some(event) = events.next().await {
        push_error(
            &errors,
            format!("richiesta di rete bloccata: {}", event.url),
        );
    }
}

async fn watch_console(mut events: EventStream<EventConsoleApiCalled>, errors: DemoErrors) {
    while let Some(event) = events.next().await {
        if event.r#type != ConsoleApiCalledType::Error {
            continue;
        }
        let args: Vec<String> = event.args.iter().map(console_arg).collect();
        push_error(&errors, format!("console.error: {}", args.join(" ")));
    }
}

async fn watch_entries(mut events: EventStream<EventEntryAdded>, errors: DemoErrors) {
    while let Some(event) = events.next().await {
        if event.entry.level != LogEntryLevel::Error || event.entry.source == LogEntrySource::Worker
        {
            continue;
        }
        push_error(&errors, format!("console.error: {}", event.entry.text));
    }
}

async fn watch_exceptions(mut events: EventStream<EventExceptionThrown>, errors: DemoErrors) {
    while let Some(event) = events.next().await {
        push_error(&errors, exception_text(&event.exception_details));
    }
}

fn console_arg(arg: &RemoteObject) -> String {
    if arg.r#type == RemoteObjectType::String
        && let Some(serde_json::Value::String(value)) = &arg.value
    {
        return value.clone();
    }
    if let Some(description) = arg.description.as_deref().filter(|text| !text.is_empty()) {
        return description.to_string();
    }
    arg.value
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default()
}

fn exception_text(details: &ExceptionDetails) -> String {
    let first_line = details
        .exception
        .as_ref()
        .and_then(|exception| exception.description.as_deref())
        .and_then(|description| description.lines().next())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| details.text.clone());
    format!("errore di pagina: {}", strip_error_prefix(&first_line))
}

/// Node reports `pageerror.message`, which Playwright builds from the exception text without its
/// leading `Error: ` (the exception identifier).
fn strip_error_prefix(text: &str) -> &str {
    let ident_len = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(text.len());
    let starts_with_ident = text
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$');
    if starts_with_ident && text[ident_len..].starts_with(": ") {
        &text[ident_len + 2..]
    } else {
        text
    }
}

fn push_error(errors: &DemoErrors, line: String) {
    errors.lock().expect("demo errors lock").push(line);
}

fn dedupe(errors: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    errors
        .into_iter()
        .filter(|error| seen.insert(error.clone()))
        .collect()
}

async fn wait_for_load(mut loaded: Option<EventStream<EventLoadEventFired>>) {
    if let Some(stream) = loaded.as_mut() {
        let _ = tokio::time::timeout(Duration::from_secs(3), stream.next()).await;
    }
}

fn failed(error: impl std::fmt::Display) -> RenderError {
    RenderError::Failed(error.to_string())
}

fn browser_error(error: &str) -> RenderError {
    let first = error.lines().next().unwrap_or(error);
    RenderError::NoBrowser(format!(
        "nessun browser disponibile per il rendering ({first})"
    ))
}

fn launch_counter() -> usize {
    static LAUNCHES: AtomicUsize = AtomicUsize::new(0);
    LAUNCHES.fetch_add(1, Ordering::Relaxed)
}

/// The browser holds its profile files for a moment after exit; retry instead of reporting a leak.
async fn remove_user_data_dir(dir: &Path) {
    for _ in 0..10 {
        if std::fs::remove_dir_all(dir).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    fn meta() -> String {
        format!(r#"<meta http-equiv="Content-Security-Policy" content="{DEMO_CSP}">"#)
    }

    #[test]
    fn demo_csp_matches_the_node_twin() {
        let source =
            fs::read_to_string(repo_root().join("app/shared/csp.ts")).expect("read csp.ts");
        let start = source
            .find("export const DEMO_CSP")
            .expect("DEMO_CSP in csp.ts");
        let rest = &source[start..];
        let declaration = &rest[..rest.find("\nexport ").expect("the next declaration")];
        let literal = Regex::new("\"([^\"]*)\"").expect("the literal regex");
        let parts: Vec<&str> = literal
            .captures_iter(declaration)
            .map(|part| part.get(1).expect("a literal body").as_str())
            .collect();

        assert_eq!(parts.len(), 2, "{declaration}");
        assert_eq!(DEMO_CSP, parts.concat());
    }

    #[test]
    fn with_demo_csp_keeps_a_leading_doctype() {
        let out = with_demo_csp("<!doctype html><p>x</p>");

        assert_eq!(out, format!("<!doctype html>{}<p>x</p>", meta()));
    }

    #[test]
    fn with_demo_csp_keeps_leading_spaces_and_an_upper_case_doctype() {
        let out = with_demo_csp("  \n<!DOCTYPE html><p>x</p>");

        assert_eq!(out, format!("  \n<!DOCTYPE html>{}<p>x</p>", meta()));
    }

    #[test]
    fn with_demo_csp_prepends_the_meta_without_a_doctype() {
        let out = with_demo_csp("<p>x</p>");

        assert_eq!(out, format!("{}<p>x</p>", meta()));
    }

    #[test]
    fn find_browser_in_picks_the_first_existing_candidate() {
        let dir = crate::tools::fake::temp_dir("browser-candidates");
        let missing = dir.join("missing.exe");
        let first = dir.join("first.exe");
        let second = dir.join("second.exe");
        fs::write(&first, "").expect("write first candidate");
        fs::write(&second, "").expect("write second candidate");

        let found = find_browser_in(None, &[missing, first.clone(), second]).expect("a browser");
        assert_eq!(found, first);
    }

    #[test]
    fn find_browser_in_skips_a_directory() {
        let dir = crate::tools::fake::temp_dir("browser-dir");
        let candidate_dir = dir.join("chrome-dir");
        fs::create_dir_all(&candidate_dir).expect("create the candidate dir");
        let file = dir.join("chrome.exe");
        fs::write(&file, "").expect("write the candidate");

        let found = find_browser_in(None, &[candidate_dir, file.clone()]).expect("a browser");
        assert_eq!(found, file);
    }

    #[test]
    fn find_browser_in_names_the_override_in_the_error() {
        let dir = crate::tools::fake::temp_dir("browser-override");
        let missing = dir.join("missing-browser.exe");

        let error = find_browser_in(Some(&missing), &[]).expect_err("no browser");
        let RenderError::NoBrowser(message) = error else {
            panic!("expected NoBrowser");
        };
        assert!(message.contains("Microsoft Edge"), "{message}");
        assert!(message.contains("Google Chrome"), "{message}");
        assert!(
            message.contains(&missing.display().to_string()),
            "{message}"
        );
    }

    #[test]
    fn find_browser_in_without_candidates_names_the_browsers() {
        let error = find_browser_in(None, &[]).expect_err("no browser");

        let RenderError::NoBrowser(message) = error else {
            panic!("expected NoBrowser");
        };
        assert!(message.contains("Microsoft Edge"), "{message}");
        assert!(message.contains("Google Chrome"), "{message}");
    }

    #[test]
    fn test_browser_from_rejects_a_missing_override() {
        let dir = crate::tools::fake::temp_dir("test-browser");
        let missing = dir.join("missing.exe");

        let error = test_browser_from(Some(missing.clone().into_os_string()))
            .expect_err("a missing browser is an error");
        assert!(error.contains("PROXI_TEST_BROWSER"), "{error}");
        assert!(error.contains(&missing.display().to_string()), "{error}");
    }

    #[test]
    fn test_browser_from_returns_an_existing_file() {
        let dir = crate::tools::fake::temp_dir("test-browser-file");
        let browser = dir.join("chrome.exe");
        fs::write(&browser, "").expect("write the fake browser");

        let found =
            test_browser_from(Some(browser.clone().into_os_string())).expect("the override file");
        assert_eq!(found, browser);
    }

    #[test]
    fn error_messages_show_the_message() {
        let error = RenderError::NoBrowser("nessun browser".to_string());

        assert_eq!(error.to_string(), "nessun browser");
    }

    #[test]
    fn exception_text_strips_the_identifier() {
        let details: ExceptionDetails = serde_json::from_value(serde_json::json!({
            "exceptionId": 1,
            "text": "Uncaught",
            "lineNumber": 0,
            "columnNumber": 0,
            "exception": {
                "type": "object",
                "subtype": "error",
                "description": "Error: boom\n    at <anonymous>:1:7",
            },
        }))
        .expect("exception details");

        assert_eq!(exception_text(&details), "errore di pagina: boom");
    }

    #[test]
    fn exception_text_falls_back_to_the_details_text() {
        let details = ExceptionDetails::new(1, "Uncaught", 0, 0);

        assert_eq!(exception_text(&details), "errore di pagina: Uncaught");
    }

    #[test]
    fn demo_errors_are_deduplicated_in_first_seen_order() {
        let errors = vec![
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
        ];

        assert_eq!(dedupe(errors), ["b", "a", "c"]);
    }
}
