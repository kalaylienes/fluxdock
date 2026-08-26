//! Telling the user a new version exists, and installing it when they say so.
//!
//! The check is a plain HTTPS request for a small manifest, and the installer it
//! points at is verified against a public key compiled into this binary before
//! anything is run. Without that signature the app would be downloading an
//! executable off the internet and trusting it, which is not a thing to do
//! quietly on somebody else's machine.
//!
//! Nothing installs itself. A found update sits in the tray menu until it is
//! clicked, because an app that replaces itself while a game is in front is the
//! same class of rudeness as one that opens a console window.

use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::RwLock;
use tauri::{AppHandle, Manager};

/// Long enough after start that the first check never competes with the work of
/// getting the widget on screen.
const FIRST_CHECK_DELAY_SECS: u64 = 90;

/// A release every few hours would be unusual, and this runs on somebody's
/// machine for weeks at a time.
const CHECK_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// Version string of an update that is waiting, if there is one.
static AVAILABLE: RwLock<Option<String>> = RwLock::new(None);

/// Set while a download is in flight so a second click cannot start another.
static INSTALLING: AtomicBool = AtomicBool::new(false);

pub fn available() -> Option<String> {
    AVAILABLE.read().clone()
}

pub fn installing() -> bool {
    INSTALLING.load(Ordering::Relaxed)
}

/// Whether this build can replace itself.
///
/// Windows always can. On Linux only an AppImage can: a `.deb` is owned by the
/// package manager, and replacing it means `dpkg -i` as root, which a widget in
/// the tray has no business asking for. Where the answer is no, the menu says
/// nothing about updates at all rather than offering something that fails.
pub fn self_updatable() -> bool {
    if cfg!(windows) {
        return true;
    }
    std::env::var_os("APPIMAGE").is_some()
}

/// Starts the background check loop. Honours the setting on every pass rather
/// than only at startup, so turning it off takes effect without a restart.
pub fn watch(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(FIRST_CHECK_DELAY_SECS)).await;
        loop {
            if enabled(&app) {
                check(&app).await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(CHECK_INTERVAL_SECS)).await;
        }
    });
}

fn enabled(app: &AppHandle) -> bool {
    app.try_state::<std::sync::Arc<crate::state::AppState>>()
        .map(|s| s.settings.get().updates.check)
        .unwrap_or(true)
}

/// Asks once and records the answer. Quiet on failure: no network is a normal
/// state for a machine, not something to interrupt anybody about.
pub async fn check(app: &AppHandle) -> Option<String> {
    use tauri_plugin_updater::UpdaterExt;

    let updater = app.updater().ok()?;
    let found = match updater.check().await {
        Ok(found) => found,
        Err(e) => {
            tracing::debug!("update check failed: {e}");
            return None;
        }
    };

    let version = found.map(|u| u.version.clone());
    if version.is_some() {
        tracing::info!("update available: {}", version.as_deref().unwrap_or("?"));
    }
    *AVAILABLE.write() = version.clone();
    if let Some(state) = app.try_state::<std::sync::Arc<crate::state::AppState>>() {
        state
            .settings
            .update(|s| s.updates.last_check = Some(chrono::Utc::now()));
    }
    crate::tray::rebuild(app);
    version
}

/// Downloads, verifies and installs. The installer restarts the app itself, so
/// this only has to get out of its way.
pub async fn install(app: &AppHandle) {
    use tauri_plugin_updater::UpdaterExt;

    if INSTALLING.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::tray::rebuild(app);

    let outcome = async {
        let updater = app.updater().map_err(|e| e.to_string())?;
        let update = updater
            .check()
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no update to install".to_string())?;
        update
            .download_and_install(|_, _| {}, || {})
            .await
            .map_err(|e| e.to_string())
    }
    .await;

    match outcome {
        Ok(()) => {
            // The bundle was replaced under us. Leaving the old process running
            // would keep the single instance lock and block the copy the
            // installer is about to start.
            tracing::info!("update installed, exiting for the installer to restart us");
            crate::state::begin_quit();
            app.exit(0);
        }
        Err(e) => {
            INSTALLING.store(false, Ordering::SeqCst);
            tracing::warn!("update failed: {e}");
            crate::tray::rebuild(app);
            crate::diagnostics::message_box(
                "FluxDock",
                &format!("The update could not be installed.\n\n{e}"),
            );
        }
    }
}
