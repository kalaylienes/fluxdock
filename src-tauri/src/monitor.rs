//! Monitor and panel geometry.
//!
//! Placement is always derived, never restored from stored coordinates. What
//! gets saved is a persistent monitor identity plus a corner and an offset, so
//! a display that comes back on a different arrangement cannot strand the
//! window off screen.
//!
//! On Windows the identity comes from
//! `DISPLAYCONFIG_TARGET_DEVICE_NAME.monitorDevicePath`, which carries the
//! vendor and product codes along with the connector instance. Adapter LUIDs
//! are deliberately not part of it: they can change across reboots, which would
//! make the identity unstable.
//!
//! On Linux the same three facts come out of the EDID the kernel exposes under
//! `/sys/class/drm`, keyed by the connector the display is plugged into. The
//! shape of the string differs between the two platforms, which does not
//! matter: it is only ever compared with itself.
//!
//! Everything below the identity is arithmetic on a rectangle, and is shared.

use serde::Serialize;

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use windows::core::BOOL;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY,
};
#[cfg(windows)]
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_GETTASKBARPOS, ABS_AUTOHIDE, APPBARDATA,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

/// Not re-exported by the windows crate.
#[cfg(windows)]
const MONITORINFOF_PRIMARY: u32 = 1;

/// Floating window width in logical pixels.
pub const LOGICAL_W: f64 = 300.0;

/// Floating window height before the interface has measured itself. Two
/// providers with two windows each. It is deliberately not the tallest case:
/// the interface reports its real height within a frame of the first paint, and
/// a window that starts too tall shows a band of empty surface until it does.
pub const DEFAULT_LOGICAL_H: f64 = 88.0;

/// Bounds for a measured height, so a broken measurement cannot produce a
/// window that is invisible or takes over the screen.
pub const MIN_LOGICAL_H: f64 = 24.0;
pub const MAX_LOGICAL_H: f64 = 400.0;

const MARGIN_X: f64 = 12.0;
const MARGIN_Y: f64 = 8.0;

/// Width of one provider column when pinned to the taskbar.
const TASKBAR_COLUMN_W: f64 = 112.0;
const TASKBAR_COLUMN_GAP: f64 = 16.0;
const TASKBAR_PADDING: f64 = 12.0;

/// A rectangle in physical pixels.
///
/// The field names are deliberately the same as `windows::Win32::Foundation::RECT`
/// so that the Win32 bodies that were written against it read the same after the
/// conversion at the boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn from_size(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }
}

#[cfg(windows)]
impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorInfo {
    pub stable_id: String,
    pub friendly_name: String,
    /// The name the platform gives the output. `\\.\DISPLAY1` on Windows, the
    /// connector name such as `DP-1` on Linux.
    pub gdi_name: String,
    pub primary: bool,
    #[serde(skip)]
    pub work: Rect,
    #[serde(skip)]
    pub bounds: Rect,
    /// Stored as dots per inch on both platforms so that one definition of
    /// scale serves the whole module. Linux reports a factor, which is
    /// multiplied back up.
    #[serde(skip)]
    pub dpi: u32,
}

impl MonitorInfo {
    pub fn scale(&self) -> f64 {
        self.dpi as f64 / 96.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The monitor the user picked is attached.
    Pinned,
    /// A monitor was picked but is not attached; the choice is kept.
    FallbackPrimary,
    /// Nothing was picked, so the primary display is followed.
    FollowPrimary,
}

#[cfg(windows)]
struct EnumCtx {
    out: Vec<(HMONITOR, MONITORINFOEXW)>,
}

#[cfg(windows)]
unsafe extern "system" fn enum_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumCtx);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
        ctx.out.push((hmon, info));
    }
    BOOL(1)
}

