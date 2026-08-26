//! FluxDock, a usage widget for Claude Code, Codex CLI and Antigravity.

pub mod aggregator;
pub mod autostart;
pub mod diagnostics;
#[cfg(not(windows))]
pub mod fullscreen_x11;
pub mod jsonl;
pub mod model;
pub mod monitor;
pub mod net;
pub mod providers;
pub mod settings;
pub mod shell;
pub mod state;
pub mod theme;
pub mod tray;
pub mod update;
pub mod window;

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::model::{AppearanceConfig, UsagePayload};
use crate::providers::UsageProvider;
use crate::settings::SettingsStore;
use crate::state::AppState;

#[tauri::command]
fn get_usage(state: tauri::State<'_, Arc<AppState>>) -> Option<UsagePayload> {
    state.payload.read().clone()
}

#[tauri::command]
fn get_appearance(state: tauri::State<'_, Arc<AppState>>) -> AppearanceConfig {
    state.appearance()
}

/// The interface reports how tall its content actually is, and the floating
/// window is sized to that. One provider needs half the height of two, and a
/// fixed size left the smaller case padded with dead space at both ends.
#[tauri::command]
fn set_content_height(app: AppHandle, height: f64) {
    window::set_content_height(&app, height);
}

#[tauri::command]
fn show_context_menu(app: AppHandle, window: tauri::WebviewWindow) {
    let menu = app.state::<tray::MenuHandle>().0.read().clone();
    if let Some(menu) = menu {
        window::foreground_for_menu(&window);
        // Displaying the menu borrows it for the whole modal loop, and that
        // loop keeps pumping queued work. Any rebuild that lands in the middle
        // would take a second borrow of the same object and abort the process,
        // so rebuilds are held back until the guard drops.
        let guard = tray::PopupGuard::new();
        let _ = window.popup_menu(&menu);
        drop(guard);
        tray::flush_pending_rebuild(&app);
    }
}

/// Records where a panic happened before the process goes away.
///
/// The release profile aborts on panic and the app runs without a console, so
/// without this a crash leaves nothing behind but a Windows fault code.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".into());

        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".into());

        // A panic while the session is being torn down is not a fault worth a
        // crash report: the shell has already destroyed the window and the
        // process was going away regardless. Filing it would bury the reports
        // that do mean something.
        if session_ending() {
            tracing::warn!("panic while the session was closing, at {location}: {message}");
            previous(info);
            return;
        }

        tracing::error!("panic at {location}: {message}");

        // Written separately because the tracing appender may not flush before
        // the process aborts.
        let path = settings::data_dir().join("last-crash.txt");
        let _ = std::fs::create_dir_all(settings::data_dir());
        let _ = std::fs::write(
            path,
            format!(
                "{}\npanic at {location}\n{message}\n",
                chrono::Utc::now().to_rfc3339()
            ),
        );

        previous(info);
    }));
}

/// Marks a launch by the watchdog task rather than by a person.
const WATCHDOG_FLAG: &str = "--watchdog";

/// Followed by the process id to wait for. Used by the restart menu item.
const RELAUNCH_FLAG: &str = "--relaunch-after";

fn launched_by_watchdog() -> bool {
    std::env::args().any(|a| a == WATCHDOG_FLAG)
}

