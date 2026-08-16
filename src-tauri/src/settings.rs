//! Persistent settings, stored at `%APPDATA%\FluxDock\settings.json`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetSettings {
    pub visible: bool,
    /// Persistent monitor identity. Raw coordinates are never stored, so a
    /// display coming back on a different arrangement cannot strand the window.
    pub monitor_stable_id: Option<String>,
    pub monitor_friendly_name: Option<String>,
    pub corner: String,
    pub edge_offset_x: i32,
    pub edge_offset_y: i32,
    pub compact_mode: bool,
    pub click_through: bool,
    /// "float" for a free floating window, "taskbar" to pin into the strip.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Logical gap between the widget and the notification area.
    #[serde(default = "default_tray_gap")]
    pub tray_gap: i32,
    #[serde(default = "yes")]
    pub hide_on_fullscreen: bool,
}

fn default_mode() -> String {
    "float".into()
}

fn default_tray_gap() -> i32 {
    8
}

impl Default for WidgetSettings {
    fn default() -> Self {
        Self {
            visible: true,
            monitor_stable_id: None,
            monitor_friendly_name: None,
            corner: "bottom-right".into(),
            edge_offset_x: 0,
            edge_offset_y: 0,
            compact_mode: false,
            click_through: false,
            mode: default_mode(),
            tray_gap: default_tray_gap(),
            hide_on_fullscreen: true,
        }
    }
}

impl WidgetSettings {
    pub fn taskbar_mode(&self) -> bool {
        self.mode == "taskbar"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// "system", "dark" or "light".
    pub theme: String,
    pub animations: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            animations: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollingSettings {
    pub http_interval_secs: u64,
}

impl Default for PollingSettings {
    fn default() -> Self {
        Self {
            http_interval_secs: 180,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeSettings {
    pub enabled: bool,
    /// Extra credential locations, for multi account setups.
    pub credential_paths: Vec<String>,
    pub show_model_weekly: bool,
    /// Let the CLI refresh an expired token. The command consumes a small
    /// amount of quota and creates a session log, so it is rate limited.
    pub allow_cli_refresh: bool,
}

impl Default for ClaudeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            credential_paths: Vec::new(),
            show_model_weekly: false,
            allow_cli_refresh: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexSettings {
    pub enabled: bool,
    /// Undocumented HTTP fallback, off by default.
    pub allow_http_fallback: bool,
}

impl Default for CodexSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_http_fallback: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderSettings {
    #[serde(default)]
    pub claude: ClaudeSettings,
    #[serde(default)]
    pub codex: CodexSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSettings {
    pub at_70: bool,
    pub at_90: bool,
    /// Keyed by provider, window and reset time so a toast fires once per window.
    #[serde(default)]
    pub fired: HashMap<String, bool>,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            at_70: true,
            at_90: true,
            fired: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateSettings {
    #[serde(default = "yes")]
    pub check: bool,
    pub last_check: Option<DateTime<Utc>>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastSeen {
    pub claude_seven_day_resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub schema_version: u32,
    #[serde(default)]
    pub widget: WidgetSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub polling: PollingSettings,
    #[serde(default)]
    pub providers: ProviderSettings,
    #[serde(default)]
    pub notifications: NotificationSettings,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub updates: UpdateSettings,
    #[serde(default)]
    pub last_seen: LastSeen,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            widget: Default::default(),
            appearance: Default::default(),
            polling: Default::default(),
            providers: Default::default(),
            notifications: Default::default(),
            autostart: false,
            updates: UpdateSettings {
                check: true,
                last_check: None,
            },
            last_seen: Default::default(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("FluxDock")
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

/// Single instance for the lifetime of the process. Every mutation is written
/// through immediately.
pub struct SettingsStore {
    inner: RwLock<Settings>,
    path: PathBuf,
}

impl SettingsStore {
    pub fn load() -> Self {
        let path = settings_path();
        let settings = std::fs::read_to_string(&path)
            .ok()
            .map(|s| strip_bom(&s).to_string())
            .and_then(|s| match serde_json::from_str::<Settings>(&s) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("settings.json could not be read, using defaults: {e}");
                    None
                }
            })
            .unwrap_or_default();

        let store = Self {
            inner: RwLock::new(settings),
            path,
        };
        let _ = store.persist();
        store
    }

    pub fn get(&self) -> Settings {
        self.inner.read().clone()
    }

    pub fn update<F: FnOnce(&mut Settings)>(&self, f: F) -> Settings {
        let snapshot = {
            let mut guard = self.inner.write();
            f(&mut guard);
            guard.clone()
        };
        if let Err(e) = self.persist() {
            tracing::error!("settings could not be written: {e}");
        }
        snapshot
    }

    fn persist(&self) -> Result<()> {
        let dir = self
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(&*self.inner.read())?;
        write_atomic(&self.path, json.as_bytes())
    }
}

/// The settings file is meant to be hand edited, and both Notepad and
/// PowerShell write UTF-8 with a byte order mark that serde refuses to parse.
/// Dropping it quietly avoids resetting every preference.
pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Temp file plus rename, so a crash never leaves a half written file behind.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_order_mark_does_not_reset_settings() {
        let json = "\u{feff}{\"schema_version\":1,\"widget\":{\"visible\":false,\"monitor_stable_id\":null,\"monitor_friendly_name\":null,\"corner\":\"bottom-right\",\"edge_offset_x\":0,\"edge_offset_y\":0,\"compact_mode\":false,\"click_through\":false}}";
        let parsed: Settings = serde_json::from_str(strip_bom(json)).expect("parses");
        assert!(!parsed.widget.visible);
    }

    #[test]
    fn missing_sections_fall_back_to_defaults() {
        let parsed: Settings = serde_json::from_str(r#"{"schema_version":1}"#).expect("parses");
        assert!(parsed.widget.visible);
        assert_eq!(parsed.polling.http_interval_secs, 180);
        assert!(parsed.providers.claude.enabled);
        assert!(!parsed.widget.taskbar_mode());
    }
}