#[cfg(windows)]
pub fn enumerate() -> Vec<MonitorInfo> {
    let mut ctx = EnumCtx { out: Vec::new() };
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
    }

    let names = query_display_names();

    ctx.out
        .into_iter()
        .map(|(hmon, info)| {
            let gdi_name = wide_to_string(&info.szDevice);
            let (stable_id, friendly_name) = names
                .iter()
                .find(|n| n.gdi_name == gdi_name)
                .map(|n| (n.device_path.clone(), n.friendly_name.clone()))
                .unwrap_or_else(|| (gdi_name.clone(), gdi_name.clone()));

            let dpi = unsafe {
                let mut x = 96u32;
                let mut y = 96u32;
                let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
                x.max(48)
            };

            MonitorInfo {
                stable_id,
                friendly_name: if friendly_name.trim().is_empty() {
                    gdi_name.clone()
                } else {
                    friendly_name
                },
                gdi_name,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                work: info.monitorInfo.rcWork.into(),
                bounds: info.monitorInfo.rcMonitor.into(),
                dpi,
            }
        })
        .collect()
}

pub fn primary() -> Option<MonitorInfo> {
    let all = enumerate();
    all.iter()
        .find(|m| m.primary)
        .cloned()
        .or_else(|| all.first().cloned())
}

pub fn resolve(stable_id: Option<&str>) -> Option<(MonitorInfo, Resolution)> {
    let all = enumerate();
    if all.is_empty() {
        return None;
    }
    if let Some(id) = stable_id {
        if let Some(m) = all.iter().find(|m| m.stable_id == id) {
            return Some((m.clone(), Resolution::Pinned));
        }
    }
    let fallback = all
        .iter()
        .find(|m| m.primary)
        .cloned()
        .or_else(|| all.first().cloned())?;
    let how = if stable_id.is_some() {
        Resolution::FallbackPrimary
    } else {
        Resolution::FollowPrimary
    };
    Some((fallback, how))
}

/// Bottom right of the work area, in physical pixels.
///
/// The height is passed in rather than fixed, because what the widget shows
/// decides how tall it has to be: one provider needs half of what two do, and a
/// window sized for the larger case leaves the smaller one padded with dead
/// space at both ends.
pub fn target_position(
    monitor: &MonitorInfo,
    offset_x: i32,
    offset_y: i32,
    logical_h: f64,
) -> (i32, i32, i32, i32) {
    let scale = monitor.scale();
    let w = (LOGICAL_W * scale).round() as i32;
    let h = (logical_h.clamp(MIN_LOGICAL_H, MAX_LOGICAL_H) * scale).round() as i32;

    let mut work = monitor.work;

    // With an auto hiding taskbar the work area covers the whole display, so
    // the widget would sit on top of the reveal strip.
    if taskbar_autohide() {
        if let Some(bar) = taskbar_rect() {
            let bar_h = (bar.bottom - bar.top).abs();
            let bar_w = (bar.right - bar.left).abs();
            if bar_w >= bar_h {
                if bar.top >= (monitor.bounds.top + monitor.bounds.bottom) / 2 {
                    work.bottom -= bar_h;
                } else {
                    work.top += bar_h;
                }
            } else if bar.left >= (monitor.bounds.left + monitor.bounds.right) / 2 {
                work.right -= bar_w;
            } else {
                work.left += bar_w;
            }
        }
    }

    let mx = (MARGIN_X * scale).round() as i32;
    let my = (MARGIN_Y * scale).round() as i32;

    (
        work.right - w - mx - offset_x,
        work.bottom - h - my - offset_y,
        w,
        h,
    )
}

/// Guarantees the window keeps at least half of its area on a work area,
/// falling back to the primary corner when it would not.
pub fn clamp_or_primary(x: i32, y: i32, w: i32, h: i32, logical_h: f64) -> (i32, i32, i32, i32) {
    let area = (w as i64) * (h as i64);
    if area <= 0 {
        return (x, y, w, h);
    }
    let rect = Rect::from_size(x, y, w, h);
    let best = enumerate()
        .iter()
        .map(|m| intersect_area(&rect, &m.work))
        .max()
        .unwrap_or(0);

    if best * 2 >= area {
        return (x, y, w, h);
    }
    match primary() {
        Some(p) => target_position(&p, 0, 0, logical_h),
        None => (x, y, w, h),
    }
}

