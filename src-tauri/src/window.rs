//! Widget window: placement, visibility, and motion permission.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};

use crate::state::AppState;

pub const WIDGET_LABEL: &str = "widget";

/// Differences below this are compositor rounding, not a deliberate move.
/// Persisting them would accumulate into visible drift across restarts.
const OFFSET_NOISE_PX: i32 = 4;

/// Base cadence of the reconciliation loop.
const TICK: std::time::Duration = std::time::Duration::from_millis(350);

/// How many ticks between topology scans in floating mode, roughly two seconds.
const TOPOLOGY_EVERY: u32 = 6;

/// Fullscreen state oscillates for a few hundred milliseconds while a game
/// enters or leaves, so restoring is delayed.
const RESTORE_DELAY: std::time::Duration = std::time::Duration::from_millis(1500);

static MOVE_TICKET: AtomicU64 = AtomicU64::new(0);

/// Programmatic moves are followed by a `Moved` event that arrives later
/// through the event loop, so a plain flag cleared inline is not enough.
static SUPPRESS_MOVES_UNTIL: AtomicU64 = AtomicU64::new(0);

/// Hidden because of a fullscreen application, as opposed to the user's own
/// visibility preference.
static AUTO_HIDDEN: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn suppress_moves(ms: u64) {
    SUPPRESS_MOVES_UNTIL.store(now_ms() + ms, Ordering::SeqCst);
}

pub fn widget(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(WIDGET_LABEL)
}

pub fn reposition(app: &AppHandle) {
    #[cfg(windows)]
    {
        use crate::monitor;

        let Some(win) = widget(app) else { return };
        let state = app.state::<Arc<AppState>>();
        let s = state.settings.get();

        let Some((mon, how)) = monitor::resolve(s.widget.monitor_stable_id.as_deref()) else {
            return;
        };

        let taskbar_target = if s.widget.taskbar_mode() {
            match monitor::taskbar_for(&mon) {
                Some(bar) => {
                    if !bar.on_screen {
                        let _ = win.hide();
                        return;
                    }
                    let columns = [s.providers.claude.enabled, s.providers.codex.enabled]
                        .iter()
                        .filter(|on| **on)
                        .count();
                    let target = monitor::taskbar_position(&mon, &bar, s.widget.tray_gap, columns);
                    if target.is_none() {
                        tracing::warn!("vertical taskbar cannot host the widget, using floating placement");
                        state.settings.update(|s| s.widget.mode = "float".into());
                    }
                    target
                }
                None => {
                    tracing::warn!("no taskbar found, using floating placement");
                    state.settings.update(|s| s.widget.mode = "float".into());
                    None
                }
            }
        } else {
            None
        };

        let (x, y, w, h) = match taskbar_target {
            Some(t) => t,
            None => {
                let (tx, ty, w, h) =
                    monitor::target_position(&mon, s.widget.edge_offset_x, s.widget.edge_offset_y);
                let (x, y, w, h) = monitor::clamp_or_primary(tx, ty, w, h);

                // If the clamp had to step in, the stored offset is invalid and
                // would keep producing the same wrong target.
                if (x, y) != (tx, ty) && (s.widget.edge_offset_x != 0 || s.widget.edge_offset_y != 0)
                {
                    tracing::info!("stored offset pointed off screen, resetting it");
                    state.settings.update(|s| {
                        s.widget.edge_offset_x = 0;
                        s.widget.edge_offset_y = 0;
                    });
                }
                (x, y, w, h)
            }
        };

        if s.widget.taskbar_mode()
            && s.widget.visible
            && !AUTO_HIDDEN.load(Ordering::SeqCst)
            && !win.is_visible().unwrap_or(true)
        {
            let _ = win.show();
        }

        suppress_moves(1500);
        // Position first, then size, then position again: moving across a DPI
        // boundary otherwise leaves the window at the wrong size.
        let _ = win.set_position(PhysicalPosition::new(x, y));
        let _ = win.set_size(PhysicalSize::new(w as u32, h as u32));
        let _ = win.set_position(PhysicalPosition::new(x, y));
        let _ = win.set_always_on_top(true);
        apply_ex_styles(app);

        // The friendly name is only stored for an explicitly pinned monitor.
        if how == monitor::Resolution::Pinned
            && s.widget.monitor_friendly_name.as_deref() != Some(mon.friendly_name.as_str())
        {
            state.settings.update(|s| {
                s.widget.monitor_friendly_name = Some(mon.friendly_name.clone());
            });
        }
    }
    #[cfg(not(windows))]
    {
        let _ = app;
    }
}

