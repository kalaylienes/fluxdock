//! HTTP client. Proxy environment variables are handled by reqwest; the
//! Windows system proxy is picked up when they are not set.

use std::time::Duration;

pub fn build_client(user_agent: &str) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90));

    if !env_proxy_set() {
        if let Some(server) = system_proxy() {
            if let Ok(p) = reqwest::Proxy::all(&server) {
                builder = builder.proxy(p);
            }
        }
    }

    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

fn env_proxy_set() -> bool {
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .any(|k| std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false))
}

#[cfg(windows)]
fn system_proxy() -> Option<String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        REG_VALUE_TYPE,
    };

    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings"),
            Some(0),
            KEY_READ,
            &mut key,
        )
        .is_err()
        {
            return None;
        }

        let mut enabled: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let ok = RegQueryValueExW(
            key,
            w!("ProxyEnable"),
            None,
            Some(&mut kind),
            Some(&mut enabled as *mut u32 as *mut u8),
            Some(&mut size),
        )
        .is_ok();

        if !ok || enabled == 0 {
            let _ = RegCloseKey(key);
            return None;
        }

        let mut buf = [0u16; 512];
        let mut bytes = (buf.len() * 2) as u32;
        let ok = RegQueryValueExW(
            key,
            w!("ProxyServer"),
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut bytes),
        )
        .is_ok();
        let _ = RegCloseKey(key);

        if !ok {
            return None;
        }
        let len = (bytes as usize / 2).saturating_sub(1).min(buf.len());
        let raw = String::from_utf16_lossy(&buf[..len]);
        let raw = raw.trim_end_matches('\0').trim();
        if raw.is_empty() {
            return None;
        }

        // The value can be a single host or a per protocol list.
        let picked = raw
            .split(';')
            .find_map(|part| part.strip_prefix("https="))
            .or_else(|| raw.split(';').find_map(|p| p.strip_prefix("http=")))
            .unwrap_or(raw)
            .trim();

        if picked.is_empty() {
            return None;
        }
        Some(if picked.contains("://") {
            picked.to_string()
        } else {
            format!("http://{picked}")
        })
    }
}

#[cfg(not(windows))]
fn system_proxy() -> Option<String> {
    None
}
