// Proxi desktop shell (Tauri v2 / WebView2). Spawns `proxi serve`, opens the hidden window on the
// URL the backend prints, keeps a single instance, kills the backend tree on quit and relaunches
// the app when the backend exits cleanly — the Electron contract of app/desktop/main.mjs, ported.
// All spawn/port/URL logic lives in shell.rs where the unit tests exercise it directly; the web
// guards live in guards.rs and are attached to every window here. The main window is built in
// setup (not tauri.conf.json) because tauri 2.12.6 exposes the guard handlers only on builders.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::webview::{NewWindowFeatures, NewWindowResponse, PermissionKind, PermissionResponse};
use tauri::{
    AppHandle, Manager, RunEvent, Url, Webview, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_window_controls_overlay::{TitleBarOverlayOptions, WindowControlsOverlayExt};

mod guards;
mod shell;
mod window;
use guards::ShellGuards;
use shell::Vars;

/// The backend process the shell owns; `pid` is None while showing an already-running Proxi.
#[derive(Default)]
struct BackendState {
    pid: Mutex<Option<u32>>,
    quitting: AtomicBool,
}

struct ShellWindowState {
    chrome: Mutex<window::Chrome>,
    zoom: Mutex<f64>,
    last_normal: Mutex<Option<(f64, f64, f64, f64)>>,
}

fn hex_to_color(hex: &str) -> Option<tauri::webview::Color> {
    if hex.len() != 7 || !hex.starts_with('#') {
        return None;
    }
    let r = u8::from_str_radix(&hex[1..3], 16).ok()?;
    let g = u8::from_str_radix(&hex[3..5], 16).ok()?;
    let b = u8::from_str_radix(&hex[5..7], 16).ok()?;
    Some(tauri::webview::Color(r, g, b, 255))
}

fn save_current_window_state(app: &AppHandle) -> Result<(), String> {
    let Some(win) = app.get_webview_window("main") else {
        return Ok(());
    };
    let state = app.state::<ShellWindowState>();
    let maximized = win.is_maximized().unwrap_or(false);
    let scale = win.scale_factor().unwrap_or(1.0);
    let (x, y, width, height) = if maximized {
        state.last_normal.lock().unwrap().unwrap_or_else(|| {
            let pos = win.outer_position().unwrap_or_default();
            let size = win.inner_size().unwrap_or_default();
            (
                pos.x as f64 / scale,
                pos.y as f64 / scale,
                size.width as f64 / scale,
                size.height as f64 / scale,
            )
        })
    } else {
        let pos = win.outer_position().unwrap_or_default();
        let size = win.inner_size().unwrap_or_default();
        let normal = (
            pos.x as f64 / scale,
            pos.y as f64 / scale,
            size.width as f64 / scale,
            size.height as f64 / scale,
        );
        *state.last_normal.lock().unwrap() = Some(normal);
        normal
    };
    let save_state = window::SaveState {
        x,
        y,
        width,
        height,
        maximized,
        chrome: state.chrome.lock().unwrap().clone(),
        zoom: *state.zoom.lock().unwrap(),
    };
    let path = app
        .path()
        .app_data_dir()
        .map(|d| d.join("window.json"))
        .map_err(|e| format!("cannot determine app data dir: {e}"))?;
    window::save_window_state(path, &save_state)
        .map_err(|e| format!("failed to save window state: {e}"))?;
    Ok(())
}

#[tauri::command]
fn set_chrome(app: AppHandle, webview: Webview, chrome: serde_json::Value) -> Result<(), String> {
    if webview.label() != "main" {
        return Ok(());
    }
    let Some(parsed) = window::parse_chrome(&chrome) else {
        return Ok(());
    };
    let state = app.state::<ShellWindowState>();
    *state.chrome.lock().unwrap() = parsed.clone();

    let theme = match parsed.mode_pref.as_ref() {
        "dark" => Some(tauri::Theme::Dark),
        "light" => Some(tauri::Theme::Light),
        _ => None,
    };
    let _ = app.set_theme(theme);

    let os_dark = webview
        .window()
        .theme()
        .map(|t| t == tauri::Theme::Dark)
        .unwrap_or(false);
    let pair = window::resolve_chrome(&parsed, os_dark);

    let _ = app.window_controls_overlay().update(
        &webview.window(),
        TitleBarOverlayOptions::new()
            .color(pair.color.clone())
            .symbol_color(pair.symbol_color.clone())
            .height(window::OVERLAY_HEIGHT as f64),
    );
    if let Some(color) = hex_to_color(&pair.color) {
        let _ = webview.set_background_color(Some(color));
    }

    save_current_window_state(&app)?;
    Ok(())
}

#[tauri::command]
fn set_zoom(webview: Webview, zoom: f64) -> Result<(), String> {
    if webview.label() != "main" {
        return Ok(());
    }
    if !zoom.is_finite() {
        return Ok(());
    }
    let clamped = window::clamp_zoom(zoom);
    let app = webview.app_handle();
    let state = app.state::<ShellWindowState>();
    *state.zoom.lock().unwrap() = clamped;
    let _ = webview.set_zoom(clamped);
    Ok(())
}

fn main() {
    // Fallback everywhere (serve_args, the binary probe, APPDATA): empty means unset. The
    // single-instance skip below must see the same value, so the filter lives in one place.
    let user_data = std::env::var_os("LEARNER_USER_DATA").filter(|d| !d.is_empty());
    if let Some(dir) = &user_data {
        // app_data_dir is %APPDATA%\<identifier> with identifier "proxi": point APPDATA at the
        // override so smoke/e2e runs keep their window.json (and keychain-free state) isolated.
        std::env::set_var("APPDATA", &dir);
    }
    prepend_path();
    let env = shell::vars();

    let mut builder = tauri::Builder::default();
    if user_data.is_none() {
        // Register FIRST: a duplicate instance exits inside this plugin's setup, before ours runs,
        // so it never spawns a second backend. Its lock keys derive from the identifier only and
        // cannot honor LEARNER_USER_DATA, hence the skip above.
        builder = builder.plugin(tauri_plugin_single_instance::init(
            |app: &AppHandle, _argv: Vec<String>, _cwd: String| {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.unminimize();
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            },
        ));
    }

    builder
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_window_controls_overlay::init())
        .invoke_handler(tauri::generate_handler![set_chrome, set_zoom])
        .setup(move |app| {
            // The main window cannot come from tauri.conf.json: the guard handlers only exist on
            // the builders, so it is built here, before the backend URL is known (about:blank).
            let handle = app.handle().clone();
            app.manage(ShellGuards::default());

            let window_path = app
                .path()
                .app_data_dir()
                .map(|d| d.join("window.json"))
                .map_err(|e| format!("cannot determine app data dir: {e}"))?;

            // Feed monitors from app.available_monitors() using work_area in logical DIP units
            let monitors = app.available_monitors().unwrap_or_default();
            let displays: Vec<(f64, f64, f64, f64)> = monitors
                .iter()
                .map(|m| {
                    let scale = m.scale_factor();
                    let area = m.work_area();
                    (
                        area.position.x as f64 / scale,
                        area.position.y as f64 / scale,
                        area.size.width as f64 / scale,
                        area.size.height as f64 / scale,
                    )
                })
                .collect();

            let saved = window::load_window_state(&window_path, &displays);
            let last_normal = saved.position.map(|(x, y)| (x, y, saved.width, saved.height));
            app.manage(ShellWindowState {
                chrome: Mutex::new(saved.chrome.clone()),
                zoom: Mutex::new(saved.zoom),
                last_normal: Mutex::new(last_normal),
            });

            let mut win_builder = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External("about:blank".parse().unwrap()),
            )
            .title("Proxi")
            .decorations(false)
            .min_inner_size(1000.0, 680.0)
            .visible(false);

            // WebviewWindowBuilder::position and inner_size take logical DIP units in Tauri 2.12.6,
            // so no physical conversion with scale_factor is required here.
            if let Some((x, y)) = saved.position {
                win_builder = win_builder.position(x, y).inner_size(saved.width, saved.height);
            } else {
                win_builder = win_builder.inner_size(saved.width, saved.height);
            }

            let win = win_builder
                .on_navigation({
                    let handle = handle.clone();
                    move |u| {
                        let app_origin = handle.state::<ShellGuards>().app_origin();
                        if guards::main_navigation_allowed(u, app_origin.as_deref()) {
                            return true;
                        }
                        // main.mjs will-navigate: a foreign http(s) URL goes to the system browser
                        if guards::is_external_http(u.as_str()) {
                            let _ = tauri_plugin_opener::open_url(u.as_str(), None::<&str>);
                        }
                        false
                    }
                })
                .on_new_window({
                    let handle = handle.clone();
                    move |url, features| new_window_response(&handle, url, features)
                })
                .on_permission_request({
                    let handle = handle.clone();
                    move |webview, kind| permission_response(&handle, &webview, kind)
                })
                .build()
                .map_err(|e| format!("failed to build the main window: {e}"))?;

            if saved.maximized {
                let _ = win.maximize();
            }

            let os_dark = win.theme().map(|t| t == tauri::Theme::Dark).unwrap_or(false);
            let pair = window::resolve_chrome(&saved.chrome, os_dark);

            let overlay_opts = TitleBarOverlayOptions::new()
                .color(pair.color.clone())
                .symbol_color(pair.symbol_color.clone())
                .height(window::OVERLAY_HEIGHT as f64);
            let _ = app.window_controls_overlay().enable(&win, overlay_opts);

            let theme = match saved.chrome.mode_pref.as_ref() {
                "dark" => Some(tauri::Theme::Dark),
                "light" => Some(tauri::Theme::Light),
                _ => None,
            };
            let _ = app.set_theme(theme);

            if let Some(color) = hex_to_color(&pair.color) {
                let _ = win.set_background_color(Some(color));
            }

            if let Err(e) = start_backend(app, &env) {
                eprintln!("{e}");
                app.handle().exit(1);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Proxi shell")
        .run(|app, event| {
            match event {
                RunEvent::ExitRequested { .. } | RunEvent::Exit => kill_backend_tree(app),
                RunEvent::WindowEvent {
                    label,
                    event: ref win_event,
                    ..
                } => {
                    if label == "main" {
                        match win_event {
                            WindowEvent::CloseRequested { .. } => {
                                let _ = save_current_window_state(app);
                            }
                            WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                                if let Some(win) = app.get_webview_window("main") {
                                    if !win.is_maximized().unwrap_or(false) {
                                        let scale = win.scale_factor().unwrap_or(1.0);
                                        if let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) {
                                            let normal = (
                                                pos.x as f64 / scale,
                                                pos.y as f64 / scale,
                                                size.width as f64 / scale,
                                                size.height as f64 / scale,
                                            );
                                            if let Some(state) = app.try_state::<ShellWindowState>() {
                                                *state.last_normal.lock().unwrap() = Some(normal);
                                            }
                                        }
                                    }
                                }
                            }
                            WindowEvent::ThemeChanged(theme) => {
                                if let Some(win) = app.get_webview_window("main") {
                                    if let Some(state) = app.try_state::<ShellWindowState>() {
                                        let chrome = state.chrome.lock().unwrap().clone();
                                        if chrome.mode_pref == "system" {
                                            let os_dark = *theme == tauri::Theme::Dark;
                                            let pair = window::resolve_chrome(&chrome, os_dark);
                                            let _ = app.window_controls_overlay().update(
                                                &win,
                                                TitleBarOverlayOptions::new()
                                                    .color(pair.color.clone())
                                                    .symbol_color(pair.symbol_color.clone())
                                                    .height(window::OVERLAY_HEIGHT as f64),
                                            );
                                            if let Some(c) = hex_to_color(&pair.color) {
                                                let _ = win.set_background_color(Some(c));
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // a destroyed lesson pop-up frees its label and its slot in the window cap
                    if label.starts_with(guards::POPUP_LABEL_PREFIX) {
                        if let WindowEvent::Destroyed = win_event {
                            if let Some(g) = app.try_state::<ShellGuards>() {
                                guards::release_popup(&mut g.gate.lock(), &label);
                            }
                        }
                    }
                }
                _ => {}
            }
        });
}

/// The `setWindowOpenHandler` of main.mjs, shared by the main window and every lesson pop-up:
/// a `/files/` page of our own server opens as a guarded pop-up (capped by the gate locks),
/// http(s) goes to the system browser, everything else is denied. The admission check, the label
/// registration, the window count and the build all happen under one gate lock.
fn new_window_response<R: tauri::Runtime>(
    handle: &AppHandle<R>,
    url: Url,
    features: NewWindowFeatures,
) -> NewWindowResponse<R> {
    let guards = handle.state::<ShellGuards>();
    let app_origin = guards.app_origin();
    let mut gate = guards.gate.lock();
    let Some(label) = guards::admit_popup(
        &mut gate,
        url.as_str(),
        app_origin.as_deref(),
        handle.webview_windows().len(),
    ) else {
        drop(gate);
        if guards::is_external_http(url.as_str()) {
            let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
        }
        return NewWindowResponse::Deny;
    };
    let locked = url.as_str().to_string();
    let popup = WebviewWindowBuilder::new(handle, &label, WebviewUrl::External(url.clone()))
        .window_features(features)
        .inner_size(1100.0, 800.0)
        .min_inner_size(1000.0, 680.0)
        .on_navigation({
            let locked = locked.clone();
            move |u| guards::popup_navigation_allowed(u, &locked)
        })
        .on_new_window({
            let handle = handle.clone();
            move |url, features| new_window_response(&handle, url, features)
        })
        .on_permission_request({
            let handle = handle.clone();
            move |webview, kind| permission_response(&handle, &webview, kind)
        });
    match popup.build() {
        Ok(window) => NewWindowResponse::Create { window },
        Err(e) => {
            // the label must not stay held by a window that never existed
            guards::release_popup(&mut gate, &label);
            eprintln!("failed to open the lesson window {label}: {e}");
            NewWindowResponse::Deny
        }
    }
}

/// The `setPermissionRequestHandler` of main.mjs: the microphone of the app origin, nothing else
/// (see guards::permission_decision). A webview whose URL cannot be read counts as foreign.
fn permission_response<R: tauri::Runtime>(
    handle: &AppHandle<R>,
    webview: &Webview<R>,
    kind: PermissionKind,
) -> PermissionResponse {
    let webview_url = webview.url().map(|u| u.to_string()).unwrap_or_default();
    let app_origin = handle.state::<ShellGuards>().app_origin();
    guards::permission_decision(kind, &webview_url, app_origin.as_deref())
}

/// Shortcuts start us with a minimal PATH: make sure `claude` (native installer / npm global)
/// resolves (main.mjs keeps the same two dirs, when they exist).
fn prepend_path() {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut parts: Vec<PathBuf> = [
        std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".local").join("bin")),
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("npm")),
    ]
    .into_iter()
    .flatten()
    .filter(|p| p.is_dir())
    .collect();
    parts.extend(std::env::split_paths(&current));
    if let Ok(joined) = std::env::join_paths(&parts) {
        std::env::set_var("PATH", joined);
    }
}

fn start_backend(app: &tauri::App, env: &Vars) -> Result<(), String> {
    app.manage(BackendState::default());
    let exe_dir = std::env::current_exe()
        .map_err(|e| format!("cannot locate the shell exe: {e}"))?
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "the shell exe has no parent dir".to_string())?;
    let root = shell::repo_root(env, &exe_dir);
    let dist = root.join("app").join("web").join("dist");
    let bin = shell::proxi_binary_path(&root, env)?;
    let bin_str = bin.display().to_string();
    let root_str = root.display().to_string();
    let dist_str = dist.display().to_string();

    let spawn = |port: u16| -> Result<shell::Backend, String> {
        let args = shell::serve_args(&root_str, &dist_str, port, env)?;
        shell::spawn_backend(&bin_str, &args, env)
    };
    let started = shell::start_server(spawn, |base| shell::is_learner(base), env)?;

    // The window URL always comes from the backend stdout line, never a hard-coded port.
    let base = match &started {
        shell::ServerStart::Own(backend) => backend.url.clone(),
        shell::ServerStart::External { url } => {
            // Not ours: never kill it, never watch it.
            url.clone()
        }
    };
    // The guards stay closed until the origin is known (is_allowed_origin falls back to
    // localhost); it is stored before anything navigates or opens a pop-up.
    if let Ok(base_url) = Url::parse(&base) {
        app.state::<ShellGuards>()
            .set_app_origin(base_url.origin().ascii_serialization());
    }
    let url = shell::start_url(&base, env.get("LEARNER_OPEN_HASH").map(String::as_str));
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| "the main window is missing from the setup".to_string())?;
    win.navigate(
        url.parse()
            .map_err(|e| format!("invalid start URL {url}: {e}"))?,
    )
    .map_err(|e| format!("cannot open {url}: {e}"))?;
    if let Some(state) = app.try_state::<ShellWindowState>() {
        let zoom = *state.zoom.lock().unwrap();
        if zoom != 1.0 {
            let _ = win.set_zoom(zoom);
        }
    }
    let _ = win.show();
    let _ = win.set_focus();

    if let shell::ServerStart::Own(backend) = started {
        *app.state::<BackendState>().pid.lock().expect("pid mutex") = Some(backend.pid);
        watch_backend(app.handle().clone(), backend);
    }
    Ok(())
}

/// The backend exiting with code 0 while we are not quitting is the self-update relaunch signal
/// (serve_cli exits after the relaunch notify); anything else is a crash: log it, never relaunch.
fn watch_backend(app: AppHandle, backend: shell::Backend) {
    let shell::Backend { exit_code, .. } = backend;
    std::thread::spawn(move || {
        let code = exit_code.recv().unwrap_or(None);
        let state = app.state::<BackendState>();
        if state.quitting.load(Ordering::SeqCst) {
            return;
        }
        if code == Some(0) {
            app.restart();
        }
        eprintln!("backend exited with code {}", shell::exit_code_text(code));
        app.exit(0);
    });
}

/// Best-effort, idempotent tree kill on every exit path (the tree also takes down any engine
/// child the backend spawned); a foreign Proxi never has a pid here, so it is never touched.
fn kill_backend_tree(app: &AppHandle) {
    let state = app.state::<BackendState>();
    state.quitting.store(true, Ordering::SeqCst);
    let Some(pid) = *state.pid.lock().expect("pid mutex") else {
        return;
    };
    let args = shell::kill_tree_args(pid);
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd.status(); // the backend may already be gone
}
