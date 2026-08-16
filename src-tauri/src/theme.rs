//! Windows app theme. The registry value is the source of truth and changes
//! arrive through `RegNotifyChangeKeyValue`, so nothing is polled.

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
    pub fn system_is_dark() -> bool {
        true
    }
    pub fn watch<F: Fn(bool) + Send + 'static>(_on_change: F) {}
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
