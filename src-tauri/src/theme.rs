//! Which of the two themes the desktop is asking for.
//!
//! On Windows the registry value is the source of truth and changes arrive
//! through `RegNotifyChangeKeyValue`, so nothing is polled.
//!
//! On Linux there is no single source. The desktop settings are asked once,
//! through `gsettings`, and the toolkit configuration files are watched for
//! changes so the answer is refreshed when one of them is written rather than
//! on a timer. Neither side polls.

#[cfg(windows)]
mod imp {
    use windows::core::w;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, RegQueryValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_VALUE_TYPE,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    const PERSONALIZE: windows::core::PCWSTR =
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");

    pub fn system_is_dark() -> bool {
        unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(HKEY_CURRENT_USER, PERSONALIZE, Some(0), KEY_READ, &mut key).is_err() {
                return true;
            }
            let mut value: u32 = 1;
            let mut size = std::mem::size_of::<u32>() as u32;
            let mut kind = REG_VALUE_TYPE::default();
            let ok = RegQueryValueExW(
                key,
                w!("AppsUseLightTheme"),
                None,
                Some(&mut kind),
                Some(&mut value as *mut u32 as *mut u8),
                Some(&mut size),
            )
            .is_ok();
            let _ = RegCloseKey(key);
            if ok {
                value == 0
            } else {
                true
            }
        }
    }

    /// Blocks on its own thread and calls back on every change.
    pub fn watch<F: Fn(bool) + Send + 'static>(on_change: F) {
        std::thread::spawn(move || unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PERSONALIZE,
                Some(0),
                KEY_READ | KEY_NOTIFY,
                &mut key,
            )
            .is_err()
            {
                return;
            }
            let Ok(event) = CreateEventW(None, true, false, None) else {
                let _ = RegCloseKey(key);
                return;
            };

            let mut last = system_is_dark();
            loop {
                if RegNotifyChangeKeyValue(key, true, REG_NOTIFY_CHANGE_LAST_SET, Some(event), true)
                    .is_err()
                {
                    break;
                }
                if WaitForSingleObject(event, u32::MAX) != WAIT_OBJECT_0 {
                    break;
                }
                let now = system_is_dark();
                if now != last {
                    last = now;
                    on_change(now);
                }
            }
            let _ = CloseHandle(HANDLE(event.0));
            let _ = RegCloseKey(key);
        });
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    /// Asked in order of authority: the desktop's own preference first, then
    /// the theme it picked, then the toolkit files, and dark when nothing
    /// answers. Dark is the fallback because the widget sits over a taskbar,
    /// and a light panel on a dark strip is the more obvious mistake.
    pub fn system_is_dark() -> bool {
        if let Some(scheme) = gsettings("color-scheme") {
            if scheme.contains("prefer-dark") {
                return true;
            }
            if scheme.contains("prefer-light") {
                return false;
            }
        }
        if let Some(theme) = gsettings("gtk-theme") {
            if !theme.is_empty() {
                return theme.to_ascii_lowercase().contains("dark");
            }
        }
        if let Some(dark) = from_gtk_settings() {
            return dark;
        }
        if let Some(dark) = from_kdeglobals() {
            return dark;
        }
        true
    }

    /// Watches the toolkit configuration files rather than polling. A desktop
    /// that changes theme writes at least one of them, and the ones that do not
    /// exist are simply not watched.
    pub fn watch<F: Fn(bool) + Send + 'static>(on_change: F) {
        let paths: Vec<PathBuf> = config_files().into_iter().filter(|p| p.exists()).collect();
        if paths.is_empty() {
            tracing::info!("no theme configuration to watch, the theme is read once");
            return;
        }

        std::thread::spawn(move || {
            use notify::RecursiveMode;
            use notify_debouncer_full::new_debouncer;

            let (tx, rx) = std::sync::mpsc::channel();
            let Ok(mut debouncer) = new_debouncer(std::time::Duration::from_millis(300), None, tx)
            else {
                return;
            };
            for path in &paths {
                if let Err(e) = debouncer.watch(path, RecursiveMode::NonRecursive) {
                    tracing::warn!("could not watch {path:?}: {e}");
                }
            }

            let mut last = system_is_dark();
            for result in rx {
                if result.is_err() {
                    continue;
                }
                let now = system_is_dark();
                if now != last {
                    last = now;
                    on_change(now);
                }
            }
            drop(debouncer);
        });
    }

    fn config_files() -> Vec<PathBuf> {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
        vec![
            config.join("gtk-3.0/settings.ini"),
            config.join("gtk-4.0/settings.ini"),
            config.join("kdeglobals"),
        ]
    }

    /// One value out of the desktop interface schema. Missing `gsettings`, a
    /// missing schema and a missing key all mean the same thing here: this
    /// desktop is not the one being asked.
    fn gsettings(key: &str) -> Option<String> {
        let out = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", key])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .trim_matches('\'')
                .to_string(),
        )
    }

    fn from_gtk_settings() -> Option<bool> {
        for path in config_files() {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim();
                if let Some(value) = line.strip_prefix("gtk-application-prefer-dark-theme") {
                    let value = value.trim_start_matches(['=', ' ']).trim();
                    return Some(value == "1" || value.eq_ignore_ascii_case("true"));
                }
                if let Some(value) = line.strip_prefix("gtk-theme-name") {
                    return Some(value.to_ascii_lowercase().contains("dark"));
                }
            }
        }
        None
    }

    fn from_kdeglobals() -> Option<bool> {
        let config = dirs::config_dir()?;
        let text = std::fs::read_to_string(config.join("kdeglobals")).ok()?;
        text.lines()
            .find_map(|line| line.trim().strip_prefix("ColorScheme="))
            .map(|scheme| scheme.to_ascii_lowercase().contains("dark"))
    }
}

pub use imp::{system_is_dark, watch};

pub fn resolve(pref: &str) -> &'static str {
    match pref {
        "dark" => "dark",
        "light" => "light",
        _ => {
            if system_is_dark() {
                "dark"
            } else {
                "light"
            }
        }
    }
}
