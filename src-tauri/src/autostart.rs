//! Start with Windows, through the per user Run key. No elevation needed.

use tauri::AppHandle;

#[cfg(windows)]
pub fn apply(app: &AppHandle, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();

    // Removing an entry that was never created reports a missing file, so the
    // current state is checked first.
    if manager.is_enabled().unwrap_or(false) == enabled {
        return;
    }
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(e) = result {
        tracing::error!("autostart could not be updated: {e}");
    }
}

#[cfg(not(windows))]
pub fn apply(_app: &AppHandle, _enabled: bool) {}

pub fn sync(app: &AppHandle, enabled: bool) {
    apply(app, enabled);
}
