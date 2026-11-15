//! The write seam: regenerate `index.html` and tell the app to refetch (port of
//! `app/server/tools/board.ts` and `coursesChanged` in `app/server/lesson.ts`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::anki;
use crate::anki_client;
use crate::events::EventBus;
use crate::homepage::{self, GenerateOptions};

/// Data roots with a refresh already scheduled: one pending timer per root, like Node's `pending`.
static PENDING: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Regenerate the board: with card counts when Anki answers, without otherwise. Fire-and-forget.
pub fn refresh_board(data_root: PathBuf) {
    // courses_changed may fire from a plain thread (no runtime): spawn_blocking has no home there.
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let _ = handle.spawn_blocking(move || generate(&data_root));
        }
        Err(_) => {
            std::thread::spawn(move || generate(&data_root));
        }
    }
}

fn generate(data_root: &Path) {
    if run(data_root, false).is_ok() {
        return;
    }
    // Anki down is the common case: the board without card counts is still worth writing.
    if let Err(error) = run(data_root, true) {
        eprintln!("[board] refresh failed: {error}");
    }
}

fn run(data_root: &Path, no_anki: bool) -> Result<(), String> {
    let options = GenerateOptions {
        base_dir: data_root.to_path_buf(),
        courses_dir: None,
        curriculum: None,
        out: None,
        no_anki,
        anki_url: std::env::var(anki_client::ANKI_URL_ENV)
            .ok()
            .filter(|url| !url.is_empty()),
        timeout: anki::DEFAULT_TIMEOUT,
    };
    // generate_homepage drives a blocking AnkiConnect client: block_on needs a thread with no
    // other runtime on it, like the `home generate` CLI.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime
        .block_on(homepage::generate_homepage(&options, &mut std::io::sink()))
        .map(|_| ())
}

/// A lesson, reference, plan or exercise changed: refresh the board and tell the app to refetch
/// `/api/courses`, once per 300 ms burst so a turn's writes cost one refresh.
pub fn courses_changed(data_root: &Path, events: &EventBus) {
    let root = data_root.to_path_buf();
    if !PENDING.lock().expect("board timers").insert(root.clone()) {
        return;
    }
    let events = events.clone();
    let fire = move || {
        PENDING.lock().expect("board timers").remove(&root);
        refresh_board(root.clone());
        events.emit(&json!({ "type": "courses" }));
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                fire();
            });
        }
        Err(_) => {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                fire();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::tools::fake::temp_dir;

    #[tokio::test]
    async fn one_refresh_per_burst_writes_the_board_and_emits_one_event() {
        let data = temp_dir("board");
        fs::create_dir_all(data.join("courses")).expect("create courses dir");
        fs::write(data.join("curriculum.json"), "{\"chapters\":[]}").expect("seed curriculum");
        let events = EventBus::new();
        let mut rx = events.subscribe();

        courses_changed(&data, &events);
        courses_changed(&data, &events);

        let event = rx.recv().await.expect("one courses event");
        assert_eq!(event, r#"{"type":"courses"}"#);

        // The refresh is fire-and-forget: the event may arrive before index.html exists.
        let mut written = false;
        for _ in 0..100 {
            if data.join("index.html").is_file() {
                written = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(written, "the board refresh did not write index.html");

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(rx.try_recv().is_err(), "the second call joined the same burst");
    }
}
