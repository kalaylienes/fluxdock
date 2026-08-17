//! FluxDock, a usage widget for Claude Code and Codex CLI on Windows.

pub mod aggregator;
pub mod autostart;
pub mod diagnostics;
pub mod jsonl;
pub mod model;
#[cfg(windows)]
pub mod monitor;
pub mod net;
pub mod providers;
pub mod settings;
pub mod state;
pub mod theme;
pub mod tray;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_logging();
    install_panic_hook();

    let mut builder = tauri::Builder::default();

    // Single instance has to register first so a second launch reaches the
    // running process and brings the widget back.
    #[cfg(windows)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            window::show(app);
        }));
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostarted"]),
        ));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            get_usage,
            get_appearance,
            show_context_menu
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            settings::clear_stay_closed();

            let store = Arc::new(SettingsStore::load());
            let (state, rx) = AppState::new(store.clone());
            app.manage(state.clone());

            tray::setup(&handle)?;
            autostart::sync(&handle, store.get().autostart);

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
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                // Hiding the last window must not end the process, but a
                // deliberate quit has to get through. Preventing this
                // unconditionally made the Exit menu item do nothing at all.
                if !state::is_quitting() {
                    api.prevent_exit();
                }
            }
        });
}

/// Collected from the providers themselves so a new source does not need a
/// second list of paths kept in sync by hand.
fn watch_paths(state: &Arc<AppState>) -> Vec<std::path::PathBuf> {
    let claude = state.claude.blocking_lock().watch_paths();
    let codex = state.codex.blocking_lock().watch_paths();
    claude.into_iter().chain(codex).collect()
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