/// The process this launch is supposed to outlive, if it is a restart helper.
fn relaunch_target() -> Option<u32> {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == RELAUNCH_FLAG {
            return args.next().and_then(|v| v.parse().ok());
        }
    }
    None
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The watchdog runs this executable itself rather than going through a shell,
    // because a scheduled task that starts a console program flashes a window
    // over whatever is in front, once a minute, forever. That means the "stay
    // closed" marker has to be honoured here: reaching this point at all means
    // no instance was running, so this launch would undo a deliberate quit.
    if launched_by_watchdog() && settings::stay_closed_marker().exists() {
        return;
    }

    // Restart helper. Nothing of the app is built in this mode: it waits for the
    // process that asked for the restart, starts a fresh one and returns.
    if let Some(pid) = relaunch_target() {
        shell::relaunch_after(pid, std::time::Duration::from_secs(20));
        return;
    }

    init_logging();
    install_panic_hook();

    let mut builder = tauri::Builder::default();

    // Single instance has to register first so a second launch reaches the
    // running process and brings the widget back.
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // A watchdog check is not a request to see the widget. Unhiding it
            // every minute would override the user, and showing anything over a
            // fullscreen game costs the game its mode.
            if argv.iter().any(|a| a == WATCHDOG_FLAG) {
                return;
            }
            window::show_by_request(app);
        }));
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostarted"]),
        ));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_usage,
            get_appearance,
            set_content_height,
            show_context_menu
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            settings::clear_stay_closed();

            let store = Arc::new(SettingsStore::load());
            let (state, rx) = AppState::new(store.clone());
            app.manage(state.clone());

            // Before the tray menu, which lists the displays, and before any
            // placement. On Linux the layout can only be read from this thread,
            // and both of those need it immediately.
            monitor::init(&handle);

            tray::setup(&handle)?;
            autostart::sync(&handle, store.get().autostart);
            update::watch(&handle);

            window::apply_ex_styles(&handle);
            window::reposition(&handle);

            let settings = store.get();
            if settings.widget.click_through {
                window::set_click_through(&handle, true);
            }
            let force_show = std::env::args().any(|a| a == "--show");
            if settings.widget.visible || force_show {
                window::show(&handle);
            }

            {
                let h = handle.clone();
                theme::watch(move |_dark| {
                    if let Some(state) = h.try_state::<Arc<AppState>>() {
                        let _ = h.emit("config", state.appearance());
                    }
                });
            }

            if std::env::args().any(|a| a == "--demo") {
                let h = handle.clone();
                let s = state.clone();
                tauri::async_runtime::spawn(async move {
                    aggregator::run_demo(h, s).await;
                });
            } else {
                let paths = watch_paths(&state);
                aggregator::spawn_watchers(&handle, paths);

                let h = handle.clone();
                let s = state.clone();
                tauri::async_runtime::spawn(async move {
                    aggregator::run(h, s, rx).await;
                });
            }

            window::spawn_reconciler(handle.clone());

            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Moved(_) => window::on_moved(window.app_handle()),
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // The widget hides instead of closing; the tray is the way back.
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("failed to start the application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                // Hiding the last window must not end the process, but a
                // deliberate quit has to get through. Preventing this
                // unconditionally made the Exit menu item do nothing at all.
                //
                // Windows signing the user out has to get through as well. The
                // shell destroys the window first and the event loop cannot be
                // driven past that point: keeping it alive aborts inside the
                // toolkit with "cannot move state from Destroyed", which lands
                // in the crash report as if something had gone wrong during an
                // otherwise orderly shutdown.
                let torn_down = session_ending() || window::widget(app).is_none();
                if !state::is_quitting() && !torn_down {
                    api.prevent_exit();
                }
            }
        });
}

/// Has the shell started tearing the session down?
#[cfg(windows)]
fn session_ending() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};
    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
}

#[cfg(not(windows))]
fn session_ending() -> bool {
    false
}

/// Collected from the providers themselves so a new source does not need a
/// second list of paths kept in sync by hand.
fn watch_paths(state: &Arc<AppState>) -> Vec<std::path::PathBuf> {
    let claude = state.claude.blocking_lock().watch_paths();
    let codex = state.codex.blocking_lock().watch_paths();
    let antigravity = state.antigravity.blocking_lock().watch_paths();
    claude.into_iter().chain(codex).chain(antigravity).collect()
}

fn init_logging() {
    use tracing_subscriber::prelude::*;

    let dir = settings::data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);

    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("fluxdock")
        .filename_suffix("log")
        .max_log_files(3)
        .build(&dir);

    let filter = tracing_subscriber::EnvFilter::try_from_env("FLUXDOCK_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    match appender {
        Ok(writer) => {
            let (nb, guard) = tracing_appender::non_blocking(writer);
            // The guard has to outlive the process for the writer to flush.
            std::mem::forget(guard);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(nb))
                .try_init();
        }
        Err(_) => {
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer())
                .try_init();
        }
    }
}
