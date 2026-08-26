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

/// Fullscreen state oscillates while a game enters or leaves, and anything that
/// flashes in front of it breaks the signal for a moment: a scheduled task, an
/// installer, a console window. Restoring the widget in that gap raises a
/// topmost window over the game, which costs an exclusive fullscreen game its
/// mode and drops the player to the desktop. The delay is therefore longer than
/// any flash can hold the foreground.
const RESTORE_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

static MOVE_TICKET: AtomicU64 = AtomicU64::new(0);

/// Programmatic moves are followed by a `Moved` event that arrives later
/// through the event loop, so a plain flag cleared inline is not enough.
static SUPPRESS_MOVES_UNTIL: AtomicU64 = AtomicU64::new(0);

/// Hidden because of a fullscreen application, as opposed to the user's own
/// visibility preference.
static AUTO_HIDDEN: AtomicBool = AtomicBool::new(false);

/// Whether a fullscreen application is in front right now, published for the
/// providers. Spawning a process during a game can flash a console window over
/// it, and that flash alone is enough to knock the game out of fullscreen, so
/// every path that starts one checks this first.
static FULLSCREEN_NOW: AtomicBool = AtomicBool::new(false);

/// The window last seen covering its monitor, kept as the raw pointer value.
/// A game that loses the foreground for a moment is still a game.
#[cfg(windows)]
static LAST_FULLSCREEN_HWND: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);

/// Set when the user asks for the widget while something fullscreen is in front.
/// Their request wins until that application goes away.
static SHOWN_ON_PURPOSE: AtomicBool = AtomicBool::new(false);

/// Height the interface last reported needing, in logical pixels, stored as
/// hundredths so it fits an integer. Zero means it has not measured itself yet.
static MEASURED_HEIGHT: AtomicU64 = AtomicU64::new(0);

/// Takes the height the interface measured for its own content and resizes the
/// floating window to match. Taskbar placement ignores it: there the height is
/// the strip's, not the content's.
pub fn set_content_height(app: &AppHandle, logical_h: f64) {
    if !logical_h.is_finite() || logical_h <= 0.0 {
        return;
    }
    let hundredths = (logical_h * 100.0).round() as u64;
    if MEASURED_HEIGHT.swap(hundredths, Ordering::SeqCst) == hundredths {
        return;
    }
    reposition(app);
}

/// What the floating window should be, in logical pixels.
fn floating_height() -> f64 {
    let stored = MEASURED_HEIGHT.load(Ordering::SeqCst);
    if stored == 0 {
        crate::monitor::DEFAULT_LOGICAL_H
    } else {
        stored as f64 / 100.0
    }
}