/// Resolves the monitor again after a drag and stores the new corner offset.
pub fn on_moved(app: &AppHandle) {
    if now_ms() < SUPPRESS_MOVES_UNTIL.load(Ordering::SeqCst) {
        return;
    }
    let ticket = MOVE_TICKET.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        if MOVE_TICKET.load(Ordering::SeqCst) != ticket {
            return;
        }
        if now_ms() < SUPPRESS_MOVES_UNTIL.load(Ordering::SeqCst) {
            return;
        }
        finalize_move(&app);
    });
}

#[cfg(windows)]
fn finalize_move(app: &AppHandle) {
    use crate::monitor;

    let Some(win) = widget(app) else { return };

    // In taskbar mode the position is derived, so there is nothing to learn.
    if app.state::<Arc<AppState>>().settings.get().widget.taskbar_mode() {
        return;
    }

    // A hidden window reports whatever position it was created at, which would
    // pin the widget to a meaningless corner.
    if !win.is_visible().unwrap_or(false) {
        return;
    }

    let Ok(pos) = win.outer_position() else { return };
    let Ok(size) = win.outer_size() else { return };

    let center_x = pos.x + size.width as i32 / 2;
    let center_y = pos.y + size.height as i32 / 2;
    let Some(mon) = monitor::monitor_at(center_x, center_y) else {
        return;
    };

    if !monitor::mostly_inside(&mon, pos.x, pos.y, size.width as i32, size.height as i32) {
        return;
    }

    let (base_x, base_y, _, _) = monitor::target_position(&mon, 0, 0);
    let snap = |d: i32| if d.abs() < OFFSET_NOISE_PX { 0 } else { d };
    let offset_x = snap(base_x - pos.x);
    let offset_y = snap(base_y - pos.y);

    let state = app.state::<Arc<AppState>>();
    let current = state.settings.get().widget;

    // Sitting at the default corner of the primary display with no pin is not
    // worth recording, and pinning here would disable following the primary.
    if offset_x == 0 && offset_y == 0 && current.monitor_stable_id.is_none() && mon.primary {
        return;
    }
    if current.monitor_stable_id.as_deref() == Some(mon.stable_id.as_str())
        && current.edge_offset_x == offset_x
        && current.edge_offset_y == offset_y
    {
        return;
    }

    state.settings.update(|s| {
        s.widget.monitor_stable_id = Some(mon.stable_id.clone());
        s.widget.monitor_friendly_name = Some(mon.friendly_name.clone());
        s.widget.edge_offset_x = offset_x;
        s.widget.edge_offset_y = offset_y;
    });
    crate::tray::rebuild(app);
}

#[cfg(not(windows))]
fn finalize_move(_app: &AppHandle) {}

pub fn show(app: &AppHandle) {
    let Some(win) = widget(app) else { return };
    AUTO_HIDDEN.store(false, Ordering::SeqCst);
    reposition(app);
    let _ = win.show();
    let _ = win.set_always_on_top(true);
    apply_ex_styles(app);
    let state = app.state::<Arc<AppState>>();
    state.settings.update(|s| s.widget.visible = true);
    crate::tray::rebuild(app);
    let _ = app.emit("config", state.appearance());
}

pub fn toggle_visibility(app: &AppHandle) {
    let Some(win) = widget(app) else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        app.state::<Arc<AppState>>()
            .settings
            .update(|s| s.widget.visible = false);
        crate::tray::rebuild(app);
    } else {
        show(app);
    }
}

/// `WS_EX_NOACTIVATE` keeps clicks from stealing focus from the foreground
/// application, `WS_EX_TOOLWINDOW` keeps the widget out of alt tab. Tao rewrites
/// its own flag set on visibility and z order changes, so these are reapplied
/// after every placement pass.
#[cfg(windows)]
pub fn apply_ex_styles(app: &AppHandle) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    let Some(win) = widget(app) else { return };
    let Some(hwnd) = crate::monitor::hwnd_of(&win) else {
        return;
    };
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let updated = current | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
        if current != updated {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, updated);
        }
    }
}

#[cfg(not(windows))]
pub fn apply_ex_styles(_app: &AppHandle) {}

