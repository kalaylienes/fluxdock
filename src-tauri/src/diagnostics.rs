//! Diagnostic report writer for the tray menu.

use std::sync::Arc;

use chrono::Utc;
use tauri::{AppHandle, Manager};

use crate::settings::data_dir;
use crate::state::AppState;

/// Writes an environment summary next to the log files and opens the folder.
/// Access tokens are never included.
pub fn save_report(app: &AppHandle) {
    let state = app.state::<Arc<AppState>>();
    let settings = state.settings.get();
    let payload = state.payload.read().clone();

    let mut out = String::new();
    out.push_str(&format!("FluxDock diagnostic report\n{}\n\n", Utc::now().to_rfc3339()));
    out.push_str(&format!("version: {}\n", env!("CARGO_PKG_VERSION")));
    out.push_str(&format!("os: {}\n\n", std::env::consts::OS));

    out.push_str("paths\n");
    if let Some(home) = dirs::home_dir() {
        for rel in [
            ".claude",
            ".claude/.credentials.json",
            ".claude/projects",
            ".codex",
            ".codex/sessions",
        ] {
            let present = home.join(rel).exists();
            out.push_str(&format!(
                "  {:<32} {}\n",
                rel,
                if present { "found" } else { "missing" }
            ));
        }
    }
    out.push_str(&format!("  data directory: {}\n\n", data_dir().display()));

    out.push_str("settings\n");
    out.push_str(&serde_json::to_string_pretty(&settings).unwrap_or_default());
    out.push_str("\n\nlast payload\n");
    out.push_str(
        &payload
            .as_ref()
            .and_then(|p| serde_json::to_string_pretty(p).ok())
            .unwrap_or_else(|| "none".into()),
    );
    out.push('\n');

    // Which usage windows an account is given varies by plan, and the parsed
    // payload above cannot show a window that was never sent. This is the raw
    // thing, so a report from a machine nobody can inspect still settles it.
    out.push_str("\nlast codex rate limit payload\n");
    out.push_str(&crate::providers::codex::last_raw_payload().unwrap_or_else(|| "none".into()));
    out.push('\n');

    #[cfg(windows)]
    {
        out.push_str("\nmonitors\n");
        for m in crate::monitor::enumerate() {
            out.push_str(&format!(
                "  {} | {} | dpi {} | primary {} | id {}\n",
                m.gdi_name, m.friendly_name, m.dpi, m.primary, m.stable_id
            ));
        }
    }

    let path = data_dir().join(format!("diagnostic-{}.txt", Utc::now().format("%Y%m%d-%H%M%S")));
    let _ = std::fs::create_dir_all(data_dir());
    match std::fs::write(&path, out) {
        Ok(_) => {
            message_box("FluxDock", &format!("Report written to\n{}", path.display()));
            crate::shell::open(&data_dir());
        }
        Err(e) => message_box("FluxDock", &format!("Report could not be written: {e}")),
    }
}

#[cfg(windows)]
fn message_box(title: &str, text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(not(windows))]
fn message_box(_title: &str, text: &str) {
    tracing::info!("{text}");
}