/// Does this rectangle overlap the monitor's work area by at least half?
pub fn mostly_inside(monitor: &MonitorInfo, x: i32, y: i32, w: i32, h: i32) -> bool {
    let area = (w as i64) * (h as i64);
    if area <= 0 {
        return false;
    }
    let rect = Rect::from_size(x, y, w, h);
    intersect_area(&rect, &monitor.work) * 2 >= area
}

fn intersect_area(a: &Rect, b: &Rect) -> i64 {
    let l = a.left.max(b.left);
    let t = a.top.max(b.top);
    let r = a.right.min(b.right);
    let bo = a.bottom.min(b.bottom);
    if r <= l || bo <= t {
        0
    } else {
        (r - l) as i64 * (bo - t) as i64
    }
}

#[cfg(windows)]
pub fn monitor_at(x: i32, y: i32) -> Option<MonitorInfo> {
    let hmon = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    unsafe {
        if !GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            return None;
        }
    }
    let gdi = wide_to_string(&info.szDevice);
    enumerate().into_iter().find(|m| m.gdi_name == gdi)
}

/// Taskbar geometry.
///
/// The taskbar window is only ever read. The widget is not reparented into it
/// and is not registered as an appbar, so it stays an ordinary top level window
/// that survives an Explorer restart and never reserves desktop space.
#[derive(Debug, Clone, Copy)]
pub struct TaskbarInfo {
    pub rect: Rect,
    /// Left edge of the notification area, estimated when it cannot be found.
    pub tray_left: i32,
    pub horizontal: bool,
    pub auto_hide: bool,
    /// False while an auto hiding taskbar is slid away.
    pub on_screen: bool,
}

#[cfg(windows)]
struct TrayScan {
    monitor_bounds: Rect,
    found: Option<HWND>,
}

#[cfg(windows)]
unsafe extern "system" fn secondary_tray_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowRect};

    let ctx = &mut *(lparam.0 as *mut TrayScan);
    let mut class = [0u16; 64];
    let len = GetClassNameW(hwnd, &mut class);
    if len > 0 && wide_to_string(&class[..len as usize]) == "Shell_SecondaryTrayWnd" {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok()
            && intersect_area(&Rect::from(rect), &ctx.monitor_bounds) > 0
        {
            ctx.found = Some(hwnd);
            return BOOL(0);
        }
    }
    BOOL(1)
}

#[cfg(windows)]
pub fn taskbar_for(monitor: &MonitorInfo) -> Option<TaskbarInfo> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW, GetWindowRect};

    unsafe {
        let mut chosen: Option<HWND> = None;

        if let Ok(hwnd) = FindWindowW(w!("Shell_TrayWnd"), None) {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok()
                && intersect_area(&Rect::from(rect), &monitor.bounds) > 0
            {
                chosen = Some(hwnd);
            }
        }

        if chosen.is_none() {
            let mut ctx = TrayScan {
                monitor_bounds: monitor.bounds,
                found: None,
            };
            let _ = EnumWindows(Some(secondary_tray_proc), LPARAM(&mut ctx as *mut _ as isize));
            chosen = ctx.found;
        }

        let hwnd = chosen?;
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }

        // The notification area on the primary strip, the clock on secondary
        // ones. If neither is found a reasonable margin is reserved instead.
        let tray_left = [w!("TrayNotifyWnd"), w!("ClockButton")]
            .into_iter()
            .find_map(|class| {
                let child = FindWindowExW(Some(hwnd), None, class, None).ok()?;
                let mut child_rect = RECT::default();
                GetWindowRect(child, &mut child_rect).ok()?;
                (child_rect.right > child_rect.left).then_some(child_rect.left)
            })
            .unwrap_or_else(|| rect.right - (200.0 * monitor.scale()).round() as i32);

        let visible_area = intersect_area(&Rect::from(rect), &monitor.bounds);
        let total = (width as i64) * (height as i64);

        Some(TaskbarInfo {
            rect: rect.into(),
            tray_left,
            horizontal: width >= height,
            auto_hide: taskbar_autohide(),
            on_screen: total > 0 && visible_area * 2 >= total,
        })
    }
}

