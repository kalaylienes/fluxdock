//! Handing a path to the shell.
//!
//! `ShellExecuteW` is used rather than `cmd /C start` or a PowerShell one liner.
//! Both of those start a console program, and where Windows Terminal is the
//! default console host such a program gets a real window whatever flags it was
//! given. A window this app never meant to show is a window the user is right to
//! be annoyed by, so no path here starts a console at all.

use std::path::Path;

#[cfg(windows)]
pub fn open(path: &Path) {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let target = HSTRING::from(path.to_string_lossy().as_ref());
    unsafe {
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &target,
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// The desktop's own handler. `xdg-open` is the portable one; `gio open`
/// covers a GNOME session that has no xdg-utils installed.
#[cfg(not(windows))]
pub fn open(path: &Path) {
    let target = path.as_os_str();
    for (program, args) in [("xdg-open", vec![]), ("gio", vec!["open"])] {
        let started = std::process::Command::new(program)
            .args(&args)
            .arg(target)
            .spawn();
        if started.is_ok() {
            return;
        }
    }
    tracing::warn!("nothing on this system knows how to open {path:?}");
}

/// Waits for a process to disappear, then starts this executable again.
///
/// The restart menu item used to hand this to PowerShell. Doing it in the
/// binary itself keeps the whole app free of console programs, and the wait is
/// what makes a restart work at all: the single instance lock is only released
/// once the old process is gone.
#[cfg(windows)]
pub fn relaunch_after(pid: u32, timeout: std::time::Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    unsafe {
        if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(handle, timeout.as_millis() as u32);
            let _ = CloseHandle(handle);
        }
    }

    // The old process releases its single instance window as it goes, and the
    // handle above can be signalled a moment before that finishes.
    std::thread::sleep(std::time::Duration::from_millis(500));

    if let Ok(exe) = std::env::current_exe() {
        let _ = crate::providers::quiet_command(&exe.to_string_lossy()).spawn();
    }
}

/// The same wait, without a handle to wait on. `/proc/<pid>` disappears when
/// the process is reaped, which is the signal that the single instance lock has
/// been released.
#[cfg(not(windows))]
pub fn relaunch_after(pid: u32, timeout: std::time::Duration) {
    let deadline = std::time::Instant::now() + timeout;
    let entry = std::path::PathBuf::from(format!("/proc/{pid}"));
    while entry.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
}
