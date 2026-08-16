//! Tray icon, badge and context menu.

use std::sync::Arc;

use parking_lot::RwLock;
use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::model::{ProviderStatus, UsagePayload};
use crate::state::{AppState, Refresh};
use crate::{diagnostics, monitor, window};

pub const TRAY_ID: &str = "main";

/// Right clicking the widget opens the same menu, so it is kept here.
pub struct MenuHandle(pub RwLock<Option<Menu<Wry>>>);

const ICON_GREEN: &[u8] = include_bytes!("../icons/tray-green.png");
const ICON_AMBER: &[u8] = include_bytes!("../icons/tray-amber.png");
const ICON_RED: &[u8] = include_bytes!("../icons/tray-red.png");
const ICON_GRAY: &[u8] = include_bytes!("../icons/tray-gray.png");

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    app.manage(MenuHandle(RwLock::new(None)));

    let menu = build_menu(app)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(ICON_GRAY)?)
        .tooltip("FluxDock")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                window::toggle_visibility(tray.app_handle());
            }
        })
        .build(app)?;

    app.state::<MenuHandle>().0.write().replace(menu);
    Ok(())
}

/// Rebuilt whenever settings or attached monitors change, because there is no
/// reliable menu-about-to-open event on Windows tray menus.
pub fn rebuild(app: &AppHandle) {
    let Ok(menu) = build_menu(app) else { return };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_menu(Some(menu.clone()));
    }
    app.state::<MenuHandle>().0.write().replace(menu);
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let state = app.state::<Arc<AppState>>();
    let s = state.settings.get();

    let header =
        MenuItemBuilder::with_id("header", format!("FluxDock v{}", env!("CARGO_PKG_VERSION")))
            .enabled(false)
            .build(app)?;

    let refresh = MenuItemBuilder::with_id("refresh", "Refresh now").build(app)?;
    let restart = MenuItemBuilder::with_id("restart", "Force restart").build(app)?;
    let show_widget = CheckMenuItemBuilder::with_id("show_widget", "Show widget")
        .checked(s.widget.visible)
        .build(app)?;
    let animations = CheckMenuItemBuilder::with_id("animations", "Animations")
        .checked(s.appearance.animations)
        .build(app)?;

    let mut freq = SubmenuBuilder::new(app, "Update frequency");
    for (secs, label) in [
        (60u64, "1 minute"),
        (180, "3 minutes"),
        (300, "5 minutes"),
        (900, "15 minutes"),
    ] {
        freq = freq.item(
            &CheckMenuItemBuilder::with_id(format!("freq:{secs}"), label)
                .checked(s.polling.http_interval_secs == secs)
                .build(app)?,
        );
    }
    let freq = freq.build()?;

    let placement = SubmenuBuilder::new(app, "Placement")
        .item(
            &CheckMenuItemBuilder::with_id("place:float", "Floating widget")
                .checked(!s.widget.taskbar_mode())
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("place:taskbar", "Pinned to taskbar")
                .checked(s.widget.taskbar_mode())
                .build(app)?,
        )
        .build()?;

    let mut mon = SubmenuBuilder::new(app, "Monitor");
    #[cfg(windows)]
    {
        let monitors = monitor::enumerate();
        let selected = s.widget.monitor_stable_id.clone();
        let mut saved_present = false;

        for (i, m) in monitors.iter().enumerate() {
            let checked = selected.as_deref() == Some(m.stable_id.as_str())
                || (selected.is_none() && m.primary);
            if selected.as_deref() == Some(m.stable_id.as_str()) {
                saved_present = true;
            }
            let label = format!(
                "{}: {}{}",
                i + 1,
                m.friendly_name,
                if m.primary { " (primary)" } else { "" }
            );
            mon = mon.item(
                &CheckMenuItemBuilder::with_id(format!("mon:{}", m.stable_id), label)
                    .checked(checked)
                    .build(app)?,
            );
        }

        // A saved monitor that is currently unplugged stays listed so the
        // choice is visible and comes back when the display returns.
        if let (Some(id), false) = (selected.as_deref(), saved_present) {
            let name = s
                .widget
                .monitor_friendly_name
                .clone()
                .unwrap_or_else(|| "saved monitor".into());
            mon = mon.item(
                &CheckMenuItemBuilder::with_id(format!("mon:{id}"), format!("(not connected) {name}"))
                    .checked(true)
                    .build(app)?,
            );
        }
    }
    let mon = mon
        .separator()
        .item(&MenuItemBuilder::with_id("reset_position", "Reset position").build(app)?)
        .build()?;

    let providers = SubmenuBuilder::new(app, "Providers")
        .item(
            &CheckMenuItemBuilder::with_id("prov:claude", "Claude Code")
                .checked(s.providers.claude.enabled)
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("prov:codex", "Codex CLI")
                .checked(s.providers.codex.enabled)
                .build(app)?,
        )
        .separator()
        .item(
            &CheckMenuItemBuilder::with_id("model_weekly", "Show Opus and Sonnet weekly rows")
                .checked(s.providers.claude.show_model_weekly)
                .build(app)?,
        )
        .build()?;

    let mut appearance = SubmenuBuilder::new(app, "Appearance");
    for (id, label) in [
        ("theme:system", "Theme: System"),
        ("theme:dark", "Theme: Dark"),
        ("theme:light", "Theme: Light"),
    ] {
        let key = id.split(':').nth(1).unwrap_or("system");
        appearance = appearance.item(
            &CheckMenuItemBuilder::with_id(id, label)
                .checked(s.appearance.theme == key)
                .build(app)?,
        );
    }
    let appearance = appearance
        .separator()
        .item(
            &CheckMenuItemBuilder::with_id("compact", "Compact mode")
                .checked(s.widget.compact_mode)
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("click_through", "Click-through")
                .checked(s.widget.click_through)
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("hide_fullscreen", "Hide during fullscreen apps")
                .checked(s.widget.hide_on_fullscreen)
                .build(app)?,
        )
        .build()?;

    let notifications = SubmenuBuilder::new(app, "Notifications")
        .item(
            &CheckMenuItemBuilder::with_id("notify:70", "Notify at 70%")
                .checked(s.notifications.at_70)
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("notify:90", "Notify at 90%")
                .checked(s.notifications.at_90)
                .build(app)?,
        )
        .build()?;

    let autostart = CheckMenuItemBuilder::with_id("autostart", "Start with Windows")
        .checked(s.autostart)
        .build(app)?;

    let diag = SubmenuBuilder::new(app, "Diagnostics")
        .item(&MenuItemBuilder::with_id("diag:report", "Save diagnostic report").build(app)?)
        .item(&MenuItemBuilder::with_id("diag:logs", "Open log folder").build(app)?)
        .build()?;

    MenuBuilder::new(app)
        .item(&header)
        .separator()
        .item(&refresh)
        .item(&restart)
        .item(&show_widget)
        .separator()
        .item(&animations)
        .item(&freq)
        .item(&placement)
        .item(&mon)
        .item(&providers)
        .item(&appearance)
        .item(&notifications)
        .item(&autostart)
        .separator()
        .item(&MenuItemBuilder::with_id("open_settings", "Open settings file").build(app)?)
        .item(&diag)
        .separator()
        .item(&MenuItemBuilder::with_id("exit", "Exit").build(app)?)
        .build()
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().0.clone();
    let state = app.state::<Arc<AppState>>().inner().clone();

    let mut needs_rebuild = true;
    let mut needs_config = false;
    let mut needs_reposition = false;

    match id.as_str() {
        "refresh" => {
            state.request(Refresh::Manual);
            needs_rebuild = false;
        }
        "restart" => {
            app.restart();
        }
        "show_widget" => window::toggle_visibility(app),
        "animations" => {
            state
                .settings
                .update(|s| s.appearance.animations = !s.appearance.animations);
            needs_config = true;
        }
        "reset_position" => {
            state.settings.update(|s| {
                s.widget.monitor_stable_id = None;
                s.widget.monitor_friendly_name = None;
                s.widget.edge_offset_x = 0;
                s.widget.edge_offset_y = 0;
            });
            needs_reposition = true;
        }
        "model_weekly" => {
            state.settings.update(|s| {
                s.providers.claude.show_model_weekly = !s.providers.claude.show_model_weekly
            });
            needs_config = true;
        }
        "compact" => {
            state
                .settings
                .update(|s| s.widget.compact_mode = !s.widget.compact_mode);
            needs_config = true;
        }
        "click_through" => {
            let now = state
                .settings
                .update(|s| s.widget.click_through = !s.widget.click_through)
                .widget
                .click_through;
            window::set_click_through(app, now);
        }
        "hide_fullscreen" => {
            state
                .settings
                .update(|s| s.widget.hide_on_fullscreen = !s.widget.hide_on_fullscreen);
        }
        "autostart" => {
            let enabled = state.settings.update(|s| s.autostart = !s.autostart).autostart;
            crate::autostart::apply(app, enabled);
        }
        "open_settings" => {
            let _ = open_path(&crate::settings::settings_path());
        }
        "diag:report" => diagnostics::save_report(app),
        "diag:logs" => {
            let _ = open_path(&crate::settings::data_dir().join("logs"));
        }
        "exit" => {
            app.exit(0);
            return;
        }
        other => {
            if let Some(secs) = other.strip_prefix("freq:").and_then(|v| v.parse::<u64>().ok()) {
                state.settings.update(|s| s.polling.http_interval_secs = secs);
                state.request(Refresh::Settings);
            } else if let Some(mode) = other.strip_prefix("place:") {
                let mode = mode.to_string();
                state.settings.update(|s| s.widget.mode = mode.clone());
                needs_reposition = true;
                needs_config = true;
            } else if let Some(theme) = other.strip_prefix("theme:") {
                let theme = theme.to_string();
                state.settings.update(|s| s.appearance.theme = theme.clone());
                needs_config = true;
            } else if let Some(mon_id) = other.strip_prefix("mon:") {
                let mon_id = mon_id.to_string();
                #[cfg(windows)]
                let friendly = monitor::enumerate()
                    .into_iter()
                    .find(|m| m.stable_id == mon_id)
                    .map(|m| m.friendly_name);
                #[cfg(not(windows))]
                let friendly: Option<String> = None;
                state.settings.update(|s| {
                    s.widget.monitor_stable_id = Some(mon_id.clone());
                    s.widget.monitor_friendly_name = friendly.clone();
                    s.widget.edge_offset_x = 0;
                    s.widget.edge_offset_y = 0;
                });
                needs_reposition = true;
            } else if let Some(level) = other.strip_prefix("notify:") {
                let level = level.to_string();
                state.settings.update(|s| match level.as_str() {
                    "70" => s.notifications.at_70 = !s.notifications.at_70,
                    "90" => s.notifications.at_90 = !s.notifications.at_90,
                    _ => {}
                });
            } else if let Some(p) = other.strip_prefix("prov:") {
                let p = p.to_string();
                state.settings.update(|s| match p.as_str() {
                    "claude" => s.providers.claude.enabled = !s.providers.claude.enabled,
                    "codex" => s.providers.codex.enabled = !s.providers.codex.enabled,
                    _ => {}
                });
                state.request(Refresh::Settings);
                needs_reposition = true;
            } else {
                needs_rebuild = false;
            }
        }
    }

    if needs_reposition {
        window::reposition(app);
    }
    if needs_config {
        let _ = app.emit("config", state.appearance());
    }
    if needs_rebuild {
        rebuild(app);
    }
}