/// Taskbar placement grows with the number of columns instead of reserving a
/// fixed block of the strip.
pub fn taskbar_width_logical(columns: usize) -> f64 {
    let n = columns.max(1) as f64;
    TASKBAR_PADDING + n * TASKBAR_COLUMN_W + (n - 1.0) * TASKBAR_COLUMN_GAP
}

/// Left of the notification area, centred in the strip. A vertical taskbar
/// cannot fit the widget, so the caller falls back to floating placement.
pub fn taskbar_position(
    monitor: &MonitorInfo,
    bar: &TaskbarInfo,
    tray_gap_logical: i32,
    columns: usize,
) -> Option<(i32, i32, i32, i32)> {
    if !bar.horizontal {
        return None;
    }
    let scale = monitor.scale();
    let strip_h = bar.rect.bottom - bar.rect.top;
    let h = (strip_h - (4.0 * scale).round() as i32).max((24.0 * scale).round() as i32);
    let w = (taskbar_width_logical(columns) * scale).round() as i32;
    let gap = (tray_gap_logical as f64 * scale).round() as i32;

    Some((
        bar.tray_left - w - gap,
        bar.rect.top + (strip_h - h) / 2,
        w,
        h,
    ))
}

#[cfg(windows)]
fn taskbar_autohide() -> bool {
    unsafe {
        let mut data = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        (SHAppBarMessage(ABM_GETSTATE, &mut data) as u32) & ABS_AUTOHIDE != 0
    }
}

#[cfg(windows)]
fn taskbar_rect() -> Option<Rect> {
    unsafe {
        let mut data = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        if SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) == 0 {
            return None;
        }
        Some(data.rc.into())
    }
}

/// There is no shell panel to dock into on Linux. Both of these say so, which
/// is what makes `target_position` and `reposition` compile and behave without
/// a second version of either.
#[cfg(not(windows))]
fn taskbar_autohide() -> bool {
    false
}

#[cfg(not(windows))]
fn taskbar_rect() -> Option<Rect> {
    None
}

#[cfg(not(windows))]
pub fn taskbar_for(_monitor: &MonitorInfo) -> Option<TaskbarInfo> {
    None
}

/// Whether this desktop has a panel the widget can be pinned into. Only the
/// Windows taskbar is understood, so pinned placement is offered nowhere else.
pub fn supports_panel_docking() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(windows)]
struct DisplayName {
    gdi_name: String,
    friendly_name: String,
    device_path: String,
}

#[cfg(windows)]
fn query_display_names() -> Vec<DisplayName> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;

    unsafe {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            != ERROR_SUCCESS
        {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return Vec::new();
        }
        paths.truncate(path_count as usize);

        let mut out = Vec::new();
        for path in paths {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != ERROR_SUCCESS.0 as i32 {
                continue;
            }

            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            let has_target =
                DisplayConfigGetDeviceInfo(&mut target.header) == ERROR_SUCCESS.0 as i32;

            out.push(DisplayName {
                gdi_name: wide_to_string(&source.viewGdiDeviceName),
                friendly_name: if has_target {
                    wide_to_string(&target.monitorFriendlyDeviceName)
                } else {
                    String::new()
                },
                device_path: if has_target {
                    wide_to_string(&target.monitorDevicePath)
                } else {
                    String::new()
                },
            });
        }
        out
    }
}

/// Rebuilt from the raw pointer because Tauri returns the `HWND` type from its
/// own version of the windows crate, which can drift from the one used here.
#[cfg(windows)]
#[allow(clippy::unnecessary_cast)]
pub fn hwnd_of(window: &tauri::WebviewWindow) -> Option<HWND> {
    window.hwnd().ok().map(|h| HWND(h.0 as *mut c_void))
}

// ---------------------------------------------------------------- Linux side

/// Takes a copy of the display layout. On Windows this is unnecessary, because
/// `EnumDisplayMonitors` is safe to call from anywhere; on Linux the only
/// source is GDK by way of Tauri, and GDK belongs to the thread that owns the
/// main loop. The reconciler does not run there, so it reads a copy instead.
pub fn refresh() {
    #[cfg(not(windows))]
    linux::refresh();
}