pub fn fullscreen_now() -> bool {
    FULLSCREEN_NOW.load(Ordering::Relaxed)
}

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
    {
        use crate::monitor;

        let Some(win) = widget(app) else { return };
        let state = app.state::<Arc<AppState>>();
        let s = state.settings.get();

        let Some((mon, how)) = monitor::resolve(s.widget.monitor_stable_id.as_deref()) else {
            return;
        };

        // A desktop with no panel this widget understands cannot host pinned
        // placement at all. Correcting the stored preference rather than
        // returning early matters: the pinned branch below leaves the window
        // unplaced, so a setting carried over from Windows would otherwise
        // leave the widget wherever the compositor first dropped it.
        let pinned = s.widget.taskbar_mode();
        if pinned && !monitor::supports_panel_docking() {
            tracing::info!("no panel to pin into on this desktop, using floating placement");
            state.settings.update(|s| s.widget.mode = "float".into());
        }
        let pinned = pinned && monitor::supports_panel_docking();

        let taskbar_target = if pinned {
            match monitor::taskbar_for(&mon) {
                Some(bar) => {
                    if !bar.on_screen {
                        let _ = win.hide();
                        return;
                    }
                    let columns = s.providers.enabled_count();
                    let target = monitor::taskbar_position(&mon, &bar, s.widget.tray_gap, columns);
                    if target.is_none() {
                        // A vertical strip cannot host the layout at all, so
                        // this one is a real, lasting incompatibility.
                        tracing::warn!("vertical taskbar cannot host the widget, using floating placement");
                        state.settings.update(|s| s.widget.mode = "float".into());
                    }
                    target
                }
                None => {
                    // The strip is not always reachable, for instance while the
                    // session is locked or Explorer is restarting. Rewriting the
                    // preference here would quietly undo the user's choice, so
                    // this pass is simply skipped.
                    tracing::debug!("taskbar not reachable, leaving placement untouched");
                    return;
                }
            }
        } else {
            None
        };

        let (x, y, w, h) = match taskbar_target {
            Some(t) => t,
            None => {
                let logical_h = floating_height();
                let (tx, ty, w, h) = monitor::target_position(
                    &mon,
                    s.widget.edge_offset_x,
                    s.widget.edge_offset_y,
                    logical_h,
                );
                let (x, y, w, h) = monitor::clamp_or_primary(tx, ty, w, h, logical_h);

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

        if pinned
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

    let (base_x, base_y, _, _) = monitor::target_position(&mon, 0, 0, floating_height());
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

/// Shows the widget because the user asked for it, from the tray or by starting
/// the app a second time. Whatever is in front, the answer to that is to show
/// it, so this outranks fullscreen hiding until that application goes away.
///
/// Starting up is not such a request, which is why it calls `show` instead: an
/// app that comes back mid game, through the watchdog for example, has to stay
/// out of the way.
pub fn show_by_request(app: &AppHandle) {
    SHOWN_ON_PURPOSE.store(true, Ordering::SeqCst);
    show(app);
}

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
        show_by_request(app);
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
        // Asking for the widget outranks hiding it. Without this, showing it
        // from the tray while something is detected as fullscreen puts it back
        // within a third of a second, and the menu item looks broken with no
        // way to tell why.
        if SHOWN_ON_PURPOSE.load(Ordering::SeqCst) {
            return;
        }
        if !AUTO_HIDDEN.load(Ordering::SeqCst) && win.is_visible().unwrap_or(false) {
            tracing::info!("hiding the widget behind {}", fullscreen_window_description());
            let _ = win.hide();
            AUTO_HIDDEN.store(true, Ordering::SeqCst);
        }
    } else if stable_clear {
        // The override lasts for one fullscreen stretch, so the next game hides
        // the widget again without the user having to undo anything.
        SHOWN_ON_PURPOSE.store(false, Ordering::SeqCst);

        if AUTO_HIDDEN.swap(false, Ordering::SeqCst) && s.widget.visible {
            tracing::info!("fullscreen ended, restoring the widget");
            reposition(app);
            let _ = win.show();
            let _ = win.set_always_on_top(true);
            apply_ex_styles(app);
        }
    }
}

/// What the widget is hiding behind, for the log. A hidden widget with no
/// explanation is the hardest kind of fault to chase: it looks like the app is
/// broken when it is doing exactly what it was told.
#[cfg(windows)]
fn fullscreen_window_description() -> String {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowRect, GetWindowTextW};

    let raw = LAST_FULLSCREEN_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return "an unidentified fullscreen window".into();
    }
    let hwnd = HWND(raw as *mut std::ffi::c_void);

    unsafe {
        let mut class = [0u16; 128];
        let len = GetClassNameW(hwnd, &mut class);
        let class = String::from_utf16_lossy(&class[..(len.max(0) as usize).min(class.len())]);

        let mut title = [0u16; 128];
        let len = GetWindowTextW(hwnd, &mut title);
        let title = String::from_utf16_lossy(&title[..(len.max(0) as usize).min(title.len())]);

        let mut rect = RECT::default();
        let geometry = if GetWindowRect(hwnd, &mut rect).is_ok() {
            format!(
                "{},{} {}x{}",
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top
            )
        } else {
            "unknown geometry".into()
        };

        format!("{class} \"{title}\" at {geometry}")
    }
}

