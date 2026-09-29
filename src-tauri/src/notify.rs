//! "Now playing" notifications on a track change (issue #94), behind the `track_notifications`
//! setting. Off by default: Linux desktops already show the MPRIS widget, and a notification per
//! song is a preference, not something everyone wants.
//!
//! Only while no Limusic window has focus: a song change the user is looking at needs no toast.
//! Best-effort like `media.rs`: a missing notification daemon is a `debug!` line.

#[cfg(all(unix, not(target_os = "macos")))]
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::{AppHandle, Manager};

/// The daemon's id for the last one we showed. Passed back as `replaces_id`, so each song replaces
/// the previous song's notification instead of leaving one per track in the history.
#[cfg(all(unix, not(target_os = "macos")))]
static LAST_ID: AtomicU32 = AtomicU32::new(0);

pub fn track_changed(app: &AppHandle, title: &str, artists: &str) {
    if app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false)) {
        return;
    }
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(artists).appname("Limusic");
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // The binary name is the icon name the .deb/.rpm install. An AppImage has no themed icon,
        // so there the daemon shows its generic one.
        n.auto_icon()
            .hint(notify_rust::Hint::SuppressSound(true))
            .id(LAST_ID.load(Ordering::Relaxed));
    }
    // A toast is only delivered for an AppUserModelID that a Start menu shortcut registers, and the
    // NSIS installer registers the bundle identifier. A dev build has no shortcut, so it keeps
    // notify-rust's default (PowerShell's).
    #[cfg(target_os = "windows")]
    if !tauri::is_dev() {
        n.app_id(&app.config().identifier);
    }
    // Which app macOS files the notification under. Errors after the first call (it is set once per
    // process), which is fine. A dev binary has no bundle, so it posts as Terminal.
    #[cfg(target_os = "macos")]
    let _ = notify_rust::set_application(if tauri::is_dev() {
        "com.apple.Terminal"
    } else {
        &app.config().identifier
    });
    // A blocking D-Bus / WinRT / AppKit call: keep it off the playback path.
    std::thread::spawn(move || match n.show() {
        #[cfg(all(unix, not(target_os = "macos")))]
        Ok(h) => LAST_ID.store(h.id(), Ordering::Relaxed),
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        Ok(_) => {}
        Err(e) => tracing::debug!("notification: {e}"),
    });
}
