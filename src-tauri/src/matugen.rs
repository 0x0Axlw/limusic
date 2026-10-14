use notify::{recommended_watcher, RecursiveMode, Watcher};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{path::BaseDirectory, AppHandle, Emitter, Manager};

const THEME_PATH: &str = "limusic/matugen.css";

fn path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().resolve(THEME_PATH, BaseDirectory::Config).map_err(|e| e.to_string())
}

pub fn read(app: &AppHandle) -> Result<Option<String>, String> {
    match fs::read_to_string(path(app)?) {
        Ok(css) => Ok(Some(css)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

struct WatcherRuntime {
    stop: Arc<AtomicBool>,
    thread: thread::JoinHandle<()>,
}

static WATCHER: Mutex<Option<WatcherRuntime>> = Mutex::new(None);

pub fn start(app: AppHandle) -> Result<(), String> {
    let mut guard = WATCHER.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_some() {
        return Ok(());
    }

    let watched =
        path(&app).map_err(|e| format!("Matugen could not resolve its config path: {e}"))?;
    let Some(parent) = watched.parent().map(PathBuf::from) else {
        return Err(format!("Matugen config path has no parent directory: {}", watched.display()));
    };
    fs::create_dir_all(&parent).map_err(|e| {
        format!("Matugen could not create its config directory {}: {e}", parent.display())
    })?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = stop.clone();

    let (tx, rx) = mpsc::channel();
    let mut watcher = recommended_watcher(move |r| {
        let _ = tx.send(r);
    })
    .map_err(|e| format!("Matugen could not create a filesystem watcher: {e}"))?;
    watcher.watch(&parent, RecursiveMode::NonRecursive).map_err(|e| {
        format!("Matugen could not watch its config directory {}: {e}", parent.display())
    })?;

    let handle_app = app.clone();
    let thread = thread::spawn(move || {
        let _watcher = watcher;
        let mut pending = false;

        while !stop_for_thread.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(event)) => {
                    if event.kind.is_access() {
                        continue;
                    }
                    if !event.paths.iter().any(|p| p == &watched) {
                        continue;
                    }
                    pending = true;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "Matugen watcher reported an error");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if pending {
                        pending = false;
                        let css = fs::read_to_string(&watched).ok();
                        let _ = handle_app.emit("matugen-theme-changed", css);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    guard.replace(WatcherRuntime { stop, thread });
    Ok(())
}

pub fn stop() {
    let runtime = {
        let mut guard = WATCHER.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    if let Some(runtime) = runtime {
        runtime.stop.store(true, Ordering::Relaxed);
        let _ = runtime.thread.join();
    }
}