#[cfg(not(windows))]
fn fullscreen_window_description() -> String {
    "a fullscreen window".into()
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

            // Read first, because everything below either skips itself or waits
            // while a game is in front.
            // `anywhere` is what the providers ask about, `fullscreen` is what
            // the widget hides for. The two differ when the game is filling
            // another monitor.
            let (anywhere, fullscreen) = fullscreen_state(&app);
            FULLSCREEN_NOW.store(anywhere, Ordering::Relaxed);

            if !fullscreen {
                let taskbar_mode = app
                    .state::<Arc<AppState>>()
                    .settings
                    .get()
                    .widget
                    .taskbar_mode();
                // The topology scan calls QueryDisplayConfig, so it does not run
                // on every tick unless the strip itself has to be tracked. It is
                // skipped entirely during a game: placement cannot be applied
                // then anyway, and leaving the signature untouched means the
                // first tick afterwards still notices anything that moved.
                if taskbar_mode || tick % TOPOLOGY_EVERY == 0 {
                    // Asks the platform for a fresh copy of the layout. On
                    // Windows this is nothing; on Linux the answer lands on the
                    // next tick, because it can only be read from the thread
                    // that owns the main loop.
                    crate::monitor::refresh();
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
            // Animation stops for a game on any monitor. It is a courtesy to
            // the frames the game is drawing, and that is not a per display
            // matter.
            let motion = visible && !power_saver() && !anywhere;
            if motion != last_motion {
                last_motion = motion;
                let state = app.state::<Arc<AppState>>();
                state.set_motion_allowed(motion);
                let _ = app.emit("config", state.appearance());
            }
        }
    });
}

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
/// Everything here comes from window geometry: the foreground window is measured
/// against its monitor, and because a game keeps running when something else
/// briefly takes the foreground, the window last seen filling its monitor is
/// remembered and rechecked. Without that second part, a window flashing for a
/// fraction of a second reads as "the game ended" and the widget climbs back
/// over it.
///
/// The shell is asked one narrow question on top of that, and only one. It used
/// to be asked broadly, which is a mistake worth spelling out:
/// `SHQueryUserNotificationState` answers "may I show a notification", not "is a
/// game running". `QUNS_BUSY` and `QUNS_PRESENTATION_MODE` also cover do not
/// disturb and presentation settings, and Windows turns do not disturb on by
/// itself while gaming and leaves it on afterwards. The widget then stayed
/// hidden with nothing on screen to explain it, and showing it from the tray was
/// undone within a third of a second. Only `QUNS_RUNNING_D3D_FULL_SCREEN` is
/// trusted now, because that one really does mean an exclusive fullscreen game.
///
/// Only window geometry and that one flag are read. No other process is opened,
/// no memory is read and nothing is injected anywhere.
#[cfg(windows)]
fn fullscreen_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN};
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    if let Some(hwnd) = foreground_covering_window() {
        return Some(hwnd);
    }

    let exclusive_d3d = unsafe {
        SHQueryUserNotificationState()
            .map(|state| state == QUNS_RUNNING_D3D_FULL_SCREEN)
            .unwrap_or(false)
    };
    if exclusive_d3d {
        // Such a game does not always measure as covering its monitor, so its
        // window is remembered and rechecked like any other.
        let hwnd = unsafe { GetForegroundWindow() };
        remember_fullscreen(hwnd);
        return Some(hwnd);
    }

    remembered_covering()
}

/// Whether a fullscreen application is in front, and whether it is on the same
/// display as the widget.
///
/// On a desk with one monitor those are the same question. On a desk with more
/// than one they are not: a game filling the middle screen leaves the side
/// screen alone, and a widget sitting there is in nobody's way, so it stays.
/// Only the first answer reaches the providers, because starting a process can
/// flash a console over the game whichever monitor the widget is on.
#[cfg(windows)]
fn fullscreen_state(app: &AppHandle) -> (bool, bool) {
    let Some(hwnd) = fullscreen_window() else {
        return (false, false);
    };
    let ours = widget_monitor(app).map(|m| m.stable_id);
    let theirs = monitor_of(hwnd).map(|m| m.stable_id);
    (true, same_display(ours.as_deref(), theirs.as_deref()))
}

#[cfg(not(windows))]
fn fullscreen_state(_app: &AppHandle) -> (bool, bool) {
    (false, false)
}

