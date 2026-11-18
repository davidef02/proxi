// Proxi desktop shell (Tauri v2 / WebView2). Spawns `proxi serve`, opens the hidden window on the
// URL the backend prints, keeps a single instance, kills the backend tree on quit and relaunches
// the app when the backend exits cleanly — the Electron contract of app/desktop/main.mjs, ported.
// All spawn/port/URL logic lives in shell.rs where the unit tests exercise it directly.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::{AppHandle, Manager, RunEvent};

mod shell;
use shell::Vars;

/// The backend process the shell owns; `pid` is None while showing an already-running Proxi.
#[derive(Default)]
struct BackendState {
    pid: Mutex<Option<u32>>,
    quitting: AtomicBool,
}

fn main() {
    if let Some(dir) = std::env::var_os("LEARNER_USER_DATA").filter(|d| !d.is_empty()) {
        // app_data_dir is %APPDATA%\<identifier> with identifier "proxi": point APPDATA at the
        // override so smoke/e2e runs keep their window.json (and keychain-free state) isolated.
        std::env::set_var("APPDATA", &dir);
    }
    prepend_path();
    let env = shell::vars();

    let mut builder = tauri::Builder::default();
    if std::env::var_os("LEARNER_USER_DATA").is_none() {
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
        .setup(move |app| {
            if let Err(e) = start_backend(app, &env) {
                eprintln!("{e}");
                app.handle().exit(1);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Proxi shell")
        .run(|app, event| {
            if let RunEvent::ExitRequested { .. } | RunEvent::Exit = event {
                kill_backend_tree(app);
            }
        });
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
        let args = shell::serve_args(&root_str, &dist_str, port, env);
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
    let url = shell::start_url(&base, env.get("LEARNER_OPEN_HASH").map(String::as_str));
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| "the main window is missing from the config".to_string())?;
    win.navigate(url.parse().map_err(|e| format!("invalid start URL {url}: {e}"))?)
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