/// Records the handle the Linux backend needs and takes the first copy of the
/// layout. Called once from setup, on the main thread. A no-op on Windows.
pub fn init(app: &tauri::AppHandle) {
    #[cfg(not(windows))]
    linux::init(app);
    #[cfg(windows)]
    let _ = app;
}

#[cfg(not(windows))]
pub fn enumerate() -> Vec<MonitorInfo> {
    linux::snapshot()
}

/// The display under a point, or the primary when the point is off every
/// screen. Windows answers this with `MonitorFromPoint`; here it is a search of
/// the same list everything else reads, which keeps the two consistent even
/// mid-hotplug.
#[cfg(not(windows))]
pub fn monitor_at(x: i32, y: i32) -> Option<MonitorInfo> {
    let all = enumerate();
    all.iter()
        .find(|m| {
            x >= m.bounds.left && x < m.bounds.right && y >= m.bounds.top && y < m.bounds.bottom
        })
        .cloned()
        .or_else(|| all.iter().find(|m| m.primary).cloned())
        .or_else(|| all.first().cloned())
}

#[cfg(not(windows))]
mod linux {
    use std::sync::OnceLock;

    use parking_lot::RwLock;
    use tauri::AppHandle;

    use super::{MonitorInfo, Rect};

    static APP: OnceLock<AppHandle> = OnceLock::new();
    static CACHE: RwLock<Vec<MonitorInfo>> = RwLock::new(Vec::new());

    pub fn init(app: &AppHandle) {
        let _ = APP.set(app.clone());
        // Setup runs on the main thread, so the first copy can be taken here
        // rather than posted, which matters: the very first `reposition` is
        // called before any posted work could have run.
        *CACHE.write() = read(app);
    }

    pub fn refresh() {
        let Some(app) = APP.get() else { return };
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let fresh = read(&handle);
            // An empty answer is a transient state during a mode set, not a
            // desk with no monitors on it. Keeping the previous list means a
            // hotplug cannot briefly strand the widget.
            if !fresh.is_empty() {
                *CACHE.write() = fresh;
            }
        });
    }

    pub fn snapshot() -> Vec<MonitorInfo> {
        CACHE.read().clone()
    }

    fn read(app: &AppHandle) -> Vec<MonitorInfo> {
        let primary_name = app
            .primary_monitor()
            .ok()
            .flatten()
            .and_then(|m| m.name().cloned());

        let connectors = super::connected_displays();

        app.available_monitors()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, monitor)| {
                let position = *monitor.position();
                let size = *monitor.size();
                let area = monitor.work_area();
                let name = monitor.name().cloned().unwrap_or_default();

                let display = super::match_connector(&connectors, &name);

                let stable_id = display.and_then(|d| d.identity.clone()).unwrap_or_else(|| {
                    if name.is_empty() {
                        format!("linux-monitor-{index}")
                    } else {
                        name.clone()
                    }
                });

                let friendly_name = display
                    .and_then(|d| d.friendly.clone())
                    .filter(|f| !f.trim().is_empty())
                    .unwrap_or_else(|| {
                        if name.is_empty() {
                            format!("Display {}", index + 1)
                        } else {
                            name.clone()
                        }
                    });

                MonitorInfo {
                    stable_id,
                    friendly_name,
                    gdi_name: name.clone(),
                    // Wayland refuses to say which output is primary, and the
                    // fallbacks in `primary` and `resolve` already cover a list
                    // where nothing claims it.
                    primary: primary_name.as_deref() == Some(name.as_str()),
                    work: Rect::from_size(
                        area.position.x,
                        area.position.y,
                        area.size.width as i32,
                        area.size.height as i32,
                    ),
                    bounds: Rect::from_size(
                        position.x,
                        position.y,
                        size.width as i32,
                        size.height as i32,
                    ),
                    // GTK 3 only ever reports whole numbers here, so a display
                    // running at 125% is indistinguishable from one at 100%.
                    dpi: ((monitor.scale_factor() * 96.0).round() as u32).max(48),
                }
            })
            .collect()
    }
}