/// Brings the window forward so the context menu can dismiss on an outside
/// click. `WS_EX_NOACTIVATE` may refuse; the menu still opens.
#[cfg(windows)]
pub fn foreground_for_menu(window: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    if let Some(hwnd) = crate::monitor::hwnd_of(window) {
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(not(windows))]
pub fn foreground_for_menu(_window: &tauri::WebviewWindow) {}

/// Hides the widget while a fullscreen application is in front and brings it
/// back afterwards. The user's own visibility preference is left untouched.
fn apply_fullscreen_hiding(app: &AppHandle, fullscreen: bool, stable_clear: bool) {
    let state = app.state::<Arc<AppState>>();
    let s = state.settings.get();

    if !s.widget.hide_on_fullscreen {
        if AUTO_HIDDEN.swap(false, Ordering::SeqCst) && s.widget.visible {
            if let Some(win) = widget(app) {
                let _ = win.show();
            }
        }
        return;
    }

    let Some(win) = widget(app) else { return };

    if fullscreen {
        if !AUTO_HIDDEN.load(Ordering::SeqCst) && win.is_visible().unwrap_or(false) {
            tracing::info!("fullscreen application detected, hiding the widget");
            let _ = win.hide();
            AUTO_HIDDEN.store(true, Ordering::SeqCst);
        }
    } else if stable_clear && AUTO_HIDDEN.swap(false, Ordering::SeqCst) && s.widget.visible {
        tracing::info!("fullscreen ended, restoring the widget");
        reposition(app);
        let _ = win.show();
        let _ = win.set_always_on_top(true);
        apply_ex_styles(app);
    }
}

/// Reclaims the top of the topmost band when something has covered the widget.
///
/// The toolkit's always-on-top setter is a no-op here: the flag is set when the
/// window is created, so the call returns before it reaches the platform. That
/// left the widget with no way to recover its stacking position. The shell
/// raises its own window whenever the taskbar, Start menu or notification area
/// is touched, and in taskbar placement that leaves the widget behind an opaque
/// strip. Checking first means the window is only restacked when it is really
/// obscured, so other topmost windows are not fought for no reason.
#[cfg(windows)]
fn ensure_on_top(app: &AppHandle) {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetWindowRect, SetWindowPos, WindowFromPoint, GA_ROOT, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    let Some(win) = widget(app) else { return };
    if !win.is_visible().unwrap_or(false) {
        return;
    }
    let Some(hwnd) = crate::monitor::hwnd_of(&win) else {
        return;
    };

    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return;
        }
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return;
        }

        let centre = POINT {
            x: (rect.left + rect.right) / 2,
            y: (rect.top + rect.bottom) / 2,
        };
        if GetAncestor(WindowFromPoint(centre), GA_ROOT) == hwnd {
            return;
        }

        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(windows))]
fn ensure_on_top(_app: &AppHandle) {}

/// Keeps taskbar placement visible when it should be.
///
/// `reposition` hides the widget while an auto hiding strip is away and relies
/// on a later layout change to bring it back. If that change never arrives the
/// widget stays hidden with nothing to recover it, so visibility is reconciled
/// on every tick instead of only on transitions.
#[cfg(windows)]
fn reconcile_taskbar_visibility(app: &AppHandle) {
    let state = app.state::<Arc<AppState>>();
    let s = state.settings.get();

    if !s.widget.taskbar_mode() || !s.widget.visible || AUTO_HIDDEN.load(Ordering::SeqCst) {
        return;
    }
    let Some(win) = widget(app) else { return };
    if win.is_visible().unwrap_or(true) {
        return;
    }

    let Some((mon, _)) = crate::monitor::resolve(s.widget.monitor_stable_id.as_deref()) else {
        return;
    };
    if crate::monitor::taskbar_for(&mon).map(|b| b.on_screen).unwrap_or(false) {
        tracing::info!("taskbar strip is back, restoring the widget");
        reposition(app);
        let _ = win.show();
    }
}

#[cfg(not(windows))]
fn reconcile_taskbar_visibility(_app: &AppHandle) {}

pub fn set_click_through(app: &AppHandle, enabled: bool) {
    if let Some(win) = widget(app) {
        let _ = win.set_ignore_cursor_events(enabled);
    }
}

/// Keeps placement in step with the desktop.
///
/// A signature of the current work areas is compared instead of hooking window
/// messages, which covers hotplug, sleep, taskbar moves, DPI changes and auto
/// hide transitions through one path.
pub fn spawn_reconciler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last_signature = String::new();
        let mut last_motion = true;
        let mut clear_since: Option<std::time::Instant> = None;
        let mut tick: u32 = 0;

        loop {
            tokio::time::sleep(TICK).await;
            tick = tick.wrapping_add(1);

            #[cfg(windows)]
            {
                let taskbar_mode = app
                    .state::<Arc<AppState>>()
                    .settings
                    .get()
                    .widget
                    .taskbar_mode();
                // The topology scan calls QueryDisplayConfig, so it does not run
                // on every tick unless the strip itself has to be tracked.
                if taskbar_mode || tick % TOPOLOGY_EVERY == 0 {
                    let signature = topology_signature(&app);
                    if signature != last_signature {
                        last_signature = signature;
                        reposition(&app);
                        // The menu only lists monitors, so it is left to decide
                        // for itself whether anything it shows actually moved.
                        // Taskbar geometry changes constantly and must not
                        // drive menu swaps.
                        crate::tray::rebuild(&app);
                    }
                }
                if taskbar_mode {
                    reconcile_taskbar_visibility(&app);
                }
            }

            let fullscreen = fullscreen_active();
            if fullscreen {
                clear_since = None;
            } else if clear_since.is_none() {
                clear_since = Some(std::time::Instant::now());
            }
            let stable_clear = clear_since
                .map(|t| t.elapsed() >= RESTORE_DELAY)
                .unwrap_or(false);

            apply_fullscreen_hiding(&app, fullscreen, stable_clear);

            // Nothing to reclaim while a real fullscreen application is in
            // front; the widget is meant to be out of the way then.
            if !fullscreen {
                ensure_on_top(&app);
            }

            let visible = widget(&app).and_then(|w| w.is_visible().ok()).unwrap_or(false);
            let motion = visible && !power_saver() && !fullscreen;
            if motion != last_motion {
                last_motion = motion;
                let state = app.state::<Arc<AppState>>();
                state.set_motion_allowed(motion);
                let _ = app.emit("config", state.appearance());
            }
        }
    });
}