/// Are those two the same monitor? An unknown answer counts as yes, which is
/// the safer half of the mistake: a widget that hides when it need not have is
/// a moment of confusion, one that stays up over a game costs the player the
/// game.
fn same_display(ours: Option<&str>, theirs: Option<&str>) -> bool {
    match (ours, theirs) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// The monitor the widget is on, read from where the window actually sits
/// rather than from the setting, because in floating placement the user can
/// drag it to another screen and the answer has to follow.
#[cfg(windows)]
fn widget_monitor(app: &AppHandle) -> Option<crate::monitor::MonitorInfo> {
    let win = widget(app)?;
    let pos = win.outer_position().ok()?;
    let size = win.outer_size().ok()?;
    let centre = crate::monitor::monitor_at(
        pos.x + size.width as i32 / 2,
        pos.y + size.height as i32 / 2,
    );
    centre.or_else(|| {
        let s = app.state::<Arc<AppState>>().settings.get();
        crate::monitor::resolve(s.widget.monitor_stable_id.as_deref()).map(|(mon, _)| mon)
    })
}

/// The monitor a window's centre sits on.
#[cfg(windows)]
fn monitor_of(hwnd: windows::Win32::Foundation::HWND) -> Option<crate::monitor::MonitorInfo> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return None;
    }
    crate::monitor::monitor_at(
        rect.left + (rect.right - rect.left) / 2,
        rect.top + (rect.bottom - rect.top) / 2,
    )
}

#[cfg(windows)]
fn remember_fullscreen(hwnd: windows::Win32::Foundation::HWND) {
    LAST_FULLSCREEN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
}

/// Is the remembered window still open, unminimised and filling its monitor?
///
/// Alt tabbing out of a game must not be enough to bring the widget back over
/// it. Closing or minimising the game is, and both are visible here: a closed
/// window fails `IsWindow`, a minimised one reports a rectangle off screen.
#[cfg(windows)]
fn remembered_covering() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindow, IsWindowVisible};

    let raw = LAST_FULLSCREEN_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return None;
    }
    let hwnd = HWND(raw as *mut std::ffi::c_void);

    let still = unsafe {
        IsWindow(Some(hwnd)).as_bool()
            && IsWindowVisible(hwnd).as_bool()
            && !IsIconic(hwnd).as_bool()
            && covers_its_monitor(hwnd)
    };
    if !still {
        LAST_FULLSCREEN_HWND.store(0, Ordering::SeqCst);
        return None;
    }
    Some(hwnd)
}

/// Does this window fill the monitor its centre sits on?
#[cfg(windows)]
unsafe fn covers_its_monitor(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return false;
    }

    let Some(mon) = monitor_of(hwnd) else {
        return false;
    };

    let b = mon.bounds;
    rect.left <= b.left + 2
        && rect.top <= b.top + 2
        && rect.right >= b.right - 2
        && rect.bottom >= b.bottom - 2
}

#[cfg(windows)]
fn foreground_covering_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, IsWindowVisible,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() || !IsWindowVisible(hwnd).as_bool() {
            return None;
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
                return None;
            }
        }

        if !covers_its_monitor(hwnd) {
            return None;
        }
        remember_fullscreen(hwnd);
        Some(hwnd)
    }
}

#[cfg(not(windows))]
fn power_saver() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::same_display;

    #[test]
    fn a_game_on_the_widgets_own_screen_hides_it() {
        assert!(same_display(Some("LG ULTRAGEAR"), Some("LG ULTRAGEAR")));
    }

    #[test]
    fn a_game_on_the_other_screen_leaves_the_widget_alone() {
        assert!(!same_display(Some("LG ULTRAGEAR"), Some("DELL U2419H")));
    }

    #[test]
    fn an_unknown_display_hides_the_widget() {
        // Either half missing means the comparison could not be made, and the
        // old behaviour was to hide, so that is what an unknown answer keeps.
        assert!(same_display(None, Some("LG ULTRAGEAR")));
        assert!(same_display(Some("LG ULTRAGEAR"), None));
        assert!(same_display(None, None));
    }
}