/// One display the kernel says is plugged in.
#[derive(Debug, Clone, Default)]
pub struct ConnectedDisplay {
    /// Connector name as the kernel spells it, for example `HDMI-A-1`.
    pub connector: String,
    /// Vendor, product and serial, taken from the EDID.
    pub identity: Option<String>,
    /// The name printed on the monitor, when the EDID carries one.
    pub friendly: Option<String>,
}

/// Everything under `/sys/class/drm` that currently has a display on it.
#[cfg(not(windows))]
fn connected_displays() -> Vec<ConnectedDisplay> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(status) = std::fs::read_to_string(path.join("status")) else {
            continue;
        };
        if status.trim() != "connected" {
            continue;
        }

        // The directory is named `card0-DP-1`; the connector is what follows
        // the card, and the card number is not stable across boots.
        let dir = entry.file_name().to_string_lossy().into_owned();
        let connector = match dir.split_once('-') {
            Some((_, rest)) => rest.to_string(),
            None => dir.clone(),
        };

        let edid = std::fs::read(path.join("edid")).unwrap_or_default();
        out.push(ConnectedDisplay {
            connector,
            identity: edid_identity(&edid),
            friendly: edid_display_name(&edid),
        });
    }
    out
}

#[cfg(windows)]
#[allow(dead_code)]
fn connected_displays() -> Vec<ConnectedDisplay> {
    Vec::new()
}

/// Pairs a connector the kernel reported with the name the window system uses.
///
/// The two do not always agree: the kernel says `HDMI-A-1` where XRandR says
/// `HDMI-1`, so an exact match is tried first and a normalised one second.
/// Anything left unmatched simply keeps the window system's own name, which is
/// still stable for as long as the cable stays where it is.
#[cfg_attr(windows, allow(dead_code))]
fn match_connector<'a>(
    displays: &'a [ConnectedDisplay],
    name: &str,
) -> Option<&'a ConnectedDisplay> {
    if name.is_empty() {
        return None;
    }
    displays.iter().find(|d| d.connector == name).or_else(|| {
        displays
            .iter()
            .find(|d| normalise_connector(&d.connector) == normalise_connector(name))
    })
}

/// Drops the connector-type suffix the kernel adds and nobody else does, so
/// `HDMI-A-1` and `HDMI-1` compare equal.
#[cfg_attr(windows, allow(dead_code))]
fn normalise_connector(name: &str) -> String {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() == 3 && parts[1].len() == 1 {
        format!("{}-{}", parts[0], parts[2]).to_ascii_uppercase()
    } else {
        name.to_ascii_uppercase()
    }
}

/// Vendor, product and serial out of an EDID block.
///
/// Bytes 8 and 9 hold three five bit letters, big endian, with 1 meaning A.
/// Bytes 10 and 11 are the product code and 12 to 15 the serial, both little
/// endian. Together they identify the panel rather than the port, which is what
/// makes the result survive a replug into a different socket.
/// Kept out of a `cfg` gate so that its tests run on both hosts. The EDID
/// layout is byte arithmetic and does not need a Linux machine to be wrong on.
#[cfg_attr(windows, allow(dead_code))]
fn edid_identity(edid: &[u8]) -> Option<String> {
    if edid.len() < 16 || edid[..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }
    let packed = u16::from_be_bytes([edid[8], edid[9]]);
    let letter = |shift: u32| -> char {
        let value = ((packed >> shift) & 0x1F) as u8;
        if (1..=26).contains(&value) {
            (b'A' + value - 1) as char
        } else {
            '?'
        }
    };
    let vendor: String = [letter(10), letter(5), letter(0)].iter().collect();
    let product = u16::from_le_bytes([edid[10], edid[11]]);
    let serial = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);

    Some(format!("{vendor}{product:04X}-{serial:08X}"))
}