#[cfg(windows)]
fn topology_signature(app: &AppHandle) -> String {
    let mut signature = crate::monitor::enumerate()
        .iter()
        .map(|m| {
            format!(
                "{}:{},{},{},{}@{}",
                m.gdi_name, m.work.left, m.work.top, m.work.right, m.work.bottom, m.dpi
            )
        })
        .collect::<Vec<_>>()
        .join("|");

    // The work area does not change when an auto hiding taskbar slides away, so
    // taskbar placement needs the strip in the signature as well.
    let s = app.state::<Arc<AppState>>().settings.get();
    if s.widget.taskbar_mode() {
        if let Some((mon, _)) = crate::monitor::resolve(s.widget.monitor_stable_id.as_deref()) {
            match crate::monitor::taskbar_for(&mon) {
                Some(bar) => signature.push_str(&format!(
                    "|tb:{},{},{},{}:{}:{}",
                    bar.rect.left, bar.rect.top, bar.rect.right, bar.rect.bottom, bar.tray_left,
                    bar.on_screen
                )),
                None => signature.push_str("|tb:none"),
            }
        }
    }
    signature
}

#[cfg(windows)]
fn power_saver() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status).is_err() {
            return false;
        }
        status.SystemStatusFlag == 1
    }
}

/// Fullscreen detection.
///
/// Two independent signals are combined. The shell reports exclusive fullscreen
/// reliably but usually stays quiet for borderless windowed games, so the
/// foreground window is measured against its monitor as well.
///
/// Both only read window geometry. No other process is opened, no memory is
/// read and nothing is injected anywhere.
#[cfg(windows)]
fn fullscreen_active() -> bool {
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };

    unsafe {
        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_BUSY
                || state == QUNS_RUNNING_D3D_FULL_SCREEN
                || state == QUNS_PRESENTATION_MODE
            {
                return true;
            }
        }
    }
    foreground_covers_monitor()
}

#[cfg(windows)]
fn foreground_covers_monitor() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowRect, IsWindowVisible,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() || !IsWindowVisible(hwnd).as_bool() {
            return false;
        }

        // Plenty of system surfaces cover the whole display without being a
        // fullscreen application: the lock screen, the task switcher, Task
        // View, search, and the quick settings flyout among them. Treating any
        // of those as a game hides the widget for as long as they are up, and
        // the lock screen in particular means every unlock starts hidden.
        let mut class = [0u16; 128];
        let len = GetClassNameW(hwnd, &mut class);
        if len > 0 {
            let name = String::from_utf16_lossy(&class[..(len as usize).min(class.len())]);
            if matches!(
                name.as_str(),
                "Progman"
                    | "WorkerW"
                    | "Shell_TrayWnd"
                    | "Shell_SecondaryTrayWnd"
                    | "Shell_LightDismissOverlay"
                    | "Windows.UI.Core.CoreWindow"
                    | "ApplicationFrameWindow"
                    | "XamlExplorerHostIslandWindow"
                    | "MultitaskingViewFrame"
                    | "TaskSwitcherWnd"
                    | "TaskSwitcherOverlayWnd"
                    | "ForegroundStaging"
                    | "ControlCenterWindow"
                    | "NativeHWNDHost"
                    | "EdgeUiInputTopWndClass"
                    | "LockScreenControllerProxyWindow"
            ) {
                return false;
            }
        }

        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }

        let cx = rect.left + (rect.right - rect.left) / 2;
        let cy = rect.top + (rect.bottom - rect.top) / 2;
        let Some(mon) = crate::monitor::monitor_at(cx, cy) else {
            return false;
        };

        let b = mon.bounds;
        rect.left <= b.left + 2
            && rect.top <= b.top + 2
            && rect.right >= b.right - 2
            && rect.bottom >= b.bottom - 2
    }
}

#[cfg(not(windows))]
fn power_saver() -> bool {
    false
}

#[cfg(not(windows))]
fn fullscreen_active() -> bool {
    false
}
