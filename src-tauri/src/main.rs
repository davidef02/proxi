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

mod guards;
mod shell;
use guards::ShellGuards;
use shell::Vars;

/// The backend process the shell owns; `pid` is None while showing an already-running Proxi.
#[derive(Default)]
struct BackendState {
    pid: Mutex<Option<u32>>,
    quitting: AtomicBool,
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
        .setup(move |app| {
            // The main window cannot come from tauri.conf.json: the guard handlers only exist on
            // the builders, so it is built here, before the backend URL is known (about:blank).
            let handle = app.handle().clone();
            app.manage(ShellGuards::default());
            WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External("about:blank".parse().unwrap()),
            )
            .title("Proxi")
            .inner_size(1440.0, 920.0)
            .min_inner_size(1000.0, 680.0)
            .visible(false)
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
            .map(|_| ())
            .map_err(|e| format!("failed to build the main window: {e}"))?;
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
                // a destroyed lesson pop-up frees its label and its slot in the window cap
                RunEvent::WindowEvent {
                    label,
                    event: WindowEvent::Destroyed,
                    ..
                } => {
                    if label.starts_with(guards::POPUP_LABEL_PREFIX) {
                        if let Some(g) = app.try_state::<ShellGuards>() {
                            guards::release_popup(&mut g.gate.lock(), &label);
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
