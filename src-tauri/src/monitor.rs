//! Monitor and taskbar geometry.
//!
//! Placement is always derived, never restored from stored coordinates. What
//! gets saved is a persistent monitor identity plus a corner and an offset, so
//! a display that comes back on a different arrangement cannot strand the
//! window off screen.
//!
//! The identity comes from `DISPLAYCONFIG_TARGET_DEVICE_NAME.monitorDevicePath`,
//! which carries the vendor and product codes along with the connector
//! instance. Adapter LUIDs are deliberately not part of it: they can change
//! across reboots, which would make the identity unstable.

#![cfg(windows)]

use std::ffi::c_void;

use serde::Serialize;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_GETTASKBARPOS, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

/// Not re-exported by the windows crate.
const MONITORINFOF_PRIMARY: u32 = 1;

/// Floating window size in logical pixels.
pub const LOGICAL_W: f64 = 300.0;
pub const LOGICAL_H: f64 = 88.0;

const MARGIN_X: f64 = 12.0;
const MARGIN_Y: f64 = 8.0;

/// Width of one provider column when pinned to the taskbar.
const TASKBAR_COLUMN_W: f64 = 112.0;
const TASKBAR_COLUMN_GAP: f64 = 16.0;
const TASKBAR_PADDING: f64 = 12.0;

#[derive(Debug, Clone, Serialize)]
pub struct MonitorInfo {
    pub stable_id: String,
    pub friendly_name: String,
    pub gdi_name: String,
    pub primary: bool,
    #[serde(skip)]
    pub work: RECT,
    #[serde(skip)]
    pub bounds: RECT,
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

struct EnumCtx {
    out: Vec<(HMONITOR, MONITORINFOEXW)>,
}

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
                work: info.monitorInfo.rcWork,
                bounds: info.monitorInfo.rcMonitor,
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
pub fn target_position(monitor: &MonitorInfo, offset_x: i32, offset_y: i32) -> (i32, i32, i32, i32) {
    let scale = monitor.scale();
    let w = (LOGICAL_W * scale).round() as i32;
    let h = (LOGICAL_H * scale).round() as i32;

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
pub fn clamp_or_primary(x: i32, y: i32, w: i32, h: i32) -> (i32, i32, i32, i32) {
    let area = (w as i64) * (h as i64);
    if area <= 0 {
        return (x, y, w, h);
    }
    let rect = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    let best = enumerate()
        .iter()
        .map(|m| intersect_area(&rect, &m.work))
        .max()
        .unwrap_or(0);

    if best * 2 >= area {
        return (x, y, w, h);
    }
    match primary() {
        Some(p) => target_position(&p, 0, 0),
        None => (x, y, w, h),
    }
}

/// Does this rectangle overlap the monitor's work area by at least half?
pub fn mostly_inside(monitor: &MonitorInfo, x: i32, y: i32, w: i32, h: i32) -> bool {
    let area = (w as i64) * (h as i64);
    if area <= 0 {
        return false;
    }
    let rect = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    intersect_area(&rect, &monitor.work) * 2 >= area
}

fn intersect_area(a: &RECT, b: &RECT) -> i64 {
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
    pub rect: RECT,
    /// Left edge of the notification area, estimated when it cannot be found.
    pub tray_left: i32,
    pub horizontal: bool,
    pub auto_hide: bool,
    /// False while an auto hiding taskbar is slid away.
    pub on_screen: bool,
}

struct TrayScan {
    monitor_bounds: RECT,
    found: Option<HWND>,
}

unsafe extern "system" fn secondary_tray_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowRect};

    let ctx = &mut *(lparam.0 as *mut TrayScan);
    let mut class = [0u16; 64];
    let len = GetClassNameW(hwnd, &mut class);
    if len > 0 && wide_to_string(&class[..len as usize]) == "Shell_SecondaryTrayWnd" {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() && intersect_area(&rect, &ctx.monitor_bounds) > 0 {
            ctx.found = Some(hwnd);
            return BOOL(0);
        }
    }
    BOOL(1)
}

pub fn taskbar_for(monitor: &MonitorInfo) -> Option<TaskbarInfo> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW, GetWindowRect};

    unsafe {
        let mut chosen: Option<HWND> = None;

        if let Ok(hwnd) = FindWindowW(w!("Shell_TrayWnd"), None) {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok() && intersect_area(&rect, &monitor.bounds) > 0 {
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

        let visible_area = intersect_area(&rect, &monitor.bounds);
        let total = (width as i64) * (height as i64);

        Some(TaskbarInfo {
            rect,
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

fn taskbar_autohide() -> bool {
    unsafe {
        let mut data = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        (SHAppBarMessage(ABM_GETSTATE, &mut data) as u32) & ABS_AUTOHIDE != 0
    }
}

fn taskbar_rect() -> Option<RECT> {
    unsafe {
        let mut data = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        if SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) == 0 {
            return None;
        }
        Some(data.rc)
    }
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

struct DisplayName {
    gdi_name: String,
    friendly_name: String,
    device_path: String,
}

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
#[allow(clippy::unnecessary_cast)]
pub fn hwnd_of(window: &tauri::WebviewWindow) -> Option<HWND> {
    window.hwnd().ok().map(|h| HWND(h.0 as *mut c_void))
}