/// The name printed on the monitor, when one of the four descriptor blocks
/// carries it. Descriptors start at byte 54 and are eighteen bytes each; a
/// display name is tagged 0xFC and its text is padded with a newline.
#[cfg_attr(windows, allow(dead_code))]
fn edid_display_name(edid: &[u8]) -> Option<String> {
    if edid.len() < 126 {
        return None;
    }
    for block in 0..4 {
        let start = 54 + block * 18;
        let descriptor = &edid[start..start + 18];
        if descriptor[0..3] != [0, 0, 0] || descriptor[3] != 0xFC {
            continue;
        }
        let text: String = descriptor[5..18]
            .iter()
            .take_while(|&&b| b != 0x0A)
            .map(|&b| b as char)
            .collect();
        let text = text.trim().to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but structurally real EDID: the magic header, ACME as the
    /// vendor, product 0x1234, serial 0xDEADBEEF, and a display name
    /// descriptor in the first slot.
    fn sample_edid() -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);

        // A=1, C=3, M=13, E=5 packed as three five bit fields, big endian.
        let packed: u16 = (1 << 10) | (3 << 5) | 13;
        edid[8..10].copy_from_slice(&packed.to_be_bytes());
        edid[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        edid[12..16].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

        edid[54..57].copy_from_slice(&[0, 0, 0]);
        edid[57] = 0xFC;
        let name = b"Studio 27\x0A     ";
        edid[59..59 + name.len()].copy_from_slice(name);
        edid
    }

    #[test]
    fn an_edid_identifies_the_panel_rather_than_the_port() {
        assert_eq!(
            edid_identity(&sample_edid()).as_deref(),
            Some("ACM1234-DEADBEEF")
        );
        assert_eq!(
            edid_display_name(&sample_edid()).as_deref(),
            Some("Studio 27")
        );
    }

    /// A blank read is what an unpowered or unplugged display gives back, and
    /// it must not turn into an identity every such display would share.
    #[test]
    fn a_blank_edid_has_no_identity() {
        assert_eq!(edid_identity(&[]), None);
        assert_eq!(edid_identity(&[0u8; 128]), None);
        assert_eq!(edid_display_name(&[0u8; 128]), None);
    }

    #[test]
    fn the_kernel_and_the_window_system_spell_hdmi_differently() {
        let displays = vec![
            ConnectedDisplay {
                connector: "HDMI-A-1".into(),
                identity: Some("ACM1234-DEADBEEF".into()),
                friendly: Some("Studio 27".into()),
            },
            ConnectedDisplay {
                connector: "DP-2".into(),
                identity: Some("XYZ0001-00000001".into()),
                friendly: None,
            },
        ];

        assert_eq!(
            match_connector(&displays, "HDMI-1").and_then(|d| d.identity.clone()),
            Some("ACM1234-DEADBEEF".into())
        );
        assert_eq!(
            match_connector(&displays, "DP-2").and_then(|d| d.identity.clone()),
            Some("XYZ0001-00000001".into())
        );
        assert!(match_connector(&displays, "DP-9").is_none());
        assert!(match_connector(&displays, "").is_none());
    }

    /// Two rectangles that share an edge overlap by nothing, and a window is
    /// only on a screen if half of it is.
    #[test]
    fn touching_rectangles_do_not_overlap() {
        let a = Rect::from_size(0, 0, 100, 100);
        let b = Rect::from_size(100, 0, 100, 100);
        assert_eq!(intersect_area(&a, &b), 0);
        assert_eq!(intersect_area(&a, &Rect::from_size(50, 0, 100, 100)), 5000);
    }

    #[test]
    fn a_column_per_provider_widens_the_strip() {
        let one = taskbar_width_logical(1);
        let two = taskbar_width_logical(2);
        let three = taskbar_width_logical(3);
        assert!(three > two && two > one);
        assert_eq!(three - two, two - one);
        // A count of zero is still one column wide rather than negative.
        assert_eq!(taskbar_width_logical(0), one);
    }

    #[test]
    fn pinning_to_a_panel_is_a_windows_only_offer() {
        assert_eq!(supports_panel_docking(), cfg!(windows));
    }
}
