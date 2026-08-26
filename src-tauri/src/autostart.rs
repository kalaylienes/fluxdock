//! Start with the session. The per user Run key on Windows, an XDG autostart
//! entry on Linux. Neither needs elevation, and the plugin picks the right one.

use tauri::AppHandle;

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

pub fn sync(app: &AppHandle, enabled: bool) {
    apply(app, enabled);
}
