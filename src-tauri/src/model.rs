//! Types shared with the frontend. Field names mirror `src/lib/types.ts`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Percentage reported by the provider's own server.
    Official,
    /// Derived from local logs.
    Estimate,
    /// Local estimate that has drifted from the last official reading, which
    /// usually means the account is also being used from another machine.
    EstimateLocalOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    Ok,
    CliNotFound,
    NoDataYet,
    PlanUnsupported,
    ReauthNeeded,
    EndpointError,
    Stale,
    LimitReached,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    FiveHour,
    Weekly,
    WeeklyOpus,
    WeeklySonnet,
}

impl WindowKind {
    pub fn key(self) -> &'static str {
        match self {
            WindowKind::FiveHour => "five_hour",
            WindowKind::Weekly => "seven_day",
            WindowKind::WeeklyOpus => "seven_day_opus",
            WindowKind::WeeklySonnet => "seven_day_sonnet",
        }
    }

    /// A snapshot older than the window it describes cannot be trusted.
    /// Written out in full on purpose: a catch-all would hand a seven day
    /// threshold to any short window added later without a word about it.
    pub fn duration(self) -> chrono::Duration {
        match self {
            WindowKind::FiveHour => chrono::Duration::hours(5),
            WindowKind::Weekly | WindowKind::WeeklyOpus | WindowKind::WeeklySonnet => {
                chrono::Duration::days(7)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowSnapshot {
    /// Percentage of the window consumed, 0 to 100.
    pub utilization: f32,
    /// Short row label when the provider states the window length itself.
    /// Codex does, and its `primary` slot is not always the five hour one, so
    /// the label rather than the field name is what the row actually means.
    pub label: Option<String>,
    pub resets_at: Option<DateTime<Utc>>,
    pub source: Source,
    /// When this value was measured, not when it was rendered.
    pub as_of: DateTime<Utc>,
    pub stale: bool,
    pub eta: Option<DateTime<Utc>>,
    /// Percentage points per hour.
    pub burn_rate: Option<f32>,
}

impl WindowSnapshot {
    pub fn official(utilization: f32, resets_at: Option<DateTime<Utc>>) -> Self {
        Self {
            utilization,
            label: None,
            resets_at,
            source: Source::Official,
            as_of: Utc::now(),
            stale: false,
            eta: None,
            burn_rate: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderSnapshot {
    pub id: &'static str,
    pub label: &'static str,
    pub enabled: bool,
    pub status: ProviderStatus,
    pub detail: Option<String>,
    pub install_url: Option<&'static str>,
    pub five_hour: Option<WindowSnapshot>,
    pub weekly: Option<WindowSnapshot>,
    pub weekly_opus: Option<WindowSnapshot>,
    pub weekly_sonnet: Option<WindowSnapshot>,
    pub plan_type: Option<String>,
    pub extra_usage: Option<f32>,
    pub tokens: Option<TokenTotals>,
}

impl ProviderSnapshot {
    pub fn empty(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            enabled: true,
            status: ProviderStatus::NoDataYet,
            detail: None,
            install_url: None,
            five_hour: None,
            weekly: None,
            weekly_opus: None,
            weekly_sonnet: None,
            plan_type: None,
            extra_usage: None,
            tokens: None,
        }
    }

    /// Highest live window, used to colour the tray badge.
    pub fn peak(&self) -> Option<f32> {
        self.windows()
            .into_iter()
            .filter(|w| !w.stale)
            .map(|w| w.utilization)
            .fold(None, |acc: Option<f32>, v| Some(acc.map_or(v, |a| a.max(v))))
    }

    pub fn all_stale(&self) -> bool {
        let windows = self.windows();
        windows.is_empty() || windows.iter().all(|w| w.stale)
    }

    fn windows(&self) -> Vec<&WindowSnapshot> {
        [
            self.five_hour.as_ref(),
            self.weekly.as_ref(),
            self.weekly_opus.as_ref(),
            self.weekly_sonnet.as_ref(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UsagePayload {
    pub providers: Vec<ProviderSnapshot>,
    pub generated_at: DateTime<Utc>,
    /// True when no supported CLI was found at all.
    pub onboarding: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppearanceConfig {
    pub theme: String,
    pub animations: bool,
    pub compact_mode: bool,
    pub show_model_weekly: bool,
    /// "float" or "taskbar".
    pub placement: String,
    /// Occlusion and power state, decided on the Rust side.
    pub motion_allowed: bool,
    pub resolved_theme: String,
}