/// The badge shows the highest live window across enabled providers.
pub fn update_badge(app: &AppHandle, payload: &UsagePayload) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    let peak = payload
        .providers
        .iter()
        .filter_map(|p| p.peak())
        .fold(None, |acc: Option<f32>, v| Some(acc.map_or(v, |a| a.max(v))));

    let needs_attention = payload.providers.iter().any(|p| {
        matches!(
            p.status,
            ProviderStatus::ReauthNeeded | ProviderStatus::PlanUnsupported
        )
    });

    let (kind, bytes) = match peak {
        None => (0u8, ICON_GRAY),
        Some(_) if needs_attention => (1, ICON_AMBER),
        Some(v) if v >= 90.0 => (2, ICON_RED),
        Some(v) if v >= 70.0 => (1, ICON_AMBER),
        Some(_) => (3, ICON_GREEN),
    };

    static LAST_ICON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);
    if LAST_ICON.swap(kind, std::sync::atomic::Ordering::Relaxed) != kind {
        if let Ok(icon) = Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(icon));
        }
    }

    let mut parts: Vec<String> = Vec::new();
    for p in &payload.providers {
        match (p.five_hour.as_ref(), p.weekly.as_ref()) {
            (Some(a), Some(b)) => parts.push(format!(
                "{} 5h {:.0}% / 7d {:.0}%",
                p.label, a.utilization, b.utilization
            )),
            (Some(a), None) => parts.push(format!("{} 5h {:.0}%", p.label, a.utilization)),
            (None, Some(b)) => parts.push(format!("{} 7d {:.0}%", p.label, b.utilization)),
            (None, None) => {
                if let Some(d) = &p.detail {
                    parts.push(format!("{}: {}", p.label, d));
                }
            }
        }
    }
    let tooltip = if parts.is_empty() {
        "FluxDock".to_string()
    } else {
        parts.join("  |  ")
    };
    let _ = tray.set_tooltip(Some(tooltip));
}

fn open_path(path: &std::path::Path) -> std::io::Result<()> {
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(path));
    crate::providers::quiet_command("cmd")
        .args(["/C", "start", "", &path.to_string_lossy()])
        .spawn()
        .map(|_| ())
}
